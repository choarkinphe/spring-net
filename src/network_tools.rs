//! One deliberately small, bounded Ping/Traceroute task at a time.
use crate::network_process::{self, OUTPUT_LIMIT, OutputCallback};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, ToSocketAddrs},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    process::Command,
    sync::{Mutex as AsyncMutex, oneshot},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    Ping,
    Traceroute,
    Dns,
    Tcp,
    Tls,
    Http,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: String,
    pub target: String,
    pub tool: Tool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}
/// Equality includes the effective port and HTTP path, not merely the host.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedRequest {
    request_id: String,
    target: String,
    host: String,
    tool: Tool,
    port: Option<u16>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub id: String,
    pub request_id: String,
    pub tool: Tool,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub resolved_ip: Option<String>,
    pub status: String,
    pub output: String,
    pub summary: String,
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}
impl Snapshot {
    pub fn active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "resolving" | "running" | "stopping" | "draining"
        )
    }
}
type Shared = Arc<Mutex<Snapshot>>;
type Resolver = Arc<dyn Fn(String) -> Result<Vec<IpAddr>, String> + Send + Sync>;
struct Active {
    request: NormalizedRequest,
    shared: Shared,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}
#[derive(Default)]
struct State {
    current: Option<Active>,
    last_start: Option<Instant>,
}
pub struct NetworkToolManager {
    state: AsyncMutex<State>,
    shutdown: CancellationToken,
    resolver: Resolver,
    dns_budget: Duration,
    control_port: Option<u16>,
}
pub type Manager = NetworkToolManager;
impl NetworkToolManager {
    pub fn new(shutdown: CancellationToken) -> Self {
        Self {
            state: AsyncMutex::new(State::default()),
            shutdown,
            resolver: Arc::new(|host| {
                (host.as_str(), 0)
                    .to_socket_addrs()
                    .map(|iter| {
                        let mut addresses = Vec::new();
                        for address in iter {
                            if !addresses.contains(&address.ip()) {
                                addresses.push(address.ip());
                                if addresses.len() == 65 {
                                    break;
                                }
                            }
                        }
                        addresses
                    })
                    .map_err(|e| e.to_string())
            }),
            dns_budget: Duration::from_secs(5),
            control_port: None,
        }
    }
    /// Blocks probes against the service's own listener.
    pub fn with_control_port(mut self, port: Option<u16>) -> Self {
        self.control_port = port;
        self
    }
    /// Only the currently retained request is idempotent. Nothing is replayed
    /// from persistence, and no request can supply command flags or budgets.
    pub async fn existing_request(&self, request: &Request) -> Result<Option<Snapshot>, String> {
        let request = normalize(request.clone())?;
        validate_control_port(&request, self.control_port)?;
        let state = self.state.lock().await;
        duplicate(&state, &request)
    }
    pub async fn start(&self, request: Request) -> Result<Snapshot, String> {
        let request = normalize(request)?;
        validate_control_port(&request, self.control_port)?;
        let mut state = self.state.lock().await;
        if let Some(snapshot) = duplicate(&state, &request)? {
            return Ok(snapshot);
        }
        if self.shutdown.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if state.current.as_ref().is_some_and(|a| {
            a.shared.lock().unwrap().active() || a.task.as_ref().is_some_and(|t| !t.is_finished())
        }) {
            return Err("网络诊断正在运行、停止或收尾中".into());
        }
        if let Some(task) = state.current.as_mut().and_then(|a| a.task.take()) {
            task.await.map_err(|e| e.to_string())?;
        }
        let now = Instant::now();
        if state
            .last_start
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(5))
        {
            return Err("网络诊断冷却时间：两次开始至少间隔 5 秒".into());
        }
        if let Ok(ip) = request.host.parse::<IpAddr>() {
            checked_answers(&request, vec![ip], true)?;
        }
        let stamp = stamp();
        let snapshot = Snapshot {
            id: uuid::Uuid::new_v4().to_string(),
            request_id: request.request_id.clone(),
            tool: request.tool,
            target: request.target.clone(),
            port: request.port,
            resolved_ip: None,
            status: "resolving".into(),
            output: String::new(),
            summary: "正在解析目标地址；尚未发送探测".into(),
            exit_code: None,
            started_at: stamp.clone(),
            updated_at: stamp,
            finished_at: None,
        };
        let returned = snapshot.clone();
        let shared = Arc::new(Mutex::new(snapshot));
        let cancel = self.shutdown.child_token();
        let task = tokio::spawn(execute(
            shared.clone(),
            request.clone(),
            cancel.clone(),
            self.shutdown.clone(),
            self.resolver.clone(),
            self.dns_budget,
        ));
        state.last_start = Some(now);
        state.current = Some(Active {
            request,
            shared,
            cancel,
            task: Some(task),
        });
        Ok(returned)
    }
    pub async fn snapshot(&self) -> Option<Snapshot> {
        self.state
            .lock()
            .await
            .current
            .as_ref()
            .map(|a| a.shared.lock().unwrap().clone())
    }
    pub async fn stop(&self, id: &str) -> Result<Snapshot, String> {
        let state = self.state.lock().await;
        let active = state.current.as_ref().ok_or("尚无网络诊断任务")?;
        let mut s = active.shared.lock().unwrap();
        if s.id != id {
            return Err("任务 ID 与最近一次诊断不匹配".into());
        }
        if s.active() {
            if s.status != "draining" {
                s.status = "stopping".into();
            }
            s.summary = "正在停止；等待子进程回收或系统 DNS 收尾".into();
            s.updated_at = stamp();
            active.cancel.cancel();
        }
        Ok(s.clone())
    }
    /// Admission barrier for the other service managers. A terminal snapshot
    /// alone does not admit work until its worker has actually returned.
    pub async fn settle(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if let Some(active) = state.current.as_mut() {
            if active.shared.lock().unwrap().active()
                || active.task.as_ref().is_some_and(|t| !t.is_finished())
            {
                return Err("网络诊断正在运行、停止或收尾中".into());
            }
            if let Some(task) = active.task.take() {
                task.await.map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    pub async fn shutdown_and_wait(&self) {
        self.shutdown.cancel();
        let task = {
            let mut state = self.state.lock().await;
            state.current.as_mut().and_then(|a| {
                a.cancel.cancel();
                a.task.take()
            })
        };
        // Never abort a process owner. Only OS DNS has a 2 second shutdown exception.
        if let Some(task) = task
            && let Err(error) = task.await
        {
            eprintln!("network tool shutdown: {error}");
        }
    }
}
impl Drop for NetworkToolManager {
    fn drop(&mut self) {
        // Manager owns its child token, not the caller's root shutdown token.
        if let Some(active) = self.state.get_mut().current.as_ref() {
            active.cancel.cancel();
        }
    }
}
fn duplicate(state: &State, request: &NormalizedRequest) -> Result<Option<Snapshot>, String> {
    if let Some(a) = &state.current
        && a.request.request_id == request.request_id
    {
        if a.request != *request {
            return Err("request_id 已用于不同的规范化目标或工具".into());
        }
        return Ok(Some(a.shared.lock().unwrap().clone()));
    }
    Ok(None)
}
fn stamp() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn update(shared: &Shared, f: impl FnOnce(&mut Snapshot)) {
    let mut s = shared.lock().unwrap();
    f(&mut s);
    s.updated_at = stamp();
}
fn finish(shared: &Shared, status: &str, summary: String) {
    update(shared, |s| {
        s.status = status.into();
        s.summary = summary;
        s.finished_at = Some(stamp());
    });
}

fn normalize(request: Request) -> Result<NormalizedRequest, String> {
    if request.request_id.is_empty()
        || request.request_id.len() > 128
        || !request
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err("request_id 须为 1–128 位 ASCII 字母、数字或 - _ .".into());
    }
    let input = request.target.trim();
    if input.is_empty()
        || input.len() > 2048
        || input.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("请输入一个域名、IP 地址或 HTTP(S) 网址".into());
    }
    if request.port == Some(0) {
        return Err("端口必须为 1–65535".into());
    }
    if request.port.is_some() && !matches!(request.tool, Tool::Tcp | Tool::Tls) {
        return Err("仅 TCP/TLS 接受单独端口字段".into());
    }
    let mut full_url = None;
    let mut url_port = None;
    let host = if input.contains("://") {
        let url = Url::parse(input).map_err(|_| "HTTP(S) 网址格式无效")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("仅接受无凭据、查询参数和片段的 HTTP(S) 网址".into());
        }
        // Url drops explicit default ports, so retain their original intent for
        // TCP's required port and TLS/manual conflict detection.
        url_port = explicit_url_port(input)?;
        if url_port == Some(0) && matches!(request.tool, Tool::Tcp | Tool::Tls | Tool::Http) {
            return Err("网址端口须为 1–65535".into());
        }
        if input.contains('\\') {
            return Err("网址不接受反斜线".into());
        }
        if request.tool == Tool::Http {
            full_url = Some(url.clone());
        }
        match url.host().ok_or("网址必须包含主机地址")? {
            Host::Domain(domain) => domain.to_owned(),
            Host::Ipv4(ip) => ip.to_string(),
            Host::Ipv6(ip) => ip.to_string(),
        }
    } else if let Ok(ip) = input.parse::<IpAddr>() {
        ip.to_string()
    } else if input.starts_with('[') && input.ends_with(']') {
        input[1..input.len() - 1]
            .parse::<IpAddr>()
            .map_err(|_| "方括号内的 IP 地址无效")?
            .to_string()
    } else {
        if input.contains([
            '/', ':', '@', '?', '#', '%', '\\', '*', ',', ';', '|', '&', '$', '`', '(', ')',
        ]) {
            return Err("仅接受单个域名或 IP；不支持参数、端口、列表或 IPv6 区域标识".into());
        }
        match Host::parse(input).map_err(|_| "域名格式无效")? {
            Host::Domain(domain) => domain,
            // Reject nonstandard numeric IP forms instead of silently turning
            // octal/hex/integer forms into a permitted loopback self-test.
            _ => return Err("请使用标准 IP 地址格式".into()),
        }
    };
    if host.ends_with('.') {
        return Err("暂不支持带尾随点的绝对域名；为避免改变系统解析语义，未自动删除尾随点".into());
    }
    let host = host.to_ascii_lowercase();
    if host.parse::<IpAddr>().is_err() {
        if host.len() > 253
            || host.is_empty()
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err("域名格式无效 labels".into());
        }
        // Names resolving to loopback are blocked after DNS; only explicit IP
        // loopback is eligible for the bounded local ICMP self-test.
        if request.tool != Tool::Dns
            && (host == "localhost"
                || host.ends_with(".localhost")
                || host == "metadata.google.internal")
        {
            return Err("本机自检请直接输入回环 IP；禁止元数据服务名称".into());
        }
    }
    let port = match request.tool {
        Tool::Tcp | Tool::Tls => {
            if request.port.zip(url_port).is_some_and(|(a, b)| a != b) {
                return Err("单独端口与网址显式端口冲突".into());
            }
            Some(
                request
                    .port
                    .or(url_port)
                    .or((request.tool == Tool::Tls).then_some(443))
                    .ok_or("TCP 必须填写端口或提供含显式端口的网址")?,
            )
        }
        Tool::Http => Some(
            full_url
                .as_ref()
                .ok_or("HTTP 必须输入完整 HTTP(S) 网址")?
                .port_or_known_default()
                .ok_or("网址端口无效")?,
        ),
        _ => None,
    };
    let target = if let Some(mut url) = full_url {
        // Use precisely the same normalized host for fixed DNS override, Host
        // and TLS identity; preserve case/escaping in the path, never lowercase it.
        let url_host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.clone()
        };
        url.set_host(Some(&url_host)).map_err(|_| "网址主机无效")?;
        url.to_string()
    } else {
        host.clone()
    };
    let normalized = NormalizedRequest {
        request_id: request.request_id,
        target,
        host,
        tool: request.tool,
        port,
    };
    if let Ok(ip) = normalized.host.parse() {
        checked_answers(&normalized, vec![ip], true)?;
    }
    Ok(normalized)
}

