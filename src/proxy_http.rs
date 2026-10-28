//! Explicit HTTP requests over an authorized, pinned CONNECT proxy tunnel.
//! No environment proxy, redirect following, credentials, or direct fallback.
use http_body_util::BodyExt;
use hyper::{
    Request, StatusCode,
    body::{Bytes, Incoming},
    client::conn::http1,
};
use hyper_util::rt::TokioIo;
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio_rustls::{
    TlsConnector,
    rustls::{self, ClientConfig, RootCertStore, pki_types::ServerName},
};
use tokio_util::sync::CancellationToken;
use url::Url;

const MAX_BODY: usize = 64 * 1024;
trait AsyncIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncIo for T {}

#[derive(Debug)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: hyper::HeaderMap,
    pub body: Vec<u8>,
    pub truncated: bool,
    pub latency_ms: f64,
}

fn client_config() -> Result<ClientConfig, String> {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| e.to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth())
}

pub async fn request(
    url: &Url,
    addr: SocketAddr,
    proxy: &crate::proxy::ResolvedProxy,
    method: reqwest::Method,
    timeout: Duration,
    cancel: &CancellationToken,
    on_headers: impl FnOnce(reqwest::StatusCode, &reqwest::header::HeaderMap),
) -> Result<HttpResponse, String> {
    if method != reqwest::Method::GET && method != reqwest::Method::HEAD {
        return Err("only HEAD and GET are permitted".into());
    }
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("request URL must be credential-free HTTP(S) with a hostname".into());
    }
    if url.port_or_known_default() != Some(addr.port()) {
        return Err("pinned target port differs from URL port".into());
    }
    let started = Instant::now();
    let op = async {
        let tunnel = crate::proxy::connect_resolved_proxy_tunnel(proxy, addr, cancel, timeout)
            .await
            .map_err(|e| e.to_string())?;
        let io: Box<dyn AsyncIo> = if url.scheme() == "https" {
            let name =
                ServerName::try_from(url.host_str().unwrap().trim_matches(['[', ']']).to_owned())
                    .map_err(|e| e.to_string())?;
            let tls = TlsConnector::from(std::sync::Arc::new(client_config()?));
            Box::new(
                tls.connect(name, tunnel.stream)
                    .await
                    .map_err(|e| format!("TLS handshake: {e}"))?,
            )
        } else {
            Box::new(tunnel.stream)
        };
        let mut builder = http1::Builder::new();
        builder.max_headers(64).max_buf_size(16 * 1024);
        let (mut sender, connection) = builder
            .handshake(TokioIo::new(io))
            .await
            .map_err(|e| format!("HTTP handshake: {e}"))?;
        // Poll IO in this request's own future: cancellation drops the connection
        // (and its socket) before returning, with no detached driver to reap.
        let response = async {
            let host = url.host_str().unwrap();
            let authority = match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            };
            let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
            let request = Request::builder()
                .method(if method == reqwest::Method::HEAD {
                    hyper::Method::HEAD
                } else {
                    hyper::Method::GET
                })
                .uri(if path.is_empty() { "/" } else { path })
                .header(hyper::header::HOST, authority)
                .header(
                    hyper::header::USER_AGENT,
                    concat!(
                        "AoNet/",
                        env!("CARGO_PKG_VERSION"),
                        " local-network-diagnostics"
                    ),
                )
                .body(http_body_util::Empty::<Bytes>::new())
                .map_err(|e| e.to_string())?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|e| format!("HTTP request: {e}"))?;
            let status = response.status();
            let headers = response.headers().clone();
            on_headers(
                reqwest::StatusCode::from_u16(status.as_u16()).map_err(|e| e.to_string())?,
                &headers,
            );
            let content_length = headers
                .get(hyper::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let mut body = Vec::new();
            let mut truncated = method != reqwest::Method::HEAD
                && content_length.is_some_and(|length| length > MAX_BODY as u64);
            if method != reqwest::Method::HEAD {
                let mut incoming: Incoming = response.into_body();
                while body.len() < MAX_BODY {
                    let Some(frame) = incoming.frame().await else {
                        break;
                    };
                    let frame = frame.map_err(|e| format!("HTTP response body: {e}"))?;
                    if let Ok(data) = frame.into_data() {
                        let remaining = MAX_BODY - body.len();
                        body.extend_from_slice(&data[..data.len().min(remaining)]);
                        if data.len() > remaining {
                            truncated = true;
                            break;
                        }
                        if body.len() == MAX_BODY {
                            truncated = content_length != Some(MAX_BODY as u64);
                            break;
                        }
                    }
                }
            }
            Ok(HttpResponse {
                status,
                headers,
                body,
                truncated,
                latency_ms: started.elapsed().as_secs_f64() * 1000.0,
            })
        };
        tokio::pin!(connection);
        tokio::pin!(response);
        tokio::select! {
            biased;
            result = &mut response => result,
            result = &mut connection => {
                result.map_err(|e| format!("HTTP connection: {e}"))?;
                response.await
            }
        }
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err("cancelled".into()),
        result = tokio::time::timeout(timeout, op) => result.map_err(|_| "timeout".to_string())?,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn fixture(
        method: reqwest::Method,
        payload_len: usize,
        status: u16,
    ) -> (HttpResponse, String) {
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        let target = "127.0.0.1:18231".parse::<SocketAddr>().unwrap();
        let payload = vec![b'x'; payload_len];
        let server_method = method.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = proxy_listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            assert!(
                String::from_utf8(request)
                    .unwrap()
                    .starts_with(&format!("CONNECT {target} HTTP/1.1\r\nHost: {target}"))
            );
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            stream
                .write_all(
                    format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {payload_len}\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            if server_method != reqwest::Method::HEAD {
                stream.write_all(&payload).await.unwrap();
            }
            request
        });
        let proxy = crate::proxy::resolve_proxy(
            &crate::proxy::ProxyConfig {
                url: format!("http://{proxy_addr}"),
            },
            None,
            &CancellationToken::new(),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let response = request(
            &Url::parse(&format!("http://example.test:{}/path?q=1", target.port())).unwrap(),
            target,
            &proxy,
            method.clone(),
            Duration::from_secs(2),
            &CancellationToken::new(),
            move |code, _| {
                called.fetch_add(1, Ordering::SeqCst);
                assert_eq!(code.as_u16(), status);
            },
        )
        .await
        .unwrap();
        let sent = server.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        (response, sent)
    }

    #[tokio::test]
    async fn get_uses_connect_preserves_url_caps_body_and_calls_headers_once() {
        let (response, sent) = fixture(reqwest::Method::GET, MAX_BODY + 1, 503).await;
        assert!(sent.starts_with("GET /path?q=1 HTTP/1.1\r\n"));
        assert!(sent.to_ascii_lowercase().contains("host: example.test:"));
        assert_eq!(response.status.as_u16(), 503);
        assert_eq!(response.body.len(), MAX_BODY);
        assert!(response.truncated);
    }

    #[tokio::test]
    async fn headers_callback_precedes_hanging_body_and_cancellation_closes_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut byte = [0u8; 1];
            for response in [
                b"HTTP/1.1 200 Connected\r\n\r\n".as_slice(),
                b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3\r\nContent-Length: 10\r\n\r\n"
                    .as_slice(),
            ] {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                stream.write_all(response).await.unwrap();
            }
            let _ = seen_tx.send(());
            tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
                .await
                .unwrap()
                .unwrap()
        });
        let cancel = CancellationToken::new();
        let proxy = crate::proxy::resolve_proxy(
            &crate::proxy::ProxyConfig {
                url: format!("http://{addr}"),
            },
            None,
            &cancel,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
        let worker_cancel = cancel.clone();
        let worker = tokio::spawn(async move {
            request(
                &Url::parse("http://original.test/").unwrap(),
                "8.8.8.8:80".parse().unwrap(),
                &proxy,
                reqwest::Method::GET,
                Duration::from_secs(2),
                &worker_cancel,
                |status, headers| {
                    assert_eq!(status.as_u16(), 429);
                    assert_eq!(headers["retry-after"], "3");
                    headers_tx.send(()).unwrap();
                },
            )
            .await
        });
        seen_rx.await.unwrap();
        headers_rx.await.unwrap();
        cancel.cancel();
        assert_eq!(worker.await.unwrap().unwrap_err(), "cancelled");
        assert_eq!(server.await.unwrap(), 0);
    }

    #[tokio::test]
    async fn rejected_connect_surfaces_407_without_header_callback_or_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            stream
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let cancel = CancellationToken::new();
        let proxy = crate::proxy::resolve_proxy(
            &crate::proxy::ProxyConfig {
                url: format!("http://{addr}"),
            },
            None,
            &cancel,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let error = request(
            &Url::parse("http://original.test/").unwrap(),
            "8.8.8.8:80".parse().unwrap(),
            &proxy,
            reqwest::Method::GET,
            Duration::from_secs(2),
            &cancel,
            |_, _| panic!("CONNECT error is not target response headers"),
        )
        .await
        .unwrap_err();
        assert!(error.contains("407"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn exact_cap_length_is_complete_not_truncated() {
        let (response, _) = fixture(reqwest::Method::GET, MAX_BODY, 200).await;
        assert_eq!(response.body.len(), MAX_BODY);
        assert!(!response.truncated);
    }

    #[tokio::test]
    async fn whole_request_timeout_closes_hanging_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut byte = [0; 1];
            loop {
                if stream.read(&mut byte).await.unwrap() == 0 {
                    break;
                }
            }
        });
        let cancel = CancellationToken::new();
        let proxy = crate::proxy::resolve_proxy(
            &crate::proxy::ProxyConfig {
                url: format!("http://{addr}"),
            },
            None,
            &cancel,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let error = request(
            &Url::parse("http://example.test/").unwrap(),
            "8.8.8.8:80".parse().unwrap(),
            &proxy,
            reqwest::Method::GET,
            Duration::from_millis(20),
            &cancel,
            |_, _| panic!("no final response"),
        )
        .await
        .unwrap_err();
        assert!(error.contains("timeout") || error.contains("timed out"));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn head_does_not_read_response_body() {
        let (response, sent) = fixture(reqwest::Method::HEAD, MAX_BODY + 1, 200).await;
        assert!(sent.starts_with("HEAD /path?q=1 HTTP/1.1\r\n"));
        assert!(response.body.is_empty());
        assert!(!response.truncated);
    }
}
