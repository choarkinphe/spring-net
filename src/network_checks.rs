//! Single, fixed-address connection checks. Address policy belongs to the
//! manager; fixtures call these private crate helpers directly, never via a
//! production loopback exemption. No resolver, fallback, proxy, or persistence.
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpStream, time::Instant};
use tokio_rustls::{
    TlsConnector,
    rustls::{self, ClientConfig, RootCertStore, pki_types::ServerName},
};
use tokio_util::sync::CancellationToken;

pub(crate) struct CheckResult {
    pub status: &'static str,
    pub output: String,
    pub summary: String,
}
impl CheckResult {
    fn new(status: &'static str, detail: String) -> Self {
        Self {
            status,
            output: detail.clone(),
            summary: detail,
        }
    }
}

// Same ring provider and bundled Mozilla roots as diagnostics/transport.rs.
// Deliberately no OS/enterprise roots, client identity or insecure verifier.
fn client_config() -> Result<ClientConfig, String> {
    config_with_roots(RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    ))
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

async fn connect(
    address: SocketAddr,
    cancel: &CancellationToken,
) -> Result<(TcpStream, f64), CheckResult> {
    let started = Instant::now();
    let connected = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(CheckResult::new("cancelled", "已停止；TCP 连接未完成".into())),
        result = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(address)) => result,
    };
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    match connected {
        Ok(Ok(stream)) => Ok((stream, elapsed)),
        Ok(Err(error)) => {
            let (status, kind) = match error.kind() {
                io::ErrorKind::ConnectionRefused => ("error", "连接被拒绝"),
                io::ErrorKind::TimedOut => ("timeout", "连接超时"),
                _ => ("error", "连接失败"),
            };
            Err(CheckResult::new(
                status,
                format!("TCP {kind}：{address}；{elapsed:.2} ms；{error}"),
            ))
        }
        Err(_) => Err(CheckResult::new(
            "timeout",
            format!("TCP 连接超时（5 秒）：{address}"),
        )),
    }
}

pub(crate) async fn tcp(address: SocketAddr, cancel: &CancellationToken) -> CheckResult {
    match connect(address, cancel).await {
        Ok((stream, elapsed)) => {
            drop(stream); // Close before terminal publication, with no app bytes.
            CheckResult::new(
                "completed",
                format!("TCP 连接成功：{address}；{elapsed:.2} ms；已关闭，未发送应用数据"),
            )
        }
        Err(result) => result,
    }
}

pub(crate) async fn tls(
    host: &str,
    address: SocketAddr,
    cancel: &CancellationToken,
) -> CheckResult {
    tls_with_config(host, address, cancel, client_config()).await
}
async fn tls_with_config(
    host: &str,
    address: SocketAddr,
    cancel: &CancellationToken,
    config: Result<ClientConfig, String>,
) -> CheckResult {
    let (stream, tcp_ms) = match connect(address, cancel).await {
        Ok(result) => result,
        Err(mut result) => {
            result.output.push_str("；TLS 未测试");
            result.summary = result.output.clone();
            return result;
        }
    };
    let fact = format!("TCP 连接成功：{address}；{tcp_ms:.2} ms");
    let trust = "证书链和名称校验开启；信任随包 Mozilla 根（不是系统/企业 CA）；不获取 AIA/OCSP；不发送 HTTP/认证数据";
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            drop(stream);
            return CheckResult::new("error", format!("{fact}\nTLS 配置失败：{error}；{trust}"));
        }
    };
    let server_name = match ServerName::try_from(host.to_owned()) {
        Ok(name) => name,
        Err(error) => {
            drop(stream);
            return CheckResult::new("error", format!("{fact}\nTLS 主机名无效：{error}；{trust}"));
        }
    };
    let started = Instant::now();
    let connector = TlsConnector::from(Arc::new(config));
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return CheckResult::new("cancelled", format!("{fact}\nTLS 已停止；{trust}")),
        result = tokio::time::timeout(Duration::from_secs(5), connector.connect(server_name, stream)) => result,
    };
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let (status, detail) = match result {
        Ok(Ok(tls)) => {
            let (_, session) = tls.get_ref();
            let protocol = session
                .protocol_version()
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "unknown".into());
            let cipher = session
                .negotiated_cipher_suite()
                .map(|v| format!("{:?}", v.suite()))
                .unwrap_or_else(|| "unknown".into());
            drop(tls);
            (
                "completed",
                format!("TLS 握手成功：{protocol} / {cipher}；{elapsed:.2} ms；身份 {host}"),
            )
        }
        Ok(Err(error)) => ("error", format!("TLS 握手失败：{error}；{elapsed:.2} ms")),
        Err(_) => ("timeout", "TLS 握手超时（5 秒）".into()),
    };
    CheckResult::new(status, format!("{fact}\n{detail}\n{trust}"))
}

