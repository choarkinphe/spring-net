use crate::{
    connections,
    model::{ConnectionPreset, Permit, RunConfig},
    policy,
    service::{AppService, ServiceConfig},
    storage,
};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path};
use tokio_util::sync::CancellationToken;

fn catalog() -> Vec<ConnectionPreset> {
    connections::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/connections")).unwrap()
}
fn first() -> Value {
    serde_json::to_value(catalog().remove(0)).unwrap()
}
fn load_values(values: Value) -> Result<Vec<ConnectionPreset>, String> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("test.json"),
        serde_json::to_vec(&values).unwrap(),
    )
    .unwrap();
    connections::load(dir.path())
}
fn permit(url: &str) -> Permit {
    Permit {
        id: "owned-fixture".into(),
        name: "Owned fixture".into(),
        url: url.into(),
        modes: vec!["http".into()],
        allowed_ips: vec!["192.0.2.1/32".parse().unwrap()],
        max_concurrency: 1,
        max_rps: 1,
        max_duration: 1,
        authorization: "Local validation only; no traffic".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        proxy_routes: vec![],
    }
}
fn config(mode: &str) -> RunConfig {
    RunConfig {
        mode: mode.into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: vec![],
        concurrency: 1,
        rps: 1,
        duration_secs: 1,
        timeout_secs: 1,
        ramp_secs: 0,
        fresh_connections: false,
        proxy: None,
    }
}
async fn service(root: &Path, auth: Value) -> (tempfile::TempDir, AppService) {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");
    std::fs::write(&auth_file, serde_json::to_vec(&auth).unwrap()).unwrap();
    let service = AppService::initialize(
        ServiceConfig {
            root: root.into(),
            data: dir.path().join("data"),
            auth_file,
            control_port: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    (dir, service)
}

#[test]
fn six_cards_keep_product_boundaries_and_all_cloud_rules() {
    let cards = catalog();
    assert_eq!(cards.len(), 6);
    assert_eq!(cards.iter().map(|p| &p.id).collect::<HashSet<_>>().len(), 6);
    let cloud = cards
        .iter()
        .find(|p| p.id == "travelport-smartpoint-cloud")
        .unwrap();
    assert_eq!(cloud.endpoints.len(), 24);
    let rules: Vec<_> = cloud
        .endpoints
        .iter()
        .filter(|e| e.source_ids == ["cloud-rules"])
        .collect();
    assert_eq!(rules.len(), 23);
    assert!(
        rules
            .iter()
            .all(|e| e.port == Some(443) && e.protocol.is_none())
    );
    assert!(
        rules
            .iter()
            .any(|e| e.address.as_deref() == Some("*.browser-intake-datadoghq.eu"))
    );
    let desktop = cards
        .iter()
        .find(|p| p.id == "travelport-smartpoint-desktop")
        .unwrap();
    assert!(
        desktop
            .endpoints
            .iter()
            .all(|e| e.address.is_none() && e.port.is_none() && e.protocol.is_none())
    );
    assert_eq!(desktop.version.as_deref(), Some("12"));
    let air = cards
        .iter()
        .find(|p| p.id == "travelport-air-api-v11")
        .unwrap();
    assert_eq!(air.endpoints.len(), 4);
    assert!(
        air.endpoints
            .iter()
            .all(|e| e.address.as_ref().unwrap().contains("travelport.net/"))
    );
    let ibe = cards.iter().find(|p| p.id == "travelsky-ibe-plus").unwrap();
    assert!(ibe.endpoints.iter().all(|e| e.address.is_none()));
    assert_eq!(ibe.sources[0].published_at.as_deref(), Some("2014-06-23"));
    assert!(
        cards
            .iter()
            .flat_map(|p| &p.sources)
            .all(|s| s.checked_at == "2026-10-27")
    );
}

#[test]
fn strict_schema_rejects_execution_fields_at_every_level() {
    for (level, key) in [
        ("card", "targets"),
        ("endpoint", "method"),
        ("source", "headers"),
    ] {
        let mut value = first();
        match level {
            "card" => value[key] = json!([]),
            "endpoint" => value["endpoints"][0][key] = json!("GET"),
            _ => value["sources"][0][key] = json!({}),
        }
        assert!(load_values(json!([value])).is_err(), "{level}");
    }
    let mut value = first();
    value["endpoints"][0]["kind"] = json!("tcp");
    assert!(load_values(json!([value])).is_err());
    let mut run = serde_json::to_value(config("probe")).unwrap();
    run["connection_ids"] = json!(["travelport-smartpoint-cloud"]);
    assert!(serde_json::from_value::<RunConfig>(run).is_err());
}

#[test]
fn source_references_dates_ids_and_duplicates_are_checked() {
    let value = first();
    assert!(load_values(json!([value.clone(), value.clone()])).is_err());
    for (field, bad) in [
        ("source_ids", json!(["nonexistent"])),
        ("port", json!(0)),
        ("address", json!("*.example.com")),
    ] {
        let mut changed = value.clone();
        changed["endpoints"][0][field] = bad;
        assert!(load_values(json!([changed])).is_err(), "{field}");
    }
    let mut changed = value.clone();
    changed["sources"][0]["checked_at"] = json!("2026-02-30");
    assert!(load_values(json!([changed])).is_err());
    let mut changed = value.clone();
    changed["sources"]
        .as_array_mut()
        .unwrap()
        .push(value["sources"][0].clone());
    assert!(load_values(json!([changed])).is_err());
    let mut changed = value.clone();
    changed["endpoints"]
        .as_array_mut()
        .unwrap()
        .push(value["endpoints"][0].clone());
    assert!(load_values(json!([changed])).is_err());
    let mut changed = value;
    changed["endpoints"][0]["port"] = json!(8443);
    assert!(load_values(json!([changed])).is_err());
}

#[test]
fn unsafe_links_and_malformed_host_rules_fail_without_network() {
    for address in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "http://example.com/",
        "https://u:p@example.com/",
        "https://example.com/#fragment",
        "https://example.com/?token=secret",
        "https://127.0.0.1/",
        "https://localhost/",
        "https://example.com\\evil",
        "https://example.com/\n",
    ] {
        for field in ["source", "endpoint"] {
            let mut value = first();
            if field == "source" {
                value["sources"][0]["url"] = json!(address);
            } else {
                value["endpoints"][0]["address"] = json!(address);
            }
            assert!(load_values(json!([value])).is_err(), "{field}: {address}");
        }
    }
    for address in [
        "*.com",
        "*example.com",
        "*.example.com/path",
        "https://example.com",
        "*.example.com:443",
    ] {
        let mut value = first();
        value["endpoints"][0]["kind"] = json!("wildcard");
        value["endpoints"][0]["address"] = json!(address);
        assert!(load_values(json!([value])).is_err(), "{address}");
    }
}

#[test]
fn bounded_loader_isolated_missing_empty_and_corrupt_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        connections::load(&dir.path().join("missing"))
            .unwrap()
            .is_empty()
    );
    assert!(connections::load(dir.path()).unwrap().is_empty());
    std::fs::write(dir.path().join("not-a-catalog.txt"), "ignored").unwrap();
    assert!(connections::load(dir.path()).unwrap().is_empty());
    std::fs::write(dir.path().join("bad.json"), "not json").unwrap();
    assert!(connections::load(dir.path()).is_err());
    std::fs::write(dir.path().join("bad.json"), vec![b' '; 262_145]).unwrap();
    assert!(connections::load(dir.path()).is_err());
    std::fs::remove_file(dir.path().join("bad.json")).unwrap();
    for n in 0..33 {
        std::fs::write(dir.path().join(format!("{n}.json")), "[]").unwrap();
    }
    assert!(connections::load(dir.path()).is_err());
    let presets = Path::new(env!("CARGO_MANIFEST_DIR")).join("presets");
    let old = storage::load_presets(&presets).unwrap();
    assert_eq!(old.len(), 7);
    assert_eq!(old.iter().map(|l| l.targets.len()).sum::<usize>(), 55);
    assert!(serde_json::from_value::<crate::model::SiteList>(first()).is_err());
}

