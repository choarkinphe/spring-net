use crate::{
    model::{Permit, ProxyPermit},
    storage::{atomic_json, read_bounded},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

const MAX_PENDING_TARGET_DNS: usize = 4;
static TARGET_DNS_PENDING: AtomicUsize = AtomicUsize::new(0);

pub fn target_dns_pending() -> usize {
    TARGET_DNS_PENDING.load(Ordering::Acquire)
}

struct TargetDnsPendingGuard(&'static AtomicUsize);
impl TargetDnsPendingGuard {
    fn acquire(pending_count: &'static AtomicUsize) -> Result<Self, String> {
        pending_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                (pending < MAX_PENDING_TARGET_DNS).then_some(pending + 1)
            })
            .map(|_| Self(pending_count))
            .map_err(|_| "目标 DNS 查询额度已满".to_owned())
    }
}
impl Drop for TargetDnsPendingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn parse_url(raw: &str) -> Result<Url, String> {
    if raw.len() > 2048
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
        || raw.contains('\\')
    {
        return Err("URL 含空白、控制字符或长度超限".into());
    }
    let url = Url::parse(raw).map_err(|_| "请输入完整 HTTP/HTTPS URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err("只支持无凭据、无查询参数、无 fragment 的 HTTP/HTTPS 地址".into());
    }
    if url.host_str().unwrap().ends_with('.') {
        return Err("不支持带尾随点的域名".into());
    }
    Ok(url)
}
pub fn host(url: &Url) -> String {
    url.host_str()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_owned()
}
pub fn domain_key(url: &Url) -> String {
    let h = host(url);
    if h.parse::<IpAddr>().is_ok() {
        return h;
    }
    // Conservative quota bucket: co.uk and similar suffixes are over-grouped,
    // never split into looser per-subdomain quotas. This is not a PSL classifier.
    let parts: Vec<_> = h.rsplit('.').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join(".")
}
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, c, _] = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_multicast()
                || v.is_unspecified()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && (c == 0 || c == 2))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            // Conservatively allow only global unicast, excluding special-purpose 2001 ranges.
            s[0] & 0xe000 == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
pub fn check_public_url(url: &Url) -> Result<(), String> {
    if ![80, 443].contains(&url.port_or_known_default().unwrap_or(0)) {
        return Err("公共站点只允许 80/443 端口".into());
    }
    let h = host(url);
    if h == "localhost"
        || h.ends_with(".localhost")
        || !h.contains('.') && h.parse::<IpAddr>().is_err()
    {
        return Err("公共模式不支持本机或单标签主机".into());
    }
    if let Ok(ip) = h.parse::<IpAddr>()
        && !public_ip(ip)
    {
        return Err("公共模式不允许私网或特殊用途地址".into());
    }
    Ok(())
}
async fn bounded_target_lookup(
    lookup: impl FnOnce() -> Result<Vec<SocketAddr>, String> + Send + 'static,
) -> Result<Vec<SocketAddr>, String> {
    target_lookup_with_counter(&TARGET_DNS_PENDING, lookup).await
}

async fn target_lookup_with_counter(
    pending: &'static AtomicUsize,
    lookup: impl FnOnce() -> Result<Vec<SocketAddr>, String> + Send + 'static,
) -> Result<Vec<SocketAddr>, String> {
    let guard = TargetDnsPendingGuard::acquire(pending)?;
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("aonet-target-dns".into())
        .spawn(move || {
            let _guard = guard;
            let result = lookup();
            let _ = send.send(result);
        })
        .map_err(|e| format!("无法启动目标 DNS 查询：{e}"))?;
    tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .map_err(|_| "DNS 超时".to_owned())?
        .map_err(|_| "目标 DNS 查询意外结束".to_owned())?
}

pub async fn resolve(
    url: &Url,
    permit: Option<&Permit>,
    control_port: Option<u16>,
) -> Result<Vec<SocketAddr>, String> {
    resolve_inner(url, permit, control_port, false).await
}

