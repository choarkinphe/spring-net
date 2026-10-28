//! Cached, endpoint-only proxy health. No dependency on AppService or business admission.
use crate::{
    proxy::{self, ProxyError, ProxyProtocol},
    proxy_settings::RouteSnapshot,
};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

pub type ContextProvider =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = (RouteSnapshot, bool)> + Send>> + Send + Sync>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Unknown,
    Disabled,
    Checking,
    Reachable,
    Failed,
    Timeout,
    Draining,
    Paused,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub status: Status,
    pub route: RouteSnapshot,
    pub checked_at: Option<String>,
    pub latency_ms: Option<f64>,
    pub proxy_addr: Option<String>,
    pub error: Option<String>,
    pub evidence: String,
}
impl Snapshot {
    fn empty(route: RouteSnapshot) -> Self {
        Self {
            status: Status::Unknown,
            route,
            checked_at: None,
            latency_ms: None,
            proxy_addr: None,
            error: None,
            evidence: "仅代理端点，未验证转发能力".into(),
        }
    }
}
struct State {
    snapshot: Snapshot,
    current: Option<CancellationToken>,
    pause_revision: u64,
}

pub struct Manager {
    control_port: Option<u16>,
    stop: CancellationToken,
    state: Mutex<State>,
    wake: Arc<Notify>,
    // This handle owns the only loop, which holds only a Weak reference back.
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    period: Duration,
    probe_timeout: Duration,
    tick: Duration,
}
impl Manager {
    pub fn new(control_port: Option<u16>, shutdown: CancellationToken) -> Self {
        Self {
            control_port,
            stop: shutdown.child_token(),
            state: Mutex::new(State {
                snapshot: Snapshot::empty(RouteSnapshot::default()),
                current: None,
                pause_revision: 0,
            }),
            wake: Arc::new(Notify::new()),
            task: tokio::sync::Mutex::new(None),
            period: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(5),
            tick: Duration::from_secs(1),
        }
    }

    /// Explicit, idempotent startup; snapshot reads never start work.
    pub async fn start(self: &Arc<Self>, provider: ContextProvider) {
        let mut task = self.task.lock().await;
        if task.is_some() || self.stop.is_cancelled() {
            return;
        }
        *task = Some(tokio::spawn(run(
            Arc::downgrade(self),
            provider,
            self.stop.clone(),
            self.control_port,
            self.period,
            self.probe_timeout,
            self.tick,
        )));
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
    }

    /// Foreground admission calls this synchronously, without waiting for DNS.
    /// The provider must report busy for the entire foreground/draining lifetime.
    pub fn pause(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pause_revision = state.pause_revision.wrapping_add(1);
        if let Some(cancel) = state.current.take() {
            cancel.cancel();
        }
        if state.snapshot.route.proxy.is_some() {
            state.snapshot.status = Status::Paused;
        }
        drop(state);
        self.wake.notify_one();
    }

