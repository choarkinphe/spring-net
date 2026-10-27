//! Crate-local regression tests. All network traffic stays on loopback; no cargo
//! commands, fixed listening ports, external DNS, or production data are needed.

use crate::{
    api::{self, AppState},
    demo,
    engine::Engine,
    metrics::{self, Metrics},
    model::{Permit, Run, RunConfig, SiteList, Target},
    policy, storage,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn permit(id: &str, url: String) -> Permit {
    Permit {
        id: id.into(),
        name: format!("test {id}"),
        url,
        modes: vec!["http".into(), "tcp".into()],
        allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
        max_concurrency: 1000,
        max_rps: 1000,
        max_duration: 300,
        authorization: "Regression fixture: loopback traffic only".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        proxy_routes: vec![],
    }
}

fn config(mode: &str, ids: &[&str]) -> RunConfig {
    RunConfig {
        mode: mode.into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: ids.iter().map(|id| (*id).into()).collect(),
        concurrency: 1,
        rps: 10,
        duration_secs: 1,
        timeout_secs: 3,
        ramp_secs: 0,
        fresh_connections: false,
        proxy: None,
    }
}

fn target(id: &str, url: &str) -> Target {
    Target {
        id: id.into(),
        name: format!("test {id}"),
        url: url.into(),
        region: String::new(),
        source: String::new(),
        checked_at: String::new(),
        probe_only: true,
    }
}

fn site_list(id: &str, builtin: bool, targets: Vec<Target>) -> SiteList {
    SiteList {
        id: id.into(),
        name: format!("test {id}"),
        description: "regression fixture".into(),
        builtin,
        targets,
    }
}

// A virtual control port: never bound, and explicitly distinct from every
// dynamically allocated fixture port. No dependency on the real GUI's port.
fn control_port(target_ports: &[u16]) -> u16 {
    (1..=u16::MAX)
        .find(|port| !target_ports.contains(port))
        .unwrap()
}

struct EngineFixture {
    engine: Engine,
    shutdown: CancellationToken,
    _data: TempDir,
}
impl EngineFixture {
    fn new(target_ports: &[u16]) -> Self {
        let data = tempfile::tempdir().unwrap();
        let shutdown = CancellationToken::new();
        let engine = Engine::new(
            data.path().to_owned(),
            Some(control_port(target_ports)),
            shutdown.clone(),
        )
        .unwrap();
        Self {
            engine,
            shutdown,
            _data: data,
        }
    }
}
impl Drop for EngineFixture {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct DemoFixture {
    http: u16,
    tcp: u16,
    shutdown: CancellationToken,
}
impl DemoFixture {
    async fn new() -> Self {
        let shutdown = CancellationToken::new();
        let (http, tcp) = demo::start(shutdown.clone()).await.unwrap();
        Self {
            http,
            tcp,
            shutdown,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.http)
    }
}
impl Drop for DemoFixture {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct LocalServer {
    port: u16,
    task: JoinHandle<()>,
}
impl LocalServer {
    async fn start(app: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { port, task }
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}
impl Drop for LocalServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn exactly_64_kib_response_is_not_marked_truncated() {
    let server =
        LocalServer::start(Router::new().route("/", get(|| async { vec![b'x'; 65536] }))).await;
    let fixture = EngineFixture::new(&[server.port]);
    fixture
        .engine
        .start(
            config("http", &["exact"]),
            &[],
            &[permit("exact", server.url("/"))],
        )
        .await
        .unwrap();
    let run = terminal(&fixture.engine).await;
    assert!(run.responses > 0);
    assert_eq!(run.targets[0].truncated, 0);
}

#[test]
fn public_global_dispatch_cooldown_survives_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cooldowns.json");
    let mut cooldowns = policy::Cooldowns::default();
    cooldowns.reserve_dispatch("first.example", &path).unwrap();
    let reloaded = policy::Cooldowns::load(&path).unwrap();
    assert!(!reloaded.ready("different.example"));
}

// Short polling is confined to tests, including intermediate-state waits.
async fn wait_for(engine: &Engine, condition: impl Fn(&Run) -> bool) -> Run {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let run = engine.snapshot().await.expect("a run has been started");
            if condition(&run) {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    match result {
        Ok(run) => run,
        Err(_) => panic!(
            "run did not reach expected state in 5s: {:?}",
            engine.snapshot().await
        ),
    }
}

async fn terminal(engine: &Engine) -> Run {
    wait_for(engine, |run| !run.active()).await
}

fn assert_drained(run: &Run) {
    assert!(!run.active(), "{run:?}");
    assert_eq!(run.active, 0);
    assert_eq!(run.connected, 0);
    assert_eq!(run.attempted, run.completed + run.cancelled);
    assert_eq!(
        run.attempted,
        run.targets.iter().map(|t| t.attempted).sum::<u64>()
    );
    for t in &run.targets {
        assert_eq!(t.active, 0);
        assert_eq!(t.connected, 0);
        assert_eq!(t.attempted, t.completed + t.cancelled);
    }
    let last = run.series.last().expect("terminal sample");
    assert_eq!(last.active, 0);
    assert_eq!(last.connected, 0);
}

#[tokio::test]
async fn engine_local_http_completes_and_can_start_another_run() {
    let demo = DemoFixture::new().await;
    let f = EngineFixture::new(&[demo.http, demo.tcp]);
    let permits = [permit("ok", demo.url("/probe"))];
    let mut c = config("http", &["ok"]);
    c.concurrency = 2;
    let initial = f.engine.start(c.clone(), &[], &permits).await.unwrap();
    assert_eq!(initial.status, "running");
    let run = terminal(&f.engine).await;
    assert_drained(&run);
    assert_eq!(run.status, "completed");
    assert!(run.responses > 0);
    assert_eq!(run.responses, run.completed);
    assert_eq!(run.good_status, run.responses);
    assert_eq!(run.errors, 0);
    assert_eq!(run.bytes, run.responses * b"AoNet demo only\n".len() as u64);
    assert!(run.p50_ms.is_some());
    assert!(run.p95_ms.unwrap() >= run.p50_ms.unwrap());
    assert!(run.peak_active <= c.concurrency);
    assert_eq!(run.targets[0].last_status, Some(200));

    let next = f.engine.start(c, &[], &permits).await.unwrap();
    assert_ne!(initial.id, next.id);
    f.engine.stop().await;
    assert_drained(&terminal(&f.engine).await);
}

#[derive(Default)]
struct RequestCounts {
    requests: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
}
struct ActiveRequest(Arc<RequestCounts>);
impl ActiveRequest {
    fn enter(counts: Arc<RequestCounts>) -> Self {
        counts.requests.fetch_add(1, Ordering::SeqCst);
        let active = counts.active.fetch_add(1, Ordering::SeqCst) + 1;
        counts.peak.fetch_max(active, Ordering::SeqCst);
        Self(counts)
    }
}
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn engine_bounds_http_workers_rejects_second_run_and_cancels_inflight() {
    let counts = Arc::new(RequestCounts::default());
    let captured = counts.clone();
    let server = LocalServer::start(Router::new().route(
        "/held",
        get(move || {
            let counts = captured.clone();
            async move {
                let _active = ActiveRequest::enter(counts);
                tokio::time::sleep(Duration::from_secs(2)).await;
                "held response"
            }
        }),
    ))
    .await;
    let f = EngineFixture::new(&[server.port]);
    let permits = [permit("held", server.url("/held"))];
    let mut c = config("http", &["held"]);
    c.concurrency = 3;
    c.rps = 100;
    c.duration_secs = 3;
    let initial = f.engine.start(c.clone(), &[], &permits).await.unwrap();
    wait_for(&f.engine, |r| {
        r.active == 3 && counts.requests.load(Ordering::SeqCst) == 3
    })
    .await;
    let error = f.engine.start(c, &[], &permits).await.unwrap_err();
    assert!(error.contains("已有测试"), "{error}");
    assert_eq!(f.engine.snapshot().await.unwrap().id, initial.id);

    // Allow many scheduler ticks while every slot is occupied: no queued or
    // extra requests may escape the configured concurrency cap.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(counts.requests.load(Ordering::SeqCst), 3);
    assert_eq!(counts.peak.load(Ordering::SeqCst), 3);
    let stopping = f.engine.stop().await.unwrap();
    assert_eq!(stopping.status, "stopping");
    let run = terminal(&f.engine).await;
    assert_drained(&run);
    assert_eq!(run.status, "stopped");
    assert_eq!(run.peak_active, 3);
    assert_eq!(run.attempted, 3);
    assert_eq!(run.completed, 0);
    assert_eq!(run.cancelled, 3);
    assert_eq!(run.errors, 0);
    assert_eq!(f.engine.stop().await.unwrap().status, "stopped");
}

#[tokio::test]
async fn engine_http_timeout_is_an_error_not_a_response() {
    let demo = DemoFixture::new().await;
    let f = EngineFixture::new(&[demo.http, demo.tcp]);
    let permits = [permit("slow", demo.url("/slow"))];
    let mut c = config("http", &["slow"]);
    c.duration_secs = 3;
    c.timeout_secs = 1;
    c.rps = 1;
    f.engine.start(c, &[], &permits).await.unwrap();
    let timed_out = wait_for(&f.engine, |r| r.errors > 0).await;
    assert_eq!(timed_out.responses, 0);
    assert!(
        timed_out.targets[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("超时")
    );
    f.engine.stop().await;
    let run = terminal(&f.engine).await;
    assert_drained(&run);
    assert!(run.errors >= 1);
    assert_eq!(run.responses, 0);
}

#[tokio::test]
async fn engine_tcp_holds_connections_without_reissuing_and_drains_on_stop() {
    let demo = DemoFixture::new().await;
    let f = EngineFixture::new(&[demo.http, demo.tcp]);
    let permits = [permit("tcp", format!("http://127.0.0.1:{}/", demo.tcp))];
    let mut c = config("tcp", &["tcp"]);
    c.concurrency = 3;
    c.rps = 100;
    c.duration_secs = 3;
    f.engine.start(c, &[], &permits).await.unwrap();
    wait_for(&f.engine, |r| r.connected == 3).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    let held = f.engine.snapshot().await.unwrap();
    assert_eq!(held.connected, 3);
    assert_eq!(held.active, 3);
    assert_eq!(held.attempted, 3);
    assert_eq!(held.completed, 0);
    f.engine.stop().await;
    let run = terminal(&f.engine).await;
    assert_drained(&run);
    assert_eq!(run.status, "stopped");
    assert_eq!(run.peak_connected, 3);
    assert_eq!(run.peak_active, 3);
    assert_eq!(run.attempted, 3);
    assert_eq!(run.errors, 0);
    assert_eq!(run.responses, 0);
}

#[tokio::test]
async fn engine_429_pauses_other_endpoints_of_same_domain_even_on_another_port() {
    let limited = Arc::new(AtomicUsize::new(0));
    let sibling = Arc::new(AtomicUsize::new(0));
    let captured = limited.clone();
    let first = LocalServer::start(Router::new().route(
        "/limited",
        get(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "30")],
                    "limited",
                )
            }
        }),
    ))
    .await;
    let captured = sibling.clone();
    let second = LocalServer::start(Router::new().route(
        "/ok",
        get(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async { "should not be requested" }
        }),
    ))
    .await;
    let f = EngineFixture::new(&[first.port, second.port]);
    let permits = [
        permit("limited", first.url("/limited")),
        permit("sibling", second.url("/ok")),
    ];
    // One worker ensures the 429 is observed before another request can start.
    f.engine
        .start(config("http", &["limited", "sibling"]), &[], &permits)
        .await
        .unwrap();
    let run = terminal(&f.engine).await;
    assert_drained(&run);
    assert_eq!(run.status, "completed");
    assert_eq!(limited.load(Ordering::SeqCst), 1);
    assert_eq!(sibling.load(Ordering::SeqCst), 0);
    assert_eq!(run.attempted, 1);
    assert_eq!(run.responses, 1);
    assert_eq!(run.good_status, 0);
    assert_eq!(run.errors, 0);
    assert_eq!(run.targets[0].last_status, Some(429));
    assert_eq!(run.targets[1].attempted, 0);
}

