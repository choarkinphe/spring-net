use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fake(AtomicUsize);
impl MonitorWorker for Fake {
    fn check(&self, _: MonitorTarget) -> Pin<Box<dyn Future<Output = MonitorOutcome> + Send + '_>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            MonitorOutcome {
                success: false,
                tcp_ok: Some(false),
                tls_ok: None,
                elapsed_ms: Some(1),
                detail: "fixture".into(),
                path: "fake".into(),
            }
        })
    }
}
fn target() -> MonitorTarget {
    MonitorTarget {
        id: "one".into(),
        name: "One".into(),
        group: "test".into(),
        host: "fixture.invalid".into(),
        port: 443,
        tls: true,
        allow_lan: false,
        interval_secs: 60,
    }
}
#[tokio::test(start_paused = true)]
async fn explicit_start_lease_and_history_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let manager = MonitorManager::new(dir.path(), None, CancellationToken::new()).unwrap();
    let worker = Arc::new(Fake(AtomicUsize::new(0)));
    manager.save_config(0, vec![target()]).unwrap();
    tokio::task::yield_now().await;
    assert_eq!(worker.0.load(Ordering::SeqCst), 0);
    let generation = Uuid::new_v4();
    manager
        .start(generation, 1, worker.clone(), MonitorLifetime::Desktop)
        .unwrap();
    tokio::task::yield_now().await;
    for _ in 0..25 {
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
    }
    let status = &manager.snapshot().targets["one"];
    assert_eq!(status.history.len(), 20);
    assert_eq!(status.consecutive_failures, 26);
    assert_eq!(status.history_dropped, 6);
    manager.stop(generation).await.unwrap();
    assert!(
        manager
            .start(generation, 1, worker.clone(), MonitorLifetime::Desktop)
            .is_err()
    );
    let next = Uuid::new_v4();
    manager
        .start(next, 1, worker, MonitorLifetime::Browser)
        .unwrap();
    assert!(manager.heartbeat(generation).is_err());
    tokio::time::advance(Duration::from_secs(16)).await;
    tokio::task::yield_now().await;
    assert!(!manager.snapshot().active);
    manager.stop(next).await.unwrap();
    manager.stop(next).await.unwrap();
    let newer = Uuid::new_v4();
    manager
        .start(
            newer,
            1,
            Arc::new(Fake(AtomicUsize::new(0))),
            MonitorLifetime::Desktop,
        )
        .unwrap();
    assert!(manager.stop(next).await.is_err());
    assert!(manager.snapshot().active);
    manager.stop(newer).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn duplicate_start_must_match_revision_and_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let manager = MonitorManager::new(dir.path(), None, CancellationToken::new()).unwrap();
    manager.save_config(0, vec![target()]).unwrap();
    let worker = Arc::new(Fake(AtomicUsize::new(0)));
    let id = Uuid::new_v4();
    assert_eq!(
        manager
            .start(id, 1, worker.clone(), MonitorLifetime::Browser)
            .unwrap(),
        id
    );
    assert_eq!(
        manager
            .start(id, 1, worker.clone(), MonitorLifetime::Browser)
            .unwrap(),
        id
    );
    assert!(
        manager
            .start(id, 2, worker.clone(), MonitorLifetime::Browser)
            .is_err()
    );
    assert!(
        manager
            .start(id, 1, worker.clone(), MonitorLifetime::Desktop)
            .is_err()
    );
    tokio::time::advance(LEASE).await;
    assert!(
        manager
            .start(id, 1, worker, MonitorLifetime::Browser)
            .is_err()
    );
    assert!(manager.heartbeat(id).is_err());
    manager.stop(id).await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn saving_config_clears_stale_target_deadlines() {
    let dir = tempfile::tempdir().unwrap();
    let manager = MonitorManager::new(dir.path(), None, CancellationToken::new()).unwrap();
    manager.save_config(0, vec![target()]).unwrap();
    {
        let mut state = manager.inner.state.lock().unwrap();
        state.due.insert(
            "one".into(),
            Instant::now() + Duration::from_secs(365 * 24 * 60 * 60),
        );
    }
    manager.save_config(1, vec![target()]).unwrap();
    assert!(!manager.inner.state.lock().unwrap().due.contains_key("one"));
    manager
        .start(
            Uuid::new_v4(),
            2,
            Arc::new(Fake(AtomicUsize::new(0))),
            MonitorLifetime::Desktop,
        )
        .unwrap();
    assert!(manager.inner.state.lock().unwrap().due["one"] <= Instant::now());
    manager.shutdown_and_wait().await.unwrap();
}

#[test]
fn config_validation_atomic_save_and_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let manager = MonitorManager::new(dir.path(), None, CancellationToken::new()).unwrap();
    let mut bad = target();
    bad.interval_secs = 59;
    assert!(manager.save_config(0, vec![bad]).is_err());
    let mut bad = target();
    bad.host = "https://example.invalid/path".into();
    assert!(manager.save_config(0, vec![bad]).is_err());
    let mut duplicate = target();
    duplicate.name = "Duplicate ID".into();
    assert!(manager.save_config(0, vec![target(), duplicate]).is_err());
    let unknown: Result<MonitorConfig, _> =
        serde_json::from_str(r#"{"version":1,"revision":0,"targets":[],"autostart":true}"#);
    assert!(unknown.is_err());
    manager.save_config(0, vec![target()]).unwrap();
    assert!(manager.save_config(0, vec![]).is_err());
    drop(manager);
    assert_eq!(
        MonitorManager::new(dir.path(), None, CancellationToken::new())
            .unwrap()
            .snapshot()
            .config
            .targets
            .len(),
        1
    );
    std::fs::write(dir.path().join("monitor.json"), "bad").unwrap();
    assert!(MonitorManager::new(dir.path(), None, CancellationToken::new()).is_err());
    assert_eq!(
        std::fs::read(dir.path().join("monitor.json")).unwrap(),
        b"bad"
    );
}
