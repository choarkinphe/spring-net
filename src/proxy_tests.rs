use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn config(port: u16) -> ProxyConfig {
    ProxyConfig {
        url: format!("http://127.0.0.1:{port}"),
    }
}
#[tokio::test]
async fn resolver_slots_survive_dropped_join_handle() {
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let guard = ProxyDnsPendingGuard::acquire().unwrap();
    let job = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        wait.recv().unwrap();
    });
    drop(job);
    let second = ProxyDnsPendingGuard::acquire().unwrap();
    assert_eq!(proxy_dns_pending(), 2);
    assert!(ProxyDnsPendingGuard::acquire().is_err());
    drop(second);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while proxy_dns_pending() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn target() -> SocketAddr {
    "192.0.2.1:443".parse().unwrap()
}

async fn fixture(response: Vec<u8>) -> (ProxyConfig, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(listener.local_addr().unwrap().port());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        let _ = socket.write_all(&response).await;
        request
    });
    (config, handle)
}
async fn connect(config: &ProxyConfig) -> Result<ProxyTunnel, ProxyError> {
    connect_proxy_tunnel(
        config,
        target(),
        &CancellationToken::new(),
        Duration::from_secs(2),
        None,
    )
    .await
}

#[tokio::test]
async fn successful_tunnel_preserves_extra_bytes_and_numeric_target() {
    let (config, handle) = fixture(b"HTTP/1.1 200 OK\r\nX-Test: yes\r\n\r\nhello".to_vec()).await;
    let mut tunnel = connect(&config).await.unwrap();
    let mut bytes = [0; 5];
    tunnel.stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"hello");
    assert_eq!(
        handle.await.unwrap(),
        b"CONNECT 192.0.2.1:443 HTTP/1.1\r\nHost: 192.0.2.1:443\r\n\r\n"
    );
}
#[tokio::test]
async fn ipv6_authority_and_407() {
    let (config, handle) = fixture(b"HTTP/1.1 407 Auth\r\n\r\n".to_vec()).await;
    let target = "[2001:db8::1]:443".parse().unwrap();
    assert!(matches!(
        connect_proxy_tunnel(
            &config,
            target,
            &CancellationToken::new(),
            Duration::from_secs(2),
            None
        )
        .await,
        Err(ProxyError::AuthenticationRequired)
    ));
    assert_eq!(
        handle.await.unwrap(),
        b"CONNECT [2001:db8::1]:443 HTTP/1.1\r\nHost: [2001:db8::1]:443\r\n\r\n"
    );
}
#[tokio::test]
async fn rejects_non_success_and_oversized_and_partial() {
    for (response, expected) in [
        (b"HTTP/1.1 403 Forbidden\r\n\r\n".to_vec(), 0),
        (vec![b'a'; MAX_HEADER + 1], 1),
        (b"HTTP/1.1 200 OK\r\n".to_vec(), 2),
    ] {
        let (config, handle) = fixture(response).await;
        let result = connect(&config).await;
        assert!(match expected {
            0 => matches!(result, Err(ProxyError::Rejected(403))),
            1 => matches!(result, Err(ProxyError::HeaderTooLarge)),
            _ => matches!(result, Err(ProxyError::Protocol)),
        });
        handle.await.unwrap();
    }
}
#[tokio::test]
async fn cancellation_closes_socket_and_timeout_is_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_config = config(listener.local_addr().unwrap().port());
    let token = CancellationToken::new();
    let task_token = token.clone();
    let task = tokio::spawn(async move {
        connect_proxy_tunnel(
            &first_config,
            target(),
            &task_token,
            Duration::from_secs(10),
            None,
        )
        .await
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut request = [0; 256];
    assert!(socket.read(&mut request).await.unwrap() > 0);
    token.cancel();
    assert!(matches!(task.await.unwrap(), Err(ProxyError::Cancelled)));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut request))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(listener.local_addr().unwrap().port());
    assert!(matches!(
        connect_proxy_tunnel(
            &config,
            target(),
            &CancellationToken::new(),
            Duration::from_millis(30),
            None
        )
        .await,
        Err(ProxyError::Timeout)
    ));
}
#[test]
fn endpoint_and_all_answer_checks() {
    for url in [
        "http://host",
        "https://host:80",
        "http://user@host:80",
        "http://host:80/a",
        "http://host:80?x",
        "http://host:80#x",
        "http://host:0",
        "http://host:80/../",
        "http://host:80\r\nX:a",
    ] {
        assert!(
            endpoint(&ProxyConfig { url: url.into() }, None).is_err(),
            "{url}"
        );
    }
    assert!(endpoint(&config(9000), Some(9000)).is_err());
    assert!(
        validate_answers(
            [
                "127.0.0.1:9000".parse().unwrap(),
                "169.254.169.254:9000".parse().unwrap()
            ],
            9000,
            ProxyProtocol::Http
        )
        .is_err()
    );
    assert!(
        validate_answers(
            ["[::ffff:169.254.169.254]:9000".parse().unwrap()],
            9000,
            ProxyProtocol::Http
        )
        .is_err()
    );
    assert!(
        validate_answers(
            ["[fd00:ec2::254]:9000".parse().unwrap()],
            9000,
            ProxyProtocol::Http
        )
        .is_err()
    );
    assert!(
        validate_answers(
            ["127.0.0.1:9000".parse().unwrap()],
            9000,
            ProxyProtocol::Http
        )
        .is_ok()
    );
    assert!(
        serde_json::from_str::<ProxyConfig>(r#"{"url":"http://localhost:80","password":"x"}"#)
            .is_err()
    );
}

fn socks_config(port: u16) -> ProxyConfig {
    ProxyConfig {
        url: format!("socks5://127.0.0.1:{port}"),
    }
}

#[test]
fn both_protocols_normalize_strict_endpoints_without_network() {
    for scheme in ["http", "socks5"] {
        for (input, expected) in [
            ("LOCALHOST:080/", "localhost:80"),
            ("127.0.0.1:1080", "127.0.0.1:1080"),
            ("[2001:0DB8::1]:1080", "[2001:db8::1]:1080"),
            ("BÜCHER.invalid:1080", "xn--bcher-kva.invalid:1080"),
        ] {
            let normalized = normalize_proxy_config(
                &ProxyConfig {
                    url: format!("{scheme}://{input}"),
                },
                None,
            )
            .unwrap();
            assert_eq!(normalized.url, format!("{scheme}://{expected}"));
            assert_eq!(
                normalize_proxy_config(&normalized, None).unwrap(),
                normalized
            );
        }
        for authority in [
            "host",
            "host:0",
            "host:65536",
            "host:+80",
            "host:80/a",
            "host:80/../",
            "host:80//",
            "host:80?",
            "host:80#",
            "@host:80",
            "user:pass@host:80",
            "host.:80",
            "host..invalid:80",
            "host:80\\",
            "host:80\r\n",
            "host:80 ",
            "127.1:80",
            "2130706433:80",
            "0177.0.0.1:80",
            "0x7f.0.0.1:80",
            "127.0.0.999:80",
            "127.0.0.01:80",
            "%31%32%37.0.0.1:80",
            "[::gg]:80",
            "::1:80",
            "[fe80::1%25en0]:80",
            "-host.invalid:80",
            "host_.invalid:80",
            "host.invalid:80:90",
        ] {
            let config = ProxyConfig {
                url: format!("{scheme}://{authority}"),
            };
            assert!(
                normalize_proxy_config(&config, None).is_err(),
                "{}",
                config.url
            );
        }
        assert!(
            normalize_proxy_config(
                &ProxyConfig {
                    url: format!("{scheme}://host:9000")
                },
                Some(9000)
            )
            .is_err()
        );
    }
    for scheme in ["https", "socks", "socks4", "socks5h", "HTTP"] {
        assert!(
            normalize_proxy_config(
                &ProxyConfig {
                    url: format!("{scheme}://host:80")
                },
                None
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn socks_resolution_keeps_protocol_and_validates_all_answers() {
    let resolved = resolve_proxy(
        &socks_config(1080),
        None,
        &CancellationToken::new(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(resolved.protocol(), ProxyProtocol::Socks5);
    assert_eq!(
        resolved.addresses(),
        &["127.0.0.1:1080".parse::<SocketAddr>().unwrap()]
    );
    for protocol in [ProxyProtocol::Http, ProxyProtocol::Socks5] {
        for bad in [
            "169.254.169.254",
            "168.63.129.16",
            "0.0.0.0",
            "224.0.0.1",
            "fd00:ec2::254",
            "::ffff:169.254.169.254",
        ] {
            assert!(matches!(
                validate_answers(
                    [
                        "127.0.0.1:1080".parse().unwrap(),
                        SocketAddr::new(bad.parse().unwrap(), 1080),
                    ],
                    1080,
                    protocol
                ),
                Err(ProxyError::ForbiddenAddress)
            ));
        }
        assert!(matches!(
            validate_answers([], 1080, protocol),
            Err(ProxyError::Resolution)
        ));
        assert!(matches!(
            validate_answers(
                std::iter::repeat_n("127.0.0.1:1080".parse().unwrap(), MAX_ADDRESSES + 1),
                1080,
                protocol
            ),
            Err(ProxyError::Resolution)
        ));
        let snapshot = validate_answers(
            [
                "127.0.0.2:1080".parse().unwrap(),
                "127.0.0.1:1080".parse().unwrap(),
                "127.0.0.2:1080".parse().unwrap(),
            ],
            1080,
            protocol,
        )
        .unwrap();
        assert_eq!(snapshot.addresses().len(), 2);
        assert_eq!(
            snapshot.addresses()[0],
            "127.0.0.2:1080".parse::<SocketAddr>().unwrap()
        );
    }
}

async fn socks_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut greeting = [0; 3];
    stream.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 1, 0]);
    // Deliberately fragment method selection too.
    for byte in [5, 0] {
        stream.write_all(&[byte]).await.unwrap();
        tokio::task::yield_now().await;
    }
    let mut header = [0; 4];
    stream.read_exact(&mut header).await.unwrap();
    assert_eq!(&header[..3], &[5, 1, 0]);
    let mut request = header.to_vec();
    let len = match header[3] {
        1 => 6,
        4 => 18,
        other => panic!("non-numeric ATYP {other}"),
    };
    request.resize(4 + len, 0);
    stream.read_exact(&mut request[4..]).await.unwrap();
    request
}

async fn socks_fixture(reply: Vec<u8>) -> (ProxyConfig, tokio::task::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = socks_config(listener.local_addr().unwrap().port());
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = socks_request(&mut stream).await;
        for byte in reply {
            if stream.write_all(&[byte]).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
        request
    });
    (config, handle)
}

#[tokio::test]
async fn socks_fragmented_numeric_ipv4_ipv6_and_bounded_reply_preserve_payload() {
    let mut domain_reply = vec![5, 0, 0, 3, 255];
    // Not a resolvable domain: this field is discarded, not resolved.
    domain_reply.extend_from_slice(&[0xff; 255]);
    domain_reply.extend_from_slice(&[0, 80]);
    let mut ipv6_reply = vec![5, 0, 0, 4];
    ipv6_reply.extend_from_slice(&[0; 18]);
    for destination in [target(), "[2001:db8::1]:8443".parse().unwrap()] {
        for mut reply in [
            vec![5, 0, 0, 1, 127, 0, 0, 1, 0, 80],
            ipv6_reply.clone(),
            domain_reply.clone(),
        ] {
            reply.extend_from_slice(b"hello");
            let (config, server) = socks_fixture(reply).await;
            let resolved = resolve_proxy(
                &config,
                None,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            let mut tunnel = connect_resolved_proxy_tunnel(
                &resolved,
                destination,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            let mut payload = [0; 5];
            tunnel.stream.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"hello");
            let mut expected = vec![5, 1, 0];
            match destination.ip() {
                IpAddr::V4(ip) => {
                    expected.push(1);
                    expected.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    expected.push(4);
                    expected.extend_from_slice(&ip.octets());
                }
            }
            expected.extend_from_slice(&destination.port().to_be_bytes());
            assert_eq!(server.await.unwrap(), expected);
        }
    }
}

#[tokio::test]
async fn socks_refuses_auth_and_never_sends_credentials_or_connect() {
    for selection in [vec![5, 2], vec![5, 1], vec![5, 0xff], vec![4, 0], vec![5]] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = socks_config(listener.local_addr().unwrap().port());
        let expected_protocol = selection.len() != 2 || selection[0] != 5;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&selection).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut rest = Vec::new();
            stream.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        });
        let error = connect(&config).await.unwrap_err();
        if expected_protocol {
            assert!(matches!(error, ProxyError::Socks5Protocol));
        } else {
            assert!(matches!(error, ProxyError::Socks5AuthenticationRequired));
        }
        assert!(error.to_string().contains("SOCKS5"));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn socks_rejects_malformed_truncated_and_failure_replies() {
    for reply in [
        vec![],
        vec![5],
        vec![4, 0, 0, 1],
        vec![5, 0, 1, 1],
        vec![5, 0, 0, 0],
        vec![5, 0, 0, 2],
        vec![5, 0, 0, 3, 0],
        vec![5, 0, 0, 3, 2, b'a'],
        vec![5, 0, 0, 1, 127, 0, 0, 1, 0],
        vec![5, 0, 0, 4, 0, 0],
        vec![5, 9, 0, 1, 0, 0, 0, 0, 0, 0],
    ] {
        let (config, server) = socks_fixture(reply).await;
        assert!(matches!(
            connect(&config).await,
            Err(ProxyError::Socks5Protocol)
        ));
        server.await.unwrap();
    }
    for rep in 1..=8 {
        let (config, server) = socks_fixture(vec![5, rep, 0, 1, 0, 0, 0, 0, 0, 0]).await;
        assert!(
            matches!(connect(&config).await, Err(ProxyError::Socks5Rejected(code)) if code == rep)
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn socks_cancel_and_timeout_close_sockets_at_every_handshake_stage() {
    // Waiting for method, reply header, domain length, or domain address / port.
    for partial_reply in [
        None,
        Some(vec![]),
        Some(vec![5, 0, 0, 3]),
        Some(vec![5, 0, 0, 3, 255, b'a']),
    ] {
        for cancel_operation in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = socks_config(listener.local_addr().unwrap().port());
            let token = CancellationToken::new();
            let task_token = token.clone();
            let client = tokio::spawn(async move {
                connect_proxy_tunnel(
                    &config,
                    target(),
                    &task_token,
                    if cancel_operation {
                        Duration::from_secs(2)
                    } else {
                        Duration::from_millis(50)
                    },
                    None,
                )
                .await
            });
            let (mut stream, _) = listener.accept().await.unwrap();
            if let Some(reply) = partial_reply.clone() {
                socks_request(&mut stream).await;
                stream.write_all(&reply).await.unwrap();
            } else {
                let mut greeting = [0; 3];
                stream.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1, 0]);
            }
            if cancel_operation {
                token.cancel();
            }
            let result = tokio::time::timeout(Duration::from_secs(1), client)
                .await
                .unwrap()
                .unwrap();
            if cancel_operation {
                assert!(matches!(result, Err(ProxyError::Cancelled)));
            } else {
                assert!(matches!(result, Err(ProxyError::Timeout)));
            }
            assert_socket_closed(&mut stream).await;
        }
    }
}

async fn assert_socket_closed(stream: &mut TcpStream) {
    let mut byte = [0];
    let result = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte))
        .await
        .unwrap();
    assert!(
        matches!(result, Ok(0))
            || matches!(result, Err(ref error) if error.kind() == std::io::ErrorKind::ConnectionReset),
        "socket was not closed: {result:?}"
    );
}

