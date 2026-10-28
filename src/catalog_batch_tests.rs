use super::*;
use crate::{batch_service::BatchManager, policy::Cooldowns};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;
fn request(mode: Mode) -> Request {
    Request {
        request_id: uuid::Uuid::new_v4().to_string(),
        catalog_revision: 1,
        group_ids: vec!["g".into()],
        site_ids: vec![],
        resource_selections: None,
        mode,
        permit_bindings: vec![],
        concurrency: 4,
        connection_limit: 2,
        queue_capacity: 1,
        attempts_per_site: 3,
        start_rate: 32,
        request_timeout_secs: 2,
        batch_timeout_secs: 10,
        per_site_connections: (mode == Mode::Tcp).then_some(1),
        tcp_hold_secs: (mode == Mode::Tcp).then_some(1),
        confirmed_authorized: mode != Mode::PublicHttp,
    }
}
fn fixture(address: &str, n: usize) -> (site_catalog::Snapshot, Permit) {
    let sites: Vec<_> = (0..n)
        .map(|i| site_catalog::CatalogSite {
            site: site_catalog::Site {
                id: format!("s{i}"),
                name: format!("site{i}"),
                address: address.into(),
                port: None,
                transport: site_catalog::Transport::Tcp,
                protocol: if address.starts_with("https://") {
                    site_catalog::Protocol::Https
                } else {
                    site_catalog::Protocol::Http
                },
                purpose: String::new(),
                alpn: None,
            },
            builtin: false,
            source: String::new(),
            checked_at: String::new(),
            capabilities: vec!["http".into(), "tcp".into()],
            unavailable_reasons: vec![],
        })
        .collect();
    let applications = vec![site_catalog::CatalogApplication {
        application: site_catalog::Application {
            id: "app".into(),
            name: "fixture application".into(),
            kind: site_catalog::ApplicationKind::Application,
            category: site_catalog::ApplicationCategory::Custom,
            provider: String::new(),
            product: String::new(),
            aliases: vec![],
            access_type: String::new(),
            site_ids: sites.iter().map(|s| s.site.id.clone()).collect(),
            reference_rules: vec![],
        },
        tag_ids: vec![],
        supplement: None,
        builtin: false,
        sources: vec![],
        missing: vec![],
        limitations: vec![],
        summary: String::new(),
        status: "custom".into(),
        environment: None,
        version: None,
    }];
    let group = site_catalog::CatalogGroup {
        group: site_catalog::Group {
            id: "g".into(),
            name: "group".into(),
            description: String::new(),
            tag_ids: vec![],
            application_ids: vec!["app".into()],
        },
        builtin: false,
    };
    (
        site_catalog::Snapshot {
            revision: 1,
            sites,
            applications,
            groups: vec![group],
            ..site_catalog::Manager::new(tempfile::tempdir().unwrap().path(), &[])
                .unwrap()
                .snapshot()
        },
        Permit {
            id: "p".into(),
            name: "fixture".into(),
            url: address.into(),
            modes: vec!["http".into(), "tcp".into()],
            allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
            max_concurrency: 32,
            max_rps: 32,
            max_duration: 300,
            authorization: "test-only fixture".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            proxy_routes: vec![],
        },
    )
}
#[test]
fn v2_groups_expand_applications_and_freeze_identities_without_url_deduplication() {
    let (mut catalog, permit) = fixture("http://127.0.0.1:18080/path", 2);
    let mut alias = catalog.applications[0].clone();
    alias.application.id = "alias".into();
    alias.application.name = "Alias application".into();
    catalog.applications.push(alias);
    let mut group = catalog.groups[0].clone();
    group.group.id = "g2".into();
    group.group.application_ids.push("alias".into());
    catalog.groups.push(group);
    let mut r = request(Mode::AuthorizedHttp);
    r.group_ids.push("g2".into());
    bind(&mut r, 2);
    r.normalize().unwrap();
    let (frozen, targets) = freeze(&r, &catalog, &[permit], None).unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(frozen.counts.planned, 6);
    assert!(
        frozen
            .sites
            .iter()
            .all(|s| s.group_ids == ["g", "g2"] && s.applications.len() == 2)
    );
    catalog.applications[0].application.name = "changed later".into();
    assert_eq!(frozen.sites[0].applications[0].name, "fixture application");
}

