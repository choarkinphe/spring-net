use crate::{
    metrics::{Metrics, Outcome},
    model::{Permit, Run, RunConfig, SiteList, Target},
    policy::{self, Cooldowns},
    proxy::{self, ResolvedProxy},
    proxy_settings::RouteSnapshot,
};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
#[path = "proxy_engine_tests.rs"]
mod proxy_tests;
#[cfg(test)]
#[path = "engine_safety_tests.rs"]
mod safety_tests;

const BYTE_BUDGET: u64 = 256 * 1024 * 1024;
struct Active {
    metrics: Arc<Mutex<Metrics>>,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}
pub struct Engine {
    current: AsyncMutex<Option<Active>>,
    cooldowns: Arc<Mutex<Cooldowns>>,
    cooldown_path: PathBuf,
    pub control_port: Option<u16>,
    shutdown: CancellationToken,
    history: Option<crate::history::History>,
}
#[derive(Clone)]
struct WorkTarget {
    target: Target,
    permit: Option<Permit>,
    domain: String,
    client: Option<reqwest::Client>,
    addr: Option<std::net::SocketAddr>,
    proxy: Option<Arc<ResolvedProxy>>,
}
// Header-time policy state is independent of worker completion: cancellation or
// a slow response body must never erase a rate-limit signal.
struct Backoff {
    stopped: Mutex<HashSet<String>>,
    error: Mutex<Option<String>>,
    cooldowns: Arc<Mutex<Cooldowns>>,
    path: PathBuf,
    probe: bool,
}
impl Backoff {
    fn observe(&self, domain: &str, seconds: u64) {
        self.stopped.lock().unwrap().insert(domain.to_owned());
        if self.probe
            && let Err(error) = self.cooldowns.lock().unwrap().reserve(
                domain,
                Cooldowns::now().saturating_add(seconds.saturating_mul(1000)),
                &self.path,
            )
        {
            *self.error.lock().unwrap() = Some(error);
        }
    }
    fn stopped(&self, domain: &str) -> bool {
        self.stopped.lock().unwrap().contains(domain)
    }
}

