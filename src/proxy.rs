//! Explicit, unauthenticated HTTP CONNECT / SOCKS5 transport. Target authorization and
//! TLS hostname verification remain the caller's responsibility.
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::sync::CancellationToken;
use url::Host;

const MAX_HEADER: usize = 16 * 1024;
const MAX_ADDRESSES: usize = 64;
const MAX_PENDING_PROXY_DNS: usize = 2;
static PROXY_DNS_PENDING: AtomicUsize = AtomicUsize::new(0);
// Background health DNS must never consume foreground admission slots.
static HEALTH_DNS_PENDING: AtomicUsize = AtomicUsize::new(0);

pub fn health_dns_pending() -> usize {
    HEALTH_DNS_PENDING.load(Ordering::Acquire)
}

struct HealthDnsPendingGuard;
impl HealthDnsPendingGuard {
    fn acquire() -> Result<Self, ProxyError> {
        HEALTH_DNS_PENDING
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| ProxyError::Resolution)
    }
}
impl Drop for HealthDnsPendingGuard {
    fn drop(&mut self) {
        HEALTH_DNS_PENDING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Number of proxy DNS lookups submitted to the OS resolver and not yet returned.
pub fn proxy_dns_pending() -> usize {
    PROXY_DNS_PENDING.load(Ordering::Acquire)
}

struct ProxyDnsPendingGuard;
impl ProxyDnsPendingGuard {
    fn acquire() -> Result<Self, ProxyError> {
        PROXY_DNS_PENDING
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                (pending < MAX_PENDING_PROXY_DNS).then_some(pending + 1)
            })
            .map(|_| Self)
            .map_err(|_| ProxyError::Resolution)
    }
}
impl Drop for ProxyDnsPendingGuard {
    fn drop(&mut self) {
        PROXY_DNS_PENDING.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyProtocol {
    Http,
    Socks5,
}
impl ProxyProtocol {
    fn scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Socks5 => "socks5",
        }
    }
}

#[derive(Debug)]
pub enum ProxyError {
    InvalidConfig,
    ForbiddenAddress,
    Resolution,
    Connect,
    Protocol,
    HeaderTooLarge,
    AuthenticationRequired,
    Rejected(u16),
    Socks5Protocol,
    Socks5AuthenticationRequired,
    Socks5Rejected(u8),
    Cancelled,
    Timeout,
}
impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => write!(f, "invalid explicit HTTP/SOCKS5 proxy endpoint"),
            Self::ForbiddenAddress => write!(f, "proxy address or port is forbidden"),
            Self::Resolution => write!(
                f,
                "proxy DNS resolution failed or returned too many addresses"
            ),
            Self::Connect => write!(f, "proxy TCP connection failed"),
            Self::Protocol => write!(f, "invalid or incomplete proxy CONNECT response"),
            Self::HeaderTooLarge => write!(f, "proxy CONNECT response headers exceed 16 KiB"),
            Self::AuthenticationRequired => write!(
                f,
                "proxy authentication required (407); authentication is not supported"
            ),
            Self::Rejected(code) => write!(f, "proxy rejected CONNECT with HTTP {code}"),
            Self::Socks5Protocol => write!(f, "invalid or incomplete SOCKS5 proxy response"),
            Self::Socks5AuthenticationRequired => write!(
                f,
                "SOCKS5 proxy refused NOAUTH; authentication is not supported"
            ),
            Self::Socks5Rejected(code) => {
                write!(f, "SOCKS5 proxy rejected CONNECT with REP {code}")
            }
            Self::Cancelled => write!(f, "proxy operation cancelled"),
            Self::Timeout => write!(f, "proxy operation timed out"),
        }
    }
}
impl std::error::Error for ProxyError {}

/// A DNS snapshot. All answers were checked before this value was produced.
/// Callers enforcing a proxy permit must validate *every* address against it.
#[derive(Clone, Debug)]
pub struct ResolvedProxy {
    protocol: ProxyProtocol,
    addresses: Vec<SocketAddr>,
}
impl ResolvedProxy {
    pub fn protocol(&self) -> ProxyProtocol {
        self.protocol
    }

    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }
}

/// TCP latency is distinct from the CONNECT exchange and is not target TCP latency.
#[derive(Debug)]
pub struct ProxyTunnel {
    pub stream: TcpStream,
    pub proxy_addr: SocketAddr,
    pub proxy_tcp_duration: Duration,
    pub tunnel_duration: Duration,
}

