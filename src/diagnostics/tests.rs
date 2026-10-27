use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn config(target: &str, port: u16) -> DiagnosticConfig {
    DiagnosticConfig {
        request_id: uuid::Uuid::new_v4().to_string(),
        target: target.into(),
        port,
        tls: false,
        allow_lan: true,
        proxy: None,
    }
}
async fn terminal(manager: &DiagnosticManager) -> DiagnosticSnapshot {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let s = manager.snapshot().await.unwrap();
            if !s.active() {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("diagnostic should finish")
}
#[tokio::test]
async fn reservation_idempotency_cooldowns_latest_and_export() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0; 1];
        assert_eq!(s.read(&mut b).await.unwrap(), 0);
    });
    let manager = DiagnosticManager::new(Some(9891), CancellationToken::new());
    let c = config("127.0.0.1", address.port());
    let first = manager.start(c.clone(), vec![address]).await.unwrap();
    assert!(first.active());
    assert_eq!(manager.start(c.clone(), vec![]).await.unwrap().id, first.id);
    let mut changed = c.clone();
    changed.tls = true;
    assert!(manager.existing_request(&changed).await.is_err());
    assert!(
        manager
            .stop(&uuid::Uuid::new_v4().to_string())
            .await
            .is_err()
    );
    let done = terminal(&manager).await;
    assert_eq!(done.status, "completed");
    assert_eq!(done.stages.route.status, "skipped");
    assert_eq!(done.stages.tcp.status, "success");
    service.await.unwrap();
    manager.settle().await.unwrap();
    assert_eq!(manager.start(c.clone(), vec![]).await.unwrap().id, first.id);
    assert!(
        manager
            .start(config("127.0.0.1", address.port()), vec![address])
            .await
            .unwrap_err()
            .contains("cooldown")
    );
    let exported = manager.export(&first.id).await.unwrap();
    assert_eq!(exported["partial"], false);
    assert!(manager.export("wrong").await.is_err());
    manager.stop(&first.id).await.unwrap();
    // Remove only the global cooldown to exercise the independent host bound.
    manager.state.lock().await.last_start = None;
    assert!(
        manager
            .start(config("127.0.0.1", address.port()), vec![address])
            .await
            .unwrap_err()
            .contains("host cooldown")
    );
    manager.shutdown_and_wait().await;
}