#[test]
fn v2_memberships_are_validated_before_explicit_endpoint_filtering() {
    let (catalog, permit) = fixture("http://127.0.0.1:18080/path", 2);
    let mut r = request(Mode::AuthorizedHttp);
    r.site_ids = vec!["s0".into()];
    bind(&mut r, 1);
    r.normalize().unwrap();
    for missing_app in [false, true] {
        let mut c = catalog.clone();
        if missing_app {
            c.groups[0].group.application_ids.push("missing".into());
        } else {
            c.applications[0]
                .application
                .site_ids
                .push("missing".into());
        }
        assert!(
            freeze(&r, &c, std::slice::from_ref(&permit), None)
                .unwrap_err()
                .contains("unknown")
        );
    }
    let mut c = catalog.clone();
    c.applications[0].application.site_ids.clear();
    assert!(
        freeze(&r, &c, std::slice::from_ref(&permit), None)
            .unwrap_err()
            .contains("no concrete endpoints")
    );
    r.site_ids = vec!["not-a-member".into()];
    assert!(
        freeze(&r, &catalog, &[permit], None)
            .unwrap_err()
            .contains("not in selected groups")
    );
}

#[test]
fn v2_explicit_ids_never_silently_filter_incompatible_endpoints() {
    use site_catalog::{Protocol, Transport};
    let (catalog, permit) = fixture("http://127.0.0.1:18080/path", 2);
    for mode in [Mode::AuthorizedHttp, Mode::Tcp] {
        let mut r = request(mode);
        r.site_ids = vec!["s0".into(), "s1".into()];
        bind(&mut r, 2);
        r.normalize().unwrap();
        for (transport, protocol) in [
            (Transport::Udp, Protocol::UdpDns),
            (Transport::Udp, Protocol::Quic),
            (Transport::Unspecified, Protocol::Unspecified),
            (Transport::Tcp, Protocol::Unspecified),
            (Transport::Tcp, Protocol::Quic),
        ] {
            let mut c = catalog.clone();
            c.sites[1].site.transport = transport;
            c.sites[1].site.protocol = protocol;
            // Even a stale/forged capability cannot override explicit endpoint metadata.
            assert!(
                freeze(&r, &c, std::slice::from_ref(&permit), None)
                    .unwrap_err()
                    .contains("s1: endpoint does not support")
            );
        }
        let mut c = catalog.clone();
        c.sites[1].capabilities.clear();
        assert!(freeze(&r, &c, std::slice::from_ref(&permit), None).is_err());
    }
}

#[test]
fn v2_tcp_is_only_a_transport_probe_for_explicit_tcp_endpoints() {
    use site_catalog::{Protocol, Transport};
    for protocol in [Protocol::Tcp, Protocol::Tls, Protocol::Other] {
        let (mut catalog, permit) = fixture("http://127.0.0.1:18080/path", 1);
        catalog.sites[0].site.address = "127.0.0.1".into();
        catalog.sites[0].site.port = Some(18080);
        catalog.sites[0].site.transport = Transport::Tcp;
        catalog.sites[0].site.protocol = protocol;
        catalog.sites[0].capabilities = vec!["tcp".into()];
        let mut r = request(Mode::Tcp);
        bind(&mut r, 1);
        r.normalize().unwrap();
        assert!(freeze(&r, &catalog, std::slice::from_ref(&permit), None).is_ok());
        r.mode = Mode::AuthorizedHttp;
        r.per_site_connections = None;
        r.tcp_hold_secs = None;
        r.normalize().unwrap();
        assert!(freeze(&r, &catalog, &[permit], None).is_err());
    }
}

#[test]
fn public_checks_and_authorized_load_have_separate_limits() {
    let mut public = request(Mode::PublicHttp);
    public.start_rate = 2;
    assert!(public.normalize().is_ok());
    public.concurrency = 5;
    assert!(public.normalize().is_err());
    public.concurrency = 4;
    public.attempts_per_site = 4;
    assert!(public.normalize().is_err());
    let mut authorized = request(Mode::AuthorizedHttp);
    authorized.concurrency = 64;
    authorized.connection_limit = 64;
    authorized.start_rate = 64;
    assert!(authorized.normalize().is_ok());
    authorized.concurrency = 1001;
    assert!(authorized.normalize().is_err());
    let mut tcp = request(Mode::Tcp);
    tcp.connection_limit = 1000;
    tcp.concurrency = 1000;
    tcp.per_site_connections = Some(1000);
    assert!(tcp.normalize().is_ok());
    tcp.per_site_connections = Some(1001);
    assert!(tcp.normalize().is_err());
    assert_eq!(limits()["authorized"]["max_concurrency"], 1000);
}

#[test]
fn authorized_load_still_obeys_the_bound_permit_before_resource_checks() {
    let (catalog, permit) = fixture("http://127.0.0.1:18080/path", 1);
    let mut r = request(Mode::AuthorizedHttp);
    bind(&mut r, 1);
    r.concurrency = 64;
    r.connection_limit = 64;
    r.start_rate = 64;
    r.normalize().unwrap();
    let error = freeze(&r, &catalog, &[permit], None).unwrap_err();
    assert!(error.contains("permit"));
}

