use super::*;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn config(protocol: &str, addr: SocketAddr) -> crate::proxy::ResolvedProxy {
    proxy::resolve_proxy_health(
        &proxy::ProxyConfig {
            url: format!("{protocol}://{addr}"),
        },
        None,
        &CancellationToken::new(),
        Duration::from_secs(1),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn http_health_sends_zero_application_bytes() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 1];
        let read = tokio::time::timeout(Duration::from_millis(150), stream.read(&mut buf)).await;
        assert!(matches!(read, Ok(Ok(0))));
    });
    let (seen, _) = proxy::check_resolved_proxy_health(
        &config("http", addr).await,
        &CancellationToken::new(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(seen, addr);
    server.await.unwrap();
}

#[tokio::test]
async fn socks_health_sends_only_noauth_greeting() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stream.write_all(&[5, 0]).await.unwrap();
        let mut extra = [0; 1];
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(150), stream.read(&mut extra)).await,
            Ok(Ok(0))
        ));
    });
    proxy::check_resolved_proxy_health(
        &config("socks5", addr).await,
        &CancellationToken::new(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn socks_health_rejects_auth_and_bad_version() {
    for response in [[5, 2], [4, 0]] {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&response).await.unwrap();
            let mut rest = Vec::new();
            stream.read_to_end(&mut rest).await.unwrap();
            assert!(rest.is_empty());
        });
        let error = proxy::check_resolved_proxy_health(
            &config("socks5", addr).await,
            &CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ProxyError::Socks5AuthenticationRequired | ProxyError::Socks5Protocol
        ));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_closes_probe() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        let _ = accepted_tx.send(());
        let mut byte = [0; 1];
        assert!(matches!(stream.read(&mut byte).await, Ok(0)));
    });
    let cancel = CancellationToken::new();
    let probe_cancel = cancel.clone();
    let probe = tokio::spawn(async move {
        proxy::check_resolved_proxy_health(
            &config("socks5", addr).await,
            &probe_cancel,
            Duration::from_secs(3),
        )
        .await
    });
    accepted_rx.await.unwrap();
    cancel.cancel();
    assert!(matches!(probe.await.unwrap(), Err(ProxyError::Cancelled)));
    server.await.unwrap();
}

#[tokio::test]
async fn cancelled_health_resolution_never_starts_lookup() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = proxy::resolve_proxy_health(
        &crate::proxy::ProxyConfig {
            url: "http://localhost:9".into(),
        },
        None,
        &cancel,
        Duration::from_millis(1),
    )
    .await;
    assert!(matches!(result, Err(ProxyError::Cancelled)));
}

#[tokio::test]
async fn settings_change_with_pause_checks_immediately_but_busy_still_defers() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let current = Arc::new(Mutex::new((RouteSnapshot::default(), false)));
    let context = current.clone();
    let provider: ContextProvider = Arc::new(move || {
        let value = context.lock().unwrap().clone();
        Box::pin(async move { value })
    });
    let mut manager = Manager::new(None, CancellationToken::new());
    manager.tick = Duration::from_millis(5);
    let manager = Arc::new(manager);
    manager.start(provider).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while manager.snapshot().status != Status::Disabled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // No await between pause and route publication: the loop sees both together.
    manager.pause();
    *current.lock().unwrap() = (
        RouteSnapshot {
            settings_revision: 1,
            profile_id: Some("health-test".into()),
            proxy: Some(proxy::ProxyConfig {
                url: format!("http://{addr}"),
            }),
            ..RouteSnapshot::default()
        },
        false,
    );
    let (mut stream, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .expect("changed route must not wait the 30-second period")
        .unwrap();
    let mut byte = [0; 1];
    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    tokio::time::timeout(Duration::from_secs(1), async {
        while manager.snapshot().status != Status::Reachable {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    manager.pause();
    {
        let mut value = current.lock().unwrap();
        value.0.settings_revision = 2;
        value.1 = true;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(manager.snapshot().status, Status::Paused);
    assert!(manager.snapshot().checked_at.is_none());
    manager.shutdown().await;
}

#[tokio::test]
async fn probes_follow_period_without_overlapping() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let current = Arc::new(Mutex::new((
        RouteSnapshot {
            settings_revision: 1,
            profile_id: Some("period-test".into()),
            proxy: Some(proxy::ProxyConfig {
                url: format!("http://{addr}"),
            }),
            ..RouteSnapshot::default()
        },
        false,
    )));
    let context = current.clone();
    let provider: ContextProvider = Arc::new(move || {
        let value = context.lock().unwrap().clone();
        Box::pin(async move { value })
    });
    let mut manager = Manager::new(None, CancellationToken::new());
    manager.tick = Duration::from_millis(2);
    manager.period = Duration::from_millis(80);
    let manager = Arc::new(manager);
    manager.start(provider).await;
    let started = Instant::now();
    let (mut first, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut byte = [0; 1];
    assert_eq!(first.read(&mut byte).await.unwrap(), 0);
    tokio::time::timeout(Duration::from_secs(1), async {
        while manager.snapshot().checked_at.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (mut second, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(60));
    assert_eq!(second.read(&mut byte).await.unwrap(), 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn reads_are_inert_and_unstarted_manager_is_unknown() {
    let manager = Manager::new(None, CancellationToken::new());
    let first = manager.snapshot();
    let second = manager.snapshot();
    assert_eq!(first.status, Status::Unknown);
    assert_eq!(first.status, second.status);
    assert_eq!(first.route.proxy, None);
}
