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
            9000
        )
        .is_err()
    );
    assert!(validate_answers(["[::ffff:169.254.169.254]:9000".parse().unwrap()], 9000).is_err());
    assert!(validate_answers(["[fd00:ec2::254]:9000".parse().unwrap()], 9000).is_err());
    assert!(validate_answers(["127.0.0.1:9000".parse().unwrap()], 9000).is_ok());
    assert!(
        serde_json::from_str::<ProxyConfig>(r#"{"url":"http://localhost:80","password":"x"}"#)
            .is_err()
    );
}