fn endpoint(
    config: &ProxyConfig,
    control_port: Option<u16>,
) -> Result<(ProxyProtocol, String, u16), ProxyError> {
    let raw = &config.url;
    if raw.len() > 2048
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.contains('\\')
    {
        return Err(ProxyError::InvalidConfig);
    }
    let (protocol, rest) = if let Some(rest) = raw.strip_prefix("http://") {
        (ProxyProtocol::Http, rest)
    } else if let Some(rest) = raw.strip_prefix("socks5://") {
        (ProxyProtocol::Socks5, rest)
    } else {
        return Err(ProxyError::InvalidConfig);
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return Err(ProxyError::InvalidConfig);
    }
    let (host, port_text) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or(ProxyError::InvalidConfig)?;
        (
            &authority[..=end],
            authority[end + 1..]
                .strip_prefix(':')
                .ok_or(ProxyError::InvalidConfig)?,
        )
    } else {
        authority
            .rsplit_once(':')
            .ok_or(ProxyError::InvalidConfig)?
    };
    if host.is_empty() || port_text.is_empty() || !port_text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProxyError::InvalidConfig);
    }
    let port: u16 = port_text.parse().map_err(|_| ProxyError::InvalidConfig)?;
    if port == 0 {
        return Err(ProxyError::InvalidConfig);
    }
    if Some(port) == control_port {
        return Err(ProxyError::ForbiddenAddress);
    }
    // Use the same host parser for both schemes: URL treats SOCKS as a
    // non-special scheme and would otherwise leave case / IP syntax opaque.
    // Never accept percent escapes or browser-style ambiguous IPv4 spellings.
    if host.contains('%') {
        return Err(ProxyError::InvalidConfig);
    }
    let host = match Host::parse(host).map_err(|_| ProxyError::InvalidConfig)? {
        Host::Ipv4(ip) => {
            if host != ip.to_string() {
                return Err(ProxyError::InvalidConfig);
            }
            ip.to_string()
        }
        Host::Ipv6(ip) => ip.to_string(),
        Host::Domain(domain) => {
            if domain.len() > 253
                || domain.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
            {
                return Err(ProxyError::InvalidConfig);
            }
            domain
        }
    };
    Ok((protocol, host, port))
}

/// Validate and normalize configuration without DNS or network traffic.
pub fn normalize_proxy_config(
    config: &ProxyConfig,
    control_port: Option<u16>,
) -> Result<ProxyConfig, ProxyError> {
    let (protocol, host, port) = endpoint(config, control_port)?;
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host
    };
    Ok(ProxyConfig {
        url: format!("{}://{host}:{port}", protocol.scheme()),
    })
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}
fn allowed_proxy_ip(ip: IpAddr) -> bool {
    let ip = canonical(ip);
    let metadata = ip == "168.63.129.16".parse::<IpAddr>().unwrap()
        || ip == "fd00:ec2::254".parse::<IpAddr>().unwrap();
    if metadata {
        return false;
    }
    match ip {
        IpAddr::V4(v) => v.is_loopback() || v.is_private() || crate::policy::public_ip(ip),
        IpAddr::V6(v) => {
            v.is_loopback() || (v.segments()[0] & 0xfe00 == 0xfc00) || crate::policy::public_ip(ip)
        }
    }
}
fn validate_answers(
    addrs: impl IntoIterator<Item = SocketAddr>,
    port: u16,
    protocol: ProxyProtocol,
) -> Result<ResolvedProxy, ProxyError> {
    let mut result = Vec::new();
    let mut count = 0;
    for addr in addrs {
        count += 1;
        if count > MAX_ADDRESSES {
            return Err(ProxyError::Resolution);
        }
        let ip = canonical(addr.ip());
        if !allowed_proxy_ip(ip) {
            return Err(ProxyError::ForbiddenAddress);
        }
        let addr = SocketAddr::new(ip, port);
        if !result.contains(&addr) {
            result.push(addr);
        }
    }
    if result.is_empty() {
        return Err(ProxyError::Resolution);
    }
    Ok(ResolvedProxy {
        protocol,
        addresses: result,
    })
}
async fn bounded<T>(
    cancel: &CancellationToken,
    timeout: Duration,
    future: impl std::future::Future<Output = Result<T, ProxyError>>,
) -> Result<T, ProxyError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(ProxyError::Cancelled),
        result = tokio::time::timeout(timeout, future) => result.map_err(|_| ProxyError::Timeout)?,
    }
}