pub async fn resolve_proxy_target(
    url: &Url,
    permit: Option<&Permit>,
    control_port: Option<u16>,
) -> Result<Vec<SocketAddr>, String> {
    resolve_inner(url, permit, control_port, true).await
}

async fn resolve_inner(
    url: &Url,
    permit: Option<&Permit>,
    control_port: Option<u16>,
    via_proxy: bool,
) -> Result<Vec<SocketAddr>, String> {
    if permit.is_none() {
        check_public_url(url)?;
    }
    let h = host(url);
    let port = url.port_or_known_default().ok_or("缺少端口")?;
    // OS DNS is not cancellable. Keep its admission slot until the real resolver
    // returns, even when this request or its entire run has been dropped.
    let addrs = if let Ok(ip) = h.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        bounded_target_lookup(move || {
            let addresses: Vec<_> = (h.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| format!("DNS 解析失败：{e}"))?
                .take(65)
                .collect();
            Ok(addresses)
        })
        .await?
    };
    if addrs.is_empty() || addrs.len() > 64 {
        return Err("DNS 返回地址为空或过多".into());
    }
    for a in &addrs {
        let ip = a.ip();
        if via_proxy {
            let canonical = match ip {
                IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
                _ => ip,
            };
            let private = match canonical {
                IpAddr::V4(v) => v.is_private(),
                IpAddr::V6(v) => v.segments()[0] & 0xfe00 == 0xfc00,
            };
            if canonical.is_loopback()
                || !(private || public_ip(canonical))
                || canonical == "168.63.129.16".parse::<IpAddr>().unwrap()
                || canonical == "fd00:ec2::254".parse::<IpAddr>().unwrap()
            {
                return Err("代理目标不能是本机 demo、loopback、元数据或特殊用途地址".into());
            }
        }
        if Some(port) == control_port
            && (ip.is_loopback() || ip == IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        {
            return Err("不能将 AoNet 控制服务当测试目标".into());
        }
        match permit {
            None if !public_ip(ip) => return Err("DNS 返回内网/特殊地址，已阻止".into()),
            Some(p) if !p.allowed_ips.iter().any(|n| n.contains(&ip)) => {
                return Err("解析地址不在授权 IP/CIDR 中".into());
            }
            _ => {}
        }
    }
    // Pin one validated address. No hidden multi-address retries.
    Ok(vec![addrs[0]])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermitFile {
    version: u32,
    targets: Vec<Permit>,
}
pub fn validate_permits(
    permits: &[Permit],
    public_domains: &HashSet<String>,
    control_port: Option<u16>,
) -> Result<(), String> {
    if permits.len() > 100 {
        return Err("授权版本或条目数量无效".into());
    }
    let mut ids = HashSet::new();
    for p in permits {
        let u = parse_url(&p.url)?;
        if p.id.starts_with("demo-")
            || p.id.is_empty()
            || p.id.len() > 100
            || !p
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || p.name.trim().is_empty()
            || p.name.len() > 200
            || p.authorization.trim().is_empty()
            || p.authorization.len() > 2000
            || !ids.insert(&p.id)
            || p.allowed_ips.is_empty()
            || p.allowed_ips.len() > 64
            || p.allowed_ips.iter().any(|n| n.prefix_len() == 0)
            || p.modes.is_empty()
            || p.modes.len() > 2
            || p.modes.iter().any(|m| m != "http" && m != "tcp")
            || !(1..=1000).contains(&p.max_concurrency)
            || !(1..=1000).contains(&p.max_rps)
            || !(1..=300).contains(&p.max_duration)
            || control_port.is_some_and(|port| u.port_or_known_default() == Some(port))
        {
            return Err(format!("无效授权条目：{}", p.id));
        }
        chrono::DateTime::parse_from_rfc3339(&p.expires_at)
            .map_err(|_| "授权有效期应为 RFC3339")?;
        if public_domains.contains(&domain_key(&u)) {
            return Err(format!("公共预设站点不能登记为承载目标：{}", p.name));
        }
        if p.proxy_routes.len() > 64 {
            return Err(format!("代理授权路径过多：{}", p.id));
        }
        let mut routes = HashSet::new();
        for route in &p.proxy_routes {
            let normalized = crate::proxy::normalize_proxy_config(
                &crate::proxy::ProxyConfig {
                    url: route.url.clone(),
                },
                control_port,
            )
            .map_err(|e| format!("代理授权路径无效：{e}"))?;
            let proxy_url = parse_url(&normalized.url)?;
            if !routes.insert(normalized.url)
                || route.allowed_ips.is_empty()
                || route.allowed_ips.len() > 64
                || route.allowed_ips.iter().any(|net| net.prefix_len() == 0)
                || route.authorization.trim().is_empty()
                || route.authorization.len() > 2000
                || public_domains.contains(&domain_key(&proxy_url))
            {
                return Err(format!(
                    "代理授权路径无效、重复或属于受保护公共站点：{}",
                    p.id
                ));
            }
        }
    }
    Ok(())
}

/// A direct-only permit never implicitly authorizes a proxy. Limits and expiry
/// remain those of the enclosing target permit; this grants only a transport path.
pub fn proxy_route<'a>(
    permit: &'a Permit,
    proxy: &crate::proxy::ProxyConfig,
    control_port: Option<u16>,
) -> Result<&'a ProxyPermit, String> {
    let wanted =
        crate::proxy::normalize_proxy_config(proxy, control_port).map_err(|e| e.to_string())?;
    let mut matched = None;
    for route in &permit.proxy_routes {
        let normalized = crate::proxy::normalize_proxy_config(
            &crate::proxy::ProxyConfig {
                url: route.url.clone(),
            },
            control_port,
        )
        .map_err(|e| e.to_string())?;
        if normalized == wanted {
            if matched.is_some() {
                return Err("代理授权路径重复".into());
            }
            matched = Some(route);
        }
    }
    let route = matched.ok_or("目标许可未授权此代理路径（缺省仅允许直连）")?;
    if route.allowed_ips.is_empty()
        || route.allowed_ips.len() > 64
        || route.allowed_ips.iter().any(|net| net.prefix_len() == 0)
        || route.authorization.trim().is_empty()
        || route.authorization.len() > 2000
    {
        return Err("代理路径缺少有效 IP/CIDR 或授权说明".into());
    }
    Ok(route)
}

