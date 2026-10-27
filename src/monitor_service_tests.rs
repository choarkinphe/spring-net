use crate::{
    monitor::{MonitorLifetime, MonitorTarget},
    service::{AppService, ServiceConfig},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

async fn service() -> (tempfile::TempDir, AppService) {
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    std::fs::write(&auth, r#"{"version":1,"targets":[]}"#).unwrap();
    let s = AppService::initialize(
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
    (dir, s)
}
fn target() -> MonitorTarget {
    MonitorTarget {
        id: "one".into(),
        name: "fixture".into(),
        group: "本地".into(),
        host: "127.0.0.1".into(),
        port: 2,
        tls: false,
        allow_lan: false,
        interval_secs: 60,
    }
}
#[tokio::test]
async fn monitor_persists_configuration_but_never_auto_starts_and_locks_other_work() {
    let (dir, s) = service().await;
    assert!(!s.monitor_state()["active"].as_bool().unwrap());
    let saved = s.monitor_save(0, vec![target()]).await.unwrap();
    assert_eq!(saved["config"]["revision"], 1);
    assert!(!saved["active"].as_bool().unwrap());
    let id = uuid::Uuid::new_v4().to_string();
    let started = s
        .monitor_start(&id, 1, None, MonitorLifetime::Desktop)
        .await
        .unwrap();
    assert_eq!(started["generation"], id);
    assert_eq!(
        s.monitor_start(&id, 1, None, MonitorLifetime::Desktop)
            .await
            .unwrap()["generation"],
        id
    );
    assert!(
        s.monitor_start(&id, 2, None, MonitorLifetime::Desktop)
            .await
            .is_err()
    );
    assert!(
        s.monitor_start(&id, 1, None, MonitorLifetime::Browser)
            .await
            .is_err()
    );
    assert!(
        s.monitor_start(
            &id,
            1,
            Some(crate::proxy::ProxyConfig {
                url: "http://127.0.0.1:8099".into()
            }),
            MonitorLifetime::Desktop
        )
        .await
        .is_err()
    );
    assert!(s.monitor_save(1, vec![]).await.is_err());
    assert!(
        s.workspace("selftest", json!({"enabled":true}), "test")
            .await
            .is_err()
    );
    assert!(
        s.monitor_stop(&uuid::Uuid::new_v4().to_string())
            .await
            .is_err()
    );
    s.monitor_stop(&id).await.unwrap();
    assert!(!s.monitor_state()["active"].as_bool().unwrap());
    s.shutdown_and_wait().await;
    let auth = dir.path().join("auth.json");
    let fresh = AppService::initialize(
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
    assert_eq!(
        fresh.monitor_state()["config"]["targets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fresh.monitor_state()["active"], false);
    fresh.shutdown_and_wait().await;
}

#[tokio::test]
async fn proxy_permits_survive_preview_save_reload_and_bootstrap() {
    let (_dir, s) = service().await;
    let permit = crate::model::Permit {
        id: "owned".into(),
        name: "Owned".into(),
        url: "http://owned.test/".into(),
        modes: vec!["tcp".into()],
        allowed_ips: vec!["192.0.2.1/32".parse().unwrap()],
        max_concurrency: 1,
        max_rps: 1,
        max_duration: 1,
        authorization: "controlled destination".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        proxy_routes: vec![crate::model::ProxyPermit {
            url: "http://LOCALHOST:8080/".into(),
            allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
            authorization: " controlled proxy ".into(),
        }],
    };
    let preview = s.prepare_permits(vec![permit]).await.unwrap();
    assert_eq!(
        preview["targets"][0]["proxy_routes"][0]["url"],
        "http://localhost:8080"
    );
    s.confirm_permits(
        preview["draftToken"].as_str().unwrap(),
        preview["revision"].as_u64().unwrap(),
    )
    .await
    .unwrap();
    s.reload_permits().await.unwrap();
    assert_eq!(
        s.bootstrap("test").await["authorized"][0]["proxy_routes"][0]["authorization"],
        "controlled proxy"
    );
    assert!(s.engine.snapshot().await.is_none());
    s.shutdown_and_wait().await;
}
#[tokio::test]
async fn proxy_settings_are_inert_canonical_and_secret_free() {
    let (_dir, s) = service().await;
    let saved = s
        .workspace(
            "settings.save",
            json!({"settings":{"http_proxy":{"url":"http://LOCALHOST:8080/"}}}),
            "test",
        )
        .await
        .unwrap();
    assert_eq!(saved["http_proxy"]["url"], "http://localhost:8080");
    let read = s
        .workspace("settings.get", json!({}), "test")
        .await
        .unwrap();
    assert_eq!(read["http_proxy"], saved["http_proxy"]);
    for url in [
        "http://user:password@localhost:8080",
        "http://localhost:1",
        "socks5://localhost:8080",
        "http://localhost:8080/path",
    ] {
        assert!(
            s.workspace(
                "settings.save",
                json!({"settings":{"http_proxy":{"url":url}}}),
                "test"
            )
            .await
            .is_err()
        );
    }
    assert_eq!(s.monitor_state()["active"], false);
    assert!(s.engine.snapshot().await.is_none());
    assert!(s.diagnostic_state().await.is_none());
    s.shutdown_and_wait().await;
}