#[tokio::test]
async fn socks_overall_deadline_does_not_reset_between_phases() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = socks_config(listener.local_addr().unwrap().port());
    let token = CancellationToken::new();
    let client = connect_proxy_tunnel(&config, target(), &token, Duration::from_millis(500), None);
    let server = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        // Each phase is individually under 500 ms, together they exceed it.
        tokio::time::sleep(Duration::from_millis(300)).await;
        stream.write_all(&[5, 0]).await.unwrap();
        let mut request = [0; 10];
        stream.read_exact(&mut request).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_socket_closed(&mut stream).await;
    };
    let (result, ()) = tokio::join!(client, server);
    assert!(matches!(result, Err(ProxyError::Timeout)));
}

#[tokio::test]
async fn pre_cancelled_proxy_never_dials_or_submits_dns() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    for config in [
        config(listener.local_addr().unwrap().port()),
        socks_config(listener.local_addr().unwrap().port()),
    ] {
        let resolved = resolve_proxy(
            &config,
            None,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            connect_resolved_proxy_tunnel(&resolved, target(), &token, Duration::from_secs(1))
                .await,
            Err(ProxyError::Cancelled)
        ));
        assert!(matches!(
            connect_proxy_tunnel(&config, target(), &token, Duration::from_secs(1), None).await,
            Err(ProxyError::Cancelled)
        ));
        let dns_config = ProxyConfig {
            url: format!(
                "{}://must-not-resolve.invalid:1080",
                resolved.protocol().scheme()
            ),
        };
        assert!(matches!(
            resolve_proxy(&dns_config, None, &token, Duration::from_secs(1)).await,
            Err(ProxyError::Cancelled)
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn socks_ipv6_loopback_proxy_endpoint() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let config = ProxyConfig {
        url: format!("socks5://{}", listener.local_addr().unwrap()),
    };
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = socks_request(&mut stream).await;
        stream
            .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        request
    });
    let tunnel = connect(&config).await.unwrap();
    assert!(tunnel.proxy_addr.is_ipv6());
    assert_eq!(
        server.await.unwrap(),
        vec![5, 1, 0, 1, 192, 0, 2, 1, 1, 187]
    );
}