pub fn validate_proxy_route_addresses(
    route: &ProxyPermit,
    proxy: &crate::proxy::ResolvedProxy,
) -> Result<(), String> {
    for addr in proxy.addresses() {
        if !route.allowed_ips.iter().any(|net| net.contains(&addr.ip())) {
            return Err("代理解析地址不在授权 IP/CIDR 中".into());
        }
    }
    Ok(())
}

pub fn load_permits(
    path: &Path,
    public_domains: &HashSet<String>,
    control_port: Option<u16>,
) -> Result<Vec<Permit>, String> {
    let f: PermitFile = serde_json::from_slice(&read_bounded(path, 1_048_576)?)
        .map_err(|e| format!("授权文件：{e}"))?;
    if f.version != 1 {
        return Err("授权版本或条目数量无效".into());
    }
    validate_permits(&f.targets, public_domains, control_port)?;
    Ok(f.targets)
}

#[derive(Default, Serialize, Deserialize)]
pub struct Cooldowns {
    domains: HashMap<String, u64>,
    #[serde(default)]
    global_until: u64,
}
impl Cooldowns {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        serde_json::from_slice(&read_bounded(path, 1_048_576)?)
            .map_err(|e| format!("冷却文件损坏：{e}"))
    }
    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
    pub fn ready(&self, key: &str) -> bool {
        let now = Self::now();
        self.global_until <= now && self.domains.get(key).is_none_or(|v| *v <= now)
    }
    pub fn reserve_dispatch(&mut self, key: &str, path: &Path) -> Result<(), String> {
        let now = Self::now();
        self.global_until = now + 500;
        self.reserve(key, now + 30_000, path)
    }
    pub fn reserve(&mut self, key: &str, until: u64, path: &Path) -> Result<(), String> {
        let now = Self::now();
        self.domains.retain(|_, v| *v > now);
        if self.domains.len() >= 10000 {
            return Err("冷却记录已满，请稍后重试".into());
        }
        self.domains
            .entry(key.into())
            .and_modify(|v| *v = (*v).max(until))
            .or_insert(until);
        atomic_json(path, self)
    }
}
pub fn retry_after(raw: Option<&str>) -> u64 {
    let seconds = raw
        .and_then(|v| {
            v.parse::<u64>().ok().or_else(|| {
                httpdate::parse_http_date(v).ok().map(|d| {
                    d.duration_since(SystemTime::now())
                        .unwrap_or_default()
                        .as_secs()
                })
            })
        })
        .unwrap_or(30);
    seconds.clamp(30, 86400)
}