#[test]
fn explicit_resource_provenance_excludes_unselected_shared_endpoint_applications() {
    let (mut catalog, permit) = fixture("http://127.0.0.1:18080/path", 2);
    let mut alias = catalog.applications[0].clone();
    alias.application.id = "alias".into();
    alias.application.site_ids = vec!["s0".into()];
    catalog.applications.push(alias);
    catalog.groups[0].group.application_ids.push("alias".into());
    let mut second = catalog.groups[0].clone();
    second.group.id = "g2".into();
    catalog.groups.push(second);
    let mut r = request(Mode::AuthorizedHttp);
    r.group_ids.push("g2".into());
    r.resource_selections = Some(vec![ResourceSelection {
        group_id: "g2".into(),
        resource_id: "alias".into(),
    }]);
    bind(&mut r, 1);
    r.normalize().unwrap();
    for frozen in [
        freeze_definition(&r, &catalog, None).unwrap(),
        freeze(&r, &catalog, std::slice::from_ref(&permit), None)
            .unwrap()
            .0,
    ] {
        assert_eq!(frozen.sites.len(), 1);
        assert_eq!(frozen.sites[0].site_id, "s0");
        assert_eq!(frozen.sites[0].group_ids, ["g2"]);
        assert_eq!(
            frozen.sites[0]
                .applications
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["alias"]
        );
    }
    // None retains legacy source expansion, including applications sharing the endpoint.
    r.resource_selections = None;
    r.site_ids = vec!["s0".into()];
    let legacy = freeze(&r, &catalog, &[permit], None).unwrap().0;
    assert_eq!(legacy.sites[0].applications.len(), 2);
    assert_eq!(legacy.sites[0].group_ids, ["g", "g2"]);
}

#[test]
fn resource_pairs_are_bounded_unique_and_validated_against_server_projection() {
    let (catalog, _) = fixture("http://127.0.0.1:18080/path", 1);
    let mut r = request(Mode::AuthorizedHttp);
    bind(&mut r, 1);
    let pair = ResourceSelection {
        group_id: "g".into(),
        resource_id: "app".into(),
    };
    for selections in [
        vec![pair.clone(), pair.clone()],
        vec![pair.clone(); 10001],
        vec![ResourceSelection {
            group_id: "other".into(),
            ..pair.clone()
        }],
        vec![ResourceSelection {
            resource_id: "".into(),
            ..pair.clone()
        }],
        vec![ResourceSelection {
            resource_id: "x".repeat(201),
            ..pair.clone()
        }],
    ] {
        r.resource_selections = Some(selections);
        assert!(r.normalize().is_err());
    }
    for selections in [
        vec![],
        vec![ResourceSelection {
            resource_id: "missing".into(),
            ..pair.clone()
        }],
    ] {
        r.resource_selections = Some(selections);
        r.normalize().unwrap();
        assert!(freeze_definition(&r, &catalog, None).is_err());
    }
    r.resource_selections = Some(vec![pair]);
    r.normalize().unwrap();
    let mut detached = catalog.clone();
    detached.groups[0].group.application_ids.clear();
    assert!(
        freeze_definition(&r, &detached, None)
            .unwrap_err()
            .contains("not in selected group")
    );
    let mut missing_group = catalog;
    missing_group.groups.clear();
    assert!(
        freeze_definition(&r, &missing_group, None)
            .unwrap_err()
            .contains("unknown group")
    );
}

#[test]
fn omitted_resource_selection_preserves_legacy_request_intent_bytes() {
    let mut r = request(Mode::AuthorizedHttp);
    bind(&mut r, 1);
    r.normalize().unwrap();
    let bytes = serde_json::to_vec(&r).unwrap();
    assert!(
        !String::from_utf8(bytes.clone())
            .unwrap()
            .contains("resource_selections")
    );
    let old: Request = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(old.resource_selections, None);
    assert_eq!(serde_json::to_vec(&old).unwrap(), bytes);
    let mut explicit = old.clone();
    explicit.resource_selections = Some(vec![ResourceSelection {
        group_id: "g".into(),
        resource_id: "app".into(),
    }]);
    assert_ne!(serde_json::to_vec(&explicit).unwrap(), bytes);
    let mut unknown = serde_json::to_value(explicit).unwrap();
    unknown["resource_selections"][0]["url"] = serde_json::json!("http://invalid/");
    assert!(serde_json::from_value::<Request>(unknown).is_err());
}

