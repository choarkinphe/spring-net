use super::*;
use crate::{model::ProxyPermit, proxy::ProxyConfig};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[tokio::test]
async fn total_preparation_timeout_and_shutdown_cancel_drop_pending_work() {
    let cancel = CancellationToken::new();
    let began = Instant::now();
    let error = bounded_preparation(&cancel, std::future::pending::<Result<(), String>>())
        .await
        .unwrap_err();
    assert!(error.contains("总耗时"));
    assert!(began.elapsed() < Duration::from_secs(6));
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move {
        bounded_preparation(&worker_cancel, std::future::pending::<Result<(), String>>()).await
    });
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_millis(100), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.contains("取消"));
}

#[tokio::test]
async fn proxy_probe_start_does_not_eagerly_resolve_large_target_sets() {
    let (_listener, proxy) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
    let targets = (0..1000)
        .map(|index| Target {
            id: format!("target-{index}"),
            name: "invalid fixture (no dispatch)".into(),
            url: format!("http://target-{index}.example.invalid/"),
            region: String::new(),
            source: String::new(),
            checked_at: String::new(),
            probe_only: true,
        })
        .collect();
    let lists = vec![SiteList {
        id: "fixture".into(),
        name: "fixture".into(),
        description: String::new(),
        builtin: false,
        targets,
    }];
    // Reserve the shared domain before start so no real target DNS is issued.
    engine
        .cooldowns
        .lock()
        .unwrap()
        .reserve(
            "example.invalid",
            Cooldowns::now() + 30000,
            &engine.cooldown_path,
        )
        .unwrap();
    let mut c = config("probe", Some(proxy));
    c.list_ids = vec!["fixture".into()];
    c.authorized_ids.clear();
    // On this current-thread runtime execute cannot run before the immediate stop.
    let run = tokio::time::timeout(Duration::from_millis(250), engine.start(c, &lists, &[]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.targets.len(), 1000);
    assert_eq!(run.attempted, 0);
    engine.stop().await;
    engine.shutdown_and_wait().await;
}

fn config(mode: &str, proxy: Option<ProxyConfig>) -> RunConfig {
    RunConfig {
        mode: mode.into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: vec!["owned".into()],
        concurrency: 1,
        rps: 1,
        duration_secs: 1,
        timeout_secs: 1,
        ramp_secs: 0,
        fresh_connections: false,
        proxy,
    }
}
fn permit(url: &str, proxy: &ProxyConfig) -> Permit {
    Permit {
        id: "owned".into(),
        name: "Owned target".into(),
        url: url.into(),
        modes: vec!["http".into(), "tcp".into()],
        allowed_ips: vec!["10.0.0.1/32".parse().unwrap()],
        max_concurrency: 2,
        max_rps: 2,
        max_duration: 10,
        authorization: "Target owner fixture consent".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        proxy_routes: vec![ProxyPermit {
            url: proxy.url.clone(),
            allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
            authorization: "Proxy owner fixture consent".into(),
        }],
    }
}
async fn listener() -> (TcpListener, ProxyConfig) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = ProxyConfig {
        url: format!("http://{}", listener.local_addr().unwrap()),
    };
    (listener, proxy)
}
async fn headers(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16384);
    }
    String::from_utf8(bytes).unwrap()
}
async fn terminal(engine: &Engine) -> Run {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let run = engine.snapshot().await.unwrap();
            if !run.active() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
fn work_target(proxy: Arc<ResolvedProxy>, addr: &str) -> WorkTarget {
    let u = policy::parse_url("http://original.invalid/resource").unwrap();
    WorkTarget {
        target: Target {
            id: "fixture".into(),
            name: "fixture".into(),
            url: u.to_string(),
            region: String::new(),
            source: String::new(),
            checked_at: String::new(),
            probe_only: true,
        },
        permit: None,
        domain: policy::domain_key(&u),
        client: None,
        addr: Some(addr.parse().unwrap()),
        proxy: Some(proxy),
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

#[test]
fn old_configs_and_permits_are_direct_only_and_strict() {
    let c = config("http", None);
    let value = serde_json::to_value(&c).unwrap();
    assert!(value.get("proxy").is_none());
    assert!(
        serde_json::from_value::<RunConfig>(value)
            .unwrap()
            .proxy
            .is_none()
    );
    let proxy = ProxyConfig {
        url: "http://127.0.0.1:9000".into(),
    };
    let mut p = permit("http://10.0.0.1/", &proxy);
    p.proxy_routes.clear();
    let mut value = serde_json::to_value(&p).unwrap();
    assert!(value.get("proxy_routes").is_none());
    assert!(
        serde_json::from_value::<Permit>(value.clone())
            .unwrap()
            .proxy_routes
            .is_empty()
    );
    assert!(policy::proxy_route(&p, &proxy, None).is_err());
    value["proxy_routes"] = serde_json::json!([{"url":"http://127.0.0.1:9000", "allowed_ips":["127.0.0.1/32"], "authorization":"owner", "unknown":true}]);
    assert!(serde_json::from_value::<Permit>(value).is_err());
}

#[test]
fn route_validation_rejects_duplicates_unbounded_cidrs_and_protected_hosts() {
    let proxy = ProxyConfig {
        url: "http://LOCALHOST:9000/".into(),
    };
    let mut p = permit("http://10.0.0.1/", &proxy);
    policy::validate_permits(&[p.clone()], &HashSet::new(), None).unwrap();
    let normalized = ProxyConfig {
        url: "http://localhost:9000".into(),
    };
    assert!(policy::proxy_route(&p, &normalized, None).is_ok());
    assert!(policy::validate_permits(&[p.clone()], &HashSet::new(), Some(9000)).is_err());
    p.proxy_routes.push(ProxyPermit {
        url: normalized.url,
        ..p.proxy_routes[0].clone()
    });
    assert!(policy::validate_permits(&[p.clone()], &HashSet::new(), None).is_err());
    p.proxy_routes.pop();
    p.proxy_routes[0].allowed_ips = vec!["0.0.0.0/0".parse().unwrap()];
    assert!(policy::validate_permits(&[p.clone()], &HashSet::new(), None).is_err());
    p.proxy_routes[0].allowed_ips = vec!["127.0.0.1/32".parse().unwrap()];
    p.proxy_routes[0].url = "http://www.protected.invalid:8080".into();
    assert!(
        policy::validate_permits(&[p], &HashSet::from(["protected.invalid".into()]), None).is_err()
    );
}

#[tokio::test]
async fn load_requires_exact_route_and_proxy_ip_without_opening_sockets() {
    let (listener, proxy) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
    let mut p = permit("http://10.0.0.1/", &proxy);
    p.proxy_routes.clear();
    assert!(
        engine
            .start(config("http", Some(proxy.clone())), &[], &[p.clone()])
            .await
            .unwrap_err()
            .contains("未授权")
    );
    p.proxy_routes = permit("http://10.0.0.1/", &proxy).proxy_routes;
    p.proxy_routes[0].allowed_ips = vec!["10.1.0.1/32".parse().unwrap()];
    assert!(
        engine
            .start(config("http", Some(proxy)), &[], &[p])
            .await
            .unwrap_err()
            .contains("代理解析地址")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn proxy_target_rejects_demo_metadata_and_special_even_with_cidr() {
    let proxy = ProxyConfig {
        url: "http://127.0.0.1:9000".into(),
    };
    for ip in [
        "127.0.0.1",
        "169.254.169.254",
        "168.63.129.16",
        "0.0.0.0",
        "[::1]",
        "[::ffff:127.0.0.1]",
        "[fd00:ec2::254]",
    ] {
        let url = policy::parse_url(&format!("http://{ip}:8080/")).unwrap();
        let mut p = permit(url.as_str(), &proxy);
        p.allowed_ips = vec!["0.0.0.0/0".parse().unwrap(), "::/0".parse().unwrap()];
        assert!(
            policy::resolve_proxy_target(&url, Some(&p), None)
                .await
                .is_err(),
            "{ip}"
        );
    }
}

#[tokio::test]
async fn engine_http_uses_numeric_connect_original_host_and_bounded_body() {
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(
            headers(&mut stream).await,
            "CONNECT 10.0.0.1:80 HTTP/1.1\r\nHost: 10.0.0.1:80\r\n\r\n"
        );
        stream
            .write_all(b"HTTP/1.1 200 Established\r\n\r\n")
            .await
            .unwrap();
        let request = headers(&mut stream).await.to_ascii_lowercase();
        assert!(request.starts_with("get /resource http/1.1\r\n"));
        assert!(request.contains("\r\nhost: 10.0.0.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 70000\r\n\r\n")
            .await
            .unwrap();
        let _ = stream.write_all(&vec![b'x'; 70000]).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
    let p = permit("http://10.0.0.1/resource", &proxy);
    engine
        .start(config("http", Some(proxy)), &[], &[p])
        .await
        .unwrap();
    let run = terminal(&engine).await;
    assert_eq!(run.responses, 1);
    assert_eq!(run.bytes, 65536);
    assert_eq!(run.targets[0].truncated, 1);
    assert_eq!(run.errors, 0);
    server.await.unwrap();
    engine.shutdown_and_wait().await;
}

#[tokio::test]
async fn proxy_headers_backoff_persists_before_slow_body_cancel_and_probe_is_head() {
    for mode in ["http", "probe"] {
        let (listener, proxy) = listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            headers(&mut stream).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            let request = headers(&mut stream).await.to_ascii_lowercase();
            assert!(request.starts_with(if mode == "probe" {
                "head /resource "
            } else {
                "get /resource "
            }));
            assert!(request.contains("\r\nhost: original.invalid\r\n"));
            stream
                .write_all(b"HTTP/1.1 429 Slow\r\nRetry-After: 3600\r\nContent-Length: 100\r\n\r\n")
                .await
                .unwrap();
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        });
        let cancel = CancellationToken::new();
        let resolved = Arc::new(
            proxy::resolve_proxy(&proxy, None, &cancel, Duration::from_secs(1))
                .await
                .unwrap(),
        );
        let dir = tempfile::tempdir().unwrap();
        let state = backoff(dir.path(), true);
        let worker = tokio::spawn(http_work(
            work_target(resolved, "10.0.0.1:80"),
            config(mode, Some(proxy)),
            None,
            Arc::new(AtomicU64::new(0)),
            state.clone(),
            cancel.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(1), async {
            while !state.stopped("original.invalid") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            !Cooldowns::load(&state.path)
                .unwrap()
                .ready("original.invalid")
        );
        if mode == "http" {
            cancel.cancel();
        }
        let outcome = worker.await.unwrap();
        assert_eq!(outcome.status, Some(429));
        assert_eq!(outcome.retry_after_secs, Some(3600));
        assert_eq!(outcome.bytes, 0);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn missing_prepared_proxy_cannot_use_even_an_existing_direct_client() {
    let (listener, proxy) = listener().await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let target = WorkTarget {
        target: Target {
            id: "fixture".into(),
            name: "fixture".into(),
            url,
            region: String::new(),
            source: String::new(),
            checked_at: String::new(),
            probe_only: true,
        },
        permit: None,
        domain: "fixture".into(),
        client: Some(reqwest::Client::builder().no_proxy().build().unwrap()),
        addr: None,
        proxy: None,
    };
    let outcome = http_work(
        target,
        config("http", Some(proxy)),
        None,
        Arc::new(AtomicU64::new(0)),
        backoff(dir.path(), false),
        CancellationToken::new(),
    )
    .await;
    assert!(outcome.error.unwrap().contains("缺少准备好的固定路径"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn proxy_407_never_falls_back_to_direct_target() {
    let (target_listener, _) = listener().await;
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 407 Authentication Required\r\n\r\n")
            .await
            .unwrap();
    });
    let cancel = CancellationToken::new();
    let resolved = Arc::new(
        proxy::resolve_proxy(&proxy, None, &cancel, Duration::from_secs(1))
            .await
            .unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut target = work_target(resolved, &target_listener.local_addr().unwrap().to_string());
    target.target.url = format!(
        "http://original.invalid:{}/resource",
        target_listener.local_addr().unwrap().port()
    );
    let outcome = http_work(
        target,
        config("http", Some(proxy)),
        None,
        Arc::new(AtomicU64::new(0)),
        backoff(dir.path(), false),
        cancel,
    )
    .await;
    assert!(outcome.error.unwrap().contains("407"));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), target_listener.accept())
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn proxy_authentication_failure_stops_entire_load_run() {
    for mode in ["http", "tcp"] {
        let (listener, proxy) = listener().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 407 Authentication Required\r\n\r\n")
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(700), listener.accept())
                    .await
                    .is_err()
            );
        });
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
        let p = permit("http://10.0.0.1/", &proxy);
        let mut c = config(mode, Some(proxy));
        c.rps = 2;
        c.concurrency = 2;
        engine.start(c, &[], &[p]).await.unwrap();
        let run = terminal(&engine).await;
        assert_eq!(run.status, "failed");
        assert_eq!(run.attempted, 1);
        assert!(run.reason.contains("407"));
        server.await.unwrap();
        engine.shutdown_and_wait().await;
    }
}

#[tokio::test]
async fn proxy_get_obeys_remaining_global_byte_budget() {
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        headers(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789")
            .await
            .unwrap();
    });
    let cancel = CancellationToken::new();
    let resolved = Arc::new(
        proxy::resolve_proxy(&proxy, None, &cancel, Duration::from_secs(1))
            .await
            .unwrap(),
    );
    let budget = Arc::new(AtomicU64::new(BYTE_BUDGET - 3));
    let dir = tempfile::tempdir().unwrap();
    let outcome = http_work(
        work_target(resolved, "10.0.0.1:80"),
        config("http", Some(proxy)),
        None,
        budget.clone(),
        backoff(dir.path(), false),
        cancel,
    )
    .await;
    assert_eq!(outcome.bytes, 3);
    assert!(outcome.truncated);
    assert_eq!(budget.load(Ordering::SeqCst), BYTE_BUDGET);
    server.await.unwrap();
}

#[tokio::test]
async fn public_probe_proxy_uses_head_and_no_target_network() {
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(
            headers(&mut stream)
                .await
                .starts_with("CONNECT 8.8.8.8:80 ")
        );
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        assert!(
            headers(&mut stream)
                .await
                .starts_with("HEAD / HTTP/1.1\r\n")
        );
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
    let mut c = config("probe", Some(proxy));
    c.list_ids = vec!["fixture".into()];
    c.authorized_ids.clear();
    let list = SiteList {
        id: "fixture".into(),
        name: "fixture".into(),
        description: String::new(),
        builtin: false,
        targets: vec![Target {
            id: "numeric".into(),
            name: "numeric target (never dialled)".into(),
            url: "http://8.8.8.8/".into(),
            region: String::new(),
            source: String::new(),
            checked_at: String::new(),
            probe_only: true,
        }],
    };
    engine.start(c, &[list], &[]).await.unwrap();
    let run = terminal(&engine).await;
    assert_eq!(run.responses, 1);
    assert_eq!(run.targets[0].last_status, Some(204));
    assert_eq!(run.bytes, 0);
    server.await.unwrap();
    engine.shutdown_and_wait().await;
}

#[tokio::test]
async fn tcp_proxy_deadline_releases_tunnel_without_cancel() {
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        headers(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let cancel = CancellationToken::new();
    let resolved = Arc::new(
        proxy::resolve_proxy(&proxy, None, &cancel, Duration::from_secs(1))
            .await
            .unwrap(),
    );
    let target = work_target(resolved, "10.0.0.1:80");
    let metrics = Arc::new(Mutex::new(Metrics::new(
        config("tcp", Some(proxy)),
        std::slice::from_ref(&target.target),
    )));
    metrics.lock().unwrap().start(0);
    let outcome = tcp_work(
        target,
        metrics.clone(),
        0,
        cancel.clone(),
        Duration::from_secs(1),
        Instant::now() + Duration::from_millis(100),
    )
    .await;
    assert!(outcome.tcp_success);
    assert!(!cancel.is_cancelled());
    assert_eq!(metrics.lock().unwrap().run.connected, 0);
    server.await.unwrap();
}

#[tokio::test]
async fn tcp_proxy_tunnel_is_counted_and_released_on_stop() {
    let (listener, proxy) = listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(
            headers(&mut stream)
                .await
                .starts_with("CONNECT 10.0.0.1:80 ")
        );
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    });
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(dir.path().into(), None, CancellationToken::new()).unwrap();
    let p = permit("http://10.0.0.1/", &proxy);
    engine
        .start(config("tcp", Some(proxy)), &[], &[p])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while engine.snapshot().await.unwrap().connected != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    engine.stop().await;
    let run = terminal(&engine).await;
    assert_eq!(run.peak_connected, 1);
    assert_eq!(run.connected, 0);
    assert_eq!(run.active, 0);
    assert_eq!(run.status, "stopped");
    server.await.unwrap();
    engine.shutdown_and_wait().await;
}