pub fn check_fd_budget(concurrency: usize, demo: bool) -> Result<(), String> {
    #[cfg(not(unix))]
    let _ = (concurrency, demo); // Windows socket limits are not Unix descriptor limits.
    #[cfg(unix)]
    {
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes into a valid stack-allocated rlimit.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } != 0 {
            return Err("无法读取文件描述符限制".into());
        }
        let needed = concurrency * if demo { 2 } else { 1 } + 256;
        if limits.rlim_cur < needed as libc::rlim_t {
            return Err(format!(
                "文件描述符软限制为 {}，本配置至少需要 {needed}；请减少并发或由你自行调整系统限制",
                limits.rlim_cur
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_dns_retains_global_slot_until_os_return() {
        static PENDING: AtomicUsize = AtomicUsize::new(0);
        let (release, wait) = std::sync::mpsc::channel();
        let (started, running) = tokio::sync::oneshot::channel();
        let lookup = tokio::spawn(target_lookup_with_counter(&PENDING, move || {
            let _ = started.send(());
            wait.recv().unwrap();
            Ok(vec!["10.0.0.1:80".parse().unwrap()])
        }));
        running.await.unwrap();
        lookup.abort();
        let _ = lookup.await;
        assert_eq!(PENDING.load(Ordering::Acquire), 1);
        let mut guards = Vec::new();
        while let Ok(guard) = TargetDnsPendingGuard::acquire(&PENDING) {
            guards.push(guard);
        }
        assert_eq!(PENDING.load(Ordering::Acquire), MAX_PENDING_TARGET_DNS);
        assert!(
            target_lookup_with_counter(&PENDING, || panic!("saturated lookup must not execute"))
                .await
                .is_err()
        );
        drop(guards);
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while PENDING.load(Ordering::Acquire) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn rejects_unsafe_urls() {
        for s in [
            "file:///etc/passwd",
            "http://u:p@example.com",
            "https://example.com/?x=1",
            "https://example.com/#x",
            "http://x\\@example.com",
            "https://example.com.\n",
        ] {
            assert!(parse_url(s).is_err(), "{s}");
        }
        assert!(parse_url("https://example.com/").is_ok());
    }
    #[test]
    fn special_ips_blocked() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "192.0.2.1",
            "::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
            "2002:7f00:1::",
            "64:ff9b::7f00:1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
    #[test]
    fn registered_domains_share_key() {
        assert_eq!(
            domain_key(&parse_url("https://www.ctrip.com").unwrap()),
            domain_key(&parse_url("https://m.ctrip.com").unwrap())
        );
        assert_eq!(
            domain_key(&parse_url("https://www.example.co.uk").unwrap()),
            "co.uk"
        );
    }
    #[test]
    fn persistent_cooldown() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("cooldowns.json");
        let mut c = Cooldowns::default();
        c.reserve("example.com", Cooldowns::now() + 30000, &path)
            .unwrap();
        assert!(!Cooldowns::load(&path).unwrap().ready("example.com"));
    }
    #[test]
    fn retry_after_is_bounded() {
        assert_eq!(retry_after(Some("0")), 30);
        assert_eq!(retry_after(Some("999999999")), 86400);
    }
}
