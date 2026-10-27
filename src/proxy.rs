//! Explicit, unauthenticated HTTP CONNECT transport. Target authorization and
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
use url::Url;

const MAX_HEADER: usize = 16 * 1024;
const MAX_ADDRESSES: usize = 64;
const MAX_PENDING_PROXY_DNS: usize = 2;
static PROXY_DNS_PENDING: AtomicUsize = AtomicUsize::new(0);

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
    Cancelled,
    Timeout,
}
impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => write!(f, "invalid explicit HTTP proxy endpoint"),
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
    addresses: Vec<SocketAddr>,
}
impl ResolvedProxy {
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

fn endpoint(config: &ProxyConfig, control_port: Option<u16>) -> Result<(String, u16), ProxyError> {
    let raw = &config.url;
    if raw.len() > 2048
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.contains('\\')
    {
        return Err(ProxyError::InvalidConfig);
    }
    let rest = raw
        .strip_prefix("http://")
        .ok_or(ProxyError::InvalidConfig)?;
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
    let parsed = Url::parse(raw).map_err(|_| ProxyError::InvalidConfig)?;
    let host = parsed
        .host_str()
        .ok_or(ProxyError::InvalidConfig)?
        .trim_matches(['[', ']']);
    if host.is_empty() || host.ends_with('.') {
        return Err(ProxyError::InvalidConfig);
    }
    Ok((host.to_owned(), port))
}

/// Validate and normalize configuration without DNS or network traffic.
pub fn normalize_proxy_config(
    config: &ProxyConfig,
    control_port: Option<u16>,
) -> Result<ProxyConfig, ProxyError> {
    let (host, port) = endpoint(config, control_port)?;
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host
    };
    Ok(ProxyConfig {
        url: format!("http://{host}:{port}"),
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
    Ok(ResolvedProxy { addresses: result })
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
    let (host, port) = endpoint(config, control_port)?;
    bounded(cancel, timeout, async move {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return validate_answers([SocketAddr::new(ip, port)], port);
        }
        // Hold the admission slot inside the blocking job, even if the caller
        // times out or drops its future. OS DNS cannot be cancelled reliably.
        let guard = ProxyDnsPendingGuard::acquire()?;
        let (send, receive) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("aonet-proxy-dns".into())
            .spawn(move || {
                let _guard = guard;
                let result = (host.as_str(), port)
                    .to_socket_addrs()
                    .map_err(|_| ProxyError::Resolution)
                    .and_then(|answers| validate_answers(answers, port));
                let _ = send.send(result);
            })
            .map_err(|_| ProxyError::Resolution)?;
        receive.await.map_err(|_| ProxyError::Resolution)?
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
        Ok(ProxyTunnel {
            stream,
            proxy_addr,
            proxy_tcp_duration,
            tunnel_duration: tunnel_started.elapsed(),
        })
    })
    .await
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