pub(crate) async fn http(
    url: &str,
    host: &str,
    address: SocketAddr,
    cancel: &CancellationToken,
) -> CheckResult {
    let config = match client_config() {
        Ok(config) => config,
        Err(error) => return CheckResult::new("error", format!("HTTP TLS 配置失败：{error}")),
    };
    http_with_config(url, host, address, cancel, config).await
}
async fn http_with_config(
    url: &str,
    host: &str,
    address: SocketAddr,
    cancel: &CancellationToken,
    config: ClientConfig,
) -> CheckResult {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .resolve_to_addrs(host, &[address])
        .use_preconfigured_tls(config)
        .http1_only()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(error) => return CheckResult::new("error", format!("HTTP 客户端配置失败：{error}")),
    };
    let started = Instant::now();
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => return CheckResult::new("cancelled", "HTTP HEAD 已停止，未完成响应检测".into()),
        result = tokio::time::timeout(Duration::from_secs(10), client.head(url).header(reqwest::header::CONNECTION, "close").send()) => result,
    };
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let result = match result {
        Ok(Ok(response)) => {
            let status = response.status();
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .map(|v| bounded_header(v.as_bytes()))
                .unwrap_or_else(|| "（无）".into());
            // All HTTP statuses prove receipt of a response. Never error_for_status,
            // read the body, follow Location, retry 429, or fall back to GET on 405.
            drop(response);
            CheckResult::new(
                "completed",
                format!(
                    "HEAD {url}\nHTTP {status}；响应头耗时 {elapsed:.2} ms\nLocation: {location}\n收到 HTTP 响应，不代表业务成功；未跟随重定向、未重试、未读取响应体。HTTPS 使用随包 Mozilla 根验证，不是系统/企业 CA。"
                ),
            )
        }
        Ok(Err(error)) => CheckResult::new(
            if error.is_timeout() {
                "timeout"
            } else {
                "error"
            },
            format!("HTTP HEAD 请求失败：{error}；{elapsed:.2} ms；未获得完整响应头"),
        ),
        Err(_) => CheckResult::new(
            "timeout",
            "HTTP HEAD 超时（10 秒）；未获得完整响应头".into(),
        ),
    };
    drop(client);
    result
}
fn bounded_header(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut value: String = text
        .chars()
        .take(1024)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().count() > 1024 {
        value.push_str("…（已截断）");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        },
    };

    fn fixture_config() -> ClientConfig {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(
                include_bytes!("diagnostics/fixtures/ca.der").to_vec(),
            ))
            .unwrap();
        config_with_roots(roots).unwrap()
    }
    fn acceptor() -> TlsAcceptor {
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(
                        include_bytes!("diagnostics/fixtures/server.der").to_vec(),
                    )],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                        include_bytes!("diagnostics/fixtures/server-key.der").to_vec(),
                    )),
                )
                .unwrap();
        TlsAcceptor::from(Arc::new(config))
    }
    async fn listener() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").await.unwrap()
    }

    #[tokio::test]
    async fn tcp_connects_once_and_closes_without_application_bytes() {
        let listener = listener().await;
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut data))
                .await
                .unwrap()
                .unwrap();
            assert!(data.is_empty());
            assert!(
                tokio::time::timeout(Duration::from_millis(30), listener.accept())
                    .await
                    .is_err()
            );
        });
        let result = tcp(address, &CancellationToken::new()).await;
        assert_eq!(result.status, "completed");
        server.await.unwrap();
        let listener = self::listener().await;
        let address = listener.local_addr().unwrap();
        drop(listener);
        let result = tcp(address, &CancellationToken::new()).await;
        assert_eq!(result.status, "error");
        assert!(result.summary.contains("拒绝"));
    }

    #[tokio::test]
    async fn tls_verifies_ca_and_name_on_the_same_single_connection() {
        for (name, trusted, expected) in [
            ("diagnostic.test", true, "completed"),
            ("wrong.test", true, "error"),
            ("diagnostic.test", false, "error"),
            ("127.0.0.1", true, "completed"),
        ] {
            let listener = listener().await;
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                if let Ok(mut stream) = acceptor().accept(stream).await {
                    let mut data = Vec::new();
                    let _ =
                        tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut data))
                            .await;
                    assert!(data.is_empty());
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), listener.accept())
                        .await
                        .is_err()
                );
            });
            let config = if trusted {
                Ok(fixture_config())
            } else {
                client_config()
            };
            let result = tls_with_config(name, address, &CancellationToken::new(), config).await;
            assert_eq!(result.status, expected, "{}", result.output);
            assert!(result.output.contains("TCP 连接成功"));
            if expected == "completed" {
                assert!(result.output.contains("TLSv1_3"));
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn tls_timeout_and_cancel_close_the_socket_before_returning() {
        for cancel_it in [false, true] {
            let listener = listener().await;
            let address = listener.local_addr().unwrap();
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            let (accepted, ready) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let _ = accepted.send(());
                let mut bytes = Vec::new();
                // ClientHello is allowed; no TLS server response is sent.
                stream.read_to_end(&mut bytes).await.unwrap();
            });
            let task = tokio::spawn(async move {
                tls_with_config("diagnostic.test", address, &token, Ok(fixture_config())).await
            });
            ready.await.unwrap();
            if cancel_it {
                cancel.cancel();
            } else {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(6)).await;
                tokio::time::resume();
            }
            let result = task.await.unwrap();
            assert_eq!(
                result.status,
                if cancel_it { "cancelled" } else { "timeout" }
            );
            assert!(result.output.contains("TCP 连接成功"));
            tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .unwrap()
                .unwrap();
        }
    }

    async fn read_headers(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> String {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
            bytes.push(byte[0]);
            assert!(bytes.len() < 16384);
        }
        String::from_utf8(bytes).unwrap()
    }

    #[tokio::test]
    async fn http_is_one_head_preserves_path_host_and_does_not_retry_redirect_or_read_body() {
        for status in [
            "200 OK",
            "302 Found",
            "405 Method Not Allowed",
            "429 Too Many Requests",
        ] {
            let listener = listener().await;
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_headers(&mut stream).await;
                assert!(
                    request.starts_with("HEAD /Case/%2fKeep HTTP/1.1\r\n"),
                    "{request}"
                );
                assert!(
                    request.contains(&format!("host: diagnostic.test:{}\r\n", address.port())),
                    "{request}"
                );
                // Advertise a huge body but do not send one. HEAD must complete.
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 999999\r\nLocation: http://127.0.0.1/private\r\n\r\n").as_bytes()).await.unwrap();
                let mut bytes = Vec::new();
                tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut bytes))
                    .await
                    .unwrap()
                    .unwrap();
                assert!(bytes.is_empty());
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), listener.accept())
                        .await
                        .is_err()
                );
            });
            let result = http(
                &format!("http://diagnostic.test:{}/Case/%2fKeep", address.port()),
                "diagnostic.test",
                address,
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(result.status, "completed", "{}", result.output);
            assert!(result.output.contains(status));
            assert!(result.output.contains("http://127.0.0.1/private"));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn https_fixed_ip_still_validates_original_host_and_fixture_ca() {
        for name in ["diagnostic.test", "wrong.test"] {
            let listener = listener().await;
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                if let Ok(mut stream) = acceptor().accept(stream).await {
                    assert!(
                        read_headers(&mut stream)
                            .await
                            .starts_with("HEAD /secure HTTP/1.1")
                    );
                    let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), listener.accept())
                        .await
                        .is_err()
                );
            });
            let result = http_with_config(
                &format!("https://{name}:{}/secure", address.port()),
                name,
                address,
                &CancellationToken::new(),
                fixture_config(),
            )
            .await;
            assert_eq!(
                result.status,
                if name == "diagnostic.test" {
                    "completed"
                } else {
                    "error"
                },
                "{}",
                result.output
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn http_cancel_releases_socket_and_never_falls_back() {
        let listener = listener().await;
        let address = listener.local_addr().unwrap();
        let (sent, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            sent.send(()).unwrap();
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            assert!(bytes.is_empty());
            assert!(
                tokio::time::timeout(Duration::from_millis(30), listener.accept())
                    .await
                    .is_err()
            );
        });
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move {
            http(
                &format!("http://diagnostic.test:{}/", address.port()),
                "diagnostic.test",
                address,
                &token,
            )
            .await
        });
        ready.await.unwrap();
        cancel.cancel();
        assert_eq!(task.await.unwrap().status, "cancelled");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_eof_is_error_and_never_retries() {
        let listener = listener().await;
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            drop(stream);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), listener.accept())
                    .await
                    .is_err()
            );
        });
        let result = http(
            &format!("http://diagnostic.test:{}/", address.port()),
            "diagnostic.test",
            address,
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(result.status, "error");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_timeout_is_bounded_and_drops_the_socket() {
        let listener = listener().await;
        let address = listener.local_addr().unwrap();
        let (sent, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            sent.send(()).unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert!(bytes.is_empty());
        });
        let task = tokio::spawn(async move {
            http(
                &format!("http://diagnostic.test:{}/", address.port()),
                "diagnostic.test",
                address,
                &CancellationToken::new(),
            )
            .await
        });
        ready.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::time::resume();
        assert_eq!(task.await.unwrap().status, "timeout");
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn location_is_bounded_and_controls_are_not_output() {
        let value = bounded_header(&vec![b'a'; 2000]);
        assert!(value.contains("截断"));
        assert!(value.len() < 1100);
        assert_eq!(bounded_header(b"x\r\ny\0"), "x  y ");
    }
}