#[tokio::test]
async fn proxy_diagnostic_pins_target_and_observes_only_proxy_route() {
    use crate::proxy::ProxyConfig;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        assert!(
            String::from_utf8(request)
                .unwrap()
                .starts_with("CONNECT 8.8.8.8:443 HTTP/1.1\r\n")
        );
        stream
            .write_all(b"HTTP/1.1 200 Connected\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let mut manager = DiagnosticManager::new(None, CancellationToken::new());
    manager.resolver = Arc::new(|_, port| {
        Ok(vec![
            SocketAddr::new("8.8.8.8".parse().unwrap(), port),
            SocketAddr::new("1.1.1.1".parse().unwrap(), port),
        ])
    });
    let mut c = config("diagnostic.test", 443);
    c.proxy = Some(ProxyConfig {
        url: format!("http://{proxy_address}/"),
    });
    let first = manager.start(c.clone(), vec![]).await.unwrap();
    c.proxy.as_mut().unwrap().url.pop();
    assert_eq!(
        manager.existing_request(&c).await.unwrap().unwrap().id,
        first.id
    );
    let snapshot = terminal(&manager).await;
    assert_eq!(snapshot.status, "completed");
    assert_eq!(snapshot.selected_ip.as_deref(), Some("8.8.8.8"));
    assert_eq!(snapshot.stages.route.status, "skipped");
    assert_eq!(snapshot.stages.route.observations["subject"], "proxy");
    assert_eq!(
        snapshot.stages.route.observations["target_route_observed"],
        false
    );
    assert_eq!(
        snapshot.stages.tcp.observations["proxy_tcp"]["status"],
        "success"
    );
    assert_eq!(
        snapshot.stages.tcp.observations["connect"]["status"],
        "success"
    );
    assert_eq!(
        snapshot.stages.tcp.observations["target_tcp_independently_observed"],
        false
    );
    server.await.unwrap();
}

#[tokio::test]
async fn proxy_target_policy_rejects_loopback_even_if_owned_before_proxy_contact() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut manager = DiagnosticManager::new(None, CancellationToken::new());
    manager.resolver = Arc::new(move |_, _| Ok(vec![addr]));
    let mut c = config("owned.test", addr.port());
    c.proxy = Some(crate::proxy::ProxyConfig {
        url: format!("http://{addr}"),
    });
    manager.start(c, vec![addr]).await.unwrap();
    let snapshot = terminal(&manager).await;
    assert_eq!(snapshot.status, "failed");
    assert_eq!(snapshot.stages.dns.status, "blocked");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dns_timeout_remains_draining_until_real_thread_completion() {
    let shutdown = CancellationToken::new();
    let mut manager = DiagnosticManager::new(None, shutdown);
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new(Mutex::new(release_rx));
    manager.resolver = Arc::new(move |_, port| {
        release.lock().unwrap().recv().unwrap();
        Ok(vec![SocketAddr::new("8.8.8.8".parse().unwrap(), port)])
    });
    manager.dns_budget = Duration::from_millis(10);
    let c = config("pending.test", 443);
    let first = manager.start(c.clone(), vec![]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(manager.snapshot().await.unwrap().status, "draining");
    assert!(manager.settle().await.is_err());
    assert!(
        manager
            .start(config("other.test", 443), vec![])
            .await
            .is_err()
    );
    assert_eq!(manager.start(c, vec![]).await.unwrap().id, first.id);
    assert_eq!(manager.stop(&first.id).await.unwrap().status, "draining");
    release_tx.send(()).unwrap();
    let s = terminal(&manager).await;
    assert_eq!(s.status, "cancelled");
    assert_eq!(s.stages.dns.observations["outstanding"], false);
    assert_eq!(s.stages.tcp.status, "cancelled");
    assert!(s.selected_ip.is_none());
    manager.settle().await.unwrap();
}

#[tokio::test]
async fn cancelled_dns_drains_late_answers_without_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut manager = DiagnosticManager::new(None, CancellationToken::new());
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = Arc::new(Mutex::new(rx));
    manager.resolver = Arc::new(move |_, _| {
        rx.lock().unwrap().recv().unwrap();
        Ok(vec![address])
    });
    let first = manager
        .start(config("pending.test", address.port()), vec![address])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    manager.stop(&first.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(manager.snapshot().await.unwrap().status, "draining");
    tx.send(()).unwrap();
    assert_eq!(terminal(&manager).await.status, "cancelled");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dns_all_answer_policy_blocks_before_any_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut manager = DiagnosticManager::new(None, CancellationToken::new());
    manager.resolver = Arc::new(move |_, port| {
        Ok(vec![
            address,
            SocketAddr::new("169.254.169.254".parse().unwrap(), port),
        ])
    });
    manager
        .start(config("mixed.test", address.port()), vec![address])
        .await
        .unwrap();
    let result = terminal(&manager).await;
    assert_eq!(result.status, "failed");
    assert_eq!(result.stages.dns.status, "blocked");
    assert_eq!(result.stages.tcp.status, "skipped");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn shutdown_dns_exception_is_bounded_and_truthful() {
    let mut manager = DiagnosticManager::new(None, CancellationToken::new());
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rx = Arc::new(Mutex::new(rx));
    manager.resolver = Arc::new(move |_, _| {
        rx.lock().unwrap().recv().unwrap();
        Err("late".into())
    });
    manager
        .start(config("pending.test", 443), vec![])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let started = Instant::now();
    manager.shutdown_and_wait().await;
    assert!(started.elapsed() < Duration::from_secs(3));
    let snapshot = manager.snapshot().await.unwrap();
    assert_eq!(snapshot.status, "cancelled");
    assert_eq!(snapshot.stages.dns.observations["outstanding"], true);
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|w| w.contains("not cancelled"))
    );
    tx.send(()).unwrap();
}

#[tokio::test]
async fn proxy_connect_evidence_route_subject_and_idempotency() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let service = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        let mut byte = [0; 1];
        while !header.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
        }
        assert_eq!(
            String::from_utf8(header).unwrap(),
            "CONNECT 10.23.0.1:443 HTTP/1.1\r\nHost: 10.23.0.1:443\r\n\r\n"
        );
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let manager = DiagnosticManager::new(None, CancellationToken::new());
    let mut c = config("10.23.0.1", 443);
    c.proxy = Some(crate::proxy::ProxyConfig {
        url: format!("http://{proxy_addr}/"),
    });
    let first = manager.start(c.clone(), vec![]).await.unwrap();
    c.proxy.as_mut().unwrap().url.pop();
    assert_eq!(
        manager.existing_request(&c).await.unwrap().unwrap().id,
        first.id
    );
    let mut changed = c.clone();
    changed.proxy = None;
    assert!(manager.existing_request(&changed).await.is_err());
    let done = terminal(&manager).await;
    service.await.unwrap();
    assert_eq!(done.selected_ip.as_deref(), Some("10.23.0.1"));
    assert_eq!(done.stages.route.status, "skipped");
    assert_eq!(done.stages.route.observations["subject"], "proxy");
    assert_eq!(
        done.stages.route.observations["target_route_observed"],
        false
    );
    assert_eq!(
        done.stages.tcp.observations["proxy_tcp"]["status"],
        "success"
    );
    assert_eq!(done.stages.tcp.observations["connect"]["status"], "success");
    assert_eq!(
        done.stages.tcp.observations["target_tcp_independently_observed"],
        false
    );
    assert_eq!(done.stages.tls.status, "skipped");
}