fn authorization_window(
    targets: &[WorkTarget],
    duration: Duration,
) -> Result<Option<(chrono::DateTime<chrono::Utc>, Instant)>, String> {
    let expiry = targets
        .iter()
        .filter_map(|target| target.permit.as_ref())
        .map(|permit| {
            chrono::DateTime::parse_from_rfc3339(&permit.expires_at)
                .map(|date| date.with_timezone(&chrono::Utc))
                .map_err(|_| "授权有效期无效".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min();
    let monotonic_now = Instant::now();
    let now = chrono::Utc::now();
    expiry
        .map(|expiry| {
            let remaining = (expiry - now)
                .to_std()
                .map_err(|_| "许可已过期或不能覆盖整轮时长".to_owned())?;
            if remaining <= duration {
                return Err("许可已过期或不能覆盖整轮时长".into());
            }
            Ok((expiry, monotonic_now + remaining))
        })
        .transpose()
}

impl Engine {
    pub fn new(
        data: PathBuf,
        control_port: Option<u16>,
        shutdown: CancellationToken,
    ) -> Result<Self, String> {
        let path = data.join("cooldowns.json");
        Ok(Self {
            current: AsyncMutex::new(None),
            cooldowns: Arc::new(Mutex::new(Cooldowns::load(&path)?)),
            cooldown_path: path,
            control_port,
            shutdown,
            history: None,
        })
    }
    pub(crate) fn shared_cooldowns(&self) -> (Arc<Mutex<Cooldowns>>, PathBuf) {
        (self.cooldowns.clone(), self.cooldown_path.clone())
    }
    pub fn with_history(mut self, history: crate::history::History) -> Self {
        self.history = Some(history);
        self
    }
    pub async fn snapshot(&self) -> Option<Run> {
        self.current
            .lock()
            .await
            .as_ref()
            .map(|a| a.metrics.lock().unwrap().run.clone())
    }
    pub async fn stop(&self) -> Option<Run> {
        let guard = self.current.lock().await;
        let a = guard.as_ref()?;
        let mut m = a.metrics.lock().unwrap();
        if m.run.active() {
            m.run.status = "stopping".into();
            a.cancel.cancel();
        }
        Some(m.run.clone())
    }
    pub async fn settle(&self) -> Result<(), String> {
        let mut current = self.current.lock().await;
        if let Some(active) = current.as_mut() {
            if active.metrics.lock().unwrap().run.active() {
                return Err("已有测试运行或正在收尾".into());
            }
            if let Some(task) = active.task.take() {
                task.await.map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub async fn shutdown_and_wait(&self) {
        self.shutdown.cancel();
        let active = self.current.lock().await.take();
        if let Some(mut active) = active {
            active.cancel.cancel();
            if let Some(mut task) = active.task.take()
                && tokio::time::timeout(Duration::from_secs(5), &mut task)
                    .await
                    .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
    pub async fn start(
        &self,
        config: RunConfig,
        lists: &[SiteList],
        permits: &[Permit],
    ) -> Result<Run, String> {
        let route = RouteSnapshot {
            proxy: config.proxy.clone(),
            ..RouteSnapshot::default()
        };
        self.start_with_route(config, lists, permits, route).await
    }

    /// The service freezes settings at admission; workers and persisted results use that route.
    pub async fn start_with_route(
        &self,
        mut config: RunConfig,
        lists: &[SiteList],
        permits: &[Permit],
        mut route: RouteSnapshot,
    ) -> Result<Run, String> {
        config.proxy = route.proxy.clone();
        config.validate()?;
        config.proxy = config
            .proxy
            .as_ref()
            .map(|p| proxy::normalize_proxy_config(p, self.control_port).map_err(|e| e.to_string()))
            .transpose()?;
        route.proxy = config.proxy.clone();
        let mut guard = self.current.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出，不能开始新测试".into());
        }
        if guard
            .as_ref()
            .is_some_and(|a| a.metrics.lock().unwrap().run.active())
        {
            return Err("已有测试运行或正在停止".into());
        }
        if let Some(previous) = guard.as_mut() {
            // Terminal network metrics alone do not mean the archive has finished.
            if let Some(task) = previous.task.take() {
                task.await.map_err(|e| format!("上一轮收尾失败：{e}"))?;
            }
            if previous
                .metrics
                .lock()
                .unwrap()
                .run
                .reason
                .contains("历史保存失败")
            {
                return Err("上一轮历史保存失败，请先导出结果并检查存储".into());
            }
        }
        let selected = bounded_preparation(&self.shutdown, async {
            let mut selected = vec![];
            let mut seen = HashSet::new();
            let mut prepared_proxy: Option<Arc<ResolvedProxy>> = None;
            if config.mode == "probe" {
                for id in &config.list_ids {
                    if !lists.iter().any(|l| &l.id == id) {
                        return Err(format!("未知列表：{id}"));
                    }
                }
                let candidates: Vec<_> = lists
                    .iter()
                    .filter(|l| config.list_ids.contains(&l.id))
                    .flat_map(|l| l.targets.iter().map(move |t| (l, t)))
                    .collect();
                for id in &config.target_ids {
                    let matches = candidates
                        .iter()
                        .filter(|(l, t)| format!("{}/{}", l.id, t.id) == *id || t.id == *id)
                        .count();
                    if matches != 1 {
                        return Err(format!("未知或有歧义的目标引用：{id}"));
                    }
                }
                for l in lists.iter().filter(|l| config.list_ids.contains(&l.id)) {
                    for t in &l.targets {
                        let qualified = format!("{}/{}", l.id, t.id);
                        if !config.target_ids.is_empty()
                            && !config.target_ids.contains(&qualified)
                            && !config.target_ids.contains(&t.id)
                        {
                            continue;
                        }
                        let u = policy::parse_url(&t.url)?;
                        policy::check_public_url(&u)?;
                        if seen.insert(u.to_string()) {
                            selected.push(WorkTarget {
                                target: Target {
                                    id: qualified,
                                    ..t.clone()
                                },
                                permit: None,
                                domain: policy::domain_key(&u),
                                client: None,
                                addr: None,
                                proxy: None,
                            });
                        }
                    }
                }
            } else {
                for id in &config.authorized_ids {
                    let p = permits
                        .iter()
                        .find(|p| &p.id == id)
                        .ok_or_else(|| format!("未登记授权端点：{id}"))?;
                    if !p.modes.contains(&config.mode)
                        || config.concurrency > p.max_concurrency
                        || config.rps > p.max_rps
                        || config.duration_secs > p.max_duration
                    {
                        return Err(format!("超出端点许可范围：{}", p.name));
                    }
                    let expiry = chrono::DateTime::parse_from_rfc3339(&p.expires_at)
                        .map_err(|_| "授权有效期无效")?;
                    if expiry
                        <= chrono::Utc::now()
                            + chrono::Duration::seconds(config.duration_secs as i64)
                    {
                        return Err("许可已过期或不能覆盖整轮时长".into());
                    }
                    let u = policy::parse_url(&p.url)?;
                    if !seen.insert(u.to_string()) {
                        continue;
                    }
                    let addrs = if config.proxy.is_some() {
                        policy::resolve_proxy_target(&u, Some(p), self.control_port).await?
                    } else {
                        policy::resolve(&u, Some(p), self.control_port).await?
                    };
                    if let Some(proxy_config) = &config.proxy {
                        let route = policy::proxy_route(p, proxy_config, self.control_port)?;
                        if prepared_proxy.is_none() {
                            let token = self.shutdown.child_token();
                            let resolved = proxy::resolve_proxy(
                                proxy_config,
                                self.control_port,
                                &token,
                                Duration::from_secs(config.timeout_secs),
                            )
                            .await
                            .map_err(|e| e.to_string())?;
                            prepared_proxy = Some(Arc::new(resolved));
                        }
                        policy::validate_proxy_route_addresses(
                            route,
                            prepared_proxy.as_ref().unwrap(),
                        )?;
                    }
                    let client = if config.mode == "http" && config.proxy.is_none() {
                        Some(make_client(&u, &addrs, &config)?)
                    } else {
                        None
                    };
                    selected.push(WorkTarget {
                        target: Target {
                            id: p.id.clone(),
                            name: p.name.clone(),
                            url: u.to_string(),
                            region: String::new(),
                            source: String::new(),
                            checked_at: String::new(),
                            probe_only: false,
                        },
                        permit: Some(p.clone()),
                        domain: policy::domain_key(&u),
                        client,
                        addr: Some(addrs[0]),
                        proxy: prepared_proxy.clone(),
                    });
                }
            }
            if selected.is_empty() || selected.len() > 1000 {
                return Err("请选择 1–1000 个符合当前模式的目标".into());
            }
            if config.mode == "probe"
                && let Some(proxy_config) = &config.proxy
            {
                let token = self.shutdown.child_token();
                let resolved = Arc::new(
                    proxy::resolve_proxy(
                        proxy_config,
                        self.control_port,
                        &token,
                        Duration::from_secs(config.timeout_secs),
                    )
                    .await
                    .map_err(|e| e.to_string())?,
                );
                for target in &mut selected {
                    // Public target DNS is intentionally deferred to each admitted
                    // worker so large probe sets cannot stall start for many minutes.
                    target.proxy = Some(resolved.clone());
                }
            }
            Ok(selected)
        })
        .await?;
        policy::check_fd_budget(
            config.concurrency,
            selected
                .iter()
                .any(|t| t.addr.is_some_and(|a| a.ip().is_loopback())),
        )?;
        authorization_window(&selected, Duration::from_secs(config.duration_secs))?;
        let mut metrics = Metrics::new(
            config.clone(),
            &selected
                .iter()
                .map(|s| s.target.clone())
                .collect::<Vec<_>>(),
        );
        metrics.run.route = route;
        let metrics = Arc::new(Mutex::new(metrics));
        let cancel = self.shutdown.child_token();
        let initial = metrics.lock().unwrap().run.clone();
        let history = self.history.clone();
        if let Some(history) = history.clone() {
            let marker = initial.clone();
            tokio::task::spawn_blocking(move || history.begin(&marker))
                .await
                .map_err(|e| format!("运行标记任务失败：{e}"))??;
        }
        let cooldowns = self.cooldowns.clone();
        let path = self.cooldown_path.clone();
        let port = self.control_port;
        let run_metrics = metrics.clone();
        let run_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            execute(
                config,
                selected,
                run_metrics,
                run_cancel,
                (cooldowns, path),
                port,
                history,
            )
            .await;
        });
        *guard = Some(Active {
            metrics,
            cancel,
            task: Some(task),
        });
        Ok(initial)
    }
}
async fn bounded_preparation<T>(
    shutdown: &CancellationToken,
    preparation: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => Err("应用退出，准备已取消".into()),
        result = tokio::time::timeout(Duration::from_secs(5), preparation) => {
            result.map_err(|_| "目标准备总耗时超时（最多 5 秒）".to_owned())?
        }
    }
}

fn make_client(
    url: &url::Url,
    addrs: &[std::net::SocketAddr],
    c: &RunConfig,
) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(c.timeout_secs.min(5)))
        .timeout(Duration::from_secs(c.timeout_secs))
        .resolve_to_addrs(&policy::host(url), addrs)
        .user_agent(concat!(
            "AoNet/",
            env!("CARGO_PKG_VERSION"),
            " local-network-diagnostics"
        ));
    if c.fresh_connections {
        b = b.http1_only().pool_max_idle_per_host(0);
    }
    b.build().map_err(|e| format!("HTTP 客户端初始化失败：{e}"))
}
async fn http_work(
    t: WorkTarget,
    c: RunConfig,
    port: Option<u16>,
    bytes: Arc<AtomicU64>,
    backoff: Arc<Backoff>,
    cancel: CancellationToken,
) -> Outcome {
    let began = Instant::now();
    let mut o = Outcome::default();
    let result: Result<(), String> = async {
        let u = policy::parse_url(&t.target.url)?;
        let client = if t.proxy.is_some() {
            None
        } else if c.proxy.is_some() {
            return Err("代理配置缺少准备好的固定路径".into());
        } else {
            match t.client {
                Some(client) => Some(client),
                None => {
                    let a = policy::resolve(&u, t.permit.as_ref(), port).await?;
                    Some(make_client(&u, &a, &c)?)
                }
            }
        };
        if backoff.stopped(&t.domain) || backoff.error.lock().unwrap().is_some() {
            o.cancelled = true;
            return Ok(());
        }
        if let Some(permit) = &t.permit
            && chrono::DateTime::parse_from_rfc3339(&permit.expires_at)
                .map_err(|_| "授权有效期无效")?
                <= chrono::Utc::now()
        {
            return Err("许可已过期".into());
        }
        if let Some(proxy) = &t.proxy {
            let addr = match t.addr {
                Some(addr) => addr,
                None if c.mode == "probe" => policy::resolve_proxy_target(&u, None, port).await?[0],
                None => return Err("代理目标缺少固定地址".into()),
            };
            let method = if c.mode == "probe" {
                reqwest::Method::HEAD
            } else {
                reqwest::Method::GET
            };
            let response = crate::proxy_http::request(
                &u,
                addr,
                proxy,
                method,
                Duration::from_secs(c.timeout_secs),
                &cancel,
                |status, headers| {
                    o.status = Some(status.as_u16());
                    o.location = headers
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .map(|v| v.chars().take(2048).collect());
                    if status.as_u16() == 429 || status.as_u16() == 503 {
                        let seconds = policy::retry_after(
                            headers.get("retry-after").and_then(|v| v.to_str().ok()),
                        );
                        backoff.observe(&t.domain, seconds);
                        o.retry_after_secs = Some(seconds);
                    }
                },
            )
            .await?;
            let n = response.body.len() as u64;
            let previous = bytes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                    Some(v.saturating_add(n).min(BYTE_BUDGET))
                })
                .expect("budget update always succeeds");
            o.bytes = n.min(BYTE_BUDGET.saturating_sub(previous));
            o.truncated = response.truncated || o.bytes < n;
            return Ok(());
        }
        let client = client.ok_or("缺少 HTTP 客户端")?;
        let req = if c.mode == "probe" {
            client.head(u)
        } else {
            client.get(u)
        };
        let mut response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                "请求超时".into()
            } else if e.is_connect() {
                format!("建连/TLS 失败：{e}")
            } else {
                format!("HTTP 传输失败：{e}")
            }
        })?;
        let status = response.status().as_u16();
        o.status = Some(status);
        o.location = response
            .headers()
            .get("location")
            .and_then(|s| s.to_str().ok())
            .map(|s| s.chars().take(2048).collect());
        if status == 429 || status == 503 {
            let seconds = policy::retry_after(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok()),
            );
            backoff.observe(&t.domain, seconds);
            o.retry_after_secs = Some(seconds);
        }
        if c.mode != "probe" {
            let content_length = response.content_length();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| format!("响应读取失败：{e}"))?
            {
                let remaining = 65536u64.saturating_sub(o.bytes);
                let n = (chunk.len() as u64).min(remaining);
                let previous = bytes
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                        Some(v.saturating_add(n).min(BYTE_BUDGET))
                    })
                    .expect("budget update always succeeds");
                let counted = n.min(BYTE_BUDGET.saturating_sub(previous));
                o.bytes += counted;
                if counted < n {
                    o.truncated = true;
                    break;
                }
                if chunk.len() as u64 >= remaining {
                    // Exact Content-Length means an exactly-64 KiB response is complete.
                    o.truncated = content_length != Some(o.bytes);
                    break;
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        o.error = Some(e);
    }
    o.latency_ms = began.elapsed().as_secs_f64() * 1000.0;
    o
}
async fn tcp_work(
    t: WorkTarget,
    metrics: Arc<Mutex<Metrics>>,
    i: usize,
    cancel: CancellationToken,
    timeout: Duration,
    end: Instant,
) -> Outcome {
    use tokio::io::AsyncReadExt;
    let began = Instant::now();
    let connected = tokio::select! {
        biased;
        _ = tokio::time::sleep_until(end) => return Outcome { cancelled: true, ..Default::default() },
        _ = cancel.cancelled() => return Outcome { cancelled: true, ..Default::default() },
        result = async {
            if let Some(proxy) = &t.proxy {
                proxy::connect_resolved_proxy_tunnel(proxy, t.addr.unwrap(), &cancel, timeout)
                    .await.map(|tunnel| tunnel.stream).map_err(|e| e.to_string())
            } else {
                tokio::time::timeout(timeout, tokio::net::TcpStream::connect(t.addr.unwrap()))
                    .await.map_err(|_| "TCP 建连超时".to_owned())?
                    .map_err(|e| format!("TCP 建连失败：{e}"))
            }
        } => result,
    };
    let mut socket = match connected {
        Ok(s) => s,
        Err(e) => {
            return Outcome {
                error: Some(e),
                latency_ms: began.elapsed().as_secs_f64() * 1000.0,
                ..Default::default()
            };
        }
    };
    let latency_ms = began.elapsed().as_secs_f64() * 1000.0;
    metrics.lock().unwrap().connected(i, true);
    let mut buf = [0u8; 1];
    let error = loop {
        tokio::select! {
            biased;
            _=tokio::time::sleep_until(end)=>break None,
            _=cancel.cancelled()=>break None,
            read=socket.read(&mut buf)=>match read {Ok(0)=>break Some("远端已关闭 TCP 连接".to_string()),Ok(_)=>{},Err(e)=>break Some(format!("TCP 连接读取失败：{e}"))}
        }
    };
    metrics.lock().unwrap().connected(i, false);
    Outcome {
        tcp_success: true,
        latency_ms,
        error,
        ..Default::default()
    }
}