fn explicit_url_port(input: &str) -> Result<Option<u16>, String> {
    let authority = input
        .split_once("://")
        .ok_or("网址无效")?
        .1
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return Err("网址必须在 :// 后直接包含主机地址".into());
    }
    // Reject even an empty credential marker, which Url::username hides.
    if authority.contains('@') {
        return Err("网址不能包含凭据".into());
    }
    let suffix = if authority.starts_with('[') {
        authority.split_once(']').ok_or("IPv6 网址格式无效")?.1
    } else {
        authority.find(':').map_or("", |i| &authority[i..])
    };
    if suffix.is_empty() {
        return Ok(None);
    }
    let port = suffix
        .strip_prefix(':')
        .ok_or("网址端口无效")?
        .parse::<u16>()
        .map_err(|_| "网址端口须为 1–65535")?;
    Ok(Some(port))
}

fn validate_control_port(
    request: &NormalizedRequest,
    control_port: Option<u16>,
) -> Result<(), String> {
    if request.port.is_some() && request.port == control_port {
        return Err("已阻止 AoNet 控制服务端口（对所有目标保守阻止）".into());
    }
    Ok(())
}

fn checked_answers(
    request: &NormalizedRequest,
    addresses: Vec<IpAddr>,
    literal: bool,
) -> Result<Vec<IpAddr>, String> {
    if request.tool == Tool::Dns {
        return bounded_answers(addresses);
    }
    validate_addresses(
        addresses,
        literal && matches!(request.tool, Tool::Ping | Tool::Traceroute),
    )
}