pub async fn resolve_proxy(
    config: &ProxyConfig,
    control_port: Option<u16>,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<ResolvedProxy, ProxyError> {
    let (protocol, host, port) = endpoint(config, control_port)?;
    resolve_endpoint(protocol, host, port, cancel, timeout, false).await
}

/// Same endpoint and all-answer policy as foreground resolution, with an isolated
/// single OS lookup slot. Cancelling the caller never releases an OS thread's slot.
pub async fn resolve_proxy_health(
    config: &ProxyConfig,
    control_port: Option<u16>,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<ResolvedProxy, ProxyError> {
    let (protocol, host, port) = endpoint(config, control_port)?;
    resolve_endpoint(protocol, host, port, cancel, timeout, true).await
}

async fn resolve_endpoint(
    protocol: ProxyProtocol,
    host: String,
    port: u16,
    cancel: &CancellationToken,
    timeout: Duration,
    health: bool,
) -> Result<ResolvedProxy, ProxyError> {
    bounded(cancel, timeout, async move {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return validate_answers([SocketAddr::new(ip, port)], port, protocol);
        }
        // Both guards are moved into the actual OS job, not the async waiter.
        let business_guard = if health {
            None
        } else {
            Some(ProxyDnsPendingGuard::acquire()?)
        };
        let health_guard = if health {
            Some(HealthDnsPendingGuard::acquire()?)
        } else {
            None
        };
        let (send, receive) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name(
                if health {
                    "aonet-health-dns"
                } else {
                    "aonet-proxy-dns"
                }
                .into(),
            )
            .spawn(move || {
                let _business_guard = business_guard;
                let _health_guard = health_guard;
                let result = (host.as_str(), port)
                    .to_socket_addrs()
                    .map_err(|_| ProxyError::Resolution)
                    .and_then(|answers| validate_answers(answers, port, protocol));
                let _ = send.send(result);
            })
            .map_err(|_| ProxyError::Resolution)?;
        receive.await.map_err(|_| ProxyError::Resolution)?
    })
    .await
}

/// Endpoint-only check: one pinned address, no CONNECT and no target traffic.
/// HTTP writes zero application bytes; SOCKS5 sends only the NOAUTH greeting.
pub async fn check_resolved_proxy_health(
    proxy: &ResolvedProxy,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<(SocketAddr, Duration), ProxyError> {
    bounded(cancel, timeout, async {
        let started = Instant::now();
        let addr = *proxy.addresses.first().ok_or(ProxyError::Resolution)?;
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|_| ProxyError::Connect)?;
        if proxy.protocol == ProxyProtocol::Socks5 {
            stream
                .write_all(&[5, 1, 0])
                .await
                .map_err(|_| ProxyError::Socks5Protocol)?;
            let mut method = [0; 2];
            stream
                .read_exact(&mut method)
                .await
                .map_err(|_| ProxyError::Socks5Protocol)?;
            if method[0] != 5 {
                return Err(ProxyError::Socks5Protocol);
            }
            if method[1] != 0 {
                return Err(ProxyError::Socks5AuthenticationRequired);
            }
        }
        drop(stream);
        Ok((addr, started.elapsed()))
    })
    .await
}

/// The target must already be independently authorized and pinned. No direct fallback.
pub async fn connect_resolved_proxy_tunnel(
    proxy: &ResolvedProxy,
    target: SocketAddr,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Result<ProxyTunnel, ProxyError> {
    bounded(cancel, timeout, async {
        let started = Instant::now();
        let proxy_addr = *proxy.addresses.first().ok_or(ProxyError::Resolution)?;
        let mut stream = TcpStream::connect(proxy_addr)
            .await
            .map_err(|_| ProxyError::Connect)?;
        let proxy_tcp_duration = started.elapsed();
        let tunnel_started = Instant::now();
        match proxy.protocol {
            ProxyProtocol::Http => http_connect(&mut stream, target).await?,
            ProxyProtocol::Socks5 => socks5_connect(&mut stream, target).await?,
        }
        Ok(ProxyTunnel {
            stream,
            proxy_addr,
            proxy_tcp_duration,
            tunnel_duration: tunnel_started.elapsed(),
        })
    })
    .await
}

async fn http_connect(stream: &mut TcpStream, target: SocketAddr) -> Result<(), ProxyError> {
    let request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| ProxyError::Protocol)?;
    // Bytewise reads deliberately leave any already-arrived tunnel bytes in the socket.
    let mut header = Vec::with_capacity(512);
    loop {
        if header.len() == MAX_HEADER {
            return Err(ProxyError::HeaderTooLarge);
        }
        let mut byte = [0u8; 1];
        if stream
            .read(&mut byte)
            .await
            .map_err(|_| ProxyError::Protocol)?
            == 0
        {
            return Err(ProxyError::Protocol);
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let first_line = header
        .split(|b| *b == b'\n')
        .next()
        .ok_or(ProxyError::Protocol)?;
    let first_line = first_line.strip_suffix(b"\r").ok_or(ProxyError::Protocol)?;
    let mut parts = first_line.splitn(3, |b| *b == b' ');
    let version = parts.next().ok_or(ProxyError::Protocol)?;
    let status = parts.next().ok_or(ProxyError::Protocol)?;
    if !matches!(version, b"HTTP/1.0" | b"HTTP/1.1")
        || status.len() != 3
        || !status.iter().all(u8::is_ascii_digit)
    {
        return Err(ProxyError::Protocol);
    }
    let code = ((status[0] - b'0') as u16) * 100
        + ((status[1] - b'0') as u16) * 10
        + (status[2] - b'0') as u16;
    if code == 407 {
        return Err(ProxyError::AuthenticationRequired);
    }
    if !(200..300).contains(&code) {
        return Err(ProxyError::Rejected(code));
    }
    Ok(())
}

async fn socks5_connect(stream: &mut TcpStream, target: SocketAddr) -> Result<(), ProxyError> {
    // Exactly one method: NOAUTH. Never send credentials or negotiate another method.
    stream
        .write_all(&[5, 1, 0])
        .await
        .map_err(|_| ProxyError::Socks5Protocol)?;
    let mut method = [0; 2];
    stream
        .read_exact(&mut method)
        .await
        .map_err(|_| ProxyError::Socks5Protocol)?;
    if method[0] != 5 {
        return Err(ProxyError::Socks5Protocol);
    }
    if method[1] != 0 {
        return Err(ProxyError::Socks5AuthenticationRequired);
    }

    let mut request = Vec::with_capacity(22);
    request.extend_from_slice(&[5, 1, 0]);
    match target.ip() {
        IpAddr::V4(ip) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
    }
    request.extend_from_slice(&target.port().to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|_| ProxyError::Socks5Protocol)?;

    let mut header = [0; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|_| ProxyError::Socks5Protocol)?;
    if header[0] != 5 || header[2] != 0 || header[1] > 8 {
        return Err(ProxyError::Socks5Protocol);
    }
    let address_len = match header[3] {
        1 => 4,
        4 => 16,
        3 => {
            let len = stream
                .read_u8()
                .await
                .map_err(|_| ProxyError::Socks5Protocol)?;
            if len == 0 {
                return Err(ProxyError::Socks5Protocol);
            }
            usize::from(len)
        }
        _ => return Err(ProxyError::Socks5Protocol),
    };
    // BND.ADDR is not a destination. Consume at most 255 bytes plus BND.PORT;
    // a domain here is opaque and must never trigger DNS or another connection.
    // Exact reads leave any already-arrived tunnel bytes untouched.
    let mut bound = [0; 257];
    stream
        .read_exact(&mut bound[..address_len + 2])
        .await
        .map_err(|_| ProxyError::Socks5Protocol)?;
    if header[1] != 0 {
        return Err(ProxyError::Socks5Rejected(header[1]));
    }
    Ok(())
}

/// Convenience for non-permit paths. The timeout covers DNS and CONNECT together.
pub async fn connect_proxy_tunnel(
    config: &ProxyConfig,
    target: SocketAddr,
    cancel: &CancellationToken,
    timeout: Duration,
    control_port: Option<u16>,
) -> Result<ProxyTunnel, ProxyError> {
    bounded(cancel, timeout, async {
        let proxy = resolve_proxy(config, control_port, cancel, timeout).await?;
        connect_resolved_proxy_tunnel(&proxy, target, cancel, timeout).await
    })
    .await
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