#[cfg(unix)]
#[test]
fn loader_rejects_symlinks() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    symlink(&target, dir.path().join("linked")).unwrap();
    assert!(connections::load(&dir.path().join("linked")).is_err());
    std::fs::write(dir.path().join("actual"), "[]").unwrap();
    symlink(dir.path().join("actual"), target.join("linked.json")).unwrap();
    assert!(connections::load(&target).is_err());
}

#[tokio::test]
async fn bootstrap_reference_only_and_catalog_ids_cannot_execute() {
    let (_dir, s) = service(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        json!({"version":1,"targets":[]}),
    )
    .await;
    let bootstrap = s.bootstrap("test").await;
    assert_eq!(bootstrap["connection_presets"].as_array().unwrap().len(), 6);
    assert_eq!(bootstrap["lists"].as_array().unwrap().len(), 7);
    assert_eq!(
        s.lists
            .read()
            .await
            .iter()
            .map(|l| l.targets.len())
            .sum::<usize>(),
        55
    );
    for card in catalog() {
        let mut run = config("probe");
        run.list_ids = vec![card.id.clone()];
        assert!(s.start(run).await.is_err());
        let mut run = config("probe");
        run.target_ids = vec![card.id.clone()];
        assert!(s.start(run).await.is_err());
        let mut run = config("http");
        run.authorized_ids = vec![card.id];
        assert!(s.start(run).await.is_err());
    }
    assert!(s.engine.snapshot().await.is_none());
}

