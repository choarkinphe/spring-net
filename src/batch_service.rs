//! Managed public HEAD batches. The caller serializes starts against the engine.
use crate::{
    batch::{self, AttemptOutcome, BatchConfig, BatchItem, BatchReport, BatchTarget, BatchWorker},
    model::SiteList,
    policy::{self, Cooldowns},
    proxy::{self, ProxyConfig, ResolvedProxy},
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
    pub status: String,
    pub items: Vec<BatchItem>,
    pub report: Option<BatchReport>,
    pub error: Option<String>,
}
impl BatchSnapshot {
    pub fn active(&self) -> bool {
        self.status == "running" || self.status == "stopping"
    }
}

struct Entry {
    request: RunBatchRequest,
    snapshot: Arc<Mutex<BatchSnapshot>>,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    current: Option<String>,
}
pub struct BatchManager {
    state: AsyncMutex<State>,
    cooldowns: Arc<Mutex<Cooldowns>>,
    cooldown_path: PathBuf,
    control_port: Option<u16>,
    shutdown: CancellationToken,
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
        }
    }
    pub async fn start(
        &self,
        mut request: RunBatchRequest,
        lists: &[SiteList],
    ) -> Result<BatchSnapshot, String> {
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
        let mut state = self.state.lock().await;
        if let Some(entry) = state.entries.get(&request.request_id) {
            if entry.request != request {
                return Err("request_id already used with different parameters".into());
            }
            return Ok(entry.snapshot.lock().unwrap().clone());
        }
        if self.shutdown.is_cancelled() {
            return Err("application is shutting down".into());
        }
        if state.entries.len() >= 1024 {
            return Err("batch request history limit reached".into());
        }
        if let Some(id) = state.current.clone() {
            let previous = state.entries.get_mut(&id).unwrap();
            if previous.snapshot.lock().unwrap().active() {
                return Err("a batch is already running or stopping".into());
            }
            if let Some(task) = previous.task.as_mut() {
                let result = task.await;
                previous.task = None;
                result.map_err(|e| format!("previous batch task failed: {e}"))?;
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
        let resolved_proxy = match &request.proxy {
            Some(config) => Some(Arc::new(
                proxy::resolve_proxy(
                    config,
                    self.control_port,
                    &self.shutdown,
                    Duration::from_secs(request.timeout_secs.min(5)),
                )
                .await
                .map_err(|e| e.to_string())?,
            )),
            None => None,
        };
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
            status: "running".into(),
            items,
            report: None,
            error: None,
        }));
        let initial = snapshot.lock().unwrap().clone();
        let cancel = self.shutdown.child_token();
        let worker = HeadWorker {
            cooldowns: self.cooldowns.clone(),
            path: self.cooldown_path.clone(),
            blocked: Mutex::new(HashSet::new()),
            error: Mutex::new(None),
            snapshot: snapshot.clone(),
            timeout: Duration::from_secs(request.timeout_secs),
            control_port: self.control_port,
            proxy: resolved_proxy,
            cancel: cancel.clone(),
        };
        let token = cancel.clone();
        let task_snapshot = snapshot.clone();
        let shutdown = self.shutdown.clone();
        let task = tokio::spawn(async move {
            let result = batch::run_batch(config, &worker, token.clone()).await;
            if policy::target_dns_pending() != 0 {
                task_snapshot.lock().unwrap().status = "stopping".into();
                while policy::target_dns_pending() != 0 && !shutdown.is_cancelled() {
                    tokio::select! { _ = shutdown.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(20)) => {} }
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
                    s.status = "failed".into();
                }
            }
            if let Some(error) = worker.error.lock().unwrap().clone() {
                s.error = Some(error);
                s.status = "failed".into();
            }
        });
        let id = request.request_id.clone();
        state.entries.insert(
            id.clone(),
            Entry {
                request,
                snapshot,
                cancel,
                task: Some(task),
            },
        );
        state.current = Some(id);
        Ok(initial)
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
        assert_eq!(started.status, "running");
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
