//! Managed public HEAD batches. The caller serializes starts against the engine.
use crate::{
    batch::{self, AttemptOutcome, BatchConfig, BatchItem, BatchReport, BatchTarget, BatchWorker},
    model::SiteList,
    policy::{self, Cooldowns},
    proxy::{self, ProxyConfig, ResolvedProxy},
    proxy_settings::RouteSnapshot,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchRoute {
    #[default]
    Direct,
    Proxy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunBatchRequest {
    pub request_id: String,
    pub list_ids: Vec<String>,
    #[serde(default)]
    pub target_ids: Vec<String>,
    pub concurrency: usize,
    pub rounds: usize,
    pub timeout_secs: u64,
    #[serde(default)]
    pub route: BatchRoute,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchSnapshot {
    pub request_id: String,
    pub config: RunBatchRequest,
    pub route: RouteSnapshot,
    pub status: String,
    pub items: Vec<BatchItem>,
    pub report: Option<BatchReport>,
    pub error: Option<String>,
}
impl BatchSnapshot {
    pub fn active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "preparing" | "running" | "stopping" | "draining"
        )
    }
}

struct Entry {
    request: RunBatchRequest,
    snapshot: Arc<Mutex<BatchSnapshot>>,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}
struct CatalogEntry {
    request: crate::catalog_batch::Request,
    snapshot: Arc<Mutex<crate::catalog_batch::Snapshot>>,
    cancel: CancellationToken,
    task: Arc<AsyncMutex<Option<JoinHandle<()>>>>,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    catalog_entries: HashMap<String, CatalogEntry>,
    current: Option<String>,
}
pub struct BatchManager {
    state: AsyncMutex<State>,
    cooldowns: Arc<Mutex<Cooldowns>>,
    cooldown_path: PathBuf,
    control_port: Option<u16>,
    shutdown: CancellationToken,
    #[cfg(test)]
    test_dns: Mutex<Option<Arc<crate::catalog_batch_worker::TestDns>>>,
    #[cfg(test)]
    test_proxy_dns: Mutex<Option<Arc<crate::catalog_batch_worker::TestDns>>>,
}
impl BatchManager {
    pub fn new(
        shared_cooldowns: (Arc<Mutex<Cooldowns>>, PathBuf),
        control_port: Option<u16>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            state: AsyncMutex::new(State::default()),
            cooldowns: shared_cooldowns.0,
            cooldown_path: shared_cooldowns.1,
            control_port,
            shutdown,
            #[cfg(test)]
            test_dns: Mutex::new(None),
            #[cfg(test)]
            test_proxy_dns: Mutex::new(None),
        }
    }
    #[cfg(test)]
    pub(crate) fn set_test_dns(&self, resolver: Arc<crate::catalog_batch_worker::TestDns>) {
        *self.test_dns.lock().unwrap() = Some(resolver);
    }
    #[cfg(test)]
    pub(crate) fn set_test_proxy_dns(&self, resolver: Arc<crate::catalog_batch_worker::TestDns>) {
        *self.test_proxy_dns.lock().unwrap() = Some(resolver);
    }
    fn normalize_request(&self, mut request: RunBatchRequest) -> Result<RunBatchRequest, String> {
        request.request_id = uuid::Uuid::parse_str(&request.request_id)
            .map_err(|_| "invalid request_id UUID")?
            .to_string();
        match (&request.route, &request.proxy) {
            (BatchRoute::Direct, None) => {}
            (BatchRoute::Proxy, Some(config)) => {
                request.proxy = Some(
                    proxy::normalize_proxy_config(config, self.control_port)
                        .map_err(|e| e.to_string())?,
                );
            }
            _ => {
                return Err(
                    "proxy route requires proxy configuration; direct route forbids it".into(),
                );
            }
        }
        Ok(request)
    }
    pub async fn existing_request(
        &self,
        request: &RunBatchRequest,
    ) -> Result<Option<BatchSnapshot>, String> {
        let request = self.normalize_request(request.clone())?;
        let state = self.state.lock().await;
        if state.catalog_entries.contains_key(&request.request_id) {
            return Err("request_id belongs to a catalog batch".into());
        }
        if let Some(entry) = state.entries.get(&request.request_id) {
            if entry.request != request {
                return Err("request_id already used with different parameters".into());
            }
            return Ok(Some(entry.snapshot.lock().unwrap().clone()));
        }
        Ok(None)
    }
    pub async fn start(
        &self,
        request: RunBatchRequest,
        lists: &[SiteList],
    ) -> Result<BatchSnapshot, String> {
        let request = self.normalize_request(request)?;
        let route = RouteSnapshot {
            proxy: request.proxy.clone(),
            ..Default::default()
        };
        self.start_with_route(request, lists, route).await
    }
    pub async fn start_with_route(
        &self,
        request: RunBatchRequest,
        lists: &[SiteList],
        mut route: RouteSnapshot,
    ) -> Result<BatchSnapshot, String> {
        let request = self.normalize_request(request)?;
        if let Some(existing) = self.existing_request(&request).await? {
            return Ok(existing);
        }
        if let Some(config) = &route.proxy {
            route.proxy = Some(
                proxy::normalize_proxy_config(config, self.control_port)
                    .map_err(|e| e.to_string())?,
            );
        }
        let original_request = request.clone();
        let mut request = request;
        request.route = if route.proxy.is_some() {
            BatchRoute::Proxy
        } else {
            BatchRoute::Direct
        };
        request.proxy = route.proxy.clone();
        let mut state = self.state.lock().await;
        if state.catalog_entries.contains_key(&request.request_id) {
            return Err("request_id belongs to a catalog batch".into());
        }
        if let Some(entry) = state.entries.get(&request.request_id) {
            if entry.request != original_request {
                return Err("request_id already used with different parameters".into());
            }
            return Ok(entry.snapshot.lock().unwrap().clone());
        }
        if self.shutdown.is_cancelled() {
            return Err("application is shutting down".into());
        }
        if state.entries.len() + state.catalog_entries.len() >= 1024
            || Self::history_bytes(&state).saturating_add(
                serde_json::to_vec(&original_request)
                    .map_err(|e| e.to_string())?
                    .len(),
            ) > 4 * 1024 * 1024
        {
            return Err("batch request history limit reached".into());
        }
        if let Some(id) = state.current.clone() {
            Self::reap_catalog(&mut state, &id).await?;
            if let Some(previous) = state.entries.get_mut(&id) {
                if previous.snapshot.lock().unwrap().active() {
                    return Err("a batch is already running or stopping".into());
                }
                if let Some(task) = previous.task.as_mut() {
                    let result = task.await;
                    previous.task = None;
                    result.map_err(|e| format!("previous batch task failed: {e}"))?;
                }
                // Keep detail intact until the replacement request is accepted.
            }
        }
        if !(1..=4).contains(&request.concurrency)
            || !(1..=3).contains(&request.rounds)
            || !(1..=120).contains(&request.timeout_secs)
            || request.list_ids.is_empty()
            || request.list_ids.len() > 100
            || request.target_ids.len() > 1000
        {
            return Err("batch limits: 1–100 lists, 1–1000 targets, concurrency 1–4, rounds 1–3, timeout 1–120 seconds".into());
        }
        let list_ids: HashSet<_> = request.list_ids.iter().collect();
        if list_ids.len() != request.list_ids.len() {
            return Err("duplicate list id".into());
        }
        for id in &request.list_ids {
            if lists.iter().filter(|l| &l.id == id).count() != 1 {
                return Err(format!("unknown or ambiguous list: {id}"));
            }
        }
        let refs: HashSet<_> = request.target_ids.iter().collect();
        if refs.len() != request.target_ids.len() {
            return Err("duplicate target reference".into());
        }
        let mut candidates = HashMap::new();
        for list in lists.iter().filter(|l| list_ids.contains(&l.id)) {
            for t in &list.targets {
                let id = format!("{}/{}", list.id, t.id);
                if candidates.insert(id, t).is_some() {
                    return Err("ambiguous target reference".into());
                }
            }
        }
        for id in &request.target_ids {
            if !candidates.contains_key(id) {
                return Err(format!("unknown qualified target: {id}"));
            }
        }
        let mut targets = Vec::new();
        let mut urls = HashSet::new();
        // Preserve list order for predictable reports.
        for list in lists.iter().filter(|l| list_ids.contains(&l.id)) {
            for t in &list.targets {
                let id = format!("{}/{}", list.id, t.id);
                if !refs.is_empty() && !refs.contains(&id) {
                    continue;
                }
                let url = policy::parse_url(&t.url)?;
                policy::check_public_url(&url)?;
                if urls.insert(url.to_string()) {
                    targets.push(BatchTarget {
                        id,
                        url: url.to_string(),
                    });
                }
                if targets.len() > 1000 {
                    return Err("too many targets".into());
                }
            }
        }
        if targets.is_empty() {
            return Err("batch has no targets".into());
        }
        policy::check_fd_budget(request.concurrency, false)?;
        if self.shutdown.is_cancelled() {
            return Err("application is shutting down".into());
        }
        let config = BatchConfig {
            targets: targets.clone(),
            concurrency: request.concurrency,
            rounds: request.rounds,
            timeout: Duration::from_secs(request.timeout_secs),
        };
        let items = (1..=request.rounds)
            .flat_map(|round| {
                targets.iter().cloned().map(move |target| BatchItem {
                    target,
                    round,
                    attempted: false,
                    status: batch::ItemStatus::Pending,
                })
            })
            .collect();
        let snapshot = Arc::new(Mutex::new(BatchSnapshot {
            request_id: request.request_id.clone(),
            config: request.clone(),
            route: route.clone(),
            status: "preparing".into(),
            items,
            report: None,
            error: None,
        }));
        let initial = snapshot.lock().unwrap().clone();
        let cancel = self.shutdown.child_token();
        let mut worker = HeadWorker {
            cooldowns: self.cooldowns.clone(),
            path: self.cooldown_path.clone(),
            blocked: Mutex::new(HashSet::new()),
            error: Mutex::new(None),
            snapshot: snapshot.clone(),
            timeout: Duration::from_secs(request.timeout_secs),
            control_port: self.control_port,
            proxy: None,
            cancel: cancel.clone(),
        };
        let token = cancel.clone();
        let task_snapshot = snapshot.clone();
        let shutdown = self.shutdown.clone();
        let task = tokio::spawn(async move {
            let result = async {
                if let Some(proxy_config) = &route.proxy {
                    worker.proxy = Some(Arc::new(
                        proxy::resolve_proxy(
                            proxy_config,
                            worker.control_port,
                            &token,
                            worker.timeout.min(Duration::from_secs(5)),
                        )
                        .await
                        .map_err(|e| e.to_string())?,
                    ));
                }
                if !token.is_cancelled() {
                    task_snapshot.lock().unwrap().status = "running".into();
                }
                batch::run_batch(config, &worker, token.clone()).await
            }
            .await;
            if policy::target_dns_pending() + proxy::proxy_dns_pending() != 0 {
                task_snapshot.lock().unwrap().status = "draining".into();
                while policy::target_dns_pending() + proxy::proxy_dns_pending() != 0 {
                    tokio::select! { biased;
                        _ = shutdown.cancelled() => {
                            task_snapshot.lock().unwrap().error = Some("application exiting with OS DNS still draining; resolver was not cancelled".into());
                            return;
                        },
                        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                    }
                }
            }
            let mut s = task_snapshot.lock().unwrap();
            match result {
                Ok(report) => {
                    let timed_out = report
                        .items
                        .iter()
                        .any(|i| i.status == batch::ItemStatus::TimedOut);
                    s.items = report.items.clone();
                    s.report = Some(report);
                    s.status = if token.is_cancelled() {
                        "stopped"
                    } else if timed_out {
                        "timed_out"
                    } else {
                        "completed"
                    }
                    .into();
                }
                Err(e) => {
                    s.error = Some(e);
                    s.status = if token.is_cancelled() {
                        "stopped"
                    } else {
                        "failed"
                    }
                    .into();
                }
            }
            if let Some(error) = worker.error.lock().unwrap().clone() {
                s.error = Some(error);
                s.status = "failed".into();
            }
        });
        let id = request.request_id.clone();
        Self::compact_current(&mut state);
        state.entries.insert(
            id.clone(),
            Entry {
                request: original_request,
                snapshot,
                cancel,
                task: Some(task),
            },
        );
        state.current = Some(id);
        Ok(initial)
    }
    // Exact normalized requests (not hashes) support collision-free idempotency.
    // Bound their aggregate retained bytes as well as entry count, since selection
    // arrays can otherwise make even detail-free history unnecessarily large.
    fn history_bytes(state: &State) -> usize {
        state
            .entries
            .values()
            .map(|e| serde_json::to_vec(&e.request).map_or(usize::MAX, |v| v.len()))
            .chain(
                state
                    .catalog_entries
                    .values()
                    .map(|e| serde_json::to_vec(&e.request).map_or(usize::MAX, |v| v.len())),
            )
            .fold(0usize, usize::saturating_add)
    }
    fn compact_current(state: &mut State) {
        if let Some(id) = &state.current {
            if let Some(entry) = state.entries.get(id) {
                let mut snapshot = entry.snapshot.lock().unwrap();
                snapshot.items.clear();
                snapshot.report = None;
            }
            if let Some(entry) = state.catalog_entries.get(id) {
                entry.snapshot.lock().unwrap().compact();
            }
        }
    }
    async fn reap_catalog(state: &mut State, id: &str) -> Result<(), String> {
        if let Some(entry) = state.catalog_entries.get_mut(id) {
            if entry.snapshot.lock().unwrap().active() {
                return Err("catalog batch is running or draining".into());
            }
            let mut handle = entry.task.lock().await;
            if let Some(task) = handle.as_mut() {
                let result = task.await;
                *handle = None;
                result.map_err(|e| format!("catalog batch task failed: {e}"))?;
            }
            // Reaping a handle is not acceptance of a replacement request.
        }
        Ok(())
    }
    pub async fn existing_catalog(
        &self,
        request: &crate::catalog_batch::Request,
    ) -> Result<Option<crate::catalog_batch::Snapshot>, String> {
        let state = self.state.lock().await;
        if state.entries.contains_key(&request.request_id) {
            return Err("request_id belongs to a legacy batch".into());
        }
        if let Some(entry) = state.catalog_entries.get(&request.request_id) {
            if &entry.request != request {
                return Err("request_id already used with different parameters".into());
            }
            return Ok(Some(entry.snapshot.lock().unwrap().clone()));
        }
        Ok(None)
    }
    pub async fn start_catalog(
        &self,
        request: crate::catalog_batch::Request,
        catalog: &crate::site_catalog::Snapshot,
        permits: &[crate::model::Permit],
    ) -> Result<crate::catalog_batch::Snapshot, String> {
        self.start_catalog_with_route(request, catalog, permits, RouteSnapshot::default())
            .await
    }
    pub async fn start_catalog_with_route(
        &self,
        mut request: crate::catalog_batch::Request,
        catalog: &crate::site_catalog::Snapshot,
        permits: &[crate::model::Permit],
        mut route: RouteSnapshot,
    ) -> Result<crate::catalog_batch::Snapshot, String> {
        request.normalize()?;
        if let Some(existing) = self.existing_catalog(&request).await? {
            return Ok(existing);
        }
        let (mut initial, targets) =
            crate::catalog_batch::freeze(&request, catalog, permits, self.control_port)?;
        if let Some(config) = &route.proxy {
            let config = proxy::normalize_proxy_config(config, self.control_port)
                .map_err(|e| e.to_string())?;
            for target in &targets {
                if let Some(permit) = &target.permit {
                    policy::proxy_route(permit, &config, self.control_port)?;
                }
            }
            route.proxy = Some(config);
        }
        initial.route = route;
        initial.status = "preparing".into();
        let mut state = self.state.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("application is shutting down".into());
        }
        if state.entries.len() + state.catalog_entries.len() >= 1024
            || Self::history_bytes(&state).saturating_add(
                serde_json::to_vec(&request)
                    .map_err(|e| e.to_string())?
                    .len(),
            ) > 4 * 1024 * 1024
        {
            return Err(
                "batch request history limit reached (1024 entries / 4 MiB normalized requests)"
                    .into(),
            );
        }
        if state.entries.contains_key(&request.request_id)
            || state.catalog_entries.contains_key(&request.request_id)
        {
            return Err("request_id was concurrently accepted".into());
        }
        if let Some(id) = state.current.clone() {
            Self::reap_catalog(&mut state, &id).await?;
            if let Some(entry) = state.entries.get_mut(&id) {
                if entry.snapshot.lock().unwrap().active() {
                    return Err("legacy batch is active".into());
                }
                if let Some(task) = entry.task.as_mut() {
                    let result = task.await;
                    entry.task = None;
                    result.map_err(|e| e.to_string())?;
                }
                // Old result is compacted only at the insertion commit point.
            }
        }
        let snapshot = Arc::new(Mutex::new(initial.clone()));
        let cancel = self.shutdown.child_token();
        let worker = crate::catalog_batch_worker::Worker {
            snapshot: snapshot.clone(),
            targets,
            cooldowns: self.cooldowns.clone(),
            path: self.cooldown_path.clone(),
            control_port: self.control_port,
            cancel: cancel.clone(),
            app_shutdown: self.shutdown.clone(),
            #[cfg(test)]
            test_dns: self.test_dns.lock().unwrap().clone(),
            #[cfg(test)]
            test_proxy_dns: self.test_proxy_dns.lock().unwrap().clone(),
        };
        let task = tokio::spawn(worker.run());
        Self::compact_current(&mut state);
        state.current = Some(request.request_id.clone());
        state.catalog_entries.insert(
            request.request_id.clone(),
            CatalogEntry {
                request,
                snapshot,
                cancel,
                task: Arc::new(AsyncMutex::new(Some(task))),
            },
        );
        Ok(initial)
    }
    pub async fn catalog_state(
        &self,
        request_id: Option<&str>,
    ) -> Result<crate::catalog_batch::StateResponse, String> {
        if let Some(id) = request_id {
            crate::catalog_batch::canonical_id(id)?;
        }
        let state = self.state.lock().await;
        let id = request_id.map(str::to_owned).or_else(|| {
            state
                .current
                .as_ref()
                .filter(|id| state.catalog_entries.contains_key(*id))
                .cloned()
        });
        if request_id.is_some_and(|id| state.entries.contains_key(id)) {
            return Err("request_id belongs to a legacy batch".into());
        }
        let snapshot = id
            .as_ref()
            .and_then(|id| state.catalog_entries.get(id))
            .map(|e| e.snapshot.lock().unwrap().clone());
        Ok(crate::catalog_batch::StateResponse {
            request_id: id,
            accepted: snapshot.is_some(),
            snapshot,
            external_busy: false,
            external_busy_sources: Vec::new(),
        })
    }
    pub async fn stop_catalog(&self, id: &str) -> Result<crate::catalog_batch::Snapshot, String> {
        crate::catalog_batch::canonical_id(id)?;
        let (snapshot, handle) = {
            let state = self.state.lock().await;
            if state.current.as_deref() != Some(id) {
                return Err("batch is not current; no work stopped".into());
            }
            let entry = state.catalog_entries.get(id).ok_or("not a catalog batch")?;
            {
                let mut s = entry.snapshot.lock().unwrap();
                if s.active() {
                    s.status = "stopping".into();
                    entry.cancel.cancel();
                }
            }
            (entry.snapshot.clone(), entry.task.clone())
        };
        let mut handle = handle.lock().await;
        if let Some(task) = handle.as_mut() {
            let result = task.await;
            *handle = None;
            if let Err(e) = result {
                let mut s = snapshot.lock().unwrap();
                s.status = "failed".into();
                s.error = Some(format!("catalog batch task failed: {e}"));
            }
        }
        let result = snapshot.lock().unwrap().clone();
        Ok(result)
    }
    pub async fn snapshot(&self) -> Option<BatchSnapshot> {
        let state = self.state.lock().await;
        state
            .entries
            .get(state.current.as_ref()?)
            .map(|e| e.snapshot.lock().unwrap().clone())
    }
    pub async fn stop(&self, request_id: &str) -> Option<BatchSnapshot> {
        let mut state = self.state.lock().await;
        let id = state.current.clone()?;
        if id != request_id {
            return None;
        }
        let entry = state.entries.get_mut(&id)?;
        {
            let mut s = entry.snapshot.lock().unwrap();
            if s.active() {
                s.status = "stopping".into();
                entry.cancel.cancel();
            }
        }
        if let Some(task) = entry.task.as_mut() {
            let result = task.await;
            entry.task = None;
            if let Err(e) = result {
                let mut s = entry.snapshot.lock().unwrap();
                s.status = "failed".into();
                s.error = Some(format!("batch task failed: {e}"));
            }
        }
        let result = entry.snapshot.lock().unwrap().clone();
        Some(result)
    }
    pub async fn settle(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if let Some(id) = state.current.clone() {
            if let Some(entry) = state.catalog_entries.get_mut(&id) {
                if entry.snapshot.lock().unwrap().active() {
                    return Err("catalog batch is still active or draining".into());
                }
                let mut handle = entry.task.lock().await;
                if let Some(task) = handle.as_mut() {
                    let result = task.await;
                    *handle = None;
                    result.map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
            let entry = state.entries.get_mut(&id).unwrap();
            if entry.snapshot.lock().unwrap().active() {
                return Err("batch is still active".into());
            }
            if let Some(task) = entry.task.as_mut() {
                let result = task.await;
                entry.task = None;
                result.map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub async fn shutdown_and_wait(&self) {
        self.shutdown.cancel();
        let id = self.state.lock().await.current.clone();
        if let Some(id) = id {
            let _ = self.stop(&id).await;
            let _ = self.stop_catalog(&id).await;
        }
    }
}

struct HeadWorker {
    cooldowns: Arc<Mutex<Cooldowns>>,
    path: PathBuf,
    blocked: Mutex<HashSet<String>>,
    error: Mutex<Option<String>>,
    snapshot: Arc<Mutex<BatchSnapshot>>,
    timeout: Duration,
    control_port: Option<u16>,
    proxy: Option<Arc<ResolvedProxy>>,
    cancel: CancellationToken,
}
impl HeadWorker {
    fn observe_headers(
        &self,
        key: &str,
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
    ) {
        if !matches!(status.as_u16(), 429 | 503) {
            return;
        }
        let seconds = policy::retry_after(
            headers
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
        );
        let mut c = self.cooldowns.lock().unwrap();
        self.blocked.lock().unwrap().insert(key.to_owned());
        if let Err(e) = c.reserve(
            key,
            Cooldowns::now().saturating_add(seconds.saturating_mul(1000)),
            &self.path,
        ) {
            *self.error.lock().unwrap() = Some(format!("cooldown persistence failed: {e}"));
        }
    }
}
impl BatchWorker for HeadWorker {
    fn on_update(&self, item: &BatchItem) {
        let mut s = self.snapshot.lock().unwrap();
        if let Some(existing) = s
            .items
            .iter_mut()
            .find(|i| i.round == item.round && i.target.id == item.target.id)
        {
            *existing = item.clone();
        }
    }
    fn admit<'a>(
        &'a self,
        target: &'a BatchTarget,
        _: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let url = policy::parse_url(&target.url)?;
            policy::check_public_url(&url)?;
            let key = policy::domain_key(&url);
            loop {
                if self.blocked.lock().unwrap().contains(&key) {
                    return Err("domain stopped after 429/503".into());
                }
                if let Some(e) = self.error.lock().unwrap().clone() {
                    return Err(e);
                }
                {
                    let mut c = self.cooldowns.lock().unwrap();
                    // Recheck while holding the cooldown lock, matching header publication order.
                    if self.blocked.lock().unwrap().contains(&key) {
                        return Err("domain stopped after 429/503".into());
                    }
                    if c.ready(&key) {
                        if let Err(e) = c.reserve_dispatch(&key, &self.path) {
                            let error = format!("cooldown persistence failed: {e}");
                            *self.error.lock().unwrap() = Some(error.clone());
                            return Err(error);
                        }
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    }
    fn execute<'a>(
        &'a self,
        target: &'a BatchTarget,
        _: usize,
    ) -> Pin<Box<dyn Future<Output = AttemptOutcome> + Send + 'a>> {
        Box::pin(async move {
            let work = async {
                let url = policy::parse_url(&target.url)?;
                policy::check_public_url(&url)?;
                let key = policy::domain_key(&url);
                let addrs = if self.proxy.is_some() {
                    policy::resolve_proxy_target(&url, None, self.control_port).await?
                } else {
                    policy::resolve(&url, None, self.control_port).await?
                };
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .connect_timeout(self.timeout.min(Duration::from_secs(5)))
                    .timeout(self.timeout)
                    .resolve_to_addrs(&policy::host(&url), &addrs)
                    .build()
                    .map_err(|e| format!("HTTP client: {e}"))?;
                if self.blocked.lock().unwrap().contains(&key)
                    || self.error.lock().unwrap().is_some()
                {
                    return Err("domain blocked or cooldown persistence failed".to_owned());
                }
                let status = if let Some(proxy) = &self.proxy {
                    crate::proxy_http::request(
                        &url,
                        addrs[0],
                        proxy,
                        reqwest::Method::HEAD,
                        self.timeout,
                        &self.cancel,
                        |status, headers| self.observe_headers(&key, status, headers),
                    )
                    .await?
                    .status
                } else {
                    let response = client
                        .head(url)
                        .send()
                        .await
                        .map_err(|e| format!("HEAD request failed: {e}"))?;
                    self.observe_headers(&key, response.status(), response.headers());
                    response.status()
                };
                if let Some(e) = self.error.lock().unwrap().clone() {
                    return Err(e);
                }
                if !status.is_success() {
                    return Err(format!("HTTP status {status}"));
                }
                Ok(())
            };
            match tokio::time::timeout(self.timeout, work).await {
                Ok(Ok(())) => AttemptOutcome::Success,
                Ok(Err(e)) => {
                    if self.proxy.is_some() && e.contains("proxy authentication required (407)") {
                        *self.error.lock().unwrap() = Some(e.clone());
                    }
                    AttemptOutcome::Failure(e)
                }
                Err(_) => AttemptOutcome::Failure("attempt timed out".into()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Target;

    fn fixture() -> (
        tempfile::TempDir,
        BatchManager,
        RunBatchRequest,
        Vec<SiteList>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let manager = BatchManager::new(
            (
                Arc::new(Mutex::new(Cooldowns::default())),
                dir.path().join("cooldowns.json"),
            ),
            None,
            CancellationToken::new(),
        );
        let request = RunBatchRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            list_ids: vec!["list".into()],
            target_ids: vec!["list/site".into()],
            concurrency: 1,
            rounds: 1,
            timeout_secs: 1,
            route: BatchRoute::Direct,
            proxy: None,
        };
        let lists = vec![SiteList {
            id: "list".into(),
            name: "list".into(),
            description: String::new(),
            builtin: false,
            targets: vec![Target {
                id: "site".into(),
                name: "site".into(),
                url: "https://example.com/".into(),
                region: String::new(),
                source: String::new(),
                checked_at: String::new(),
                probe_only: true,
            }],
        }];
        (dir, manager, request, lists)
    }
    #[tokio::test]
    async fn proxy_and_unsafe_targets_are_rejected_before_work() {
        let (_dir, manager, mut request, mut lists) = fixture();
        request.route = BatchRoute::Proxy;
        assert!(
            manager
                .start(request.clone(), &lists)
                .await
                .unwrap_err()
                .contains("requires")
        );
        request.route = BatchRoute::Direct;
        lists[0].targets[0].url = "http://127.0.0.1/".into();
        assert!(manager.start(request, &lists).await.is_err());
    }
    #[tokio::test]
    async fn proxy_head_is_numeric_pinned_and_persists_backoff_without_outbound() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let (dir, manager, mut request, mut lists) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            let mut byte = [0u8; 1];
            while !header.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
            }
            assert!(
                String::from_utf8(header)
                    .unwrap()
                    .starts_with("CONNECT 8.8.8.8:80 HTTP/1.1\r\n")
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
            }
            let header = String::from_utf8(header).unwrap();
            assert!(header.starts_with("HEAD /fixture HTTP/1.1\r\n"));
            assert!(header.to_ascii_lowercase().contains("host: 8.8.8.8\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 429 Fixture\r\nRetry-After: 120\r\nContent-Length: 999\r\n\r\n",
                )
                .await
                .unwrap();
        });
        request.route = BatchRoute::Proxy;
        request.proxy = Some(ProxyConfig {
            url: format!("http://{address}/"),
        });
        request.timeout_secs = 3;
        lists[0].targets[0].url = "http://8.8.8.8/fixture".into();
        let started = manager.start(request.clone(), &lists).await.unwrap();
        assert_eq!(started.status, "preparing");
        server.await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if !manager.snapshot().await.unwrap().active() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let snapshot = manager.snapshot().await.unwrap();
        assert!(
            matches!(&snapshot.items[0].status, batch::ItemStatus::Failure(e) if e.contains("429"))
        );
        assert!(
            !Cooldowns::load(&dir.path().join("cooldowns.json"))
                .unwrap()
                .ready("8.8.8.8")
        );
        request.proxy.as_mut().unwrap().url.pop(); // equivalent normalized config, no rerun
        assert_eq!(
            manager.start(request.clone(), &lists).await.unwrap().status,
            snapshot.status
        );
        request.proxy = Some(ProxyConfig {
            url: "http://127.0.0.1:1".into(),
        });
        assert!(
            manager
                .start(request, &lists)
                .await
                .unwrap_err()
                .contains("different parameters")
        );
        manager.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn proxy_metadata_target_never_connects_to_proxy() {
        let (_dir, manager, mut request, mut lists) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        request.route = BatchRoute::Proxy;
        request.proxy = Some(ProxyConfig {
            url: format!("http://{}", listener.local_addr().unwrap()),
        });
        lists[0].targets[0].url = "http://168.63.129.16/".into();
        manager.start(request, &lists).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if !manager.snapshot().await.unwrap().active() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let snapshot = manager.snapshot().await.unwrap();
        assert!(matches!(
            &snapshot.items[0].status,
            batch::ItemStatus::Failure(_)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        manager.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn unified_route_overrides_execution_but_not_original_request_identity() {
        let (_dir, manager, request, mut lists) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let route = RouteSnapshot {
            settings_revision: 9,
            profile_id: Some("shared".into()),
            profile_name: Some("Controlled SOCKS5".into()),
            proxy: Some(ProxyConfig {
                url: format!("socks5://{}", listener.local_addr().unwrap()),
            }),
            local_direct: false,
        };
        lists[0].targets[0].url = "http://8.8.8.8/fixture".into();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&[5, 0]).await.unwrap();
            let mut connect = [0u8; 10];
            stream.read_exact(&mut connect).await.unwrap();
            assert_eq!(connect, [5, 1, 0, 1, 8, 8, 8, 8, 0, 80]);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                .await
                .unwrap();
            let mut header = Vec::new();
            let mut byte = [0u8; 1];
            while !header.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
            }
            assert!(
                String::from_utf8(header)
                    .unwrap()
                    .starts_with("HEAD /fixture HTTP/1.1\r\n")
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let initial = manager
            .start_with_route(request.clone(), &lists, route.clone())
            .await
            .unwrap();
        assert_eq!(initial.status, "preparing");
        assert_eq!(initial.config.route, BatchRoute::Proxy);
        assert_eq!(initial.route, route);
        server.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.snapshot().await.unwrap().active() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let existing = manager.existing_request(&request).await.unwrap().unwrap();
        assert_eq!(existing.route, route);
        assert_eq!(existing.items[0].status, batch::ItemStatus::Success);
        // Malformed newly-selected route is irrelevant to an already accepted ID.
        let repeated = manager
            .start_with_route(
                request.clone(),
                &[],
                RouteSnapshot {
                    proxy: Some(ProxyConfig {
                        url: "invalid".into(),
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(repeated.route, route);
        let mut changed = request;
        changed.rounds = 2;
        assert!(
            manager
                .existing_request(&changed)
                .await
                .unwrap_err()
                .contains("different parameters")
        );
        manager.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn catalog_stop_remains_observable_and_joinable_when_caller_is_aborted() {
        let (_dir, manager, _, _) = fixture();
        let request: crate::catalog_batch::Request = serde_json::from_value(serde_json::json!({
            "request_id":uuid::Uuid::new_v4().to_string(),"catalog_revision":1,"group_ids":["g"],"site_ids":[],"mode":"public_http","permit_bindings":[],"concurrency":1,"connection_limit":1,"queue_capacity":1,"attempts_per_site":1,"start_rate":1,"request_timeout_secs":1,"batch_timeout_secs":1,"confirmed_authorized":false
        })).unwrap();
        let snapshot = Arc::new(Mutex::new(crate::catalog_batch::Snapshot {
            request_id: request.request_id.clone(),
            config: request.clone(),
            route: Default::default(),
            status: "running".into(),
            sites: vec![],
            attempts: vec![],
            counts: Default::default(),
            error: None,
            details_retained: true,
        }));
        let cancel = manager.shutdown.child_token();
        let token = cancel.clone();
        let observed = cancel.clone();
        let task_snapshot = snapshot.clone();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = finished.clone();
        let task = tokio::spawn(async move {
            token.cancelled().await;
            tokio::time::sleep(Duration::from_millis(40)).await;
            task_snapshot.lock().unwrap().status = "cancelled".into();
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        {
            let mut state = manager.state.lock().await;
            state.current = Some(request.request_id.clone());
            state.catalog_entries.insert(
                request.request_id.clone(),
                CatalogEntry {
                    request: request.clone(),
                    snapshot,
                    cancel,
                    task: Arc::new(AsyncMutex::new(Some(task))),
                },
            );
        }
        let manager = Arc::new(manager);
        let stopping = manager.clone();
        let id = request.request_id.clone();
        let caller = tokio::spawn(async move { stopping.stop_catalog(&id).await });
        observed.cancelled().await;
        let state = tokio::time::timeout(Duration::from_millis(20), manager.catalog_state(None))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.snapshot.unwrap().status, "stopping");
        caller.abort();
        let _ = caller.await;
        manager.shutdown_and_wait().await;
        assert!(finished.load(std::sync::atomic::Ordering::SeqCst));
    }
    #[tokio::test]
    async fn rejected_replacement_preserves_previous_legacy_and_catalog_details() {
        let (_dir, manager, request, lists) = fixture();
        let item = BatchItem {
            target: BatchTarget {
                id: "saved".into(),
                url: "https://example.com/".into(),
            },
            round: 1,
            attempted: true,
            status: batch::ItemStatus::Success,
        };
        let snapshot = Arc::new(Mutex::new(BatchSnapshot {
            request_id: request.request_id.clone(),
            config: request.clone(),
            route: Default::default(),
            status: "completed".into(),
            items: vec![item.clone()],
            report: Some(BatchReport {
                complete: true,
                items: vec![item],
            }),
            error: None,
        }));
        {
            let mut state = manager.state.lock().await;
            state.current = Some(request.request_id.clone());
            state.entries.insert(
                request.request_id.clone(),
                Entry {
                    request: request.clone(),
                    snapshot,
                    cancel: CancellationToken::new(),
                    task: None,
                },
            );
        }
        let mut bad = request.clone();
        bad.request_id = uuid::Uuid::new_v4().to_string();
        bad.concurrency = 0;
        assert!(manager.start(bad, &lists).await.is_err());
        let old = manager.snapshot().await.unwrap();
        assert_eq!(old.items.len(), 1);
        assert!(old.report.is_some());
        let catalog_request:crate::catalog_batch::Request=serde_json::from_value(serde_json::json!({"request_id":uuid::Uuid::new_v4().to_string(),"catalog_revision":1,"group_ids":["g"],"site_ids":[],"mode":"public_http","permit_bindings":[],"concurrency":1,"connection_limit":1,"queue_capacity":1,"attempts_per_site":1,"start_rate":1,"request_timeout_secs":1,"batch_timeout_secs":1,"confirmed_authorized":false})).unwrap();
        let catalog_snapshot = Arc::new(Mutex::new(crate::catalog_batch::Snapshot {
            request_id: catalog_request.request_id.clone(),
            config: catalog_request.clone(),
            route: Default::default(),
            status: "completed".into(),
            sites: vec![Default::default()],
            attempts: vec![],
            counts: Default::default(),
            error: None,
            details_retained: true,
        }));
        {
            let mut state = manager.state.lock().await;
            state.current = Some(catalog_request.request_id.clone());
            state.catalog_entries.insert(
                catalog_request.request_id.clone(),
                CatalogEntry {
                    request: catalog_request.clone(),
                    snapshot: catalog_snapshot,
                    cancel: CancellationToken::new(),
                    task: Arc::new(AsyncMutex::new(None)),
                },
            );
        }
        let mut bad = request;
        bad.request_id = uuid::Uuid::new_v4().to_string();
        bad.concurrency = 0;
        assert!(manager.start(bad, &lists).await.is_err());
        let old = manager.catalog_state(None).await.unwrap().snapshot.unwrap();
        assert!(old.details_retained);
        assert_eq!(old.sites.len(), 1);
        let mut bad = catalog_request;
        bad.request_id = uuid::Uuid::new_v4().to_string();
        assert!(
            manager
                .start_catalog(
                    bad,
                    &crate::site_catalog::Snapshot {
                        version: 2,
                        revision: 99,
                        sites: vec![],
                        applications: vec![],
                        groups: vec![]
                    },
                    &[]
                )
                .await
                .is_err()
        );
        let old = manager.catalog_state(None).await.unwrap().snapshot.unwrap();
        assert!(old.details_retained);
        assert_eq!(old.sites.len(), 1);
    }
    #[tokio::test]
    async fn aborted_stop_keeps_task_joinable_for_shutdown() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_dir, manager, request, _) = fixture();
        let finished = Arc::new(AtomicBool::new(false));
        let cancel = manager.shutdown.child_token();
        let task_cancel = cancel.clone();
        let observe_cancel = cancel.clone();
        let task_finished = finished.clone();
        let task = tokio::spawn(async move {
            task_cancel.cancelled().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            task_finished.store(true, Ordering::SeqCst);
        });
        let snapshot = Arc::new(Mutex::new(BatchSnapshot {
            request_id: request.request_id.clone(),
            config: request.clone(),
            route: Default::default(),
            status: "running".into(),
            items: vec![],
            report: None,
            error: None,
        }));
        {
            let mut state = manager.state.lock().await;
            state.current = Some(request.request_id.clone());
            state.entries.insert(
                request.request_id.clone(),
                Entry {
                    request,
                    snapshot,
                    cancel,
                    task: Some(task),
                },
            );
        }
        let stopping_manager = Arc::new(manager);
        let stopping_id = stopping_manager.state.lock().await.current.clone().unwrap();
        let stopper = {
            let manager = stopping_manager.clone();
            tokio::spawn(async move { manager.stop(&stopping_id).await })
        };
        observe_cancel.cancelled().await;
        stopper.abort();
        let _ = stopper.await;
        stopping_manager.shutdown_and_wait().await;
        assert!(finished.load(Ordering::SeqCst));
        let state = stopping_manager.state.lock().await;
        assert!(state.entries.values().next().unwrap().task.is_none());
    }

    #[tokio::test]
    async fn admission_uses_shared_cooldown_and_blocked_domains() {
        let (dir, manager, request, _) = fixture();
        let snapshot = Arc::new(Mutex::new(BatchSnapshot {
            request_id: String::new(),
            config: request,
            route: Default::default(),
            status: "running".into(),
            items: vec![],
            report: None,
            error: None,
        }));
        let worker = HeadWorker {
            cooldowns: manager.cooldowns.clone(),
            path: dir.path().join("cooldowns.json"),
            blocked: Mutex::new(HashSet::new()),
            error: Mutex::new(None),
            snapshot,
            timeout: Duration::from_secs(1),
            control_port: None,
            proxy: None,
            cancel: CancellationToken::new(),
        };
        let target = BatchTarget {
            id: "a".into(),
            url: "https://example.com/".into(),
        };
        worker.admit(&target, 1).await.unwrap();
        assert!(!manager.cooldowns.lock().unwrap().ready("example.com"));
        assert!(!Cooldowns::load(&worker.path).unwrap().ready("example.com"));
        worker.blocked.lock().unwrap().insert("example.com".into());
        assert!(worker.admit(&target, 2).await.is_err());
    }
}