    /// Drops cancellable network IO before joining. OS DNS is not cancellable:
    /// its isolated slot remains held, with at most two seconds of exit grace.
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let mut task = self.task.lock().await;
        if let Some(handle) = task.take() {
            let _ = handle.await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            while proxy::health_dns_pending() != 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

struct ProbeResult {
    result: Result<(String, Duration), ProxyError>,
    elapsed: Duration,
    evidence: &'static str,
}
type Probe = Pin<Box<dyn Future<Output = ProbeResult> + Send>>;

fn probe(
    route: RouteSnapshot,
    port: Option<u16>,
    cancel: CancellationToken,
    timeout: Duration,
) -> Probe {
    Box::pin(async move {
        let started = Instant::now();
        let mut evidence = "仅代理端点，未验证转发能力";
        let operation = async {
            let config = route.proxy.as_ref().ok_or(ProxyError::InvalidConfig)?;
            let resolved = proxy::resolve_proxy_health(config, port, &cancel, timeout).await?;
            evidence = match resolved.protocol() {
                ProxyProtocol::Http => "TCP端点可达性；HTTP零应用字节；仅代理端点，未验证转发能力",
                ProxyProtocol::Socks5 => {
                    "SOCKS5 NOAUTH greeting；未发送CONNECT；仅代理端点，未验证转发能力"
                }
            };
            let (addr, latency) =
                proxy::check_resolved_proxy_health(&resolved, &cancel, timeout).await?;
            Ok((addr.to_string(), latency))
        };
        // One deadline covers DNS + TCP + greeting, never five seconds per phase.
        let result = tokio::select! { biased;
            _ = cancel.cancelled() => Err(ProxyError::Cancelled),
            result = tokio::time::timeout(timeout, operation) => result.unwrap_or(Err(ProxyError::Timeout)),
        };
        ProbeResult {
            result,
            elapsed: started.elapsed(),
            evidence,
        }
    })
}

async fn context(
    provider: &ContextProvider,
    stop: &CancellationToken,
) -> Option<(RouteSnapshot, bool)> {
    tokio::select! { biased;
        _ = stop.cancelled() => None,
        value = provider() => Some(value),
    }
}

async fn run(
    weak: Weak<Manager>,
    provider: ContextProvider,
    stop: CancellationToken,
    control_port: Option<u16>,
    period: Duration,
    timeout: Duration,
    tick: Duration,
) {
    let mut ticker = tokio::time::interval(tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut active: Option<Probe> = None;
    let mut generation = 0u64;
    let mut probe_generation = 0u64;
    let mut seen_pause = 0u64;
    let mut next_due = Instant::now();
    loop {
        let Some(manager) = weak.upgrade() else {
            break;
        };
        let wake = manager.wake.clone();
        drop(manager); // The loop must never keep its owner alive across an await.
        let completed = tokio::select! { biased;
            _ = stop.cancelled() => break,
            _ = wake.notified() => None,
            _ = ticker.tick() => None,
            result = async { active.as_mut().expect("guarded probe").await }, if active.is_some() => Some(result),
        };
        let Some((route, busy)) = context(&provider, &stop).await else {
            break;
        };
        let Some(manager) = weak.upgrade() else {
            break;
        };
        let mut state = manager.state.lock().unwrap_or_else(|e| e.into_inner());
        let changed = state.snapshot.route != route;
        let paused = state.pause_revision != seen_pause;
        seen_pause = state.pause_revision;
        if changed || paused || busy || route.proxy.is_none() {
            if let Some(cancel) = state.current.take() {
                cancel.cancel();
            }
            active = None; // releases socket immediately; DNS guard remains in OS thread
            generation = generation.wrapping_add(1);
        }
        if changed {
            state.snapshot = Snapshot::empty(route.clone());
            next_due = Instant::now();
        }
        if route.proxy.is_none() {
            state.snapshot.status = Status::Disabled;
            continue;
        }
        // Saving settings may call pause() before publishing the new route.
        // A newly observed route gets its immediate first check unless business
        // work is actually busy; a pause for the unchanged route still defers.
        if busy || (paused && !changed) {
            state.snapshot.status = Status::Paused;
            next_due = Instant::now() + period;
            continue;
        }
        if let Some(completed) = completed {
            active = None;
            state.current = None;
            if probe_generation == generation {
                let snapshot = &mut state.snapshot;
                snapshot.checked_at = Some(chrono::Utc::now().to_rfc3339());
                snapshot.evidence = completed.evidence.into();
                snapshot.latency_ms = Some(completed.elapsed.as_secs_f64() * 1000.0);
                match completed.result {
                    Ok((addr, latency)) => {
                        snapshot.status = Status::Reachable;
                        snapshot.proxy_addr = Some(addr);
                        snapshot.latency_ms = Some(latency.as_secs_f64() * 1000.0);
                        snapshot.error = None;
                    }
                    Err(error) => {
                        snapshot.status = if matches!(error, ProxyError::Timeout) {
                            Status::Timeout
                        } else {
                            Status::Failed
                        };
                        snapshot.proxy_addr = None;
                        snapshot.error = Some(error.to_string());
                    }
                }
            }
        }
        if active.is_none() && proxy::health_dns_pending() != 0 {
            state.snapshot.status = Status::Draining;
            // No catch-up probe when the OS lookup finally returns.
            next_due = Instant::now() + period;
            continue;
        }
        if active.is_none() && Instant::now() >= next_due {
            let cancel = stop.child_token();
            state.current = Some(cancel.clone());
            state.snapshot.status = Status::Checking;
            probe_generation = generation;
            active = Some(probe(route, control_port, cancel, timeout));
            next_due = Instant::now() + period;
        }
    }
    // Drop the probe before publishing stopped state or allowing shutdown to join.
    drop(active);
    if let Some(manager) = weak.upgrade() {
        let mut state = manager.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cancel) = state.current.take() {
            cancel.cancel();
        }
        if state.snapshot.route.proxy.is_some() {
            state.snapshot.status = Status::Paused;
        }
    }
}

#[cfg(test)]
#[path = "proxy_health_tests.rs"]
mod tests;