fn bind(r: &mut Request, n: usize) {
    r.permit_bindings = (0..n)
        .map(|i| PermitBinding {
            site_id: format!("s{i}"),
            permit_id: "p".into(),
        })
        .collect();
}
fn manager(dir: &tempfile::TempDir) -> BatchManager {
    BatchManager::new(
        (
            Arc::new(Mutex::new(Cooldowns::default())),
            dir.path().join("cooldowns.json"),
        ),
        None,
        CancellationToken::new(),
    )
}
async fn done(m: &BatchManager) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let s = m.catalog_state(None).await.unwrap().snapshot.unwrap();
            if !s.active() {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[test]
fn selection_limits_expiry_and_tcp_port_rules() {
    let (mut c, mut p) = fixture("http://127.0.0.1:18888/path", 101);
    let mut r = request(Mode::AuthorizedHttp);
    bind(&mut r, 101);
    r.attempts_per_site = 100;
    r.normalize().unwrap();
    assert!(
        freeze(&r, &c, std::slice::from_ref(&p), None)
            .unwrap_err()
            .contains("10000")
    );
    r.site_ids = vec!["s0".into()];
    r.permit_bindings.truncate(1);
    r.normalize().unwrap();
    assert_eq!(
        freeze(&r, &c, std::slice::from_ref(&p), None)
            .unwrap()
            .0
            .sites
            .len(),
        1
    );
    p.expires_at = (chrono::Utc::now() + chrono::Duration::seconds(5)).to_rfc3339();
    assert!(freeze(&r, &c, std::slice::from_ref(&p), None).is_err());
    p.expires_at = "2099-01-01T00:00:00Z".into();
    assert!(freeze(&r, &c, std::slice::from_ref(&p), Some(18888)).is_err());
    r.mode = Mode::Tcp;
    r.per_site_connections = Some(1);
    r.tcp_hold_secs = Some(1);
    c.sites[0].site.address = "127.0.0.1".into();
    c.sites[0].site.protocol = site_catalog::Protocol::Tcp;
    assert!(freeze(&r, &c, std::slice::from_ref(&p), None).is_err());
    c.sites[0].site.port = Some(18888);
    assert!(freeze(&r, &c, std::slice::from_ref(&p), None).is_ok());
}
#[tokio::test]
async fn cidr_and_tls_fail_closed_without_http_or_retry() {
    for tls in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let scheme = if tls { "https" } else { "http" };
        let (c, mut p) = fixture(&format!("{scheme}://{addr}/"), 1);
        if !tls {
            p.allowed_ips = vec!["10.0.0.0/8".parse().unwrap()];
        }
        let dir = tempfile::tempdir().unwrap();
        let m = manager(&dir);
        let mut r = request(Mode::AuthorizedHttp);
        r.attempts_per_site = 1;
        bind(&mut r, 1);
        r.normalize().unwrap();
        m.start_catalog(r, &c, &[p]).await.unwrap();
        if tls {
            use tokio_rustls::{
                TlsAcceptor,
                rustls::{
                    self, ServerConfig,
                    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
                },
            };
            let config = ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
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
            let (stream, _) = listener.accept().await.unwrap();
            // A real TLS server with an untrusted fixture CA must fail closed.
            assert!(
                TlsAcceptor::from(Arc::new(config))
                    .accept(stream)
                    .await
                    .is_err()
            );
        }
        let s = done(&m).await;
        assert_eq!(s.sites[0].network_errors, 1);
        assert_eq!(s.sites[0].responses, 0);
        assert_eq!(s.counts.connection_permits, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn per_permit_rate_is_shared_across_sites_and_tcp_early_close_is_not_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut times = Vec::new();
        for _ in 0..3 {
            let (socket, _) = listener.accept().await.unwrap();
            times.push(tokio::time::Instant::now());
            drop(socket);
        }
        times
    });
    let (c, mut p) = fixture(&format!("http://{addr}/"), 3);
    p.max_rps = 2;
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let mut r = request(Mode::Tcp);
    r.start_rate = 2;
    r.attempts_per_site = 1;
    bind(&mut r, 3);
    r.normalize().unwrap();
    m.start_catalog(r, &c, &[p]).await.unwrap();
    let s = done(&m).await;
    let times = server.await.unwrap();
    assert!(
        times
            .windows(2)
            .all(|w| w[1].duration_since(w[0]) >= Duration::from_millis(480))
    );
    assert!(s.sites.iter().all(|s| s.tcp_connected == 1
        && s.early_closed == 1
        && s.attempted == 1
        && s.tcp_failed == 0));
}
#[tokio::test]
async fn request_and_batch_timeouts_are_not_unexecuted_network_failures() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = [0u8; 4096];
        while stream.read(&mut data).await.unwrap() != 0 {}
    });
    let (c, p) = fixture(&format!("http://{addr}/"), 1);
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let mut r = request(Mode::AuthorizedHttp);
    r.concurrency = 1;
    r.connection_limit = 1;
    r.batch_timeout_secs = 1;
    r.request_timeout_secs = 1;
    bind(&mut r, 1);
    r.normalize().unwrap();
    m.start_catalog(r, &c, &[p]).await.unwrap();
    let s = done(&m).await;
    assert_eq!(s.status, "timed_out");
    assert_eq!(s.counts.unexecuted, 2);
    assert_eq!(s.sites[0].timed_out, 1);
    assert_eq!(s.sites[0].cancelled, 0);
    assert_eq!(s.attempts[0].outcome.as_deref(), Some("timed_out"));
    assert_eq!(s.sites[0].network_errors, 0);
    assert_eq!(s.counts.connection_permits, 0);
    server.await.unwrap();
}
#[tokio::test]
async fn full_queue_admission_rotates_sites_under_short_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut seen = 0;
        while seen < 2 {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
            seen += 1;
        }
    });
    let (c, p) = fixture(&format!("http://{addr}/"), 2);
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let mut r = request(Mode::Tcp);
    r.queue_capacity = 256;
    r.attempts_per_site = 100;
    r.start_rate = 2;
    r.batch_timeout_secs = 1;
    bind(&mut r, 2);
    r.normalize().unwrap();
    m.start_catalog(r, &c, &[p]).await.unwrap();
    let s = done(&m).await;
    assert!(s.sites.iter().all(|s| s.attempted >= 1));
    assert_eq!(s.attempts[0].site_id, "s0");
    assert_eq!(s.attempts[1].site_id, "s1");
    server.await.unwrap();
}
#[tokio::test]
async fn stopped_dns_drains_but_app_shutdown_returns_without_claiming_dns_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let m = Arc::new(manager(&dir));
    let dns = Arc::new(crate::catalog_batch_worker::TestDns::default());
    m.set_test_dns(dns.clone());
    let (c, p) = fixture("http://127.0.0.1:18888/", 1);
    let mut r = request(Mode::AuthorizedHttp);
    r.attempts_per_site = 1;
    bind(&mut r, 1);
    r.normalize().unwrap();
    m.start_catalog(r.clone(), &c, &[p]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while dns.pending.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let stopping = m.clone();
    let id = r.request_id.clone();
    let stop = tokio::spawn(async move { stopping.stop_catalog(&id).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if m.catalog_state(None)
                .await
                .unwrap()
                .snapshot
                .unwrap()
                .status
                == "draining"
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!stop.is_finished());
    tokio::time::timeout(Duration::from_secs(1), m.shutdown_and_wait())
        .await
        .unwrap();
    stop.await.unwrap().unwrap();
    let state = m.catalog_state(None).await.unwrap().snapshot.unwrap();
    assert_eq!(state.status, "draining");
    assert!(state.error.unwrap().contains("not cancelled"));
    assert_eq!(dns.pending.load(Ordering::SeqCst), 1);
    dns.release();
    tokio::time::timeout(Duration::from_secs(1), async {
        while dns.pending.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn resolver_inner_deadline_counts_timeout_and_keeps_real_thread_draining() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let dns = Arc::new(crate::catalog_batch_worker::TestDns::default());
    *dns.timeout.lock().unwrap() = Some(Duration::from_millis(20));
    m.set_test_dns(dns.clone());
    let (c, p) = fixture("http://127.0.0.1:18888/", 1);
    let mut r = request(Mode::AuthorizedHttp);
    r.attempts_per_site = 1;
    bind(&mut r, 1);
    r.normalize().unwrap();
    m.start_catalog(r, &c, &[p]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let s = m.catalog_state(None).await.unwrap().snapshot.unwrap();
            if s.status == "draining" {
                assert_eq!(s.sites[0].timed_out, 1);
                assert_eq!(s.sites[0].network_errors, 0);
                assert_eq!(s.attempts[0].outcome.as_deref(), Some("timed_out"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    dns.release();
    assert_eq!(done(&m).await.status, "completed");
}
#[test]
fn strict_parameters_and_precise_permit_matching() {
    let mut r = request(Mode::AuthorizedHttp);
    bind(&mut r, 1);
    r.normalize().unwrap();
    let (mut c, mut p) = fixture("http://127.0.0.1:18080/path", 1);
    assert!(freeze(&r, &c, &[p.clone()], None).is_ok());
    p.url = "http://127.0.0.1:18080/other".into();
    assert!(freeze(&r, &c, &[p.clone()], None).is_err());
    r.mode = Mode::Tcp;
    r.per_site_connections = Some(1);
    r.tcp_hold_secs = Some(1);
    r.normalize().unwrap();
    assert!(freeze(&r, &c, &[p.clone()], None).is_ok());
    p.modes = vec!["http".into()];
    assert!(freeze(&r, &c, &[p.clone()], None).is_err());
    c.revision = 2;
    assert!(freeze(&r, &c, &[p], None).is_err());
    r.mode = Mode::AuthorizedHttp;
    assert!(r.normalize().is_err());
    r.per_site_connections = None;
    r.tcp_hold_secs = None;
    let mut json = serde_json::to_value(&r).unwrap();
    json["tcp_hold_secs"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<Request>(json).is_err());
    let mut json = serde_json::to_value(&r).unwrap();
    json["url"] = serde_json::json!("http://evil");
    assert!(serde_json::from_value::<Request>(json).is_err());
    r.attempts_per_site = usize::MAX;
    assert!(r.normalize().is_err());
}
#[tokio::test]
async fn http_real_connection_bound_head_no_body_no_redirect_and_compaction() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let count = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::new();
    let server_token = token.clone();
    let l = live.clone();
    let p = peak.clone();
    let c = count.clone();
    let server = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! { _=server_token.cancelled()=>break,
                result=listener.accept()=>{ let (mut stream,_)=result.unwrap();let l=l.clone();let p=p.clone();let c=c.clone();children.spawn(async move {
                    let n=l.fetch_add(1,Ordering::SeqCst)+1;p.fetch_max(n,Ordering::SeqCst);c.fetch_add(1,Ordering::SeqCst);
                    let mut b=[0u8;1];let mut head=Vec::new();while !head.ends_with(b"\r\n\r\n") { stream.read_exact(&mut b).await.unwrap();head.push(b[0]); }
                    let header=String::from_utf8(head).unwrap();assert!(header.starts_with("HEAD /fixture HTTP/1.1\r\n"));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /never\r\nConnection: close\r\nContent-Length: 999999\r\n\r\n").await.unwrap();
                    stream.shutdown().await.unwrap();drop(stream);l.fetch_sub(1,Ordering::SeqCst);
                }); }
            }
        }
        while let Some(result) = children.join_next().await {
            result.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let (mut catalog, permit) = fixture(&format!("http://{addr}/fixture"), 3);
    let mut r = request(Mode::AuthorizedHttp);
    bind(&mut r, 3);
    r.normalize().unwrap();
    m.start_catalog(r.clone(), &catalog, std::slice::from_ref(&permit))
        .await
        .unwrap();
    let s = done(&m).await;
    assert_eq!(s.status, "completed");
    assert_eq!(s.attempts.len(), 9);
    assert!(
        s.sites
            .iter()
            .all(|s| s.responses == 3 && s.good_status == 3 && s.network_errors == 0)
    );
    assert_eq!(count.load(Ordering::SeqCst), 9);
    // This gauge covers server-side request handling, not delayed peer EOF delivery.
    assert!(peak.load(Ordering::SeqCst) <= 2);
    assert!(s.counts.peak_connection_permits <= 2);
    assert_eq!(s.counts.connection_permits, 0);
    assert_eq!(
        m.start_catalog(r.clone(), &catalog, std::slice::from_ref(&permit))
            .await
            .unwrap()
            .attempts
            .len(),
        9
    );
    let mut conflict = r.clone();
    conflict.queue_capacity = 2;
    assert!(
        m.start_catalog(conflict, &catalog, std::slice::from_ref(&permit))
            .await
            .is_err()
    );
    let unknown = uuid::Uuid::new_v4().to_string();
    assert!(!m.catalog_state(Some(&unknown)).await.unwrap().accepted);
    assert!(m.stop_catalog(&unknown).await.is_err());
    catalog.revision = 2;
    r.catalog_revision = 2;
    let old = r.request_id.clone();
    r.request_id = uuid::Uuid::new_v4().to_string();
    m.start_catalog(r, &catalog, &[permit]).await.unwrap();
    let compact = m.catalog_state(Some(&old)).await.unwrap().snapshot.unwrap();
    assert!(!compact.details_retained);
    assert!(compact.attempts.is_empty());
    m.shutdown_and_wait().await;
    token.cancel();
    server.await.unwrap();
    assert_eq!(live.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn tcp_per_site_shared_permit_stop_waits_for_sockets_no_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let live = Arc::new(AtomicUsize::new(0));
    let seen = live.clone();
    let token = CancellationToken::new();
    let st = token.clone();
    let server = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! { _=st.cancelled()=>break,result=listener.accept()=>{let(mut socket,_)=result.unwrap();let seen=seen.clone();children.spawn(async move { seen.fetch_add(1,Ordering::SeqCst);let mut byte=[0u8];assert_eq!(socket.read(&mut byte).await.unwrap(),0);seen.fetch_sub(1,Ordering::SeqCst); });}}
        }
        while let Some(r) = children.join_next().await {
            r.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let (catalog, mut permit) = fixture(&format!("http://{addr}/"), 2);
    let mut r = request(Mode::Tcp);
    r.attempts_per_site = 10;
    r.tcp_hold_secs = Some(30);
    r.concurrency = 2;
    bind(&mut r, 2);
    r.normalize().unwrap();
    permit.max_concurrency = 2;
    m.start_catalog(r.clone(), &catalog, &[permit])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while live.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let s = m.stop_catalog(&r.request_id).await.unwrap();
    assert_eq!(s.status, "cancelled");
    assert_eq!(s.counts.connected, 0);
    assert_eq!(s.counts.active, 0);
    assert_eq!(s.counts.peak_connected, 2);
    assert_eq!(s.counts.unexecuted, 18);
    assert!(
        s.sites
            .iter()
            .all(|s| s.peak_connected == 1 && s.network_errors == 0)
    );
    token.cancel();
    server.await.unwrap();
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

fn proxy_route(url: String) -> crate::proxy_settings::RouteSnapshot {
    crate::proxy_settings::RouteSnapshot {
        settings_revision: 7,
        profile_id: Some("fixture".into()),
        profile_name: Some("Controlled proxy".into()),
        proxy: Some(crate::proxy::ProxyConfig { url }),
        local_direct: false,
    }
}
fn allow_proxy(p: &mut Permit, route: &crate::proxy_settings::RouteSnapshot) {
    p.proxy_routes = vec![crate::model::ProxyPermit {
        url: route.proxy.as_ref().unwrap().url.clone(),
        allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
        authorization: "controlled test proxy".into(),
    }];
}
async fn read_header(stream: &mut tokio::net::TcpStream) -> String {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        header.push(byte[0]);
        assert!(header.len() < 16384);
    }
    String::from_utf8(header).unwrap()
}
async fn handshake_proxy(
    stream: &mut tokio::net::TcpStream,
    socks: bool,
    target: std::net::SocketAddr,
) {
    if socks {
        let mut greeting = [0u8; 3];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stream.write_all(&[5, 0]).await.unwrap();
        let mut connect = [0u8; 10];
        stream.read_exact(&mut connect).await.unwrap();
        assert_eq!(&connect[..4], &[5, 1, 0, 1]);
        let std::net::IpAddr::V4(ip) = target.ip() else {
            panic!("IPv4 fixture");
        };
        assert_eq!(&connect[4..8], &ip.octets());
        assert_eq!(&connect[8..], &target.port().to_be_bytes());
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
            .await
            .unwrap();
    } else {
        assert!(
            read_header(stream)
                .await
                .starts_with(&format!("CONNECT {target} HTTP/1.1\r\n"))
        );
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn catalog_http_and_socks_proxy_keep_head_driver_pinning_and_frozen_route() {
    for socks in [false, true] {
        // Never relay: the proxy itself answers. Target address cannot be contacted.
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = proxy.local_addr().unwrap();
        let route = proxy_route(format!(
            "{}://{addr}",
            if socks { "socks5" } else { "http" }
        ));
        let server = tokio::spawn(async move {
            let (mut socket, _) = proxy.accept().await.unwrap();
            handshake_proxy(&mut socket, socks, target_addr).await;
            let header = read_header(&mut socket).await;
            assert!(header.starts_with("HEAD /fixture HTTP/1.1\r\n"));
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains(&format!("host: {target_addr}\r\n"))
            );
            socket
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /never\r\nContent-Length: 999999\r\n\r\n",
                )
                .await
                .unwrap();
            let mut byte = [0u8];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        });
        let (catalog, mut permit) = fixture(&format!("http://{target_addr}/fixture"), 1);
        allow_proxy(&mut permit, &route);
        let dir = tempfile::tempdir().unwrap();
        let m = manager(&dir);
        let mut r = request(Mode::AuthorizedHttp);
        r.attempts_per_site = 1;
        bind(&mut r, 1);
        let initial = m
            .start_catalog_with_route(r.clone(), &catalog, &[permit], route.clone())
            .await
            .unwrap();
        assert_eq!(initial.status, "preparing");
        let s = done(&m).await;
        server.await.unwrap();
        assert_eq!(s.route, route);
        assert_eq!(s.sites[0].responses, 1);
        assert_eq!(s.attempts[0].target_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(s.attempts[0].proxy_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(s.attempts[0].latency_kind, "proxied_http_headers");
        assert!(s.attempts[0].proxy_tcp_ms.is_some());
        assert!(s.attempts[0].tunnel_ms.is_some());
        assert_eq!(s.counts.connection_permits, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), target.accept())
                .await
                .is_err()
        );
        // Idempotency comes before route validation, catalog revision, and permits.
        let repeated = m
            .start_catalog_with_route(
                r.clone(),
                &site_catalog::Snapshot {
                    revision: 999,
                    ..catalog.clone()
                },
                &[],
                proxy_route("invalid-new-route".into()),
            )
            .await
            .unwrap();
        assert_eq!(repeated.route, route);
        assert_eq!(repeated.attempts.len(), 1);
        r.queue_capacity = 2;
        assert!(
            m.start_catalog_with_route(r, &catalog, &[], Default::default())
                .await
                .unwrap_err()
                .contains("different parameters")
        );
    }
}

#[tokio::test]
async fn catalog_missing_proxy_permit_rejects_before_acceptance_and_cidr_failure_never_connects() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let route = proxy_route(format!("socks5://{}", proxy.local_addr().unwrap()));
    let (catalog, mut permit) = fixture(&format!("http://{}/", target.local_addr().unwrap()), 1);
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let mut r = request(Mode::Tcp);
    r.attempts_per_site = 1;
    bind(&mut r, 1);
    assert!(
        m.start_catalog_with_route(r.clone(), &catalog, &[permit.clone()], route.clone())
            .await
            .is_err()
    );
    assert!(!m.catalog_state(Some(&r.request_id)).await.unwrap().accepted);
    allow_proxy(&mut permit, &route);
    permit.proxy_routes[0].allowed_ips = vec!["10.0.0.0/8".parse().unwrap()];
    m.start_catalog_with_route(r, &catalog, &[permit], route)
        .await
        .unwrap();
    let s = done(&m).await;
    assert_eq!(s.status, "failed");
    assert_eq!(s.counts.unexecuted, 1);
    assert_eq!(s.sites[0].attempted, 0);
    assert!(s.error.unwrap().contains("CIDR"));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), proxy.accept())
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), target.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn catalog_proxy_rejection_never_falls_back_and_cancellation_closes_tunnel_socket() {
    for cancel in [false, true] {
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let route = proxy_route(format!("http://{}", proxy.local_addr().unwrap()));
        let (ready, wait) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = proxy.accept().await.unwrap();
            if cancel {
                handshake_proxy(&mut stream, false, target_addr).await;
                let _ = ready.send(());
                let mut byte = [0u8];
                let closed = stream.read(&mut byte).await;
                assert!(
                    matches!(closed, Ok(0))
                        || closed
                            .as_ref()
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset)
                );
            } else {
                read_header(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 502 Refused\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let m = manager(&dir);
        let (catalog, mut permit) = fixture(&format!("http://{target_addr}/"), 1);
        allow_proxy(&mut permit, &route);
        let mut r = request(Mode::Tcp);
        r.attempts_per_site = 1;
        r.tcp_hold_secs = Some(30);
        bind(&mut r, 1);
        m.start_catalog_with_route(r.clone(), &catalog, &[permit], route)
            .await
            .unwrap();
        let s = if cancel {
            wait.await.unwrap();
            m.stop_catalog(&r.request_id).await.unwrap()
        } else {
            done(&m).await
        };
        server.await.unwrap();
        assert_eq!(s.counts.connection_permits, 0);
        assert_eq!(s.counts.active, 0);
        if cancel {
            assert_eq!(s.status, "cancelled");
        } else {
            assert_eq!(s.sites[0].network_errors, 1);
            assert!(s.attempts[0].error.as_ref().unwrap().contains("502"));
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), target.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn catalog_proxy_preparation_dns_failure_stays_draining_until_real_thread_exits() {
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&dir);
    let dns = Arc::new(crate::catalog_batch_worker::TestDns::default());
    *dns.timeout.lock().unwrap() = Some(Duration::from_millis(20));
    m.set_test_proxy_dns(dns.clone());
    let route = proxy_route("http://controlled.invalid:8123".into());
    let (catalog, mut permit) = fixture("http://127.0.0.1:18888/", 1);
    allow_proxy(&mut permit, &route);
    let mut r = request(Mode::Tcp);
    r.attempts_per_site = 1;
    bind(&mut r, 1);
    let initial = m
        .start_catalog_with_route(r.clone(), &catalog, &[permit], route)
        .await
        .unwrap();
    assert_eq!(initial.status, "preparing");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let s = m.catalog_state(None).await.unwrap().snapshot.unwrap();
            if s.status == "draining" {
                assert_eq!(s.sites[0].attempted, 0);
                assert!(s.error.unwrap().contains("DNS"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(m.settle().await.is_err());
    dns.release();
    assert_eq!(done(&m).await.status, "failed");
    assert_eq!(
        m.catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .unwrap()
            .counts
            .unexecuted,
        1
    );
}
