use super::{DiagnosticConfig, Shared, stage, update};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpStream, time::Instant};
use tokio_rustls::{
    TlsConnector,
    rustls::{self, ClientConfig, RootCertStore, pki_types::ServerName},
};
use tokio_util::sync::CancellationToken;

pub(super) fn client_config() -> Result<ClientConfig, String> {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    config_with_roots(roots)
}
fn config_with_roots(roots: RootCertStore) -> Result<ClientConfig, String> {
    Ok(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}
// Production and fixture use identical verification; only test anchors vary.

pub(super) async fn probe(
    shared: &Shared,
    config: &DiagnosticConfig,
    address: SocketAddr,
    proxy: Option<&crate::proxy::ResolvedProxy>,
    cancel: &CancellationToken,
) {
    probe_with_config(shared, config, address, proxy, cancel, client_config()).await;
}
async fn probe_with_config(
    shared: &Shared,
    config: &DiagnosticConfig,
    address: SocketAddr,
    proxy: Option<&crate::proxy::ResolvedProxy>,
    cancel: &CancellationToken,
    tls_config: Result<ClientConfig, String>,
) {
    let started = Instant::now();
    update(shared, |s| s.stages.tcp.status = "running".into());
    // Exactly one connect future, to the validated numeric address. The same
    // owned stream moves into TLS; there is no domain re-resolution or fallback.
    let stream = if let Some(proxy) = proxy {
        let observations = json!({
            "method":match proxy.protocol() { crate::proxy::ProxyProtocol::Http => "explicit HTTP CONNECT", crate::proxy::ProxyProtocol::Socks5 => "explicit SOCKS5 CONNECT" },
            "target_addr":address,
            "proxy_addresses":proxy.addresses(),
            "target_tcp_independently_observed":false,
            "proxy_tcp":{"status":"unknown"},
            "connect":{"status":"unknown"}
        });
        match crate::proxy::connect_resolved_proxy_tunnel(
            proxy,
            address,
            cancel,
            Duration::from_secs(5),
        )
        .await
        {
            Ok(tunnel) => {
                let mut observations = observations;
                observations["proxy_addr"] = json!(tunnel.proxy_addr);
                observations["proxy_tcp"] = json!({"status":"success","elapsed_ms":tunnel.proxy_tcp_duration.as_secs_f64()*1000.0});
                observations["connect"] = json!({"status":"success","elapsed_ms":tunnel.tunnel_duration.as_secs_f64()*1000.0,"evidence":"Proxy accepted CONNECT; target TCP latency is not independently known"});
                update(shared, |s| {
                    s.stages.tcp = stage("success", started, None, observations)
                });
                tunnel.stream
            }
            Err(error) => {
                use crate::proxy::ProxyError;
                let status = match error {
                    ProxyError::Cancelled => "cancelled",
                    ProxyError::Timeout => "timeout",
                    _ => "error",
                };
                let mut observations = observations;
                // A CONNECT-specific failure proves the underlying proxy TCP
                // connected, but shared transport does not expose its timing.
                if matches!(
                    error,
                    ProxyError::Protocol
                        | ProxyError::HeaderTooLarge
                        | ProxyError::AuthenticationRequired
                        | ProxyError::Rejected(_)
                        | ProxyError::Socks5Protocol
                        | ProxyError::Socks5AuthenticationRequired
                        | ProxyError::Socks5Rejected(_)
                ) {
                    observations["proxy_tcp"] = json!({"status":"success","elapsed_ms":null});
                    observations["connect"] = json!({"status":"error"});
                } else if matches!(error, ProxyError::Connect) {
                    observations["proxy_tcp"] = json!({"status":"error"});
                    observations["connect"] = json!({"status":"skipped"});
                }
                update(shared, |s| {
                    s.stages.tcp = stage(status, started, Some(error.to_string()), observations)
                });
                return;
            }
        }
    } else {
        let connected = tokio::select! {
            biased;
            _ = cancel.cancelled() => {update(shared,|s|s.stages.tcp=stage("cancelled",started,None,json!({"remote_addr":address})));return;}
            result = tokio::time::timeout(Duration::from_secs(5),TcpStream::connect(address)) => result,
        };
        let stream = match connected {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                update(shared, |s| {
                    s.stages.tcp = stage(
                        "error",
                        started,
                        Some(error.to_string()),
                        json!({"remote_addr":address,"error_kind":format!("{:?}",error.kind())}),
                    )
                });
                return;
            }
            Err(_) => {
                update(shared, |s| {
                    s.stages.tcp = stage("timeout", started, None, json!({"remote_addr":address}))
                });
                return;
            }
        };
        update(shared, |s| {
            s.stages.tcp = stage("success", started, None, json!({"remote_addr":address}))
        });
        stream
    };
    if !config.tls {
        drop(stream);
        update(shared, |s| {
            s.stages.tls.status = "skipped".into();
            s.stages.tls.observations = json!({"reason":"TLS disabled; not applicable"});
        });
        return;
    }
    let started = Instant::now();
    update(shared, |s| s.stages.tls.status = "running".into());
    let observations = json!({"trust_source":"bundled Mozilla roots (not OS/enterprise roots)","server_name":config.target,"ip_identity":config.target.parse::<std::net::IpAddr>().is_ok(),"same_tcp_stream":true,"via_proxy":proxy.is_some()});
    let server_name = match ServerName::try_from(config.target.clone()) {
        Ok(name) => name,
        Err(error) => {
            drop(stream);
            update(shared, |s| {
                s.stages.tls = stage("error", started, Some(error.to_string()), observations)
            });
            return;
        }
    };
    let client_config = match tls_config {
        Ok(c) => c,
        Err(error) => {
            drop(stream);
            update(shared, |s| {
                s.stages.tls = stage("error", started, Some(error), observations)
            });
            return;
        }
    };
    let connector = TlsConnector::from(Arc::new(client_config));
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {update(shared,|s|s.stages.tls=stage("cancelled",started,None,observations));return;}
        result = tokio::time::timeout(Duration::from_secs(5),connector.connect(server_name,stream)) => result,
    };
    match result {
        Ok(Ok(tls)) => {
            let (_, session) = tls.get_ref();
            let mut observations = observations;
            observations["protocol"] = json!(session.protocol_version().map(|v| format!("{v:?}")));
            observations["cipher"] = json!(
                session
                    .negotiated_cipher_suite()
                    .map(|v| format!("{:?}", v.suite()))
            );
            // Do not write application data (nor HTTP). Drop before reporting.
            drop(tls);
            update(shared, |s| {
                s.stages.tls = stage("success", started, None, observations)
            });
        }
        Ok(Err(error)) => update(shared, |s| {
            s.stages.tls = stage("error", started, Some(error.to_string()), observations)
        }),
        Err(_) => update(shared, |s| {
            s.stages.tls = stage("timeout", started, None, observations)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{io::AsyncReadExt, net::TcpListener};
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        },
    };
    fn fixture_roots() -> RootCertStore {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(
                include_bytes!("fixtures/ca.der").to_vec(),
            ))
            .unwrap();
        roots
    }
    fn shared(target: &str, port: u16) -> (Shared, DiagnosticConfig) {
        let c = DiagnosticConfig {
            request_id: uuid::Uuid::new_v4().to_string(),
            target: target.into(),
            port,
            tls: true,
            allow_lan: false,
            proxy: None,
        };
        (
            Arc::new(Mutex::new(super::super::DiagnosticSnapshot::new(c.clone()))),
            c,
        )
    }
    async fn server() -> (
        SocketAddr,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<Result<usize, String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let accepted = count.clone();
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(
                        include_bytes!("fixtures/server.der").to_vec(),
                    )],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                        include_bytes!("fixtures/server-key.der").to_vec(),
                    )),
                )
                .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
            accepted.fetch_add(1, Ordering::SeqCst);
            let mut tls = acceptor.accept(stream).await.map_err(|e| e.to_string())?;
            let mut payload = [0_u8; 32];
            match tokio::time::timeout(Duration::from_secs(1), tls.read(&mut payload)).await {
                Ok(Ok(n)) => Ok(n),
                // Abrupt TLS close without close_notify is allowed for this no-payload probe.
                Ok(Err(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                    ) =>
                {
                    Ok(0)
                }
                other => Err(format!("unexpected payload read: {other:?}")),
            }
        });
        (address, count, task)
    }
    #[tokio::test]
    async fn valid_dns_and_ip_identity_same_stream_no_payload() {
        for name in ["diagnostic.test", "127.0.0.1"] {
            let (address, count, server) = server().await;
            let (shared, c) = shared(name, address.port());
            probe_with_config(
                &shared,
                &c,
                address,
                None,
                &CancellationToken::new(),
                config_with_roots(fixture_roots()),
            )
            .await;
            assert_eq!(shared.lock().unwrap().stages.tcp.status, "success");
            let tls_stage = shared.lock().unwrap().stages.tls.clone();
            assert_eq!(tls_stage.status, "success", "{:?}", tls_stage.error);
            assert_eq!(server.await.unwrap().unwrap(), 0);
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn wrong_identity_and_untrusted_ca_preserve_tcp_success() {
        for (name, trusted) in [("wrong.test", true), ("diagnostic.test", false)] {
            let (address, count, server) = server().await;
            let (shared, c) = shared(name, address.port());
            let client = if trusted {
                config_with_roots(fixture_roots())
            } else {
                client_config()
            };
            probe_with_config(
                &shared,
                &c,
                address,
                None,
                &CancellationToken::new(),
                client,
            )
            .await;
            assert_eq!(shared.lock().unwrap().stages.tcp.status, "success");
            assert_eq!(shared.lock().unwrap().stages.tls.status, "error");
            assert!(server.await.unwrap().is_err());
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn hanging_handshake_timeout_drops_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shared, c) = shared("diagnostic.test", address.port());
        let observed = shared.clone();
        let task = tokio::spawn(async move {
            probe(&observed, &c, address, None, &CancellationToken::new()).await;
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut hello = [0; 4096];
        assert!(stream.read(&mut hello).await.unwrap() > 0);
        // Pausing only after local IO is ready makes the fixed 5s handshake
        // budget deterministic and avoids wall-clock sleeps or external peers.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(6)).await;
        task.await.unwrap();
        tokio::time::resume();
        assert_eq!(shared.lock().unwrap().stages.tcp.status, "success");
        assert_eq!(shared.lock().unwrap().stages.tls.status, "timeout");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stream.read(&mut hello))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn hanging_handshake_cancel_drops_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shared, c) = shared("diagnostic.test", address.port());
        let token = CancellationToken::new();
        let worker = token.clone();
        let observed = shared.clone();
        let task = tokio::spawn(async move {
            probe(&observed, &c, address, None, &worker).await;
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut hello = [0; 4096];
        assert!(stream.read(&mut hello).await.unwrap() > 0);
        token.cancel();
        task.await.unwrap();
        assert_eq!(shared.lock().unwrap().stages.tls.status, "cancelled");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stream.read(&mut hello))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn proxy_tunnel_retains_original_tls_identity_and_sends_no_payload() {
        use tokio::io::AsyncWriteExt;
        for name in ["diagnostic.test", "wrong.test"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server_config = ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(
                    include_bytes!("fixtures/server.der").to_vec(),
                )],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                    include_bytes!("fixtures/server-key.der").to_vec(),
                )),
            )
            .unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(server_config));
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut header = Vec::new();
                let mut byte = [0; 1];
                while !header.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).await.unwrap();
                    header.push(byte[0]);
                }
                assert!(
                    String::from_utf8(header)
                        .unwrap()
                        .starts_with("CONNECT 10.23.0.1:443 HTTP/1.1")
                );
                stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                match acceptor.accept(stream).await {
                    Ok(mut stream) => {
                        assert_eq!(stream.get_ref().1.server_name(), Some("diagnostic.test"));
                        match stream.read(&mut byte).await {
                            Ok(0) => (),
                            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => (),
                            other => panic!("unexpected application data {other:?}"),
                        }
                        true
                    }
                    Err(_) => false,
                }
            });
            let proxy = crate::proxy::resolve_proxy(
                &crate::proxy::ProxyConfig {
                    url: format!("http://{address}"),
                },
                None,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            let (shared, c) = shared(name, 443);
            probe_with_config(
                &shared,
                &c,
                "10.23.0.1:443".parse().unwrap(),
                Some(&proxy),
                &CancellationToken::new(),
                config_with_roots(fixture_roots()),
            )
            .await;
            assert_eq!(shared.lock().unwrap().stages.tcp.status, "success");
            let expected = name == "diagnostic.test";
            assert_eq!(
                shared.lock().unwrap().stages.tls.status,
                if expected { "success" } else { "error" }
            );
            assert_eq!(server.await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn proxy_407_preserves_proxy_tcp_and_rejects_target_evidence() {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            let mut byte = [0; 1];
            while !header.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
            }
            stream
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let proxy = crate::proxy::resolve_proxy(
            &crate::proxy::ProxyConfig {
                url: format!("http://{address}"),
            },
            None,
            &CancellationToken::new(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let (shared, c) = shared("diagnostic.test", 443);
        probe(
            &shared,
            &c,
            "10.23.0.1:443".parse().unwrap(),
            Some(&proxy),
            &CancellationToken::new(),
        )
        .await;
        server.await.unwrap();
        let tcp = shared.lock().unwrap().stages.tcp.clone();
        assert_eq!(tcp.status, "error");
        assert!(tcp.error.unwrap().contains("407"));
        assert_eq!(tcp.observations["proxy_tcp"]["status"], "success");
        assert_eq!(tcp.observations["connect"]["status"], "error");
        assert_eq!(tcp.observations["target_tcp_independently_observed"], false);
    }
}
