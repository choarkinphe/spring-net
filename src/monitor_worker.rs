//! Network worker for the monitor. A cancelled system resolver cannot be killed;
//! its process-wide permit remains held until the OS call actually returns.
use crate::{
    diagnostics::{DiagnosticConfig, policy},
    monitor::{MonitorOutcome, MonitorTarget, MonitorWorker},
    policy::Cooldowns,
    proxy::{ResolvedProxy, connect_resolved_proxy_tunnel},
};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpStream, sync::oneshot, time::Instant};
use tokio_rustls::{
    TlsConnector,
    rustls::{self, ClientConfig, RootCertStore, pki_types::ServerName},
};
use tokio_util::sync::CancellationToken;

static DNS_PENDING: AtomicBool = AtomicBool::new(false);

/// True even after cancellation while an uninterruptible OS resolver is draining.
pub(crate) fn monitor_dns_pending() -> bool {
    DNS_PENDING.load(Ordering::Acquire)
}

struct DnsPermit;
impl Drop for DnsPermit {
    fn drop(&mut self) {
        DNS_PENDING.store(false, Ordering::Release);
    }
}

async fn resolve(host: String, port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    DNS_PENDING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "previous monitor DNS lookup is still draining".to_owned())?;
    let permit = DnsPermit;
    let (tx, rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("monitor-dns".into())
        .spawn(move || {
            let _permit = permit;
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map(|iter| iter.take(65).collect())
                .map_err(|e| format!("DNS resolution failed: {e}"));
            let _ = tx.send(result);
        })
        .map_err(|e| format!("cannot start DNS resolver: {e}"))?;
    rx.await.map_err(|_| "DNS resolver failed".to_owned())?
}