#[tokio::test]
async fn engine_records_local_redirect_but_never_requests_its_destination() {
    let redirects = Arc::new(AtomicUsize::new(0));
    let destinations = Arc::new(AtomicUsize::new(0));
    let destination_count = destinations.clone();
    let destination = LocalServer::start(Router::new().route(
        "/destination",
        get(move || {
            destination_count.fetch_add(1, Ordering::SeqCst);
            async { "redirect was followed" }
        }),
    ))
    .await;
    let location = destination.url("/destination");
    let captured_location = location.clone();
    let redirect_count = redirects.clone();
    let source = LocalServer::start(Router::new().route(
        "/redirect",
        get(move || {
            redirect_count.fetch_add(1, Ordering::SeqCst);
            let location = captured_location.clone();
            async move { (StatusCode::FOUND, [("location", location)], "") }
        }),
    ))
    .await;
    let f = EngineFixture::new(&[source.port, destination.port]);
    let permits = [permit("redirect", source.url("/redirect"))];
    let mut c = config("http", &["redirect"]);
    c.rps = 1;
    f.engine.start(c, &[], &permits).await.unwrap();
    let run = terminal(&f.engine).await;
    assert_drained(&run);
    assert_eq!(redirects.load(Ordering::SeqCst), 1);
    assert_eq!(destinations.load(Ordering::SeqCst), 0);
    assert_eq!(run.responses, 1);
    assert_eq!(run.errors, 0);
    assert_eq!(run.targets[0].last_status, Some(302));
    assert_eq!(run.targets[0].location.as_deref(), Some(location.as_str()));
}