#[tokio::test]
async fn proxy_loopback_target_and_mixed_dns_block_before_proxy_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let proxy = crate::proxy::ProxyConfig {
        url: format!("http://{address}"),
    };
    let manager = DiagnosticManager::new(None, CancellationToken::new());
    let mut c = config("127.0.0.1", 443);
    c.proxy = Some(proxy.clone());
    assert!(
        manager
            .start(c, vec!["127.0.0.1:443".parse().unwrap()])
            .await
            .is_err()
    );
    let mut manager = DiagnosticManager::new(None, CancellationToken::new());
    manager.resolver = Arc::new(|_, port| {
        Ok(vec![
            SocketAddr::new("10.23.0.1".parse().unwrap(), port),
            SocketAddr::new("127.0.0.1".parse().unwrap(), port),
        ])
    });
    let mut c = config("mixed.test", 443);
    c.proxy = Some(proxy);
    manager
        .start(c, vec!["127.0.0.1:443".parse().unwrap()])
        .await
        .unwrap();
    let done = terminal(&manager).await;
    assert_eq!(done.stages.dns.status, "blocked");
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn proxy_connect_cancel_closes_owned_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let manager = DiagnosticManager::new(None, CancellationToken::new());
    let mut c = config("10.23.0.1", 443);
    c.proxy = Some(crate::proxy::ProxyConfig {
        url: format!("http://{address}"),
    });
    let first = manager.start(c, vec![]).await.unwrap();
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut header = Vec::new();
    let mut byte = [0; 1];
    while !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        header.push(byte[0]);
    }
    manager.stop(&first.id).await.unwrap();
    let done = terminal(&manager).await;
    assert_eq!(done.status, "cancelled");
    assert_eq!(done.stages.tcp.status, "cancelled");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