pub(crate) struct SystemMonitorWorker {
    control_port: Option<u16>,
    cooldowns: Arc<Mutex<Cooldowns>>,
    cooldown_path: PathBuf,
    proxy: Option<ResolvedProxy>,
    proxy_rejected: AtomicBool,
}
impl SystemMonitorWorker {
    pub(crate) fn new(
        control_port: Option<u16>,
        cooldowns: Arc<Mutex<Cooldowns>>,
        cooldown_path: PathBuf,
        proxy: Option<ResolvedProxy>,
    ) -> Self {
        Self {
            control_port,
            cooldowns,
            cooldown_path,
            proxy,
            proxy_rejected: AtomicBool::new(false),
        }
    }
    async fn run(&self, target: MonitorTarget) -> MonitorOutcome {
        let path = if self.proxy.is_some() {
            "proxy"
        } else {
            "direct"
        };
        let mut out = MonitorOutcome {
            success: false,
            tcp_ok: None,
            tls_ok: None,
            elapsed_ms: None,
            detail: String::new(),
            path: path.into(),
        };
        if self.proxy_rejected.load(Ordering::Acquire) {
            out.detail = "代理需要认证（407）；本次监控已停止发送代理请求，请配置后手动重开".into();
            return out;
        }
        let config = DiagnosticConfig {
            request_id: uuid::Uuid::new_v4().to_string(),
            target: target.host,
            port: target.port,
            tls: target.tls,
            allow_lan: target.allow_lan,
            proxy: None,
        };
        let config = match policy::normalize(config, self.control_port) {
            Ok(c) => c,
            Err(e) => {
                out.detail = e;
                return out;
            }
        };
        // Reject prohibited literal addresses before consuming a cooldown.
        if let Ok(ip) = config.target.parse::<IpAddr>()
            && let Err(e) =
                policy::validate_addresses(&config, vec![SocketAddr::new(ip, config.port)], &[])
        {
            out.detail = e;
            return out;
        }
        let authority = if config.target.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{}]", config.target)
        } else {
            config.target.clone()
        };
        let url = match url::Url::parse(&format!("http://{authority}/")) {
            Ok(url) => url,
            Err(_) => {
                out.detail = "invalid target host".into();
                return out;
            }
        };
        let key = crate::policy::domain_key(&url);
        // Check and persist under the same lock shared with the engine. Waiting is
        // bounded; dropping this future also cancels the timer immediately.
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            let reservation = match self.cooldowns.lock() {
                Ok(mut c) => {
                    if c.ready(&key) {
                        Some(c.reserve_dispatch(&key, &self.cooldown_path))
                    } else {
                        None
                    }
                }
                Err(_) => Some(Err("cooldown lock poisoned".into())),
            };
            match reservation {
                Some(Ok(())) => break,
                Some(Err(e)) => {
                    out.detail = format!("cooldown reservation failed: {e}");
                    return out;
                }
                None if Instant::now() >= deadline => {
                    out.detail = "cooldown wait timed out".into();
                    return out;
                }
                None => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
        let started = Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs(10), self.probe(&config, &mut out)).await;
        out.elapsed_ms = Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        match result {
            Ok(Ok(())) => {
                out.success = true;
            }
            Ok(Err(e)) => out.detail = e,
            Err(_) => {
                if out.tcp_ok == Some(true) && config.tls {
                    out.tls_ok = Some(false);
                }
                out.detail = "monitor operation timed out".into();
            }
        }
        out
    }
    async fn probe(&self, c: &DiagnosticConfig, out: &mut MonitorOutcome) -> Result<(), String> {
        let addresses = resolve(c.target.clone(), c.port).await?;
        // Check every answer, even though only one numeric destination is used.
        let address = policy::validate_addresses(c, addresses, &[])?[0];
        let stream = if let Some(proxy) = &self.proxy {
            // Dropping this local token is not cancellation, but dropping the
            // CONNECT future drops its socket. No direct fallback is permitted.
            let cancel = CancellationToken::new();
            match connect_resolved_proxy_tunnel(proxy, address, &cancel, Duration::from_secs(10))
                .await
            {
                Ok(tunnel) => tunnel.stream,
                Err(e) => {
                    if matches!(e, crate::proxy::ProxyError::AuthenticationRequired) {
                        self.proxy_rejected.store(true, Ordering::Release);
                    }
                    out.detail = format!("proxy tunnel failed: {e}");
                    return Err(out.detail.clone());
                }
            }
        } else {
            match TcpStream::connect(address).await {
                Ok(stream) => {
                    out.tcp_ok = Some(true);
                    stream
                }
                Err(e) => {
                    out.tcp_ok = Some(false);
                    return Err(format!("target TCP connection failed: {e}"));
                }
            }
        };
        // CONNECT success reports tunnel establishment, not independently
        // observed target TCP. tcp_ok remains None for the proxy route.
        if !c.tls {
            out.detail = if self.proxy.is_some() {
                format!(
                    "proxy CONNECT tunnel established to {address}; target TCP not independently observed"
                )
            } else {
                format!("target TCP connection established to {address}")
            };
            return Ok(());
        }
        let name =
            ServerName::try_from(c.target.clone()).map_err(|e| format!("invalid TLS name: {e}"))?;
        let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|e| format!("TLS configuration failed: {e}"))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        // Mark TLS attempted before awaiting, so a timeout over a proxy is
        // distinguishable from a tunnel or DNS failure.
        out.tls_ok = Some(false);
        match TlsConnector::from(Arc::new(tls))
            .connect(name, stream)
            .await
        {
            Ok(stream) => {
                drop(stream);
                out.tls_ok = Some(true);
                out.detail = if self.proxy.is_some() {
                    format!(
                        "proxy CONNECT and verified TLS succeeded to {address}; target TCP not independently observed"
                    )
                } else {
                    format!("target TCP and verified TLS succeeded to {address}")
                };
                Ok(())
            }
            Err(e) => {
                out.tls_ok = Some(false);
                Err(format!("TLS verification or handshake failed: {e}"))
            }
        }
    }
}
impl MonitorWorker for SystemMonitorWorker {
    fn check(
        &self,
        target: MonitorTarget,
    ) -> Pin<Box<dyn Future<Output = MonitorOutcome> + Send + '_>> {
        Box::pin(self.run(target))
    }
    fn fatal_error(&self) -> bool {
        self.proxy_rejected.load(Ordering::Acquire)
    }
    fn drain(&self, shutdown: CancellationToken) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            while monitor_dns_pending() && !shutdown.is_cancelled() {
                tokio::select! { _ = shutdown.cancelled() => break, _ = tokio::time::sleep(Duration::from_millis(20)) => {} }
            }
            if shutdown.is_cancelled() {
                let grace = Instant::now() + Duration::from_secs(2);
                while monitor_dns_pending() && Instant::now() < grace {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                if monitor_dns_pending() {
                    eprintln!("监控系统DNS解析仍未退出，将随进程退出结束；不代表解析已取消");
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(host: &str, port: u16, lan: bool) -> MonitorTarget {
        MonitorTarget {
            id: "one".into(),
            name: "one".into(),
            group: "".into(),
            host: host.into(),
            port,
            tls: false,
            allow_lan: lan,
            interval_secs: 60,
        }
    }
    #[tokio::test]
    async fn rejects_loopback_and_control_port_without_io() {
        let dir = tempfile::tempdir().unwrap();
        let worker = SystemMonitorWorker::new(
            Some(1234),
            Arc::new(Mutex::new(Cooldowns::default())),
            dir.path().join("cooldowns.json"),
            None,
        );
        let a = worker.check(target("127.0.0.1", 8080, true)).await;
        assert!(!a.success);
        assert_eq!(a.tcp_ok, None);
        assert_eq!(a.path, "direct");
        let b = worker.check(target("8.8.8.8", 1234, false)).await;
        assert!(!b.success);
        assert_eq!(b.tcp_ok, None);
        assert!(!dir.path().join("cooldowns.json").exists());
    }
    #[tokio::test]
    async fn mixed_dns_answers_fail_closed() {
        let c = DiagnosticConfig {
            request_id: uuid::Uuid::new_v4().to_string(),
            target: "example.com".into(),
            port: 443,
            tls: false,
            allow_lan: false,
            proxy: None,
        };
        let answers = vec![
            "8.8.8.8:443".parse().unwrap(),
            "127.0.0.1:443".parse().unwrap(),
        ];
        assert!(policy::validate_addresses(&c, answers, &[]).is_err());
    }
    #[tokio::test]
    async fn proxy_connect_is_pinned_and_sends_no_target_application_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(stream.read_u8().await.unwrap());
            }
            assert_eq!(
                header,
                b"CONNECT 8.8.8.8:443 HTTP/1.1\r\nHost: 8.8.8.8:443\r\n\r\n"
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        });
        let config = crate::proxy::ProxyConfig {
            url: format!("http://{addr}"),
        };
        let proxy = crate::proxy::resolve_proxy(
            &config,
            None,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let worker = SystemMonitorWorker::new(
            None,
            Arc::new(Mutex::new(Cooldowns::default())),
            dir.path().join("cooldowns.json"),
            Some(proxy),
        );
        let result = worker.check(target("8.8.8.8", 443, false)).await;
        assert!(result.success);
        assert_eq!(result.path, "proxy");
        assert_eq!(result.tcp_ok, None);
        assert_eq!(result.tls_ok, None);
        server.await.unwrap();
        assert!(
            !Cooldowns::load(&dir.path().join("cooldowns.json"))
                .unwrap()
                .ready("8.8.8.8")
        );
    }
    #[tokio::test]
    async fn literal_resolution_does_not_start_resolver() {
        assert_eq!(
            resolve("8.8.8.8".into(), 443).await.unwrap(),
            vec!["8.8.8.8:443".parse::<SocketAddr>().unwrap()]
        );
    }
}
