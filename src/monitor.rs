//! Explicitly started, serial monitoring scheduler. This module never performs network I/O.
//! The injected worker must enforce target policy, pinned resolution, routing and shared cooldowns.
use crate::storage;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::Notify, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(test)]
#[path = "monitor/tests.rs"]
mod tests;

const MAX_TARGETS: usize = 1000;
const MAX_BYTES: u64 = 1_048_576;
const HISTORY: usize = 20;
const LEASE: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MonitorTarget {
    pub id: String,
    pub name: String,
    pub group: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub allow_lan: bool,
    pub interval_secs: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorConfig {
    pub version: u32,
    pub revision: u64,
    pub targets: Vec<MonitorTarget>,
}
impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            targets: vec![],
        }
    }
}

/// Results are evidence reported by the worker, not a claim of application health.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorOutcome {
    pub success: bool,
    pub tcp_ok: Option<bool>,
    pub tls_ok: Option<bool>,
    pub elapsed_ms: Option<u64>,
    pub detail: String,
    pub path: String,
}
pub trait MonitorWorker: Send + Sync + 'static {
    /// Dropping the returned future must cancel and release all resources. The worker must
    /// reserve shared cooldowns before I/O and must never initiate unapproved network traffic.
    fn check(
        &self,
        target: MonitorTarget,
    ) -> Pin<Box<dyn Future<Output = MonitorOutcome> + Send + '_>>;
    fn fatal_error(&self) -> bool {
        false
    }
    /// Wait for uncancellable resources after the check future was dropped.
    fn drain(&self, _shutdown: CancellationToken) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorSample {
    pub sequence: u64,
    pub checked_at_ms: u64,
    pub outcome: MonitorOutcome,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TargetStatus {
    pub paused: bool,
    pub waiting: bool,
    pub consecutive_failures: u64,
    pub history_dropped: u64,
    pub history: VecDeque<MonitorSample>,
    pub last_error: Option<MonitorSample>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    pub config: MonitorConfig,
    pub active: bool,
    pub draining: bool,
    pub generation: Option<Uuid>,
    pub targets: HashMap<String, TargetStatus>,
}
/// Chosen only by the trusted transport, never directly by browser input.
#[derive(Clone, Copy, Debug)]
pub enum MonitorLifetime {
    Browser,
    Desktop,
}
struct State {
    config: MonitorConfig,
    statuses: HashMap<String, TargetStatus>,
    active: bool,
    draining: bool,
    generation: Option<Uuid>,
    seen: HashSet<Uuid>,
    lease: Option<Instant>,
    cancel: Option<CancellationToken>,
    task: Option<JoinHandle<()>>,
    sequence: u64,
    due: HashMap<String, Instant>,
    in_flight: Option<(String, CancellationToken)>,
}
struct Inner {
    dir: PathBuf,
    control_port: Option<u16>,
    shutdown: CancellationToken,
    state: Mutex<State>,
    notify: Notify,
}
#[derive(Clone)]
pub struct MonitorManager {
    inner: Arc<Inner>,
}

fn validate(config: &MonitorConfig) -> Result<(), String> {
    if config.version != 1 || config.targets.len() > MAX_TARGETS {
        return Err("unsupported monitor version or too many targets".into());
    }
    let mut ids = HashSet::new();
    for t in &config.targets {
        if t.id.is_empty()
            || t.id.len() > 100
            || !t
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || !ids.insert(&t.id)
            || t.name.trim().is_empty()
            || t.name.len() > 200
            || t.group.len() > 200
            || t.port == 0
            || t.interval_secs < 60
            || t.interval_secs > 31_536_000
            || t.host.is_empty()
            || t.host.len() > 253
            || t.host.contains(|c: char| {
                c.is_whitespace() || matches!(c, '/' | '@' | '?' | '#' | '*' | '[' | ']' | '%')
            })
        {
            return Err("invalid monitor target".into());
        }
        // Host parser accepts only a bare DNS name or IP; reject malformed IPv6 and URLs.
        if t.host.parse::<std::net::IpAddr>().is_err()
            && (t.host.contains(':') || url::Host::parse(&t.host).is_err())
        {
            return Err("invalid monitor host".into());
        }
    }
    Ok(())
}
impl MonitorManager {
    pub fn new(
        data: impl AsRef<Path>,
        control_port: Option<u16>,
        shutdown: CancellationToken,
    ) -> Result<Self, String> {
        let dir = data.as_ref().to_path_buf();
        let path = dir.join("monitor.json");
        let config = if path.exists() {
            serde_json::from_slice::<MonitorConfig>(&storage::read_bounded(&path, MAX_BYTES)?)
                .map_err(|e| format!("monitor.json corrupt; not overwritten: {e}"))?
        } else {
            MonitorConfig::default()
        };
        validate(&config)?;
        let statuses = config
            .targets
            .iter()
            .map(|t| (t.id.clone(), TargetStatus::default()))
            .collect();
        Ok(Self {
            inner: Arc::new(Inner {
                dir,
                control_port,
                shutdown,
                notify: Notify::new(),
                state: Mutex::new(State {
                    config,
                    statuses,
                    active: false,
                    draining: false,
                    generation: None,
                    seen: HashSet::new(),
                    lease: None,
                    cancel: None,
                    task: None,
                    sequence: 0,
                    due: HashMap::new(),
                    in_flight: None,
                }),
            }),
        })
    }
    pub fn control_port(&self) -> Option<u16> {
        self.inner.control_port
    }
    pub fn snapshot(&self) -> MonitorSnapshot {
        let s = self.inner.state.lock().unwrap();
        MonitorSnapshot {
            config: s.config.clone(),
            active: s.active,
            draining: s.draining,
            generation: s.generation,
            targets: s.statuses.clone(),
        }
    }
    /// Compare-and-swap revision; persists before changing memory, never starts monitoring.
    pub fn save_config(
        &self,
        expected_revision: u64,
        targets: Vec<MonitorTarget>,
    ) -> Result<MonitorConfig, String> {
        let mut s = self.inner.state.lock().unwrap();
        if s.active || s.draining {
            return Err("stop monitoring before changing configuration".into());
        }
        if s.config.revision != expected_revision {
            return Err("monitor revision conflict".into());
        }
        let config = MonitorConfig {
            version: 1,
            revision: expected_revision
                .checked_add(1)
                .ok_or("revision exhausted")?,
            targets,
        };
        validate(&config)?;
        let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("monitor configuration exceeds size limit".into());
        }
        storage::atomic_bytes(&self.inner.dir.join("monitor.json"), &bytes)?;
        s.statuses = config
            .targets
            .iter()
            .map(|t| (t.id.clone(), TargetStatus::default()))
            .collect();
        s.due.clear();
        s.config = config.clone();
        Ok(config)
    }
    /// The service must hold its shared admission gate while calling start.
    pub fn start(
        &self,
        request: Uuid,
        revision: u64,
        worker: Arc<dyn MonitorWorker>,
        lifetime: MonitorLifetime,
    ) -> Result<Uuid, String> {
        let mut s = self.inner.state.lock().unwrap();
        if s.generation == Some(request) && s.active {
            if revision != s.config.revision
                || s.lease.is_some() != matches!(lifetime, MonitorLifetime::Browser)
            {
                return Err("monitor request conflicts with original start".into());
            }
            if s.lease.is_some_and(|deadline| Instant::now() >= deadline)
                || self.inner.shutdown.is_cancelled()
            {
                return Err("monitor request expired".into());
            }
            return Ok(request);
        }
        if s.seen.contains(&request) {
            return Err("monitor request already completed".into());
        }
        if s.seen.len() >= 4096 {
            return Err("monitor request history full; restart application".into());
        }
        if s.active || s.draining {
            return Err("monitor is active or draining".into());
        }
        if self.inner.shutdown.is_cancelled() {
            return Err("application is shutting down".into());
        }
        if revision != s.config.revision {
            return Err("monitor revision conflict".into());
        }
        if s.config.targets.is_empty() {
            return Err("no monitoring targets".into());
        }
        s.seen.insert(request);
        s.generation = Some(request);
        s.active = true;
        s.lease = match lifetime {
            MonitorLifetime::Browser => Some(Instant::now() + LEASE),
            MonitorLifetime::Desktop => None,
        };
        let cancel = self.inner.shutdown.child_token();
        s.cancel = Some(cancel.clone());
        let target_ids: Vec<_> = s.config.targets.iter().map(|t| t.id.clone()).collect();
        for id in target_ids {
            s.due.entry(id).or_insert_with(Instant::now);
        }
        for v in s.statuses.values_mut() {
            v.paused = false;
            v.waiting = true;
        }
        let inner = self.inner.clone();
        s.task = Some(tokio::spawn(async move {
            run(inner, request, worker, cancel).await;
        }));
        Ok(request)
    }
    pub fn validate_retry(
        &self,
        request: Uuid,
        revision: u64,
        lifetime: MonitorLifetime,
    ) -> Result<Option<MonitorSnapshot>, String> {
        let s = self.inner.state.lock().unwrap();
        if s.generation != Some(request) || !s.active {
            return Ok(None);
        }
        if revision != s.config.revision
            || s.lease.is_some() != matches!(lifetime, MonitorLifetime::Browser)
            || s.lease.is_some_and(|deadline| Instant::now() >= deadline)
            || self.inner.shutdown.is_cancelled()
        {
            return Err("monitor request conflicts with original start or expired".into());
        }
        drop(s);
        Ok(Some(self.snapshot()))
    }
    pub fn heartbeat(&self, generation: Uuid) -> Result<(), String> {
        let mut s = self.inner.state.lock().unwrap();
        if !s.active || s.generation != Some(generation) {
            return Err("stale monitor generation".into());
        }
        match s.lease {
            Some(deadline) if Instant::now() < deadline => s.lease = Some(Instant::now() + LEASE),
            _ => return Err("lease expired or not a browser lease".into()),
        }
        drop(s);
        self.inner.notify.notify_one();
        Ok(())
    }
    pub fn set_paused(&self, generation: Uuid, target: &str, paused: bool) -> Result<(), String> {
        let mut s = self.inner.state.lock().unwrap();
        if !s.active || s.generation != Some(generation) {
            return Err("stale monitor generation".into());
        }
        let status = s.statuses.get_mut(target).ok_or("unknown target")?;
        status.paused = paused;
        status.waiting = !paused;
        if paused
            && let Some((id, cancel)) = &s.in_flight
            && id == target
        {
            cancel.cancel();
        }
        drop(s);
        self.inner.notify.notify_one();
        Ok(())
    }
    /// Cancel and wait for the worker to be dropped before releasing the service admission gate.
    pub async fn stop(&self, generation: Uuid) -> Result<(), String> {
        loop {
            let draining = {
                let s = self.inner.state.lock().unwrap();
                if s.generation != Some(generation) {
                    return Err("stale monitor generation".into());
                }
                if let Some(c) = &s.cancel {
                    c.cancel();
                }
                s.draining
            };
            if !draining {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let handle = {
            let mut s = self.inner.state.lock().unwrap();
            if s.generation != Some(generation) {
                return Err("stale monitor generation".into());
            }
            s.draining = true;
            s.active = false;
            if let Some(c) = &s.cancel {
                c.cancel();
            }
            s.task.take()
        };
        let inner = self.inner.clone();
        let cleanup = tokio::spawn(async move {
            let result = if let Some(h) = handle {
                h.await.map_err(|e| format!("monitor task failed: {e}"))
            } else {
                Ok(())
            };
            let mut s = inner.state.lock().unwrap();
            s.draining = false;
            s.cancel = None;
            result
        });
        cleanup
            .await
            .map_err(|e| format!("monitor cleanup failed: {e}"))?
    }
    pub async fn shutdown_and_wait(&self) -> Result<(), String> {
        let generation = self.snapshot().generation;
        if let Some(generation) = generation {
            self.stop(generation).await
        } else {
            Ok(())
        }
    }
}
async fn run(
    inner: Arc<Inner>,
    generation: Uuid,
    worker: Arc<dyn MonitorWorker>,
    cancel: CancellationToken,
) {
    loop {
        let (next, deadline, lease) = {
            let s = inner.state.lock().unwrap();
            if !s.active || s.generation != Some(generation) {
                break;
            }
            if s.lease.is_some_and(|x| Instant::now() >= x) {
                break;
            }
            let next = s
                .config
                .targets
                .iter()
                .filter(|t| !s.statuses[&t.id].paused)
                .filter_map(|t| s.due.get(&t.id).map(|d| (t.clone(), *d)))
                .min_by_key(|(_, d)| *d);
            let deadline = next.as_ref().map(|(_, d)| *d);
            (next.map(|(t, _)| t), deadline, s.lease)
        };
        let wake = match (deadline, lease) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if deadline.is_some_and(|d| d <= Instant::now()) {
            let t = next.unwrap();
            let target_cancel = CancellationToken::new();
            {
                let mut s = inner.state.lock().unwrap();
                if !s.active || s.statuses[&t.id].paused {
                    continue;
                }
                s.statuses.get_mut(&t.id).unwrap().waiting = false;
                s.in_flight = Some((t.id.clone(), target_cancel.clone()));
                s.due.insert(
                    t.id.clone(),
                    Instant::now() + Duration::from_secs(t.interval_secs),
                );
            }
            let outcome = tokio::select! { biased; _ = cancel.cancelled() => break, _ = wait_for_lease(inner.clone(), generation) => break, _ = target_cancel.cancelled() => { let mut s = inner.state.lock().unwrap(); s.in_flight = None; continue; }, result = worker.check(t.clone()) => result };
            let mut s = inner.state.lock().unwrap();
            s.in_flight = None;
            if !s.active
                || s.generation != Some(generation)
                || cancel.is_cancelled()
                || s.lease.is_some_and(|d| Instant::now() >= d)
            {
                break;
            }
            s.sequence = s.sequence.saturating_add(1);
            let sample = MonitorSample {
                sequence: s.sequence,
                checked_at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
                outcome,
            };
            let status = s.statuses.get_mut(&t.id).unwrap();
            if sample.outcome.success {
                status.consecutive_failures = 0;
            } else {
                status.consecutive_failures = status.consecutive_failures.saturating_add(1);
                status.last_error = Some(sample.clone());
            }
            status.history.push_back(sample);
            if status.history.len() > HISTORY {
                status.history.pop_front();
                status.history_dropped += 1;
            }
            status.waiting = !status.paused;
            s.due
                .insert(t.id, Instant::now() + Duration::from_secs(t.interval_secs));
            if worker.fatal_error() {
                break;
            }
        } else {
            tokio::select! { _ = cancel.cancelled() => break, _ = inner.notify.notified() => {}, _ = async { if let Some(w) = wake { tokio::time::sleep_until(w).await } else { std::future::pending::<()>().await } } => {} }
        }
    }
    {
        let mut s = inner.state.lock().unwrap();
        if s.generation == Some(generation) {
            s.active = false;
            s.draining = true;
            s.in_flight = None;
        }
    }
    worker.drain(inner.shutdown.clone()).await;
    let mut s = inner.state.lock().unwrap();
    if s.generation == Some(generation) {
        s.active = false;
        if s.task.is_some() {
            s.draining = false;
        }
        for v in s.statuses.values_mut() {
            v.waiting = false;
        }
    }
}
async fn wait_for_lease(inner: Arc<Inner>, generation: Uuid) {
    loop {
        let lease = {
            let s = inner.state.lock().unwrap();
            if s.generation != Some(generation) || !s.active {
                return;
            }
            s.lease
        };
        match lease {
            Some(deadline) => {
                tokio::select! { _ = tokio::time::sleep_until(deadline) => { let s = inner.state.lock().unwrap(); if s.lease.is_some_and(|x| x <= Instant::now()) { return; } }, _ = inner.notify.notified() => {} }
            }
            None => std::future::pending::<()>().await,
        }
    }
}