#[tokio::test]
async fn proxy_failure_never_retries_other_proxy_ips_or_direct_target() {
    for (protocol, direct_bind) in [
        (ProxyProtocol::Http, "127.0.0.1:0"),
        (ProxyProtocol::Http, "[::1]:0"),
        (ProxyProtocol::Socks5, "127.0.0.1:0"),
        (ProxyProtocol::Socks5, "[::1]:0"),
    ] {
        let fallback = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let direct = TcpListener::bind(direct_bind).await.unwrap();
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_addr = closed.local_addr().unwrap();
        drop(closed);
        let resolved = ResolvedProxy {
            protocol,
            addresses: vec![closed_addr, fallback.local_addr().unwrap()],
        };
        assert!(matches!(
            connect_resolved_proxy_tunnel(
                &resolved,
                direct.local_addr().unwrap(),
                &CancellationToken::new(),
                Duration::from_secs(1)
            )
            .await,
            Err(ProxyError::Connect)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), fallback.accept())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), direct.accept())
                .await
                .is_err()
        );
    }
    // Negotiation rejection also must not open a target socket.
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (config, server) = socks_fixture(vec![5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await;
    assert!(matches!(
        connect_proxy_tunnel(
            &config,
            direct.local_addr().unwrap(),
            &CancellationToken::new(),
            Duration::from_secs(1),
            None
        )
        .await,
        Err(ProxyError::Socks5Rejected(5))
    ));
    server.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), direct.accept())
            .await
            .is_err()
    );
}

