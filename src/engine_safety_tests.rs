use super::*;
use axum::{Router, body::Body, http::Response, routing::get};
use std::sync::atomic::AtomicUsize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(mode: &str) -> RunConfig {
    RunConfig {
        mode: mode.into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: vec![],
        concurrency: 2,
        rps: 10,
        duration_secs: 1,
        timeout_secs: 2,
        ramp_secs: 0,
        fresh_connections: false,
        proxy: None,
    }
}
fn target(url: String, permit: Option<Permit>) -> WorkTarget {
    let u = policy::parse_url(&url).unwrap();
    WorkTarget {
        target: Target {
            id: "fixture".into(),
            name: "fixture".into(),
            url,
            region: String::new(),
            source: String::new(),
            checked_at: String::new(),
            probe_only: true,
        },
        permit,
        domain: policy::domain_key(&u),
        client: Some(reqwest::Client::builder().no_proxy().build().unwrap()),
        addr: None,
        proxy: None,
    }
}
fn backoff(dir: &std::path::Path, probe: bool) -> Arc<Backoff> {
    Arc::new(Backoff {
        stopped: Mutex::new(HashSet::new()),
        error: Mutex::new(None),
        cooldowns: Arc::new(Mutex::new(Cooldowns::default())),
        path: dir.join("cooldowns.json"),
        probe,
    })
}

#[tokio::test]
async fn headers_stop_sibling_dispatch_before_slow_body_or_cancellation() {
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/",
                get(move || {
                    seen.fetch_add(1, Ordering::SeqCst);
                    async {
                        let body = Body::from_stream(futures_util::stream::pending::<
                            Result<&'static str, std::io::Error>,
                        >());
                        Response::builder()
                            .status(429)
                            .header("retry-after", "3600")
                            .body(body)
                            .unwrap()
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let c = config("http");
    let t = target(url, None);
    let metrics = Arc::new(Mutex::new(Metrics::new(
        c.clone(),
        std::slice::from_ref(&t.target),
    )));
    let cancel = CancellationToken::new();
    let worker = tokio::spawn(execute(
        c,
        vec![t],
        metrics.clone(),
        cancel,
        (
            Arc::new(Mutex::new(Cooldowns::default())),
            dir.path().join("cooldowns.json"),
        ),
        None,
        None,
    ));
    worker.await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(metrics.lock().unwrap().run.cancelled, 1);
    server.abort();
}

#[tokio::test]
async fn probe_backoff_is_durable_before_outcome_is_joined_or_drained() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(socket.read(&mut request).await.unwrap() > 0);
        socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let state = backoff(dir.path(), true);
    let t = target(url, None);
    let state_worker = state.clone();
    let worker = tokio::spawn(http_work(
        t,
        config("probe"),
        None,
        Arc::new(AtomicU64::new(0)),
        state_worker,
        CancellationToken::new(),
    ));
    // Inspect persistence before joining the finished worker: the scheduler's
    // choice between normal collection and draining cannot affect this write.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state.stopped("127.0.0.1") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let outcome = worker.await.unwrap();
    assert_eq!(outcome.retry_after_secs, Some(3600));
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state.path).unwrap()).unwrap();
    assert!(saved["domains"]["127.0.0.1"].as_u64().unwrap() > Cooldowns::now() + 3_500_000);
    server.await.unwrap();
}

#[tokio::test]
async fn post_preparation_expiry_rejects_network_and_archives_terminal_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let permit = Permit {
        id: "fixture".into(),
        name: "fixture".into(),
        url: url.clone(),
        modes: vec!["http".into()],
        allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
        max_concurrency: 2,
        max_rps: 10,
        max_duration: 5,
        authorization: "owned fixture".into(),
        expires_at: (chrono::Utc::now() + chrono::Duration::seconds(3)).to_rfc3339(),
        proxy_routes: vec![],
    };
    let t = target(url, Some(permit));
    authorization_window(std::slice::from_ref(&t), Duration::from_secs(1)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let history = crate::history::History::open(dir.path()).unwrap();
    let c = config("http");
    let metrics = Arc::new(Mutex::new(Metrics::new(
        c.clone(),
        std::slice::from_ref(&t.target),
    )));
    history.begin(&metrics.lock().unwrap().run).unwrap();
    // Model time spent in DNS or durable marker preparation after initial validation.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    execute(
        c,
        vec![t],
        metrics.clone(),
        CancellationToken::new(),
        (
            Arc::new(Mutex::new(Cooldowns::default())),
            dir.path().join("cooldowns.json"),
        ),
        None,
        Some(history.clone()),
    )
    .await;
    let run = metrics.lock().unwrap().run.clone();
    assert_eq!(run.status, "failed");
    assert_eq!(run.attempted, 0);
    assert!(run.reason.contains("许可"));
    assert_eq!(history.get(&run.id).unwrap().status, "failed");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn tcp_deadline_closes_held_socket_without_external_cancellation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
    });
    let mut t = target(format!("http://{addr}/"), None);
    t.addr = Some(addr);
    let metrics = Arc::new(Mutex::new(Metrics::new(
        config("tcp"),
        std::slice::from_ref(&t.target),
    )));
    metrics.lock().unwrap().start(0);
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        tcp_work(
            t,
            metrics.clone(),
            0,
            CancellationToken::new(),
            Duration::from_secs(2),
            Instant::now() + Duration::from_millis(50),
        ),
    )
    .await
    .unwrap();
    assert!(outcome.tcp_success);
    assert_eq!(metrics.lock().unwrap().run.connected, 0);
    server.await.unwrap();
}