async fn execute(
    c: RunConfig,
    targets: Vec<WorkTarget>,
    metrics: Arc<Mutex<Metrics>>,
    cancel: CancellationToken,
    cooldown: (Arc<Mutex<Cooldowns>>, PathBuf),
    port: Option<u16>,
    history: Option<crate::history::History>,
) {
    let (cooldowns, path) = cooldown;
    let start = Instant::now();
    let duration = Duration::from_secs(c.duration_secs);
    // Revalidate after every preparation await, including the durable marker.
    // A rejected run is still terminally archived, but issues no network work.
    let window = authorization_window(&targets, duration);
    let authorization = window.as_ref().ok().copied().flatten();
    let end = authorization
        .map(|(_, expiry)| expiry.min(start + duration))
        .unwrap_or(start + duration);
    let backoff = Arc::new(Backoff {
        stopped: Mutex::new(HashSet::new()),
        error: Mutex::new(None),
        cooldowns: cooldowns.clone(),
        path: path.clone(),
        probe: c.mode == "probe",
    });
    let mut tasks: JoinSet<(usize, Outcome)> = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut next = start;
    let mut cursor = 0;
    let mut issued = 0u64;
    let mut last_sample = start;
    let bytes = Arc::new(AtomicU64::new(0));
    let mut reason = window
        .as_ref()
        .err()
        .cloned()
        .unwrap_or_else(|| "达到设置时长".to_owned());
    let mut failed = window.is_err();
    loop {
        if failed {
            break;
        }
        if let Some(error) = backoff.error.lock().unwrap().clone() {
            reason = error;
            failed = true;
            break;
        }
        if authorization.is_some_and(|(expiry, deadline)| {
            chrono::Utc::now() >= expiry || Instant::now() >= deadline
        }) {
            reason = "许可已过期".into();
            failed = true;
            break;
        }
        if cancel.is_cancelled() {
            reason = "用户停止或应用退出".into();
            break;
        }
        if Instant::now() >= end {
            break;
        }
        if bytes.load(Ordering::SeqCst) >= BYTE_BUDGET {
            reason = "达到读取字节预算".into();
            break;
        }
        if issued >= 100000 {
            reason = "达到请求起发预算".into();
            break;
        }
        tokio::select! {
            _=cancel.cancelled()=>{reason="用户停止或应用退出".into();break;},
            Some(result)=tasks.join_next(),if !tasks.is_empty()=>{
                match result {
                    Ok((i,o))=>{
                        // Backoff was already published and durably reserved at headers.
                        metrics.lock().unwrap().finish(i,o);
                    },
                    Err(e)=>{reason=format!("工作任务异常：{e}");failed=true;break;}
                }
            },
            _=tick.tick()=>{
                let now=Instant::now();
                if now.duration_since(last_sample)>=Duration::from_secs(1){metrics.lock().unwrap().sample(start.elapsed().as_secs_f64());last_sample=now;}
                let ramp=if c.ramp_secs==0{c.concurrency}else{((start.elapsed().as_secs_f64()/c.ramp_secs as f64*c.concurrency as f64).ceil() as usize).clamp(1,c.concurrency)};
                if now<next || tasks.len()>=ramp || (c.mode=="tcp" && issued>=c.concurrency as u64){continue;}
                let index=(0..targets.len()).map(|off|(cursor+off)%targets.len()).find(|&i| !backoff.stopped(&targets[i].domain) && (c.mode!="probe" || cooldowns.lock().unwrap().ready(&targets[i].domain)));
                let Some(i)=index else{continue;};cursor=(i+1)%targets.len();
                if c.mode=="probe" {
                    let mut guard = cooldowns.lock().unwrap();
                    if !guard.ready(&targets[i].domain) { continue; }
                    if let Err(e)=guard.reserve_dispatch(&targets[i].domain,&path){reason=e;failed=true;break;}
                }
                next=Instant::now()+Duration::from_secs_f64(1.0/c.rps as f64);
                // Bound task count before spawning; no queue of waiting workers.
                metrics.lock().unwrap().start(i);issued+=1;
                let t=targets[i].clone();let conf=c.clone();let token=cancel.clone();let m=metrics.clone();let b=bytes.clone();let backoff=backoff.clone();
                tasks.spawn(async move {
                    let began=Instant::now();
                    let o=if conf.mode=="tcp" {
                        // Let TCP release its counter and socket on cancellation; do not drop its future externally.
                        tcp_work(t,m,i,token.clone(),Duration::from_secs(conf.timeout_secs),end).await
                    } else {
                        tokio::select! {
                            biased;
                            _=tokio::time::sleep_until(end)=>Outcome{cancelled:true,latency_ms:began.elapsed().as_secs_f64()*1000.0,..Default::default()},
                            _=token.cancelled()=>Outcome{cancelled:true,latency_ms:began.elapsed().as_secs_f64()*1000.0,..Default::default()},
                            result=http_work(t,conf.clone(),port,b,backoff.clone(),token.clone())=>result,
                            _=tokio::time::sleep(Duration::from_secs(conf.timeout_secs))=>Outcome{error:Some("总耗时超时（含 DNS）".into()),latency_ms:began.elapsed().as_secs_f64()*1000.0,..Default::default()},
                        }
                    };
                    if conf.proxy.is_some()
                        && o.error.as_ref().is_some_and(|error| error.contains("proxy authentication required (407)"))
                    {
                        *backoff.error.lock().unwrap() = o.error.clone();
                    }
                    (i,o)
                });
            }
        }
    }
    let user_stop = cancel.is_cancelled();
    cancel.cancel();
    {
        metrics.lock().unwrap().run.status = "stopping".into();
    }
    while let Some(result) = tasks.join_next().await {
        if let Ok((i, o)) = result {
            metrics.lock().unwrap().finish(i, o);
        }
    }
    while policy::target_dns_pending() != 0 {
        metrics.lock().unwrap().run.reason = "等待系统DNS解析收尾；连接已释放".into();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // A header can arrive while workers are being drained. Its persistence
    // failure must remain visible even though no scheduler iteration follows.
    if let Some(error) = backoff.error.lock().unwrap().clone() {
        reason = error;
        failed = true;
    }
    let terminal = {
        let mut m = metrics.lock().unwrap();
        // All sockets and workers have been dropped before exposing a terminal state.
        m.run.active = 0;
        m.run.connected = 0;
        for t in &mut m.run.targets {
            t.active = 0;
            t.connected = 0;
        }
        m.sample(start.elapsed().as_secs_f64());
        m.run.reason = reason;
        let mut terminal = m.run.clone();
        terminal.status = if failed {
            "failed"
        } else if user_stop {
            "stopped"
        } else {
            "completed"
        }
        .into();
        terminal
    };
    let mut terminal = terminal;
    if let Some(history) = history {
        let archived = terminal.clone();
        let result = tokio::task::spawn_blocking(move || history.finish(&archived))
            .await
            .map_err(|e| e.to_string())
            .and_then(|v| v);
        if let Err(error) = result {
            terminal.reason = format!("历史保存失败：{error}");
        }
    }
    metrics.lock().unwrap().run = terminal;
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::{model::ProxyPermit, proxy::ProxyConfig};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    fn config() -> RunConfig {
        RunConfig {
            mode: "http".into(),
            list_ids: vec![],
            target_ids: vec![],
            authorized_ids: vec!["owned".into()],
            concurrency: 1,
            rps: 1,
            duration_secs: 1,
            timeout_secs: 1,
            ramp_secs: 0,
            fresh_connections: false,
            proxy: None,
        }
    }

    fn permit(url: String, proxy: Option<&ProxyConfig>) -> Permit {
        Permit {
            id: "owned".into(),
            name: "Owned fixture".into(),
            url,
            modes: vec!["http".into()],
            allowed_ips: vec![
                "127.0.0.1/32".parse().unwrap(),
                "10.0.0.1/32".parse().unwrap(),
            ],
            max_concurrency: 1,
            max_rps: 1,
            max_duration: 2,
            authorization: "Controlled fixture consent".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            proxy_routes: proxy
                .map(|p| {
                    vec![ProxyPermit {
                        url: p.url.clone(),
                        allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
                        authorization: "Controlled proxy fixture consent".into(),
                    }]
                })
                .unwrap_or_default(),
        }
    }

    async fn headers(stream: &mut tokio::net::TcpStream) -> String {
        let mut bytes = vec![];
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
            assert!(bytes.len() < 16_384);
        }
        String::from_utf8(bytes).unwrap()
    }

    async fn terminal(engine: &Engine) -> Run {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = engine.snapshot().await.unwrap();
                if !run.active() {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn explicit_route_overrides_request_and_survives_snapshot_and_history() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = ProxyConfig {
            url: format!("http://{}", listener.local_addr().unwrap()),
        };
        let route = RouteSnapshot {
            settings_revision: 42,
            profile_id: Some("frozen".into()),
            profile_name: Some("Frozen proxy".into()),
            proxy: Some(proxy.clone()),
            local_direct: false,
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(
                headers(&mut stream)
                    .await
                    .starts_with("CONNECT 10.0.0.1:80 ")
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            assert!(headers(&mut stream).await.starts_with("GET / "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let history = crate::history::History::open(dir.path()).unwrap();
        let engine = Engine::new(dir.path().into(), None, CancellationToken::new())
            .unwrap()
            .with_history(history.clone());
        let mut config = config();
        config.proxy = Some(ProxyConfig {
            url: "invalid request proxy must be ignored".into(),
        });
        let initial = engine
            .start_with_route(
                config,
                &[],
                &[permit("http://10.0.0.1/".into(), Some(&proxy))],
                route.clone(),
            )
            .await
            .unwrap();
        assert_eq!(initial.route, route);
        assert_eq!(initial.config.proxy, route.proxy);
        assert_eq!(engine.snapshot().await.unwrap().route, route);
        let run = terminal(&engine).await;
        assert_eq!(run.route, route);
        assert_eq!(run.config.proxy, route.proxy);
        assert_eq!(history.get(&run.id).unwrap().route, route);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn direct_route_clears_stale_request_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(headers(&mut stream).await.starts_with("GET / "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
        let route = RouteSnapshot {
            settings_revision: 9,
            ..RouteSnapshot::default()
        };
        let mut config = config();
        config.proxy = Some(ProxyConfig {
            url: "invalid stale proxy".into(),
        });
        let initial = engine
            .start_with_route(config, &[], &[permit(url, None)], route.clone())
            .await
            .unwrap();
        assert_eq!(initial.config.proxy, None);
        assert_eq!(initial.route, route);
        let run = terminal(&engine).await;
        assert_eq!(run.route, route);
        assert_eq!(run.config.proxy, None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn legacy_start_derives_normalized_route_from_config() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = ProxyConfig {
            url: format!("http://{}/", listener.local_addr().unwrap()),
        };
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
        let mut config = config();
        config.proxy = Some(proxy.clone());
        let initial = engine
            .start(
                config,
                &[],
                &[permit("http://10.0.0.1/".into(), Some(&proxy))],
            )
            .await
            .unwrap();
        let normalized = proxy::normalize_proxy_config(&proxy, None).unwrap();
        assert_eq!(
            initial.route,
            RouteSnapshot {
                proxy: Some(normalized.clone()),
                ..RouteSnapshot::default()
            }
        );
        assert_eq!(initial.config.proxy, Some(normalized));
        engine.shutdown_and_wait().await;
    }
}