#[test]
fn socks_permit_routes_keep_exact_protocol_and_protected_domain_checks() {
    use crate::{
        model::{Permit, ProxyPermit},
        policy,
    };
    use std::collections::HashSet;
    let mut permit: Permit = serde_json::from_value(serde_json::json!({
        "id":"owned", "name":"Owned", "url":"http://10.0.0.1/",
        "modes":["tcp"], "allowed_ips":["10.0.0.1/32"], "max_concurrency":1,
        "max_rps":1, "max_duration":10, "authorization":"owner",
        "expires_at":"2099-01-01T00:00:00Z", "proxy_routes":[]
    }))
    .unwrap();
    permit.proxy_routes.push(ProxyPermit {
        url: "socks5://PROXY.invalid:1080/".into(),
        allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
        authorization: "proxy owner".into(),
    });
    policy::validate_permits(&[permit.clone()], &HashSet::new(), None).unwrap();
    let config = ProxyConfig {
        url: "socks5://proxy.invalid:1080".into(),
    };
    assert!(policy::proxy_route(&permit, &config, None).is_ok());
    assert!(
        policy::proxy_route(
            &permit,
            &ProxyConfig {
                url: "http://proxy.invalid:1080".into()
            },
            None
        )
        .is_err()
    );
    assert!(policy::validate_permits(&[permit.clone()], &HashSet::new(), Some(1080)).is_err());
    permit.proxy_routes[0].url = "socks5://WWW.PROTECTED.invalid:1080".into();
    assert!(
        policy::validate_permits(
            &[permit.clone()],
            &HashSet::from(["protected.invalid".into()]),
            None
        )
        .is_err()
    );
    // A SOCKS URL is still not a target HTTP/HTTPS URL.
    assert!(policy::parse_url(&config.url).is_err());
    let resolved = validate_answers(
        [
            "127.0.0.1:1080".parse().unwrap(),
            "10.0.0.1:1080".parse().unwrap(),
        ],
        1080,
        ProxyProtocol::Socks5,
    )
    .unwrap();
    assert!(policy::validate_proxy_route_addresses(&permit.proxy_routes[0], &resolved).is_err());
}
