//! Shared business operations for HTTP and native IPC. No desktop or HTTP dependencies here.
use crate::{
    diagnostics::{DiagnosticConfig, DiagnosticManager, DiagnosticSnapshot},
    engine::Engine,
    model::{ConnectionPreset, Permit, Run, RunConfig, SiteList},
    policy, storage,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

fn read_auth_source(path: &std::path::Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err("授权文件不能是符号链接".into()),
        Ok(_) => storage::read_bounded(path, 1_048_576).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub struct ServiceConfig {
    pub root: PathBuf,
    pub data: PathBuf,
    pub auth_file: PathBuf,
    pub control_port: Option<u16>,
}
pub(crate) struct PermitDraft {
    token: String,
    revision: u64,
    expires: Instant,
    permits: Vec<Permit>,
    committed: bool,
    source_bytes: Option<Vec<u8>>,
}
pub struct AppService {
    pub engine: Engine,
    pub(crate) diagnostics: DiagnosticManager,
    pub(crate) network_tools: crate::network_tools::NetworkToolManager,
    pub(crate) monitor: crate::monitor::MonitorManager,
    pub(crate) monitor_route:
        std::sync::Mutex<Option<(uuid::Uuid, Option<crate::proxy::ProxyConfig>)>>,
    pub(crate) batches: crate::batch_service::BatchManager,
    pub(crate) history: crate::history::History,
    pub lists: RwLock<Vec<SiteList>>,
    pub(crate) connection_presets: Vec<ConnectionPreset>,
    pub(crate) permits: RwLock<Vec<Permit>>,
    pub(crate) auth_file: PathBuf,
    pub(crate) public_domains: HashSet<String>,
    pub(crate) control_port: Option<u16>,
    pub(crate) auth_error: RwLock<Option<String>>,
    pub(crate) auth_revision: std::sync::atomic::AtomicU64,
    pub(crate) permit_drafts: Mutex<HashMap<String, PermitDraft>>,
    pub(crate) mutation: Mutex<()>,
    pub data: PathBuf,
    pub shutdown: CancellationToken,
    pub(crate) demo: tokio::sync::Mutex<Option<crate::demo::DemoHandle>>,
}
impl AppService {
    pub async fn initialize(
        config: ServiceConfig,
        shutdown: CancellationToken,
    ) -> Result<Self, String> {
        let mut lists = storage::load_presets(&config.root.join("presets"))?;
        let connection_presets =
            crate::connections::load(&config.root.join("presets/connections"))?;
        let mut domains: HashSet<_> = lists
            .iter()
            .flat_map(|l| &l.targets)
            .map(|t| policy::parse_url(&t.url).map(|u| policy::domain_key(&u)))
            .collect::<Result<_, _>>()?;
        domains.extend(crate::connections::protected_domain_keys(
            &connection_presets,
        )?);
        let (permits, auth_error) =
            match policy::load_permits(&config.auth_file, &domains, config.control_port) {
                Ok(permits) => (permits, None),
                Err(error) => (vec![], Some(error)),
            };
        lists.extend(storage::load_custom(&config.data)?);
        let history = crate::history::History::open(&config.data)?;
        let engine = Engine::new(config.data.clone(), config.control_port, shutdown.clone())?
            .with_history(history.clone());
        let batches = crate::batch_service::BatchManager::new(
            engine.shared_cooldowns(),
            config.control_port,
            shutdown.clone(),
        );
        Ok(Self {
            engine,
            batches,
            diagnostics: DiagnosticManager::new(config.control_port, shutdown.clone()),
            network_tools: crate::network_tools::NetworkToolManager::new(shutdown.clone())
                .with_control_port(config.control_port),
            monitor: crate::monitor::MonitorManager::new(
                &config.data,
                config.control_port,
                shutdown.clone(),
            )?,
            monitor_route: std::sync::Mutex::new(None),
            history,
            lists: RwLock::new(lists),
            connection_presets,
            permits: RwLock::new(permits),
            auth_file: config.auth_file,
            public_domains: domains,
            control_port: config.control_port,
            auth_error: RwLock::new(auth_error),
            auth_revision: std::sync::atomic::AtomicU64::new(1),
            permit_drafts: Mutex::new(HashMap::new()),
            mutation: Mutex::new(()),
            data: config.data,
            shutdown,
            demo: tokio::sync::Mutex::new(None),
        })
    }
    pub async fn bootstrap(&self, token: &str) -> Value {
        let permits = self.permits.read().await.clone();
        let authorized: Vec<_> = permits.iter().map(|p| json!({
            "id":p.id,"name":p.name,"url":p.url,"modes":p.modes,
            "allowed_ips":p.allowed_ips,"authorization":p.authorization,"expires_at":p.expires_at,"proxy_routes":p.proxy_routes,
            "max_concurrency":p.max_concurrency,"max_rps":p.max_rps,"max_duration":p.max_duration,
            "expired": chrono::DateTime::parse_from_rfc3339(&p.expires_at)
                .is_ok_and(|expiry| expiry <= chrono::Utc::now())
        })).collect();
        let demo = self.demo.lock().await;
        let self_test_targets: Vec<Value> = demo.as_ref().map(|d| vec![
            json!({"name":"本机 TCP 自检（不代表外网）","target":"127.0.0.1","port":d.tcp_port,"tls":false}),
        ]).unwrap_or_default();
        let self_test_enabled = demo.is_some();
        drop(demo);
        json!({"token":token,"version":env!("CARGO_PKG_VERSION"),"lists":*self.lists.read().await,"authorized":authorized,
            "diagnostics":{"enabled":true,
                "limits":{"dns_timeout_secs":5,"route_timeout_secs":30,"tcp_timeout_secs":5,"tls_timeout_secs":5,"max_hops":20,"max_output_bytes":65536,"start_interval_secs":5,"host_interval_secs":30},
                "trust_source":"随包 Mozilla 根（webpki-roots），不是系统证书库",
                "self_test_targets":self_test_targets},
            "monitoring":{"enabled":true,"min_interval_secs":60,"browser_lease_secs":15,"proxy_supported":true},
            "batch":{"enabled":true,"max_rounds":3,"max_concurrency":4,"max_timeout_secs":120},
            "proxy":{"enabled":true,"authentication_supported":false,"protocol":"http_connect","local_dns":true},
            "dns_draining":crate::policy::target_dns_pending()!=0 || crate::proxy::proxy_dns_pending()!=0 || crate::monitor_worker::monitor_dns_pending(),
            "diagnostic_active":self.diagnostics.snapshot().await.is_some_and(|d| d.active()),
            "connection_presets":self.connection_presets,
            "authorization_error":*self.auth_error.read().await,
            "authorization_revision":self.auth_revision.load(std::sync::atomic::Ordering::SeqCst),
            "self_test_enabled":self_test_enabled,
            "limits":{"public_concurrency":crate::model::PUBLIC_CONCURRENCY,"public_rps":crate::model::PUBLIC_RPS,"public_duration":crate::model::PUBLIC_DURATION,"public_cooldown":30,
            "concurrency":crate::model::MAX_CONCURRENCY,"rps":crate::model::MAX_RPS,"duration":crate::model::MAX_DURATION,"timeout":crate::model::MAX_TIMEOUT,"max_lists":50,"max_targets":1000}})
    }
    fn ensure_monitor_idle(&self) -> Result<(), String> {
        let state = self.monitor.snapshot();
        if state.active
            || state.draining
            || crate::monitor_worker::monitor_dns_pending()
            || crate::proxy::proxy_dns_pending() != 0
            || crate::policy::target_dns_pending() != 0
        {
            return Err("监控仍在运行、收尾或等待系统DNS，请停止后再试".into());
        }
        Ok(())
    }
    pub async fn monitor_save(
        &self,
        expected_revision: u64,
        targets: Vec<crate::monitor::MonitorTarget>,
    ) -> Result<Value, String> {
        let _gate = self.mutation.lock().await;
        self.ensure_mutable().await?;
        self.monitor
            .save_config(expected_revision, targets)
            .map(|_| json!(self.monitor.snapshot()))
    }
    pub async fn monitor_start(
        &self,
        request_id: &str,
        revision: u64,
        proxy: Option<crate::proxy::ProxyConfig>,
        lifetime: crate::monitor::MonitorLifetime,
    ) -> Result<Value, String> {
        let id = uuid::Uuid::parse_str(request_id).map_err(|_| "监控请求ID无效")?;
        if id.to_string() != request_id {
            return Err("监控请求ID必须为规范UUID".into());
        }
        let _gate = self.mutation.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出".into());
        }
        let proxy = proxy
            .map(|p| {
                crate::proxy::normalize_proxy_config(&p, self.control_port)
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        if let Some(existing) = self.monitor.validate_retry(id, revision, lifetime)? {
            if self.monitor_route.lock().unwrap().as_ref() != Some(&(id, proxy.clone())) {
                return Err("请求ID对应的代理路径与原请求不同".into());
            }
            return Ok(json!(existing));
        }
        self.engine.settle().await?;
        self.batches.settle().await?;
        self.diagnostics.settle().await?;
        self.network_tools.settle().await?;
        if crate::monitor_worker::monitor_dns_pending()
            || crate::proxy::proxy_dns_pending() != 0
            || crate::policy::target_dns_pending() != 0
        {
            return Err("之前的系统DNS解析仍在收尾".into());
        }
        let existing = self.monitor.snapshot();
        if existing.active || existing.draining {
            return Err("监控正在运行或收尾".into());
        }
        if existing.config.revision != revision || existing.config.targets.is_empty() {
            return Err("监控配置修订已变化或没有目标".into());
        }
        let resolved = if let Some(config) = &proxy {
            Some(
                crate::proxy::resolve_proxy(
                    config,
                    self.control_port,
                    &self.shutdown,
                    Duration::from_secs(5),
                )
                .await
                .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let (cooldowns, path) = self.engine.shared_cooldowns();
        let worker = std::sync::Arc::new(crate::monitor_worker::SystemMonitorWorker::new(
            self.control_port,
            cooldowns,
            path,
            resolved,
        ));
        self.monitor.start(id, revision, worker, lifetime)?;
        *self.monitor_route.lock().unwrap() = Some((id, proxy));
        Ok(self.monitor_state())
    }
    pub fn monitor_state(&self) -> Value {
        let mut snapshot = json!(self.monitor.snapshot());
        if let Some((generation, proxy)) = self.monitor_route.lock().unwrap().as_ref()
            && snapshot["generation"].as_str() == Some(&generation.to_string())
        {
            snapshot["proxy"] = json!(proxy);
        }
        snapshot["dns_draining"] = json!(crate::monitor_worker::monitor_dns_pending());
        snapshot
    }
    pub fn monitor_heartbeat(&self, generation: &str) -> Result<Value, String> {
        let id = uuid::Uuid::parse_str(generation).map_err(|_| "监控ID无效")?;
        self.monitor.heartbeat(id)?;
        Ok(json!(self.monitor.snapshot()))
    }
    pub fn monitor_pause(
        &self,
        generation: &str,
        target: &str,
        paused: bool,
    ) -> Result<Value, String> {
        let id = uuid::Uuid::parse_str(generation).map_err(|_| "监控ID无效")?;
        self.monitor.set_paused(id, target, paused)?;
        Ok(json!(self.monitor.snapshot()))
    }
    pub async fn monitor_stop(&self, generation: &str) -> Result<Value, String> {
        let id = uuid::Uuid::parse_str(generation).map_err(|_| "监控ID无效")?;
        self.monitor.stop(id).await?;
        Ok(json!(self.monitor.snapshot()))
    }
    pub async fn start_network_tool(
        &self,
        request: crate::network_tools::Request,
    ) -> Result<crate::network_tools::Snapshot, String> {
        let _gate = self.mutation.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if let Some(existing) = self.network_tools.existing_request(&request).await? {
            return Ok(existing);
        }
        self.ensure_monitor_idle()?;
        self.engine.settle().await?;
        self.batches.settle().await?;
        self.diagnostics.settle().await?;
        self.network_tools.start(request).await
    }
    pub async fn network_state(&self) -> Option<crate::network_tools::Snapshot> {
        self.network_tools.snapshot().await
    }
    pub async fn stop_network_tool(
        &self,
        id: &str,
    ) -> Result<crate::network_tools::Snapshot, String> {
        self.network_tools.stop(id).await
    }
    pub async fn start_batch(
        &self,
        request: crate::batch_service::RunBatchRequest,
    ) -> Result<crate::batch_service::BatchSnapshot, String> {
        let _gate = self.mutation.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出".into());
        }
        self.ensure_monitor_idle()?;
        self.engine.settle().await?;
        self.diagnostics.settle().await?;
        self.network_tools.settle().await?;
        self.batches.start(request, &self.lists.read().await).await
    }
    pub async fn batch_state(&self) -> Option<crate::batch_service::BatchSnapshot> {
        self.batches.snapshot().await
    }
    pub async fn stop_batch(
        &self,
        id: &str,
    ) -> Result<crate::batch_service::BatchSnapshot, String> {
        self.batches
            .stop(id)
            .await
            .ok_or_else(|| "批次不存在或编号已过期".into())
    }
    pub async fn start(&self, config: RunConfig) -> Result<Run, String> {
        let _gate = self.mutation.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if config.mode != "probe"
            && let Some(error) = self.auth_error.read().await.as_ref()
        {
            return Err(format!("授权配置需要修复：{error}"));
        }
        self.ensure_monitor_idle()?;
        self.batches.settle().await?;
        self.diagnostics.settle().await?;
        self.network_tools.settle().await?;
        let lists = self.lists.read().await.clone();
        let permits = self.permits.read().await.clone();
        self.engine.start(config, &lists, &permits).await
    }
    pub async fn start_diagnostic(
        &self,
        config: DiagnosticConfig,
    ) -> Result<DiagnosticSnapshot, String> {
        let _gate = self.mutation.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出".into());
        }
        let demo = self.demo.lock().await;
        let allowed_loopback = demo
            .as_ref()
            .map(|d| {
                vec![
                    std::net::SocketAddr::from(([127, 0, 0, 1], d.tcp_port)),
                    std::net::SocketAddr::from(([127, 0, 0, 1], d.http_port)),
                ]
            })
            .unwrap_or_default();
        drop(demo);
        if let Some(existing) = self.diagnostics.existing_request(&config).await? {
            return Ok(existing);
        }
        self.ensure_monitor_idle()?;
        self.batches.settle().await?;
        self.engine.settle().await?;
        self.network_tools.settle().await?;
        self.diagnostics.start(config, allowed_loopback).await
    }
    pub async fn diagnostic_state(&self) -> Option<DiagnosticSnapshot> {
        self.diagnostics.snapshot().await
    }
    pub async fn stop_diagnostic(&self, id: &str) -> Result<DiagnosticSnapshot, String> {
        self.diagnostics.stop(id).await
    }
    pub async fn save_lists(&self, mut lists: Vec<SiteList>) -> Result<(), String> {
        let _gate = self.mutation.lock().await;
        self.ensure_mutable().await?;
        let size = serde_json::to_vec(&lists).map_err(|e| e.to_string())?.len();
        if size > 1_048_576 {
            return Err("列表数据不能超过 1 MiB".into());
        }
        storage::validate_lists(&mut lists)?;
        let mut all = self.lists.write().await;
        if lists
            .iter()
            .any(|l| all.iter().any(|b| b.builtin && b.id == l.id))
        {
            return Err("自定义列表 ID 与内置列表冲突".into());
        }
        storage::save_custom(&self.data, lists.clone())?;
        all.retain(|l| l.builtin);
        all.extend(lists);
        Ok(())
    }
    async fn ensure_mutable(&self) -> Result<(), String> {
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出".into());
        }
        self.ensure_monitor_idle()?;
        self.engine.settle().await?;
        self.batches.settle().await?;
        self.diagnostics.settle().await?;
        self.network_tools.settle().await?;
        Ok(())
    }
    pub async fn prepare_permits(&self, mut permits: Vec<Permit>) -> Result<Value, String> {
        let _gate = self.mutation.lock().await;
        self.ensure_mutable().await?;
        for permit in &mut permits {
            permit.url = policy::parse_url(&permit.url)?.to_string();
            permit.name = permit.name.trim().to_owned();
            permit.authorization = permit.authorization.trim().to_owned();
            permit.modes.sort();
            permit.modes.dedup();
            for route in &mut permit.proxy_routes {
                route.url = crate::proxy::normalize_proxy_config(
                    &crate::proxy::ProxyConfig {
                        url: route.url.clone(),
                    },
                    self.control_port,
                )
                .map_err(|e| e.to_string())?
                .url;
                route.authorization = route.authorization.trim().to_owned();
            }
        }
        policy::validate_permits(&permits, &self.public_domains, self.control_port)?;
        let revision = self.auth_revision.load(std::sync::atomic::Ordering::SeqCst);
        let token = uuid::Uuid::new_v4().to_string();
        let expires = Instant::now() + Duration::from_secs(300);
        let preview = permits.clone();
        let mut drafts = self.permit_drafts.lock().await;
        // Only the latest uncommitted preview may authorize a new write.
        drafts.retain(|_, d| d.committed && d.expires > Instant::now());
        if drafts.len() >= 32 {
            return Err("审核草稿过多，请稍后再试".into());
        }
        drafts.insert(
            token.clone(),
            PermitDraft {
                token: token.clone(),
                revision,
                expires,
                permits,
                committed: false,
                source_bytes: read_auth_source(&self.auth_file)?,
            },
        );
        Ok(
            json!({"draftToken":token,"revision":revision,"expiresIn":300,"targets":preview,
            "sideEffects":["仅保存本机授权配置，不会发送网络请求","保存后将立即用于后续测试"]}),
        )
    }
    pub async fn confirm_permits(&self, token: &str, revision: u64) -> Result<(), String> {
        let _gate = self.mutation.lock().await;
        self.ensure_mutable().await?;
        let mut drafts = self.permit_drafts.lock().await;
        let draft = drafts.get_mut(token).ok_or("授权审核草稿不存在")?;
        if draft.committed {
            return if draft.revision == revision {
                Ok(())
            } else {
                Err("审核修订号不匹配".into())
            };
        }
        if draft.token != token || draft.expires <= Instant::now() {
            return Err("授权审核草稿已过期".into());
        }
        if draft.revision != revision
            || revision != self.auth_revision.load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err("授权配置已变化，请重新审核".into());
        }
        if read_auth_source(&self.auth_file)? != draft.source_bytes {
            return Err("授权文件已被外部修改，请重新加载并审核".into());
        }
        policy::validate_permits(&draft.permits, &self.public_domains, self.control_port)?;
        storage::atomic_json(
            &self.auth_file,
            &json!({"version":1,"targets":draft.permits}),
        )?;
        let demos: Vec<_> = self
            .permits
            .read()
            .await
            .iter()
            .filter(|p| p.id.starts_with("demo-"))
            .cloned()
            .collect();
        let mut next = draft.permits.clone();
        next.extend(demos);
        *self.permits.write().await = next;
        draft.committed = true;
        *self.auth_error.write().await = None;
        self.auth_revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub async fn reload_permits(&self) -> Result<(), String> {
        let _gate = self.mutation.lock().await;
        self.ensure_mutable().await?;
        let mut permits =
            match policy::load_permits(&self.auth_file, &self.public_domains, self.control_port) {
                Ok(permits) => permits,
                Err(error) => {
                    *self.auth_error.write().await = Some(error.clone());
                    return Err(error);
                }
            };
        permits.extend(
            self.permits
                .read()
                .await
                .iter()
                .filter(|p| p.id.starts_with("demo-"))
                .cloned(),
        );
        *self.permits.write().await = permits;
        *self.auth_error.write().await = None;
        self.auth_revision
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub async fn set_self_test(&self, enabled: bool) -> Result<(), String> {
        let _gate = self.mutation.lock().await;
        self.ensure_mutable().await?;
        let mut demo_slot = self.demo.lock().await;
        if enabled && demo_slot.is_none() {
            let demo = crate::demo::start_tracked(self.shutdown.child_token()).await?;
            let mut permits = self.permits.write().await;
            if permits.iter().any(|p| p.id.starts_with("demo-")) {
                demo.shutdown().await;
                return Err("授权 ID 与本机自检 ID 冲突".into());
            }
            for (id, name, url, mode) in [
                (
                    "demo-http",
                    "本机自检 · HTTP 80ms",
                    format!("http://127.0.0.1:{}/probe", demo.http_port),
                    "http",
                ),
                (
                    "demo-slow",
                    "本机自检 · HTTP 2s",
                    format!("http://127.0.0.1:{}/slow", demo.http_port),
                    "http",
                ),
                (
                    "demo-limited",
                    "本机自检 · HTTP 429",
                    format!("http://127.0.0.1:{}/limited", demo.http_port),
                    "http",
                ),
                (
                    "demo-tcp",
                    "本机自检 · TCP 保持",
                    format!("http://127.0.0.1:{}/", demo.tcp_port),
                    "tcp",
                ),
            ] {
                permits.push(Permit {
                    id: id.into(),
                    name: name.into(),
                    url,
                    modes: vec![mode.into()],
                    allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
                    max_concurrency: 1000,
                    max_rps: 1000,
                    max_duration: 300,
                    authorization: "仅本机会话自检".into(),
                    expires_at: "2099-01-01T00:00:00Z".into(),
                    proxy_routes: vec![],
                });
            }
            *demo_slot = Some(demo);
        } else if !enabled && let Some(demo) = demo_slot.take() {
            self.permits
                .write()
                .await
                .retain(|p| !p.id.starts_with("demo-"));
            demo.shutdown().await;
        }
        Ok(())
    }
    pub async fn workspace(
        &self,
        operation: &str,
        payload: Value,
        token: &str,
    ) -> Result<Value, String> {
        match operation {
            "authorization.prepare" => {
                let targets =
                    serde_json::from_value(payload.get("targets").cloned().ok_or("缺少 targets")?)
                        .map_err(|e| format!("授权字段无效：{e}"))?;
                self.prepare_permits(targets).await
            }
            "authorization.confirm" => {
                self.confirm_permits(
                    payload["draftToken"].as_str().ok_or("缺少审核令牌")?,
                    payload["revision"].as_u64().ok_or("缺少修订号")?,
                )
                .await?;
                Ok(self.bootstrap(token).await)
            }
            "authorization.reload" => {
                self.reload_permits().await?;
                Ok(self.bootstrap(token).await)
            }
            "selftest" => {
                self.set_self_test(payload["enabled"].as_bool().ok_or("缺少 enabled")?)
                    .await?;
                Ok(self.bootstrap(token).await)
            }
            "history.list" | "history.get" | "history.export" => {
                let history = self.history.clone();
                let op = operation.to_owned();
                tokio::task::spawn_blocking(move || match op.as_str() {
                    "history.list" => Ok(json!(history.list()?)),
                    "history.get" => Ok(json!(history.get(payload["id"].as_str().ok_or("缺少运行 ID")?)?)),
                    _ => {
                        let run = history.get(payload["id"].as_str().ok_or("缺少运行 ID")?)?;
                        let kind = payload["kind"].as_str().ok_or("缺少导出格式")?;
                        let content = match kind { "json" => serde_json::to_string_pretty(&run).map_err(|e| e.to_string())?, "csv" => crate::metrics::csv(&run), _ => return Err("未知导出格式".into()) };
                        Ok(json!({"content":content,"mime":if kind=="csv"{"text/csv"}else{"application/json"},"filename":format!("aonet-{}.{}",run.id,kind)}))
                    }
                }).await.map_err(|e| e.to_string())?
            }
            "history.clear" => {
                let _gate = self.mutation.lock().await;
                self.ensure_mutable().await?;
                if payload["confirm"] != true {
                    return Err("清理历史需要明确确认".into());
                }
                let history = self.history.clone();
                tokio::task::spawn_blocking(move || history.clear())
                    .await
                    .map_err(|e| e.to_string())??;
                Ok(json!(true))
            }
            "profiles.list" | "settings.get" => {
                self.load_workspace_file(if operation == "profiles.list" {
                    "profiles"
                } else {
                    "settings"
                })
            }
            "profiles.save" | "settings.save" => {
                let _gate = self.mutation.lock().await;
                self.ensure_mutable().await?;
                let profiles = operation == "profiles.save";
                let key = if profiles { "profiles" } else { "settings" };
                let mut value = payload.get(key).cloned().ok_or("缺少保存内容")?;
                if profiles {
                    let entries = value.as_array().ok_or("方案必须为数组")?;
                    if entries.len() > 50 {
                        return Err("最多保存 50 个方案".into());
                    }
                    let mut ids = HashSet::new();
                    for entry in entries {
                        let id = entry["id"].as_str().ok_or("方案缺少 ID")?;
                        if id.len() > 100
                            || id.is_empty()
                            || !id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                            || !ids.insert(id)
                        {
                            return Err("方案 ID 无效或重复".into());
                        }
                        let name = entry["name"].as_str().ok_or("方案缺少名称")?;
                        if name.trim().is_empty() || name.len() > 200 {
                            return Err("方案名称应为 1–200 UTF-8 字节".into());
                        }
                        let config: RunConfig = serde_json::from_value(entry["config"].clone())
                            .map_err(|e| e.to_string())?;
                        config.validate()?;
                        if config
                            .authorized_ids
                            .iter()
                            .any(|id| id.starts_with("demo-"))
                        {
                            return Err("会话自检目标不能保存到方案".into());
                        }
                    }
                    value = json!(entries.iter().map(|entry| json!({"id":entry["id"],"name":entry["name"],"config":entry["config"]})).collect::<Vec<_>>());
                } else {
                    let object = value.as_object_mut().ok_or("设置必须为对象")?;
                    object.retain(|k, _| {
                        [
                            "version",
                            "page",
                            "dock_width",
                            "sort",
                            "column_widths",
                            "http_proxy",
                        ]
                        .contains(&k.as_str())
                    });
                    object.insert("version".into(), json!(1));
                    if let Some(raw) = object.get("http_proxy").filter(|value| !value.is_null()) {
                        let config: crate::proxy::ProxyConfig = serde_json::from_value(raw.clone())
                            .map_err(|e| format!("代理配置格式错误：{e}"))?;
                        let config =
                            crate::proxy::normalize_proxy_config(&config, self.control_port)
                                .map_err(|e| e.to_string())?;
                        object.insert("http_proxy".into(), json!(config));
                    }
                    let width = object
                        .get("dock_width")
                        .and_then(Value::as_u64)
                        .unwrap_or(300)
                        .clamp(240, 600);
                    object.insert("dock_width".into(), json!(width));
                    let page = object
                        .get("page")
                        .and_then(Value::as_str)
                        .filter(|page| {
                            [
                                "console",
                                "batch",
                                "load",
                                "monitor",
                                "diagnostics",
                                "lists",
                                "authorized",
                                "history",
                                "settings",
                                "guide",
                            ]
                            .contains(page)
                        })
                        .unwrap_or("console")
                        .to_owned();
                    object.insert("page".into(), json!(page));
                    let sort = object
                        .get("sort")
                        .and_then(Value::as_str)
                        .filter(|sort| ["stable", "name", "attempted", "errors"].contains(sort))
                        .unwrap_or("stable")
                        .to_owned();
                    object.insert("sort".into(), json!(sort));
                    let widths: serde_json::Map<String, Value> = object
                        .get("column_widths")
                        .and_then(Value::as_object)
                        .map(|widths| {
                            widths
                                .iter()
                                .filter_map(|(key, width)| {
                                    width
                                        .as_u64()
                                        .filter(|_| {
                                            key.len() <= 40
                                                && key.bytes().all(|b| {
                                                    b.is_ascii_alphanumeric()
                                                        || b == b'_'
                                                        || b == b'-'
                                                })
                                        })
                                        .map(|width| (key.clone(), json!(width.clamp(60, 800))))
                                })
                                .take(16)
                                .collect()
                        })
                        .unwrap_or_default();
                    object.insert("column_widths".into(), Value::Object(widths));
                }
                let bytes = serde_json::to_vec_pretty(&json!({"version":1,"value":value}))
                    .map_err(|e| e.to_string())?;
                if bytes.len() > 1_048_576 {
                    return Err("工作区数据不能超过 1 MiB".into());
                }
                // Refuse silent replacement of corrupt workspace data.
                self.load_workspace_file(key)?;
                storage::atomic_bytes(&self.data.join(format!("{key}.json")), &bytes)?;
                Ok(value)
            }
            _ => Err("不支持的工作台操作".into()),
        }
    }
    fn load_workspace_file(&self, key: &str) -> Result<Value, String> {
        let path = self.data.join(format!("{key}.json"));
        if !path.exists() {
            return Ok(if key == "profiles" {
                json!([])
            } else {
                json!({"version":1,"page":"console","dock_width":300,"sort":"name","column_widths":{}})
            });
        }
        let value: Value = serde_json::from_slice(&storage::read_bounded(&path, 1_048_576)?)
            .map_err(|e| format!("{key} 数据损坏，未覆盖：{e}"))?;
        if value["version"] != 1 || value.get("value").is_none() {
            return Err(format!("{key} 数据版本无效，未覆盖"));
        }
        let mut contents = value["value"].clone();
        if key == "settings"
            && let Some(raw) = contents.get("http_proxy").filter(|p| !p.is_null())
        {
            let proxy: crate::proxy::ProxyConfig = serde_json::from_value(raw.clone())
                .map_err(|e| format!("代理配置格式错误：{e}"))?;
            let normalized = crate::proxy::normalize_proxy_config(&proxy, self.control_port)
                .map_err(|e| e.to_string())?;
            contents["http_proxy"] = json!(normalized);
        }
        Ok(contents)
    }
    pub async fn export_run(&self, kind: &str, id: Option<&str>) -> Result<Vec<u8>, String> {
        if kind == "batch" {
            let snapshot = self.batches.snapshot().await.ok_or("尚无批次结果")?;
            if Some(snapshot.request_id.as_str()) != id {
                return Err("批次编号不匹配".into());
            }
            return serde_json::to_vec_pretty(
                &json!({"kind":"batch","partial":snapshot.active(),"snapshot":snapshot}),
            )
            .map_err(|e| e.to_string());
        }
        if kind == "diagnostic" {
            let report = self.diagnostics.export(id.ok_or("缺少诊断 ID")?).await?;
            return serde_json::to_vec_pretty(&report).map_err(|e| e.to_string());
        }
        if let Some(id) = id {
            let result = self
                .workspace("history.export", json!({"id":id,"kind":kind}), "")
                .await?;
            return Ok(result["content"]
                .as_str()
                .ok_or("导出内容无效")?
                .as_bytes()
                .to_vec());
        }
        self.export(kind).await
    }
    pub async fn export(&self, kind: &str) -> Result<Vec<u8>, String> {
        if kind == "lists" {
            let lists: Vec<_> = self
                .lists
                .read()
                .await
                .iter()
                .filter(|l| !l.builtin)
                .cloned()
                .collect();
            return serde_json::to_vec_pretty(&json!({"lists":lists})).map_err(|e| e.to_string());
        }
        let run = self.engine.snapshot().await.ok_or("尚无测试结果")?;
        match kind {
            "json" => serde_json::to_vec_pretty(&run).map_err(|e| e.to_string()),
            "csv" => Ok(crate::metrics::csv(&run).into_bytes()),
            _ => Err("未知导出类型".into()),
        }
    }
    pub async fn shutdown_and_wait(&self) {
        self.shutdown.cancel();
        let (_, _, _, _, monitor_result) = tokio::join!(
            self.engine.shutdown_and_wait(),
            self.diagnostics.shutdown_and_wait(),
            self.batches.shutdown_and_wait(),
            self.network_tools.shutdown_and_wait(),
            self.monitor.shutdown_and_wait()
        );
        if let Err(error) = monitor_result {
            eprintln!("监控退出清理失败：{error}");
        }
        let dns_grace = tokio::time::Instant::now() + Duration::from_secs(2);
        while (crate::monitor_worker::monitor_dns_pending()
            || crate::proxy::proxy_dns_pending() != 0
            || crate::policy::target_dns_pending() != 0)
            && tokio::time::Instant::now() < dns_grace
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if crate::monitor_worker::monitor_dns_pending()
            || crate::proxy::proxy_dns_pending() != 0
            || crate::policy::target_dns_pending() != 0
        {
            eprintln!("系统名称解析仍在收尾，将随进程退出结束，未声称底层DNS已取消");
        }
        if let Some(demo) = self.demo.lock().await.take() {
            demo.shutdown().await;
        }
    }
}