#[tokio::test]
async fn engine_caps_large_http_body_accounting_at_64_kib() {
    let requests = Arc::new(AtomicUsize::new(0));
    let captured = requests.clone();
    let server = LocalServer::start(Router::new().route(
        "/large",
        get(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async { vec![b'x'; 128 * 1024] }
        }),
    ))
    .await;
    let f = EngineFixture::new(&[server.port]);
    let permits = [permit("large", server.url("/large"))];
    let mut c = config("http", &["large"]);
    c.rps = 1;
    f.engine.start(c, &[], &permits).await.unwrap();
    let run = terminal(&f.engine).await;
    assert_drained(&run);
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(run.responses, 1);
    assert_eq!(run.errors, 0);
    assert_eq!(run.bytes, 64 * 1024);
    assert_eq!(run.targets[0].bytes, 64 * 1024);
    assert_eq!(run.targets[0].truncated, 1);
}

#[test]
fn config_accepts_1000_but_rejects_1001_without_generating_load() {
    let mut upper = config("http", &["unused"]);
    upper.concurrency = 1000;
    upper.rps = 1000;
    upper.duration_secs = 300;
    upper.timeout_secs = 30;
    upper.ramp_secs = 300;
    upper.validate().unwrap();
    let mut invalid = upper.clone();
    invalid.concurrency = 1001;
    assert!(invalid.validate().is_err());
    invalid = upper.clone();
    invalid.rps = 1001;
    assert!(invalid.validate().is_err());
    invalid = upper.clone();
    invalid.duration_secs = 301;
    assert!(invalid.validate().is_err());
    invalid = upper.clone();
    invalid.timeout_secs = 31;
    assert!(invalid.validate().is_err());
    invalid = upper;
    invalid.ramp_secs = 301;
    assert!(invalid.validate().is_err());
    for field in ["concurrency", "rps", "duration_secs", "timeout_secs"] {
        let mut value = serde_json::to_value(config("http", &[])).unwrap();
        value[field] = json!(0);
        assert!(
            serde_json::from_value::<RunConfig>(value)
                .unwrap()
                .validate()
                .is_err(),
            "{field}"
        );
    }
    let mut public = config("probe", &[]);
    public.concurrency = 4;
    public.rps = 2;
    public.duration_secs = 120;
    public.validate().unwrap();
    for field in ["concurrency", "rps", "duration_secs"] {
        let mut value = serde_json::to_value(&public).unwrap();
        value[field] = json!(value[field].as_u64().unwrap() + 1);
        assert!(
            serde_json::from_value::<RunConfig>(value)
                .unwrap()
                .validate()
                .is_err(),
            "{field}"
        );
    }
}

