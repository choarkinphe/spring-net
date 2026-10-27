use crate::{
    model::{Permit, RunConfig},
    service::{AppService, ServiceConfig},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

async fn service() -> (tempfile::TempDir, AppService) {
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    std::fs::write(&auth, r#"{"version":1,"targets":[]}"#).unwrap();
    let service = AppService::initialize(
        ServiceConfig {
            root: env!("CARGO_MANIFEST_DIR").into(),
            data: dir.path().join("data"),
            auth_file: auth,
            control_port: Some(1),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    (dir, service)
}
fn permit() -> Permit {
    Permit {
        id: "controlled".into(),
        name: "Controlled fixture".into(),
        url: "http://127.0.0.1:2/".into(),
        modes: vec!["http".into()],
        allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
        max_concurrency: 2,
        max_rps: 2,
        max_duration: 5,
        authorization: "Owned regression fixture, no traffic from saving".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        proxy_routes: vec![],
    }
}
#[tokio::test]
async fn permit_prepare_is_no_write_confirm_is_idempotent_and_revision_bound() {
    let (dir, s) = service().await;
    let old = std::fs::read(dir.path().join("auth.json")).unwrap();
    let a = s.prepare_permits(vec![permit()]).await.unwrap();
    let b = s.prepare_permits(vec![permit()]).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("auth.json")).unwrap(), old);
    assert!(
        s.confirm_permits(
            a["draftToken"].as_str().unwrap(),
            a["revision"].as_u64().unwrap()
        )
        .await
        .is_err()
    );
    let token = b["draftToken"].as_str().unwrap();
    let revision = b["revision"].as_u64().unwrap();
    s.confirm_permits(token, revision).await.unwrap();
    let saved = std::fs::read(dir.path().join("auth.json")).unwrap();
    s.confirm_permits(token, revision).await.unwrap();
    assert_eq!(std::fs::read(dir.path().join("auth.json")).unwrap(), saved);
    assert!(s.confirm_permits(token, revision + 99).await.is_err());
    assert!(s.engine.snapshot().await.is_none());
    assert_eq!(
        s.bootstrap("test").await["authorized"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn expired_permits_are_editable_and_invalid_reload_preserves_revision() {
    let (dir, s) = service().await;
    let mut p = permit();
    p.expires_at = "2000-01-01T00:00:00Z".into();
    let a = s.prepare_permits(vec![p]).await.unwrap();
    s.confirm_permits(
        a["draftToken"].as_str().unwrap(),
        a["revision"].as_u64().unwrap(),
    )
    .await
    .unwrap();
    let old = s.bootstrap("test").await;
    assert_eq!(old["authorized"][0]["expired"], true);
    std::fs::write(dir.path().join("auth.json"), "broken").unwrap();
    assert!(s.reload_permits().await.is_err());
    let new = s.bootstrap("test").await;
    assert_eq!(old["authorization_revision"], new["authorization_revision"]);
    assert_eq!(old["authorized"], new["authorized"]);
    assert!(new["authorization_error"].is_string());
    let config = RunConfig {
        mode: "http".into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: vec!["controlled".into()],
        concurrency: 1,
        rps: 1,
        duration_secs: 1,
        timeout_secs: 1,
        ramp_secs: 0,
        fresh_connections: false,
        proxy: None,
    };
    assert!(s.start(config).await.unwrap_err().contains("需要修复"));
}
#[tokio::test]
async fn external_authorization_edit_invalidates_prepared_confirmation() {
    let (dir, s) = service().await;
    let draft = s.prepare_permits(vec![permit()]).await.unwrap();
    let edited = r#"{"version":1,"targets":[]}\n"#;
    std::fs::write(dir.path().join("auth.json"), edited).unwrap();
    assert!(
        s.confirm_permits(
            draft["draftToken"].as_str().unwrap(),
            draft["revision"].as_u64().unwrap()
        )
        .await
        .unwrap_err()
        .contains("外部修改")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("auth.json")).unwrap(),
        edited
    );
}
#[tokio::test]
async fn malformed_authorization_does_not_brick_workspace_or_overwrite_file() {
    let (dir, s) = service().await;
    s.shutdown_and_wait().await;
    std::fs::write(dir.path().join("auth.json"), "broken").unwrap();
    let reopened = AppService::initialize(
        ServiceConfig {
            root: env!("CARGO_MANIFEST_DIR").into(),
            data: dir.path().join("data"),
            auth_file: dir.path().join("auth.json"),
            control_port: Some(1),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(reopened.bootstrap("").await["authorization_error"].is_string());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("auth.json")).unwrap(),
        "broken"
    );
}
#[tokio::test]
async fn profiles_settings_roundtrip_without_auto_start_and_demo_rejection() {
    let (_dir, s) = service().await;
    let config = RunConfig {
        mode: "http".into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: vec!["controlled".into()],
        concurrency: 1,
        rps: 1,
        duration_secs: 1,
        timeout_secs: 1,
        ramp_secs: 0,
        fresh_connections: false,
        proxy: None,
    };
    let profiles = json!([{"id":"first","name":"Fixture plan","config":config}]);
    s.workspace("profiles.save", json!({"profiles":profiles}), "")
        .await
        .unwrap();
    assert_eq!(
        s.workspace("profiles.list", json!({}), "").await.unwrap(),
        profiles
    );
    let mut invalid = profiles.clone();
    invalid[0]["config"]["authorized_ids"] = json!(["demo-http"]);
    assert!(
        s.workspace("profiles.save", json!({"profiles":invalid}), "")
            .await
            .is_err()
    );
    let settings = s
        .workspace(
            "settings.save",
            json!({"settings":{"dock_width":9999,"token":"not persisted"}}),
            "",
        )
        .await
        .unwrap();
    assert_eq!(settings["dock_width"], 600);
    assert!(settings.get("token").is_none());
    for page in ["batch", "load", "monitor"] {
        let saved = s
            .workspace("settings.save", json!({"settings":{"page":page}}), "")
            .await
            .unwrap();
        assert_eq!(saved["page"], page);
        assert_eq!(
            s.workspace("settings.get", json!({}), "").await.unwrap()["page"],
            page
        );
    }
    assert!(s.engine.snapshot().await.is_none());
}
#[tokio::test]
async fn real_run_archives_before_terminal_and_rejects_mutation_during_work() {
    let (_dir, s) = service().await;
    s.set_self_test(true).await.unwrap();
    let config = RunConfig {
        mode: "http".into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: vec!["demo-http".into()],
        concurrency: 1,
        rps: 1,
        duration_secs: 1,
        timeout_secs: 2,
        ramp_secs: 0,
        fresh_connections: false,
        proxy: None,
    };
    let first = s.start(config.clone()).await.unwrap();
    assert!(s.prepare_permits(vec![]).await.is_err());
    assert!(s.save_lists(vec![]).await.is_err());
    assert!(s.set_self_test(false).await.is_err());
    for _ in 0..100 {
        if s.engine.snapshot().await.is_some_and(|r| !r.active()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    let snapshot = s.engine.snapshot().await.unwrap();
    assert!(!snapshot.active());
    assert!(snapshot.responses > 0);
    let archived = s
        .workspace("history.get", json!({"id":first.id}), "")
        .await
        .unwrap();
    assert_eq!(archived["responses"], snapshot.responses);
    s.start(config).await.unwrap();
    s.engine.stop().await;
    s.engine.settle().await.err();
    s.shutdown_and_wait().await;
    assert_eq!(
        s.workspace("history.list", json!({}), "")
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let export = s
        .workspace("history.export", json!({"id":first.id,"kind":"json"}), "")
        .await
        .unwrap();
    let run: serde_json::Value = serde_json::from_str(export["content"].as_str().unwrap()).unwrap();
    assert_eq!(run["id"], first.id);
}
#[tokio::test]
async fn qualified_probe_references_reject_unknown_and_ambiguous_ids_before_io() {
    use crate::model::{SiteList, Target};
    let (_dir, s) = service().await;
    let target = Target {
        id: "same".into(),
        name: "Fixture".into(),
        url: "http://127.0.0.1/".into(),
        region: String::new(),
        source: String::new(),
        checked_at: String::new(),
        probe_only: true,
    };
    let lists = vec![
        SiteList {
            id: "a".into(),
            name: "a".into(),
            description: String::new(),
            builtin: false,
            targets: vec![target.clone()],
        },
        SiteList {
            id: "b".into(),
            name: "b".into(),
            description: String::new(),
            builtin: false,
            targets: vec![target],
        },
    ];
    for (ids, expected) in [
        (vec!["same"], "歧义"),
        (vec!["missing"], "未知"),
        (vec!["a/same"], "公共"),
    ] {
        let config = RunConfig {
            mode: "probe".into(),
            list_ids: vec!["a".into(), "b".into()],
            target_ids: ids.into_iter().map(String::from).collect(),
            authorized_ids: vec![],
            concurrency: 1,
            rps: 1,
            duration_secs: 1,
            timeout_secs: 1,
            ramp_secs: 0,
            fresh_connections: false,
            proxy: None,
        };
        assert!(
            s.engine
                .start(config, &lists, &[])
                .await
                .unwrap_err()
                .contains(expected)
        );
    }
    assert!(s.engine.snapshot().await.is_none());
}
#[tokio::test]
async fn authorization_rejects_reserved_ids_public_domains_and_broad_cidrs() {
    let (_dir, s) = service().await;
    for (id, url, cidr) in [
        ("demo-http", "http://127.0.0.1:2/", "127.0.0.1/32"),
        ("public", "https://www.ctrip.com/", "1.2.3.4/32"),
        ("broad", "http://127.0.0.1:2/", "0.0.0.0/0"),
    ] {
        let mut p = permit();
        p.id = id.into();
        p.url = url.into();
        p.allowed_ips = vec![cidr.parse().unwrap()];
        assert!(s.prepare_permits(vec![p]).await.is_err());
    }
}
