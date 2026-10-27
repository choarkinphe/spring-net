use crate::{
    diagnostics::DiagnosticConfig,
    service::{AppService, ServiceConfig},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

async fn service() -> (tempfile::TempDir, AppService) {
    let directory = tempfile::tempdir().unwrap();
    let auth_file = directory.path().join("auth.json");
    std::fs::write(&auth_file, r#"{"version":1,"targets":[]}"#).unwrap();
    let service = AppService::initialize(
        ServiceConfig {
            root: env!("CARGO_MANIFEST_DIR").into(),
            data: directory.path().join("data"),
            auth_file,
            control_port: Some(1),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    (directory, service)
}
fn diagnostic(target: &str, port: u16) -> DiagnosticConfig {
    DiagnosticConfig {
        request_id: uuid::Uuid::new_v4().to_string(),
        target: target.into(),
        port,
        tls: false,
        allow_lan: false,
        proxy: None,
    }
}
#[tokio::test]
async fn bootstrap_reads_are_inert_and_diagnostic_page_roundtrips() {
    let (_dir, s) = service().await;
    let bootstrap = s.bootstrap("test").await;
    assert_eq!(bootstrap["diagnostics"]["enabled"], true);
    assert_eq!(bootstrap["diagnostics"]["limits"]["max_hops"], 20);
    assert!(
        bootstrap["diagnostics"]["self_test_targets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(s.diagnostic_state().await.is_none());
    assert!(s.engine.snapshot().await.is_none());
    s.workspace(
        "settings.save",
        json!({"settings":{"page":"diagnostics"}}),
        "test",
    )
    .await
    .unwrap();
    assert_eq!(
        s.workspace("settings.get", json!({}), "test")
            .await
            .unwrap()["page"],
        "diagnostics"
    );
    assert!(s.export_run("diagnostic", None).await.is_err());
    assert!(
        s.export_run("diagnostic", Some(&uuid::Uuid::new_v4().to_string()))
            .await
            .is_err()
    );
    s.shutdown_and_wait().await;
}
#[tokio::test]
async fn experiment_and_diagnostic_admission_are_reciprocally_exclusive() {
    let (_dir, s) = service().await;
    let bootstrap = s
        .workspace("selftest", json!({"enabled":true}), "test")
        .await
        .unwrap();
    let port = bootstrap["diagnostics"]["self_test_targets"][0]["port"]
        .as_u64()
        .unwrap() as u16;
    let mut config = diagnostic("127.0.0.1", port);
    config.tls = true; // Owned plain TCP listener intentionally cannot complete TLS.
    let first = s.start_diagnostic(config).await.unwrap();
    let run = crate::model::RunConfig {
        mode: "tcp".into(),
        list_ids: vec![],
        target_ids: vec![],
        authorized_ids: vec!["demo-tcp".into()],
        concurrency: 1,
        rps: 1,
        duration_secs: 10,
        timeout_secs: 5,
        ramp_secs: 0,
        fresh_connections: false,
        proxy: None,
    };
    assert!(s.start(run.clone()).await.is_err());
    assert!(
        s.workspace("selftest", json!({"enabled":false}), "test")
            .await
            .is_err()
    );
    s.stop_diagnostic(&first.id).await.unwrap();
    for _ in 0..100 {
        if s.diagnostics.settle().await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    s.start(run).await.unwrap();
    assert!(
        s.start_diagnostic(diagnostic("127.0.0.1", port))
            .await
            .is_err()
    );
    s.engine.stop().await;
    s.shutdown_and_wait().await;
}
#[tokio::test]
async fn owned_self_test_exception_requires_explicit_enable_and_does_not_grant_control_access() {
    let (_dir, s) = service().await;
    assert!(
        s.start_diagnostic(diagnostic("127.0.0.1", 1))
            .await
            .is_err()
    );
    let bootstrap = s
        .workspace("selftest", json!({"enabled":true}), "test")
        .await
        .unwrap();
    let target = &bootstrap["diagnostics"]["self_test_targets"][0];
    assert_eq!(target["target"], "127.0.0.1");
    let port = target["port"].as_u64().unwrap() as u16;
    let config = diagnostic("127.0.0.1", port);
    let first = s.start_diagnostic(config.clone()).await.unwrap();
    let duplicate = s.start_diagnostic(config.clone()).await.unwrap();
    assert_eq!(first.id, duplicate.id);
    let mut changed = config;
    changed.tls = true;
    assert!(s.start_diagnostic(changed).await.is_err());
    assert!(
        s.stop_diagnostic(&uuid::Uuid::new_v4().to_string())
            .await
            .is_err()
    );
    let _ = s.stop_diagnostic(&first.id).await.unwrap();
    for _ in 0..100 {
        if !s.diagnostic_state().await.unwrap().active() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!s.diagnostic_state().await.unwrap().active());
    let report = s.export_run("diagnostic", Some(&first.id)).await.unwrap();
    let text = String::from_utf8(report).unwrap();
    assert!(text.contains(&first.id));
    assert!(!text.contains("desktop-local-ipc"));
    s.shutdown_and_wait().await;
}