#[test]
fn permit_file_accepts_limit_boundaries_and_rejects_values_above_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("permits.json");
    let p = permit("boundary", "http://127.0.0.1:2/".into());
    storage::atomic_json(&path, &json!({"version": 1, "targets": [&p]})).unwrap();
    let loaded = policy::load_permits(&path, &Default::default(), Some(1)).unwrap();
    assert_eq!(loaded[0].max_concurrency, 1000);
    assert_eq!(loaded[0].max_rps, 1000);
    assert_eq!(loaded[0].max_duration, 300);
    for (field, value) in [
        ("max_concurrency", 1001),
        ("max_rps", 1001),
        ("max_duration", 301),
    ] {
        let mut invalid = serde_json::to_value(&p).unwrap();
        invalid[field] = json!(value);
        storage::atomic_json(&path, &json!({"version": 1, "targets": [invalid]})).unwrap();
        assert!(
            policy::load_permits(&path, &Default::default(), Some(1)).is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
async fn public_probe_rejects_private_targets_before_any_network_io() {
    let f = EngineFixture::new(&[]);
    for url in [
        "http://127.0.0.1/",
        "http://10.0.0.1/",
        "http://172.16.0.1/",
        "http://192.168.0.1/",
        "http://169.254.169.254/",
        "http://[::1]/",
        "http://[fc00::1]/",
        "http://[::ffff:127.0.0.1]/",
        "http://localhost/",
    ] {
        let parsed = policy::parse_url(url).unwrap();
        assert!(policy::check_public_url(&parsed).is_err(), "{url}");
        assert!(
            policy::resolve(&parsed, None, f.engine.control_port)
                .await
                .is_err(),
            "{url}"
        );
        let lists = [site_list("private", false, vec![target("private", url)])];
        let mut c = config("probe", &[]);
        c.list_ids = vec!["private".into()];
        c.rps = 1;
        let error = f.engine.start(c, &lists, &[]).await.unwrap_err();
        assert!(error.contains("公共"), "{url}: {error}");
        assert!(f.engine.snapshot().await.is_none());
    }
}

#[tokio::test]
async fn engine_rejects_unknown_ids_and_out_of_scope_permits_before_io() {
    let f = EngineFixture::new(&[]);
    for mode in ["http", "tcp"] {
        let error = f
            .engine
            .start(config(mode, &["not-authorized"]), &[], &[])
            .await
            .unwrap_err();
        assert!(error.contains("未登记授权端点"), "{error}");
    }
    let mut public = config("probe", &[]);
    public.rps = 1;
    public.list_ids = vec!["missing-list".into()];
    assert!(
        f.engine
            .start(public, &[], &[])
            .await
            .unwrap_err()
            .contains("未知列表")
    );

    // These errors precede DNS/connect. The numeric URL is never contacted.
    let base = permit("restricted", "http://127.0.0.1:2/".into());
    let mut invalid = base.clone();
    invalid.modes = vec!["tcp".into()];
    assert!(
        f.engine
            .start(config("http", &["restricted"]), &[], &[invalid])
            .await
            .is_err()
    );
    let mut invalid = base.clone();
    invalid.max_concurrency = 1;
    let mut c = config("http", &["restricted"]);
    c.concurrency = 2;
    assert!(f.engine.start(c, &[], &[invalid]).await.is_err());
    let mut invalid = base.clone();
    invalid.max_rps = 1;
    assert!(
        f.engine
            .start(config("http", &["restricted"]), &[], &[invalid])
            .await
            .is_err()
    );
    let mut invalid = base.clone();
    invalid.max_duration = 1;
    let mut c = config("http", &["restricted"]);
    c.duration_secs = 2;
    assert!(f.engine.start(c, &[], &[invalid]).await.is_err());
    let mut invalid = base;
    invalid.expires_at = "2000-01-01T00:00:00Z".into();
    assert!(
        f.engine
            .start(config("http", &["restricted"]), &[], &[invalid])
            .await
            .unwrap_err()
            .contains("过期")
    );
    assert!(f.engine.snapshot().await.is_none());
}

#[tokio::test]
async fn resolver_enforces_permit_cidr_and_blocks_virtual_control_port() {
    let port = control_port(&[]);
    let url = policy::parse_url(&format!("http://127.0.0.1:{port}/")).unwrap();
    let p = permit("control", url.to_string());
    assert!(
        policy::resolve(&url, Some(&p), Some(port))
            .await
            .unwrap_err()
            .contains("控制服务")
    );
    let url = policy::parse_url("http://127.0.0.1:2/").unwrap();
    let mut p = permit("wrong-cidr", url.to_string());
    p.allowed_ips = vec!["192.168.0.0/16".parse().unwrap()];
    assert!(
        policy::resolve(&url, Some(&p), Some(port))
            .await
            .unwrap_err()
            .contains("授权 IP/CIDR")
    );
}

struct ApiFixture {
    app: Router,
    state: Arc<AppState>,
    _data: TempDir,
}
impl ApiFixture {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let shutdown = CancellationToken::new();
        let port = control_port(&[]);
        let engine = Engine::new(data.path().to_owned(), Some(port), shutdown.clone()).unwrap();
        let batches = crate::batch_service::BatchManager::new(
            engine.shared_cooldowns(),
            Some(port),
            shutdown.clone(),
        );
        let state = Arc::new(AppState {
            service: Arc::new(crate::service::AppService {
                engine,
                batches,
                network_tools: crate::network_tools::NetworkToolManager::new(shutdown.clone())
                    .with_control_port(Some(port)),
                diagnostics: crate::diagnostics::DiagnosticManager::new(
                    Some(port),
                    shutdown.clone(),
                ),
                monitor_route: std::sync::Mutex::new(None),
                monitor: crate::monitor::MonitorManager::new(
                    data.path(),
                    Some(port),
                    shutdown.clone(),
                )
                .unwrap(),
                lists: RwLock::new(vec![site_list("builtin-fixture", true, vec![])]),
                connection_presets: vec![],
                permits: RwLock::new(vec![]),
                auth_file: data.path().join("auth.json"),
                public_domains: Default::default(),
                control_port: Some(port),
                auth_error: RwLock::new(None),
                auth_revision: std::sync::atomic::AtomicU64::new(1),
                permit_drafts: tokio::sync::Mutex::new(Default::default()),
                mutation: tokio::sync::Mutex::new(()),
                history: crate::history::History::open(data.path()).unwrap(),
                data: data.path().to_owned(),
                shutdown,
                demo: tokio::sync::Mutex::new(None),
            }),
            token: "regression-session-token".into(),
            instance_id: "regression-instance".into(),
            port,
        });
        Self {
            app: api::router(state.clone()),
            state,
            _data: data,
        }
    }
    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.state.port)
    }
    fn write_request(&self, method: &str, path: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header("host", self.host())
            .header("origin", format!("http://{}", self.host()))
            .header("content-type", "application/json")
            .header("x-aonet-token", &self.state.token)
            .body(Body::from(body.to_string()))
            .unwrap()
    }
    async fn send(&self, request: Request<Body>) -> axum::response::Response {
        self.app.clone().oneshot(request).await.unwrap()
    }
}
impl Drop for ApiFixture {
    fn drop(&mut self) {
        self.state.shutdown.cancel();
    }
}
#[tokio::test]
async fn diagnostics_http_reads_are_inert_and_writes_keep_guards() {
    let fixture = ApiFixture::new();
    let response = fixture
        .send(
            Request::builder()
                .uri("/api/diagnostics/state")
                .header("host", fixture.host())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(fixture.state.service.diagnostic_state().await.is_none());
    let response = fixture
        .send(
            Request::builder()
                .method("POST")
                .uri("/api/diagnostics")
                .header("host", fixture.host())
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = fixture.send(fixture.write_request("POST", "/api/diagnostics", json!({
        "request_id":uuid::Uuid::new_v4().to_string(),"target":"127.0.0.1","port":fixture.state.port,"tls":false,"allow_lan":true
    }))).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(fixture.state.service.diagnostic_state().await.is_none());
    let response = fixture
        .send(
            Request::builder()
                .uri(format!(
                    "/api/diagnostics/report?id={}",
                    uuid::Uuid::new_v4()
                ))
                .header("host", fixture.host())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    fixture.state.service.shutdown_and_wait().await;
}
#[tokio::test]
async fn shared_service_desktop_lifecycle_persistence_and_exports() {
    use crate::service::{AppService, ServiceConfig};
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    std::fs::write(&auth, r#"{"version":1,"targets":[]}"#).unwrap();
    let make_config = || ServiceConfig {
        root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        data: dir.path().join("data"),
        auth_file: auth.clone(),
        control_port: None,
    };
    let service = AppService::initialize(make_config(), CancellationToken::new())
        .await
        .unwrap();
    let b = service.bootstrap("ipc").await;
    assert_eq!(
        b["lists"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["targets"].as_array().unwrap().len())
            .sum::<usize>(),
        55
    );
    let list = site_list(
        "custom-native",
        false,
        vec![target("native-one", "https://example.com/")],
    );
    service.save_lists(vec![list]).await.unwrap();
    assert!(
        service
            .save_lists(vec![site_list("forged", true, vec![])])
            .await
            .is_err()
    );
    let exported: Value = serde_json::from_slice(&service.export("lists").await.unwrap()).unwrap();
    assert_eq!(exported["lists"][0]["id"], "custom-native");
    assert!(b["authorized"].as_array().unwrap().is_empty());
    service.set_self_test(true).await.unwrap();
    let demo_url = service
        .permits
        .read()
        .await
        .iter()
        .find(|p| p.id == "demo-http")
        .unwrap()
        .url
        .clone();
    let demo_port = url::Url::parse(&demo_url).unwrap().port().unwrap();
    service.start(config("http", &["demo-http"])).await.unwrap();
    let run = terminal(&service.engine).await;
    assert!(run.responses > 0);
    assert!(service.export("json").await.unwrap().starts_with(b"{"));
    assert!(!service.export("csv").await.unwrap().is_empty());
    service.shutdown_and_wait().await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", demo_port))
            .await
            .is_err()
    );
    assert!(service.start(config("http", &["demo-http"])).await.is_err());
    let reopened = AppService::initialize(make_config(), CancellationToken::new())
        .await
        .unwrap();
    assert!(
        reopened
            .lists
            .read()
            .await
            .iter()
            .any(|l| l.id == "custom-native")
    );
    reopened.shutdown_and_wait().await;
}

#[tokio::test]
async fn health_reports_own_instance_and_shutdown_state() {
    let fixture = ApiFixture::new();
    let request = || {
        Request::builder()
            .uri("/api/health")
            .header("host", fixture.host())
            .body(Body::empty())
            .unwrap()
    };
    let value = response_json(fixture.send(request()).await).await;
    assert_eq!(value["instance_id"], "regression-instance");
    assert_eq!(value["ready"], true);
    fixture.state.shutdown.cancel();
    let value = response_json(fixture.send(request()).await).await;
    assert_eq!(value["ready"], false);
}

#[tokio::test]
async fn shutdown_waits_for_tcp_workers_and_rejects_new_runs() {
    let demo = DemoFixture::new().await;
    let fixture = EngineFixture::new(&[demo.http, demo.tcp]);
    let permit = permit("tcp", format!("http://127.0.0.1:{}/", demo.tcp));
    fixture
        .engine
        .start(config("tcp", &["tcp"]), &[], std::slice::from_ref(&permit))
        .await
        .unwrap();
    wait_for(&fixture.engine, |r| r.connected == 1).await;
    fixture.engine.shutdown_and_wait().await;
    assert!(fixture.engine.snapshot().await.is_none());
    assert!(
        fixture
            .engine
            .start(config("tcp", &["tcp"]), &[], &[permit])
            .await
            .unwrap_err()
            .contains("退出")
    );
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn api_requires_local_host_and_matching_origin_even_for_reads() {
    let f = ApiFixture::new();
    for host in [
        None,
        Some("attacker.invalid".into()),
        Some(format!("127.0.0.1:{}", f.state.port + 1)),
    ] {
        let mut builder = Request::builder().uri("/api/bootstrap");
        if let Some(host) = host {
            builder = builder.header("host", host);
        }
        assert_eq!(
            f.send(builder.body(Body::empty()).unwrap()).await.status(),
            StatusCode::FORBIDDEN
        );
    }
    for (header, value) in [
        ("origin", "https://attacker.invalid".to_owned()),
        ("origin", format!("https://{}", f.host())),
        ("origin", format!("http://localhost:{}", f.state.port)),
        ("sec-fetch-site", "cross-site".to_owned()),
    ] {
        let request = Request::builder()
            .uri("/api/bootstrap")
            .header("host", f.host())
            .header(header, value)
            .body(Body::empty())
            .unwrap();
        assert_eq!(f.send(request).await.status(), StatusCode::FORBIDDEN);
    }
    for host in [f.host(), format!("localhost:{}", f.state.port)] {
        let request = Request::builder()
            .uri("/api/bootstrap")
            .header("host", &host)
            .header("origin", format!("http://{host}"))
            .body(Body::empty())
            .unwrap();
        let response = f.send(request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert_eq!(response_json(response).await["token"], f.state.token);
    }
    // Non-browser loopback reads need no Origin or token.
    let request = Request::builder()
        .uri("/api/health")
        .header("host", f.host())
        .body(Body::empty())
        .unwrap();
    let response = f.send(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["ready"], true);
}

#[tokio::test]
async fn api_write_routes_require_token_same_origin_and_json() {
    let f = ApiFixture::new();
    for (method, path, body) in [
        ("POST", "/api/stop", json!({})),
        (
            "POST",
            "/api/runs",
            serde_json::to_value(config("http", &["unknown"])).unwrap(),
        ),
        ("PUT", "/api/lists", json!({"lists": []})),
    ] {
        let mut missing = f.write_request(method, path, body.clone());
        missing.headers_mut().remove("x-aonet-token");
        assert_eq!(f.send(missing).await.status(), StatusCode::FORBIDDEN);
        let mut wrong = f.write_request(method, path, body.clone());
        wrong
            .headers_mut()
            .insert("x-aonet-token", "wrong-token".parse().unwrap());
        assert_eq!(f.send(wrong).await.status(), StatusCode::FORBIDDEN);
        let mut cross_origin = f.write_request(method, path, body.clone());
        cross_origin
            .headers_mut()
            .insert("origin", "http://attacker.invalid".parse().unwrap());
        assert_eq!(f.send(cross_origin).await.status(), StatusCode::FORBIDDEN);
        let mut cross_site = f.write_request(method, path, body.clone());
        cross_site
            .headers_mut()
            .insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert_eq!(f.send(cross_site).await.status(), StatusCode::FORBIDDEN);
        let mut wrong_type = f.write_request(method, path, body.clone());
        wrong_type
            .headers_mut()
            .insert("content-type", "text/plain".parse().unwrap());
        assert_eq!(
            f.send(wrong_type).await.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        let mut no_type = f.write_request(method, path, body);
        no_type.headers_mut().remove("content-type");
        assert_eq!(
            f.send(no_type).await.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }
    assert!(!f.state.data.join("lists.json").exists());
    assert!(f.state.engine.snapshot().await.is_none());
    let response = f
        .send(f.write_request("POST", "/api/stop", json!({})))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await, Value::Null);
    let response = f
        .send(f.write_request(
            "POST",
            "/api/runs",
            serde_json::to_value(config("http", &["unknown"])).unwrap(),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        response_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("未登记授权端点")
    );
}

#[tokio::test]
async fn api_saves_sanitized_lists_and_rejects_forged_or_colliding_builtins() {
    let f = ApiFixture::new();
    let mut t = target("custom-target", "HTTP://127.0.0.1");
    t.probe_only = false;
    t.source = "forged trusted source".into();
    t.checked_at = "2099-01-01".into();
    t.region = "forged region".into();
    let custom = site_list("custom", false, vec![t]);
    let response = f
        .send(f.write_request("PUT", "/api/lists", json!({"lists": [&custom]})))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["lists"].as_array().unwrap().len(), 2);
    let saved = storage::load_custom(&f.state.data).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].id, "custom");
    assert!(!saved[0].builtin);
    assert_eq!(saved[0].targets[0].url, "http://127.0.0.1/");
    assert!(saved[0].targets[0].probe_only);
    assert!(saved[0].targets[0].source.is_empty());
    assert!(saved[0].targets[0].checked_at.is_empty());
    assert!(saved[0].targets[0].region.is_empty());
    let before = std::fs::read(f.state.data.join("lists.json")).unwrap();
    let before_memory = serde_json::to_value(&*f.state.lists.read().await).unwrap();
    let mut forged = custom.clone();
    forged.builtin = true;
    let mut collision = custom.clone();
    collision.id = "builtin-fixture".into();
    let mut duplicate_target = custom.clone();
    duplicate_target
        .targets
        .push(duplicate_target.targets[0].clone());
    for lists in [
        vec![forged],
        vec![collision],
        vec![custom.clone(), custom],
        vec![duplicate_target],
    ] {
        let response = f
            .send(f.write_request("PUT", "/api/lists", json!({"lists": lists})))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            std::fs::read(f.state.data.join("lists.json")).unwrap(),
            before
        );
        assert_eq!(
            serde_json::to_value(&*f.state.lists.read().await).unwrap(),
            before_memory
        );
    }
    // Updating custom lists must not delete or persist built-in lists.
    let response = f
        .send(f.write_request("PUT", "/api/lists", json!({"lists": []})))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(storage::load_custom(&f.state.data).unwrap().is_empty());
    let lists = f.state.lists.read().await;
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].id, "builtin-fixture");
    assert!(lists[0].builtin);
}

#[test]
fn csv_quotes_cells_and_neutralizes_formula_prefixes_in_text_columns() {
    for name in [
        "=SUM(1,2)",
        "+cmd",
        "-1+1",
        "@SUM(A1)",
        "  =1",
        "\tformula",
        "\rformula",
        "\nformula",
    ] {
        let mut t = target("csv", "http://127.0.0.1/");
        t.name = name.into();
        let mut m = Metrics::new(config("http", &[]), &[t]);
        m.run.reason = "\"quoted\", reason\r\nnext line".into();
        let csv = metrics::csv(&m.run);
        assert!(csv.starts_with('\u{feff}'));
        assert!(
            csv.contains(&format!("\"'{}\"", name.replace('"', "\"\""))),
            "{csv:?}"
        );
        assert!(csv.contains("\"\"\"quoted\"\", reason\r\nnext line\""));
        assert!(csv.ends_with("\r\n"));
    }
}