#[tokio::test]
async fn vendor_protection_applies_before_initial_permit_load_and_to_prepare_reload() {
    let blocked = permit("https://api.pp.travelport.net/11/air/");
    let (_dir, s) = service(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        json!({"version":1,"targets":[blocked]}),
    )
    .await;
    assert!(s.bootstrap("").await["authorization_error"].is_string());
    assert!(
        s.bootstrap("").await["authorized"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(s.reload_permits().await.is_err());
    for domain in [
        "sp.travelport.com",
        "api.travelport.net",
        "goprivate.wspan.com",
        "smartpoint-cloud.tvptcloud.io",
        "workspace.sabre.com",
        "sellingplatformconnect.amadeus.com",
        "travelsky.cn",
    ] {
        assert!(
            s.prepare_permits(vec![permit(&format!("https://{domain}/"))])
                .await
                .is_err(),
            "{domain}"
        );
    }
    for domain in [
        "owned.azure.com",
        "owned.azureedge.net",
        "owned.googleapis.com",
        "owned.gstatic.com",
        "owned.splunkcloud.com",
        "owned.browser-intake-datadoghq.eu",
    ] {
        assert!(
            s.prepare_permits(vec![permit(&format!("https://{domain}/"))])
                .await
                .is_ok(),
            "{domain}"
        );
    }
    let keys = connections::protected_domain_keys(&catalog()).unwrap();
    assert!(!keys.contains(&policy::domain_key(
        &policy::parse_url("https://owned.azure.com/").unwrap()
    )));
    assert!(s.engine.snapshot().await.is_none());
}

#[tokio::test]
async fn legacy_fixture_root_without_connection_directory_has_no_fake_catalog() {
    let root = tempfile::tempdir().unwrap();
    let presets = root.path().join("presets");
    std::fs::create_dir(&presets).unwrap();
    for entry in std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("presets")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            std::fs::copy(entry.path(), presets.join(entry.file_name())).unwrap();
        }
    }
    let (_dir, s) = service(root.path(), json!({"version":1,"targets":[]})).await;
    let bootstrap = s.bootstrap("").await;
    assert!(
        bootstrap["connection_presets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.lists
            .read()
            .await
            .iter()
            .map(|l| l.targets.len())
            .sum::<usize>(),
        55
    );
}