fn bounded_answers(addresses: Vec<IpAddr>) -> Result<Vec<IpAddr>, String> {
    let mut unique = Vec::new();
    for ip in addresses {
        if !unique.contains(&ip) {
            unique.push(ip);
        }
        if unique.len() > 64 {
            return Err("DNS 必须返回 1–64 个去重地址".into());
        }
    }
    if unique.is_empty() {
        return Err("DNS 必须返回 1–64 个去重地址".into());
    }
    Ok(unique)
}

fn validate_addresses(addresses: Vec<IpAddr>, literal: bool) -> Result<Vec<IpAddr>, String> {
    if addresses.is_empty() || addresses.len() > 64 {
        return Err("DNS 必须返回 1–64 个地址".into());
    }
    // Validate every answer before deduplication/selection, not just the first.
    for ip in &addresses {
        let allowed = match ip {
            IpAddr::V4(v) => {
                let [a, b, c, _] = v.octets();
                (v.is_loopback() && literal)
                    || (!v.is_loopback()
                        && a != 0
                        && a < 224
                        && !(a == 169 && b == 254)
                        && !(a == 100 && (64..=127).contains(&b))
                        && !(a == 192 && ((b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                        && !(a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                        && !(a == 203 && b == 0 && c == 113)
                        && *v != std::net::Ipv4Addr::new(168, 63, 129, 16))
            }
            IpAddr::V6(v) => {
                let s = v.segments();
                (v.is_loopback() && literal)
                    || (((s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xe000) == 0x2000)
                    && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                    && !(s[0] == 0x3fff && s[1] < 0x1000) // Documentation prefix
                    && !(s[0] == 0x2002) // 6to4 can embed otherwise forbidden IPv4
                    && *v != "fd00:ec2::254".parse::<std::net::Ipv6Addr>().unwrap()
                    && *v != "fd20:ce::254".parse::<std::net::Ipv6Addr>().unwrap())
            }
        };
        if !allowed {
            return Err(format!("已阻止特殊用途或元数据地址：{ip}"));
        }
    }
    let mut unique = Vec::new();
    for ip in addresses {
        if !unique.contains(&ip) {
            unique.push(ip);
        }
    }
    Ok(unique)
}

async fn resolve(
    shared: &Shared,
    request: &NormalizedRequest,
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
    resolver: Resolver,
    budget: Duration,
) -> Result<Vec<IpAddr>, String> {
    if let Ok(ip) = request.host.parse::<IpAddr>() {
        return checked_answers(request, vec![ip], true);
    }
    let (tx, mut rx) = oneshot::channel();
    let host = request.host.clone();
    let thread = std::thread::Builder::new()
        .name("aonet-network-tool-dns".into())
        .spawn(move || {
            let _ = tx.send(resolver(host));
        })
        .map_err(|e| format!("无法启动系统 DNS：{e}"))?;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        _ = tokio::time::sleep(budget) => None,
        result = &mut rx => Some(result),
    };
    if let Some(result) = result {
        let _ = thread.join();
        return checked_answers(request, result.map_err(|e| e.to_string())??, false);
    }
    let cancelled = cancel.is_cancelled();
    update(shared, |s| {
        s.status = "draining".into();
        s.summary = if cancelled {
            "已请求停止；等待不可中断的系统 DNS 收尾（丢弃迟到结果）"
        } else {
            "DNS 已超时；等待系统解析器收尾（丢弃迟到结果）"
        }
        .into();
    });
    let ended = tokio::select! {
        _ = &mut rx => true,
        _ = shutdown.cancelled() => tokio::time::timeout(Duration::from_secs(2), &mut rx).await.is_ok(),
    };
    if ended {
        let _ = thread.join();
    } else {
        // Sole permitted detach: OS DNS cannot be interrupted; application is
        // exiting and this manager can never admit another lookup.
        drop(thread);
        return Err("退出等待 2 秒后系统 DNS 线程仍未结束；并未取消该线程，未发送探测".into());
    }
    Err(if cancelled {
        "系统 DNS 已收尾，诊断已停止"
    } else {
        "DNS 已超时且系统解析器已收尾；未发送探测"
    }
    .into())
}

#[cfg(target_os = "macos")]
fn ping_command(ip: IpAddr) -> Option<Command> {
    let mut command = Command::new(if ip.is_ipv4() {
        "/sbin/ping"
    } else {
        "/sbin/ping6"
    });
    command.current_dir("/").env_clear().env("LC_ALL", "C");
    // Both Darwin tools document -i in seconds and -c as echo count.
    // -W differs between ping and ping6; use supervisor's 10s deadline,
    // not an unverified cross-tool wait argument.
    command.args(["-n", "-c", "4", "-i", "1"]);
    Some(command)
}
#[cfg(target_os = "windows")]
fn ping_command(ip: IpAddr) -> Option<Command> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    use std::os::windows::ffi::OsStringExt;
    let mut path = vec![0_u16; 32768];
    let len = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) } as usize;
    if len == 0 || len >= path.len() {
        return None;
    }
    let base = std::path::PathBuf::from(std::ffi::OsString::from_wide(&path[..len]));
    let mut command = Command::new(base.join("ping.exe"));
    command
        .current_dir(&base)
        .env_clear()
        .env("SystemRoot", base.parent()?)
        .creation_flags(0x08000000);
    // Microsoft ping docs: /n count, /w timeout(milliseconds), /4 or /6.
    // https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/ping
    // Adapter only: native Windows execution has not been verified on macOS.
    command.args([
        "/n",
        "4",
        "/w",
        "1000",
        if ip.is_ipv4() { "/4" } else { "/6" },
    ]);
    Some(command)
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn ping_command(_ip: IpAddr) -> Option<Command> {
    None
}

async fn execute(
    shared: Shared,
    request: NormalizedRequest,
    cancel: CancellationToken,
    shutdown: CancellationToken,
    resolver: Resolver,
    dns_budget: Duration,
) {
    if cancel.is_cancelled() {
        finish(&shared, "cancelled", "已停止；未发送探测".into());
        return;
    }
    let dns_started = Instant::now();
    let addresses = match resolve(&shared, &request, &cancel, &shutdown, resolver, dns_budget).await
    {
        Ok(addresses) => addresses,
        Err(error) => {
            finish(
                &shared,
                if cancel.is_cancelled() {
                    "cancelled"
                } else if error.starts_with("DNS 已超时") {
                    "timeout"
                } else {
                    "error"
                },
                error,
            );
            return;
        }
    };
    if cancel.is_cancelled() {
        finish(&shared, "cancelled", "已停止；未发送探测".into());
        return;
    }
    if request.tool == Tool::Dns {
        let literal = request.host.parse::<IpAddr>().is_ok();
        let detail = if literal {
            "IP 字面量：无需 DNS 解析；无 DNS RTT".into()
        } else {
            format!(
                "系统名称解析耗时 {:.2} ms（可能来自 hosts 或缓存，并非权威 DNS RTT）",
                dns_started.elapsed().as_secs_f64() * 1000.0
            )
        };
        update(&shared, |s| {
            s.output = format!(
                "{detail}\n{}\n仅展示地址，不连接；特殊用途/元数据地址在连接检测中会被阻止。\n未测 DNS 服务器、TTL、CNAME、DNSSEC。",
                addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
        finish(
            &shared,
            "completed",
            format!(
                "共 {} 个去重地址。{detail}；未发送连接探测",
                addresses.len()
            ),
        );
        return;
    }
    let ip = addresses[0];
    if matches!(request.tool, Tool::Tcp | Tool::Tls | Tool::Http) {
        update(&shared, |s| {
            s.resolved_ip = Some(ip.to_string());
            if s.status != "stopping" {
                s.status = "running".into();
            }
        });
        let address =
            std::net::SocketAddr::new(ip, request.port.expect("normalized connection port"));
        let result = match request.tool {
            Tool::Tcp => crate::network_checks::tcp(address, &cancel).await,
            Tool::Tls => crate::network_checks::tls(&request.host, address, &cancel).await,
            Tool::Http => {
                crate::network_checks::http(&request.target, &request.host, address, &cancel).await
            }
            _ => unreachable!(),
        };
        update(&shared, |s| {
            s.output = network_process::bounded_text(result.output.as_bytes())
        });
        finish(
            &shared,
            result.status,
            format!(
                "{}；固定测试 {address}；另有 {} 个 DNS 地址未测试，无回退",
                result.summary,
                addresses.len() - 1
            ),
        );
        return;
    }
    let limitation = format!(
        "固定测试 {ip}；另有 {} 个 DNS 地址未测试。{}",
        addresses.len() - 1,
        if ip.is_loopback() {
            "仅为本机回环自检，不能证明外网连通性。"
        } else {
            "ICMP 丢包或路由星号不代表网站不可用，也不能直接定位故障。"
        }
    );
    update(&shared, |s| {
        s.resolved_ip = Some(ip.to_string());
        s.summary = limitation.clone();
    });
    let command = match request.tool {
        Tool::Ping => ping_command(ip),
        Tool::Traceroute => {
            crate::diagnostics::path::system_command(ip).map(|(command, _)| command)
        }
        _ => unreachable!("non-ICMP handled above"),
    };
    let Some(mut command) = command else {
        finish(
            &shared,
            "unsupported",
            format!("此平台没有已核实的受信系统工具。{limitation}"),
        );
        return;
    };
    command.arg(ip.to_string());
    update(&shared, |s| {
        if s.status != "stopping" {
            s.status = "running".into();
        }
    });
    let mut_bytes = Arc::new(Mutex::new(Vec::new()));
    let chunks = mut_bytes.clone();
    let stream_shared = shared.clone();
    let callback: OutputCallback = Arc::new(move |chunk| {
        let mut bytes = chunks.lock().unwrap();
        let remaining = OUTPUT_LIMIT.saturating_sub(bytes.len());
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        update(&stream_shared, |s| {
            s.output = network_process::bounded_text(&bytes)
        });
    });
    let budget = Duration::from_secs(if request.tool == Tool::Ping { 10 } else { 30 });
    let result = network_process::supervise(command, &cancel, budget, Some(callback)).await;
    let output = result.output();
    let lower = output.to_ascii_lowercase();
    let permission = lower.contains("permission denied")
        || lower.contains("operation not permitted")
        || lower.contains("must be root");
    let status = match result.outcome {
        "completed" if permission => "error",
        "completed"
            if request.tool == Tool::Ping && (result.success || ping_stats(&output).is_some()) =>
        {
            "completed"
        }
        "completed" if request.tool == Tool::Traceroute && result.success => "completed",
        "completed" | "process_error" | "truncated" => "error",
        "cancelled" => "cancelled",
        "timeout" => "timeout",
        "unsupported" => "unsupported",
        _ => "error",
    };
    let parsed = match request.tool {
        Tool::Ping => ping_stats(&output)
            .map(|s| s.summary())
            .unwrap_or_else(|| "无法识别 Ping 统计，请查看原始输出".into()),
        Tool::Traceroute => {
            let (_, _, reached) = crate::diagnostics::path::parse(&output, ip);
            if reached {
                "已观测到带时延的目标地址回复"
            } else {
                "路由仅部分可见或格式未知；未观测到带时延的目标地址回复"
            }
            .into()
        }
        _ => unreachable!("non-ICMP handled above"),
    };
    let detail = if permission {
        "系统工具权限不足".to_owned()
    } else if let Some(error) = &result.error {
        format!("系统工具错误：{error}")
    } else {
        format!(
            "进程结果：{}",
            match result.outcome {
                "completed" if result.success => "已结束",
                "completed" => "非零退出状态（不表示网站不可用）",
                "cancelled" => "已停止并回收",
                "timeout" => "达到时间上限，已回收",
                "truncated" => "输出达到 64 KiB 上限，已截断并回收",
                "unsupported" => "未安装受信系统工具",
                _ => "执行失败，已回收",
            }
        )
    };
    update(&shared, |s| {
        s.output = output;
        s.exit_code = result.exit_code;
    });
    finish(&shared, status, format!("{parsed}. {detail}. {limitation}"));
}

#[derive(Debug, PartialEq)]
struct PingStats {
    sent: u32,
    received: u32,
    loss: f64,
    rtt: Option<[f64; 4]>,
}
impl PingStats {
    fn summary(&self) -> String {
        let mut s = format!(
            "已发送 {}，收到 {}，丢包 {}%",
            self.sent, self.received, self.loss
        );
        if let Some([min, avg, max, stddev]) = self.rtt {
            s.push_str(&format!(
                "；RTT 最小/平均/最大/标准差 {min}/{avg}/{max}/{stddev} ms"
            ));
        }
        s
    }
}
/// Parse actual C-locale Darwin ping/ping6 summaries, never infer packet counts
/// from the requested budget or turn an unfamiliar localized output into zeros.
fn ping_stats(raw: &str) -> Option<PingStats> {
    let mut stats = None;
    for line in raw.lines() {
        let parts: Vec<_> = line.trim().split(", ").collect();
        if parts.len() >= 3 && parts[0].ends_with(" packets transmitted") {
            let sent = parts[0]
                .strip_suffix(" packets transmitted")?
                .parse::<u32>()
                .ok()?;
            let received = parts[1]
                .strip_suffix(" packets received")
                .or_else(|| parts[1].strip_suffix(" received"))?
                .parse::<u32>()
                .ok()?;
            let loss = parts[2]
                .strip_suffix("% packet loss")?
                .parse::<f64>()
                .ok()?;
            if sent == 0 || received > sent || !loss.is_finite() || !(0.0..=100.0).contains(&loss) {
                return None;
            }
            stats = Some(PingStats {
                sent,
                received,
                loss,
                rtt: None,
            });
        }
        if (line.starts_with("round-trip min/avg/max/stddev = ")
            || line.starts_with("rtt min/avg/max/mdev = "))
            && let Some(stats) = stats.as_mut()
        {
            let numbers: Vec<_> = line
                .split_once(" = ")?
                .1
                .strip_suffix(" ms")?
                .split('/')
                .map(str::parse::<f64>)
                .collect::<Result<_, _>>()
                .ok()?;
            if numbers.len() != 4 || numbers.iter().any(|v| !v.is_finite() || *v < 0.0) {
                return None;
            }
            stats.rtt = Some([numbers[0], numbers[1], numbers[2], numbers[3]]);
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req(target: &str) -> Request {
        Request {
            request_id: "fixture-1".into(),
            target: target.into(),
            tool: Tool::Ping,
            port: None,
        }
    }
    #[test]
    fn strict_inputs_and_host_normalization() {
        assert_eq!(
            normalize(req("https://Example.COM/path")).unwrap().target,
            "example.com"
        );
        assert_eq!(
            normalize(req("bücher.example")).unwrap().target,
            "xn--bcher-kva.example"
        );
        assert_eq!(normalize(req(" [::1] ")).unwrap().target, "::1");
        assert_eq!(normalize(req("192.168.0.1")).unwrap().target, "192.168.0.1");
        for input in [
            "",
            "--help",
            "a b",
            "a\nb",
            "example.com:80",
            "127.0.0.0/8",
            "a,b",
            "*.example.com",
            "fe80::1%en0",
            "a;id",
            "a|b",
            "$(id)",
            "javascript://x",
            "file:///tmp/a",
            "https://u:p@a",
            "https://a?x=1",
            "https://a#x",
            "localhost",
            "127.1",
            "2130706433",
            "metadata.google.internal",
            "example.test.",
        ] {
            assert!(normalize(req(input)).is_err(), "accepted {input}");
        }
        assert!(
            serde_json::from_str::<Request>(
                r#"{"request_id":"x","target":"a","tool":"ping","args":[]}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Request>(r#"{"request_id":"x","target":"a","tool":"unknown"}"#)
                .is_err()
        );
    }
    fn tool_req(tool: Tool, target: &str, port: Option<u16>) -> Request {
        Request {
            tool,
            port,
            ..req(target)
        }
    }
    #[test]
    fn six_tool_contract_ports_and_http_identity() {
        for tool in ["ping", "traceroute", "dns", "tcp", "tls", "http"] {
            let request: Request = serde_json::from_value(
                serde_json::json!({"request_id":"old", "target":"example.test", "tool":tool}),
            )
            .unwrap();
            assert_eq!(request.port, None);
            assert!(serde_json::to_value(request).unwrap().get("port").is_none());
        }
        for tool in [Tool::Ping, Tool::Traceroute, Tool::Dns] {
            assert_eq!(
                normalize(tool_req(tool, "https://Example.test:8443/Case/%2f", None))
                    .unwrap()
                    .target,
                "example.test"
            );
            assert!(normalize(tool_req(tool, "example.test", Some(80))).is_err());
        }
        assert!(normalize(tool_req(Tool::Tcp, "example.test", None)).is_err());
        assert!(normalize(tool_req(Tool::Tcp, "https://example.test/path", None)).is_err());
        assert_eq!(
            normalize(tool_req(Tool::Tcp, "https://example.test:443/path", None))
                .unwrap()
                .port,
            Some(443)
        );
        assert_eq!(
            normalize(tool_req(Tool::Tcp, "example.test", Some(80)))
                .unwrap()
                .port,
            Some(80)
        );
        assert_eq!(
            normalize(tool_req(Tool::Tls, "example.test", None))
                .unwrap()
                .port,
            Some(443)
        );
        for tool in [Tool::Tcp, Tool::Tls] {
            assert!(normalize(tool_req(tool, "https://example.test:443/", Some(8443))).is_err());
            assert!(normalize(tool_req(tool, "http://example.test:80/", Some(443))).is_err());
            assert_eq!(
                normalize(tool_req(tool, "https://example.test:443/", Some(443)))
                    .unwrap()
                    .port,
                Some(443)
            );
            assert_eq!(
                normalize(tool_req(tool, "http://[fd12::1]:8443/", None))
                    .unwrap()
                    .port,
                Some(8443)
            );
        }
        let http = normalize(tool_req(
            Tool::Http,
            "https://Example.TEST/Case/%2fKeep",
            None,
        ))
        .unwrap();
        assert_eq!(http.target, "https://example.test/Case/%2fKeep");
        assert_eq!(http.host, "example.test");
        assert_eq!(http.port, Some(443));
        assert_eq!(
            normalize(tool_req(Tool::Http, "http://[fd12::1]:8080/Case", None))
                .unwrap()
                .target,
            "http://[fd12::1]:8080/Case"
        );
        for input in [
            "example.test",
            "https://example.test?q=1",
            "https://example.test?",
            "https://example.test#",
            "https://u:p@example.test/",
            "https://@example.test/",
            "ftp://example.test/",
            "https://example.test:0/",
            "https:///example.test/path",
        ] {
            assert!(
                normalize(tool_req(Tool::Http, input, None)).is_err(),
                "{input}"
            );
        }
        assert!(normalize(tool_req(Tool::Http, "https://example.test/", Some(443))).is_err());
        for tool in [Tool::Tcp, Tool::Tls, Tool::Http] {
            let target = if tool == Tool::Http {
                "https://127.0.0.1/"
            } else {
                "127.0.0.1"
            };
            assert!(normalize(tool_req(tool, target, (tool == Tool::Tcp).then_some(80))).is_err());
        }
        let control = normalize(tool_req(Tool::Tls, "10.0.0.1", Some(8199))).unwrap();
        assert!(validate_control_port(&control, Some(8199)).is_err());
        assert!(validate_control_port(&control, Some(8200)).is_ok());
        assert!(validate_control_port(&control, None).is_ok());
    }
    #[tokio::test]
    async fn actual_control_port_is_blocked_before_any_resolution() {
        let mut manager =
            NetworkToolManager::new(CancellationToken::new()).with_control_port(Some(8199));
        manager.resolver = Arc::new(|_| panic!("blocked control port must not resolve"));
        for tool in [Tool::Tcp, Tool::Tls, Tool::Http] {
            let target = if tool == Tool::Http {
                "http://example.test:8199/path"
            } else {
                "example.test"
            };
            let request = tool_req(tool, target, (tool != Tool::Http).then_some(8199));
            assert!(
                manager
                    .start(request.clone())
                    .await
                    .unwrap_err()
                    .contains("控制服务端口")
            );
            assert!(
                manager
                    .existing_request(&request)
                    .await
                    .unwrap_err()
                    .contains("控制服务端口")
            );
        }
        assert!(manager.snapshot().await.is_none());
    }
    #[tokio::test]
    async fn dns_only_allows_special_answers_and_literals_without_resolver_or_connect() {
        let mut manager = NetworkToolManager::new(CancellationToken::new());
        manager.resolver = Arc::new(|host| {
            assert_eq!(host, "metadata.google.internal");
            Ok(vec![
                "127.0.0.1".parse().unwrap(),
                "169.254.169.254".parse().unwrap(),
                "127.0.0.1".parse().unwrap(),
            ])
        });
        manager
            .start(tool_req(Tool::Dns, "metadata.google.internal", None))
            .await
            .unwrap();
        let done = wait_terminal(&manager).await;
        assert_eq!(done.status, "completed");
        assert!(done.summary.contains("2 个去重地址"));
        assert!(done.output.contains("169.254.169.254"));
        assert!(done.resolved_ip.is_none());
        manager.shutdown_and_wait().await;
        let mut manager = NetworkToolManager::new(CancellationToken::new());
        manager.resolver = Arc::new(|_| panic!("literal must not resolve"));
        manager
            .start(tool_req(Tool::Dns, "127.0.0.1", None))
            .await
            .unwrap();
        let done = wait_terminal(&manager).await;
        assert_eq!(done.status, "completed");
        assert!(done.output.contains("无 DNS RTT"));
        manager.shutdown_and_wait().await;
    }
    #[tokio::test]
    async fn connecting_tools_reject_every_mixed_dns_answer_before_connecting() {
        for tool in [Tool::Tcp, Tool::Tls, Tool::Http] {
            let mut manager = NetworkToolManager::new(CancellationToken::new());
            manager.resolver = Arc::new(|_| {
                Ok(vec![
                    "10.0.0.1".parse().unwrap(),
                    "127.0.0.1".parse().unwrap(),
                ])
            });
            let target = if tool == Tool::Http {
                "http://example.test/path"
            } else {
                "example.test"
            };
            manager
                .start(tool_req(tool, target, (tool == Tool::Tcp).then_some(80)))
                .await
                .unwrap();
            let done = wait_terminal(&manager).await;
            assert_eq!(done.status, "error");
            assert!(done.summary.contains("127.0.0.1"));
            assert!(done.output.is_empty());
            assert!(done.resolved_ip.is_none());
            manager.shutdown_and_wait().await;
        }
    }
    #[tokio::test]
    async fn duplicate_identity_includes_port_and_http_case_sensitive_path() {
        for tool in [Tool::Tcp, Tool::Tls, Tool::Http] {
            let mut manager = NetworkToolManager::new(CancellationToken::new());
            manager.resolver = Arc::new(|_| Err("fake DNS".into()));
            let target = if tool == Tool::Http {
                "https://example.test/Case/%2f"
            } else {
                "example.test"
            };
            let first = tool_req(tool, target, (tool != Tool::Http).then_some(443));
            manager.start(first.clone()).await.unwrap();
            assert!(manager.existing_request(&first).await.unwrap().is_some());
            let mut changed = first;
            if tool == Tool::Http {
                changed.target = "https://example.test/case/%2f".into();
            } else {
                changed.port = Some(8443);
            }
            assert!(manager.existing_request(&changed).await.is_err());
            manager.shutdown_and_wait().await;
        }
    }
    #[test]
    fn dns_only_deduplicates_and_bounds_without_connection_policy() {
        let request = normalize(tool_req(Tool::Dns, "127.0.0.1", None)).unwrap();
        assert_eq!(
            checked_answers(&request, vec!["127.0.0.1".parse().unwrap(); 100], false)
                .unwrap()
                .len(),
            1
        );
        let addresses = (1..=65)
            .map(|i| format!("10.0.0.{i}").parse().unwrap())
            .collect();
        assert!(checked_answers(&request, addresses, false).is_err());
    }
    #[test]
    fn all_dns_answers_checked_and_office_ranges_allowed() {
        for address in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "fd12::1",
            "2606:4700:4700::1111",
        ] {
            assert!(validate_addresses(vec![address.parse().unwrap()], false).is_ok());
        }
        for address in [
            "0.0.0.0",
            "0.2.3.4",
            "169.254.169.254",
            "100.100.100.200",
            "168.63.129.16",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
            "::",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "fd00:ec2::254",
            "fd20:ce::254",
            "2001:db8::1",
        ] {
            assert!(
                validate_addresses(
                    vec!["10.0.0.1".parse().unwrap(), address.parse().unwrap()],
                    false
                )
                .is_err(),
                "accepted {address}"
            );
        }
        for address in ["127.0.0.1", "::1"] {
            assert!(validate_addresses(vec![address.parse().unwrap()], true).is_ok());
            assert!(validate_addresses(vec![address.parse().unwrap()], false).is_err());
        }
        assert!(validate_addresses(vec!["10.0.0.1".parse().unwrap(); 65], false).is_err());
        assert!(validate_addresses(vec![], false).is_err());
    }
    #[test]
    fn actual_ping_statistics_and_unknown_formats() {
        let stats = ping_stats("PING x\n4 packets transmitted, 3 packets received, 25.0% packet loss\nround-trip min/avg/max/stddev = 0.1/0.2/0.3/0.04 ms\n").unwrap();
        assert_eq!((stats.sent, stats.received, stats.loss), (4, 3, 25.0));
        assert_eq!(stats.rtt, Some([0.1, 0.2, 0.3, 0.04]));
        assert_eq!(
            ping_stats("4 packets transmitted, 0 packets received, 100.0% packet loss")
                .unwrap()
                .received,
            0
        );
        assert!(ping_stats("Packets: Sent = 4, Received = 3, Lost = 1 (25% loss)").is_none());
        assert!(ping_stats("未知语言").is_none());
        assert!(
            ping_stats("4 packets transmitted, 5 packets received, 0.0% packet loss").is_none()
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn fixed_mac_commands() {
        for address in ["127.0.0.1", "::1"] {
            let ip: IpAddr = address.parse().unwrap();
            let command = ping_command(ip).unwrap();
            let std = command.as_std();
            assert_eq!(
                std.get_program(),
                if ip.is_ipv4() {
                    "/sbin/ping"
                } else {
                    "/sbin/ping6"
                }
            );
            assert_eq!(
                std.get_args().collect::<Vec<_>>(),
                ["-n", "-c", "4", "-i", "1"]
            );
        }
    }
    async fn wait_terminal(manager: &NetworkToolManager) -> Snapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = manager.snapshot().await.unwrap();
                if !snapshot.active() {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn manager_idempotency_and_global_cooldown_without_probes() {
        let mut manager = NetworkToolManager::new(CancellationToken::new());
        manager.resolver = Arc::new(|_| Err("fixture DNS error".into()));
        let first = manager.start(req("example.test")).await.unwrap();
        let duplicate = manager
            .start(req("https://EXAMPLE.test/path"))
            .await
            .unwrap();
        assert_eq!(first.id, duplicate.id);
        assert!(manager.start(req("other.test")).await.is_err());
        let done = wait_terminal(&manager).await;
        assert_eq!(done.status, "error");
        let mut next = req("other.test");
        next.request_id = "fixture-2".into();
        assert!(manager.start(next).await.unwrap_err().contains("冷却时间"));
        assert_eq!(
            manager.start(req("example.test")).await.unwrap().id,
            first.id
        );
        assert!(manager.stop("stale-id").await.is_err());
        manager.shutdown_and_wait().await;
    }
    #[tokio::test]
    async fn dns_cancel_drains_before_terminal_or_new_admission() {
        let mut manager = NetworkToolManager::new(CancellationToken::new());
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = release.clone();
        let e = entered.clone();
        manager.resolver = Arc::new(move |_| {
            e.store(true, std::sync::atomic::Ordering::SeqCst);
            let (lock, cv) = &*r;
            let _guard = cv
                .wait_while(lock.lock().unwrap(), |ready| !*ready)
                .unwrap();
            Ok(vec!["10.0.0.1".parse().unwrap()])
        });
        let first = manager.start(req("example.test")).await.unwrap();
        while !entered.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(manager.stop(&first.id).await.unwrap().status, "stopping");
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(manager.snapshot().await.unwrap().status, "draining");
        assert!(manager.settle().await.is_err());
        let mut next = req("other.test");
        next.request_id = "fixture-2".into();
        assert!(manager.start(next).await.is_err());
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        assert_eq!(wait_terminal(&manager).await.status, "cancelled");
        manager.shutdown_and_wait().await;
    }
    #[tokio::test]
    async fn dns_timeout_discards_late_answers() {
        let mut manager = NetworkToolManager::new(CancellationToken::new());
        manager.dns_budget = Duration::from_millis(5);
        manager.resolver = Arc::new(|_| {
            std::thread::sleep(Duration::from_millis(30));
            Ok(vec!["10.0.0.1".parse().unwrap()])
        });
        manager.start(req("example.test")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(manager.snapshot().await.unwrap().status, "draining");
        let done = wait_terminal(&manager).await;
        assert_eq!(done.status, "timeout");
        assert!(done.resolved_ip.is_none());
        assert!(done.output.is_empty());
        manager.shutdown_and_wait().await;
    }
}
