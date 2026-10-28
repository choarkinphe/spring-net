use crate::{
    batch_task_service::*,
    batch_task_store::{Config, Store},
    catalog_batch::{Mode, PermitBinding},
    service::{AppService, ServiceConfig},
    site_catalog::*,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;
async fn service(data: &std::path::Path) -> AppService {
    let auth = data.join("auth.json");
    if !auth.exists() {
        std::fs::write(&auth, r#"{"version":1,"targets":[]}"#).unwrap();
    }
    AppService::initialize(
        ServiceConfig {
            root: env!("CARGO_MANIFEST_DIR").into(),
            data: data.join("data"),
            auth_file: auth,
            control_port: Some(1),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap()
}
fn catalog(revision: u64, url: &str) -> SaveRequest {
    SaveRequest {
        schema_version: 3,
        expected_revision: revision,
        tags: vec![Tag {
            id: "task-fixture-tag".into(),
            name: "fixture tag".into(),
        }],
        tag_assignments: vec![TagAssignment {
            resource_id: "task-fixture-app".into(),
            tag_ids: vec!["task-fixture-tag".into()],
        }],
        resource_supplements: vec![],
        sites: vec![Site {
            id: "task-fixture-site".into(),
            name: "fixture".into(),
            address: url.into(),
            port: None,
            transport: Transport::Tcp,
            protocol: Protocol::Http,
            purpose: String::new(),
            alpn: None,
        }],
        applications: vec![Application {
            id: "task-fixture-app".into(),
            name: "application".into(),
            kind: ApplicationKind::Application,
            category: ApplicationCategory::Custom,
            provider: String::new(),
            product: String::new(),
            aliases: vec![],
            access_type: String::new(),
            site_ids: vec!["task-fixture-site".into()],
            reference_rules: vec![],
        }],
        groups: vec![GroupInput {
            id: "task-fixture-group".into(),
            name: "group".into(),
            description: String::new(),
            tag_ids: vec!["task-fixture-tag".into()],
        }],
    }
}
async fn create(s: &AppService, url: &str) -> (TaskRequest, CreateRequest) {
    let c = s
        .site_catalog
        .save(catalog(s.site_catalog.snapshot().revision, url))
        .unwrap();
    let p = crate::model::Permit {
        id: "fixture-permit".into(),
        name: "fixture permit".into(),
        url: url.into(),
        modes: vec!["http".into(), "tcp".into()],
        allowed_ips: vec!["127.0.0.1/32".parse().unwrap()],
        max_concurrency: 4,
        max_rps: 32,
        max_duration: 300,
        authorization: "owned local fixture".into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
        proxy_routes: vec![],
    };
    *s.permits.write().await = vec![p];
    let r = CreateRequest {
        task_id: uuid::Uuid::new_v4().to_string(),
        create_id: uuid::Uuid::new_v4().to_string(),
        name: "Task fixture".into(),
        catalog_revision: c.revision,
        config: Config {
            group_ids: vec!["task-fixture-group".into()],
            site_ids: vec![],
            resource_selections: None,
            mode: Mode::AuthorizedHttp,
            permit_bindings: vec![PermitBinding {
                site_id: "task-fixture-site".into(),
                permit_id: "fixture-permit".into(),
            }],
            concurrency: 1,
            connection_limit: 1,
            queue_capacity: 1,
            attempts_per_site: 1,
            start_rate: 10,
            request_timeout_secs: 2,
            batch_timeout_secs: 4,
            per_site_connections: None,
            tcp_hold_secs: None,
        },
    };
    s.batch_tasks_create(r.clone()).await.unwrap();
    (
        TaskRequest {
            task_id: r.task_id.clone(),
        },
        r,
    )
}
async fn prepare(s: &AppService, t: &TaskRequest) -> StartRequest {
    let revision = s.batch_tasks.store.task(&t.task_id).unwrap().revision;
    let p = s
        .batch_tasks_preflight(PreflightRequest {
            task_id: t.task_id.clone(),
            expected_revision: revision,
        })
        .await
        .unwrap();
    assert_eq!(p["can_execute"], true, "{p}");
    StartRequest {
        task_id: t.task_id.clone(),
        task_revision: revision,
        execution_id: uuid::Uuid::new_v4().to_string(),
        preflight_token: p["token"].as_str().unwrap().into(),
        confirmed_authorized: true,
    }
}
async fn terminal(s: &AppService, id: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let e = s.batch_tasks.store.execution(id).unwrap().unwrap();
            if !e.snapshot.active() && e.archive_status != "pending" {
                break json!(e);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn create_preflight_and_cas_are_network_free_fixed_scope_does_not_expand() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (t, r) = create(&s, &url).await;
    assert!(
        s.batches
            .catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .is_none()
    );
    assert_eq!(
        s.batch_tasks_create(r.clone()).await.unwrap()["task"]["revision"],
        1
    );
    let intent = prepare(&s, &t).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    let mut c = catalog(s.site_catalog.snapshot().revision, &url);
    c.sites[0].name = "renamed".into();
    let mut added = c.sites[0].clone();
    added.id = "added-site".into();
    c.sites.push(added);
    c.applications[0].site_ids.push("added-site".into());
    s.site_catalog.save(c).unwrap();
    let p = s
        .batch_tasks_preflight(PreflightRequest {
            task_id: t.task_id.clone(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    assert_eq!(p["can_execute"], true, "{p}");
    assert_eq!(p["preview"]["sites"].as_array().unwrap().len(), 1);
    assert_eq!(p["preview"]["sites"][0]["name"], "renamed");
    // An unrelated revision/name change does not invalidate the original token.
    let server = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut b = [0; 4096];
        let _ = conn.read(&mut b).await;
        conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });
    s.batch_execution_start(intent.clone()).await.unwrap();
    server.await.unwrap();
    let e = terminal(&s, &intent.execution_id).await;
    assert_eq!(e["archive_status"], "archived");
    assert_eq!(e["snapshot"]["sites"][0]["responses"], 1);
    let bad = s
        .batch_tasks_update(UpdateRequest {
            task_id: t.task_id.clone(),
            expected_revision: 7,
            name: "edit".into(),
            catalog_revision: s.site_catalog.snapshot().revision,
            config: r.config.clone(),
        })
        .await;
    assert!(bad.is_err());
    let update = s
        .batch_tasks_update(UpdateRequest {
            task_id: t.task_id.clone(),
            expected_revision: 1,
            name: "edit".into(),
            catalog_revision: s.site_catalog.snapshot().revision,
            config: r.config,
        })
        .await;
    assert!(update.is_err(), "new endpoint requires a permit binding");
}
#[tokio::test]
async fn explicit_pairs_freeze_only_selected_sources_and_kind_tag_growth_does_not_expand() {
    use crate::catalog_batch::ResourceSelection;
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, mut r) = create(&s, "http://127.0.0.1:2/").await;
    let mut c = catalog(s.site_catalog.snapshot().revision, "http://127.0.0.1:2/");
    let mut alias = c.applications[0].clone();
    alias.id = "task-alias".into();
    alias.name = "shared endpoint alias".into();
    c.applications.push(alias);
    c.tag_assignments.push(TagAssignment {
        resource_id: "task-alias".into(),
        tag_ids: vec!["task-fixture-tag".into()],
    });
    let mut second = c.groups[0].clone();
    second.id = "task-second-group".into();
    second.name = "second".into();
    c.groups.push(second);
    let saved = s.site_catalog.save(c).unwrap();
    r.config.group_ids.push("task-second-group".into());
    r.config.resource_selections = Some(vec![
        ResourceSelection {
            group_id: "task-fixture-group".into(),
            resource_id: "task-fixture-app".into(),
        },
        ResourceSelection {
            group_id: "task-second-group".into(),
            resource_id: "task-alias".into(),
        },
    ]);
    s.batch_tasks_update(UpdateRequest {
        task_id: t.task_id.clone(),
        expected_revision: 1,
        name: "explicit pair task".into(),
        catalog_revision: saved.revision,
        config: r.config.clone(),
    })
    .await
    .unwrap();
    let task = s.batch_tasks.store.task(&t.task_id).unwrap();
    assert_eq!(task.scope.len(), 1);
    assert_eq!(
        task.identities[0].associations,
        vec![
            ("task-fixture-group".into(), "task-fixture-app".into()),
            ("task-second-group".into(), "task-alias".into()),
        ]
    );
    let token = prepare(&s, &t).await;
    let mut c = catalog(s.site_catalog.snapshot().revision, "http://127.0.0.1:2/");
    c.applications[0].kind = ApplicationKind::Website;
    let mut alias = c.applications[0].clone();
    alias.id = "task-alias".into();
    alias.name = "renamed alias".into();
    c.applications.push(alias);
    let mut added = c.applications[0].clone();
    added.id = "tag-new-member".into();
    added.name = "new member".into();
    c.applications.push(added);
    c.tags.push(Tag {
        id: "unrelated-tag".into(),
        name: "unrelated".into(),
    });
    c.tag_assignments[0].tag_ids.push("unrelated-tag".into());
    for resource_id in ["task-alias", "tag-new-member"] {
        c.tag_assignments.push(TagAssignment {
            resource_id: resource_id.into(),
            tag_ids: vec!["task-fixture-tag".into()],
        });
    }
    let mut second = c.groups[0].clone();
    second.id = "task-second-group".into();
    second.name = "second".into();
    c.groups.push(second);
    s.site_catalog.save(c.clone()).unwrap();
    let preview = s
        .batch_tasks_preflight(PreflightRequest {
            task_id: t.task_id.clone(),
            expected_revision: 2,
        })
        .await
        .unwrap();
    assert_eq!(preview["can_execute"], true, "{preview}");
    assert_eq!(preview["preview"]["sites"].as_array().unwrap().len(), 1);
    assert_eq!(
        preview["preview"]["sites"][0]["applications"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        !preview["preview"]["sites"][0]["applications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "tag-new-member")
    );
    // No runtime worker or network was started by either preflight.
    assert!(
        s.batches
            .catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .is_none()
    );
    // Detaching an original source still blocks instead of silently dropping it.
    c.expected_revision = s.site_catalog.snapshot().revision;
    c.tag_assignments
        .iter_mut()
        .find(|a| a.resource_id == "task-alias")
        .unwrap()
        .tag_ids
        .clear();
    s.site_catalog.save(c).unwrap();
    let blocked = s
        .batch_tasks_preflight(PreflightRequest {
            task_id: t.task_id.clone(),
            expected_revision: 2,
        })
        .await
        .unwrap();
    assert_eq!(blocked["can_execute"], false, "{blocked}");
    assert!(
        blocked["blockers"][0]
            .as_str()
            .unwrap()
            .contains("membership")
    );
    assert!(s.batch_execution_start(token).await.is_err());
}

#[tokio::test]
async fn old_task_config_and_create_intent_bytes_survive_reopen_without_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, r) = create(&s, "http://127.0.0.1:2/").await;
    let path = s.data.join("batch-tasks/tasks.json");
    let original = std::fs::read(&path).unwrap();
    let task = s.batch_tasks.store.task(&t.task_id).unwrap();
    let config = serde_json::to_vec(&task.config).unwrap();
    assert!(
        !String::from_utf8(config.clone())
            .unwrap()
            .contains("resource_selections")
    );
    let parsed: Config = serde_json::from_slice(&config).unwrap();
    assert_eq!(parsed.resource_selections, None);
    assert_eq!(serde_json::to_vec(&parsed).unwrap(), config);
    let reopened = service(dir.path()).await;
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        reopened
            .batch_tasks
            .store
            .task(&t.task_id)
            .unwrap()
            .create_intent,
        task.create_intent
    );
    reopened.batch_tasks_create(r).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn two_local_executions_archive_without_poll_and_survive_restart_and_idempotency() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (t, r) = create(&s, &url).await;
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut b = [0; 4096];
            let _ = conn.read(&mut b).await;
            conn.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        }
    });
    let a = prepare(&s, &t).await;
    s.batch_execution_start(a.clone()).await.unwrap();
    // Wait for disk itself, never call get/state to trigger completion.
    let path = s
        .data
        .join("batch-tasks")
        .join(format!("{}.report.json", a.execution_id));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await
        }
    })
    .await
    .unwrap();
    let original = std::fs::read(&path).unwrap();
    let b = prepare(&s, &t).await;
    assert_ne!(a.execution_id, b.execution_id);
    s.batch_execution_start(b.clone()).await.unwrap();
    server.await.unwrap();
    terminal(&s, &b.execution_id).await;
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let reopened = service(dir.path()).await;
    assert_eq!(
        reopened.batch_execution_list(t).unwrap()["executions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        reopened.batch_execution_start(a.clone()).await.unwrap()["execution"]["archive_status"],
        "archived"
    );
    assert_eq!(
        reopened.batch_tasks_create(r).await.unwrap()["task"]["revision"],
        1
    );
    let mut changed = a;
    changed.task_revision += 1;
    assert!(reopened.batch_execution_start(changed).await.is_err());
    assert!(
        reopened
            .batches
            .catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .is_none()
    );
}
#[tokio::test]
async fn archive_failure_blocks_both_start_apis_and_retry_is_disk_only() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (t, _) = create(&s, &url).await;
    let a = prepare(&s, &t).await;
    let path = s
        .data
        .join("batch-tasks")
        .join(format!("{}.report.json", a.execution_id));
    std::fs::create_dir(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut b = [0; 4096];
        let _ = conn.read(&mut b).await;
        conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });
    s.batch_execution_start(a.clone()).await.unwrap();
    server.await.unwrap();
    let e = terminal(&s, &a.execution_id).await;
    assert_eq!(e["archive_status"], "failed");
    assert_eq!(e["snapshot"]["sites"][0]["responses"], 1);
    let b = prepare(&s, &t).await;
    assert!(
        s.batch_execution_start(b)
            .await
            .unwrap_err()
            .contains("report archive failed")
    );
    let mut legacy = s
        .batch_tasks
        .store
        .task(&t.task_id)
        .unwrap()
        .config
        .request(
            &uuid::Uuid::new_v4().to_string(),
            s.site_catalog.snapshot().revision,
        );
    legacy.site_ids = vec![];
    assert!(
        s.start_catalog_batch(legacy)
            .await
            .unwrap_err()
            .contains("report archive failed")
    );
    std::fs::remove_dir(&path).unwrap();
    let result = s
        .batch_execution_retry_archive(ExecutionRequest {
            execution_id: a.execution_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(result["execution"]["archive_status"], "archived");
    assert_eq!(result["execution"]["snapshot"]["sites"][0]["responses"], 1);
    s.batches.settle().await.unwrap();
    assert!(path.is_file());
}
#[tokio::test]
async fn stopped_execution_is_archived_and_exact_stop_does_not_stop_another() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (t, _) = create(&s, &url).await;
    let a = prepare(&s, &t).await;
    s.batch_execution_start(a.clone()).await.unwrap();
    let (conn, _) = listener.accept().await.unwrap();
    let stopped = s
        .batch_execution_stop(ExecutionRequest {
            execution_id: a.execution_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(stopped["execution"]["archive_status"], "archived");
    assert_eq!(stopped["execution"]["status"], "cancelled");
    drop(conn);
    let b = prepare(&s, &t).await;
    s.batch_execution_start(b.clone()).await.unwrap();
    s.batch_execution_stop(ExecutionRequest {
        execution_id: a.execution_id,
    })
    .await
    .unwrap();
    assert!(
        s.batch_tasks
            .store
            .execution(&b.execution_id)
            .unwrap()
            .unwrap()
            .snapshot
            .active()
    );
    s.batch_execution_stop(ExecutionRequest {
        execution_id: b.execution_id,
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn pending_recovery_is_interrupted_and_never_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let a = prepare(&s, &t).await;
    let task = s.batch_tasks.store.task(&t.task_id).unwrap();
    let catalog = s.site_catalog.snapshot();
    let mut request = task.config.request(&a.execution_id, catalog.revision);
    request.site_ids = vec!["task-fixture-site".into()];
    let (mut snapshot, _) =
        crate::catalog_batch::freeze(&request, &catalog, &s.permits.read().await, None).unwrap();
    snapshot.status = "preparing".into();
    s.batch_tasks
        .store
        .reserve(crate::batch_task_store::Execution {
            version: 1,
            execution_id: a.execution_id.clone(),
            task_id: task.task_id.clone(),
            task_revision: 1,
            task,
            snapshot,
            status: "preparing".into(),
            archive_status: "pending".into(),
            archive_error: None,
            checkpoint_error: None,
            complete: false,
            accepted_at: crate::batch_task_store::now(),
            started_at: None,
            ended_at: None,
            preflight_token: a.preflight_token.clone(),
            confirmed_authorized: true,
            reservation: 32768,
        })
        .unwrap();
    let reopened = service(dir.path()).await;
    let e = reopened.batch_execution_start(a).await.unwrap();
    assert_eq!(e["execution"]["status"], "interrupted");
    assert_eq!(e["execution"]["complete"], false);
    assert!(
        reopened
            .batches
            .catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .is_none()
    );
}
#[tokio::test]
async fn semantic_route_and_permit_changes_invalidate_preflight_and_strict_dtos() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let a = prepare(&s, &t).await;
    s.permits.write().await[0].max_rps = 31;
    assert!(
        s.batch_execution_start(a)
            .await
            .unwrap_err()
            .contains("preflight")
    );
    let mut c = catalog(s.site_catalog.snapshot().revision, "http://127.0.0.1:3/");
    s.site_catalog.save(c.clone()).unwrap();
    let p = s
        .batch_tasks_preflight(PreflightRequest {
            task_id: t.task_id.clone(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    assert_eq!(p["can_execute"], false);
    c.expected_revision = s.site_catalog.snapshot().revision;
    c.sites[0].address = "http://127.0.0.1:2/".into();
    s.site_catalog.save(c).unwrap();
    let a = prepare(&s, &t).await;
    s.permits.write().await[0].expires_at = "2000-01-01T00:00:00Z".into();
    assert!(s.batch_execution_start(a).await.is_err());
    assert!(serde_json::from_value::<CreateRequest>(json!({"request_id":"smuggled"})).is_err());
    assert!(s.batch_tasks.store.execution("../outside").is_err());
}
#[tokio::test]
async fn store_rejects_corruption_symlinks_unknown_schema_and_task_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let mut task = s.batch_tasks.store.task(&t.task_id).unwrap();
    for _ in 1..100 {
        task.task_id = uuid::Uuid::new_v4().to_string();
        task.create_id = uuid::Uuid::new_v4().to_string();
        s.batch_tasks.store.save_task(task.clone(), None).unwrap();
    }
    task.task_id = uuid::Uuid::new_v4().to_string();
    task.create_id = uuid::Uuid::new_v4().to_string();
    assert!(s.batch_tasks.store.save_task(task, None).is_err());
    let path = s.data.join("batch-tasks/tasks.json");
    std::fs::write(&path, b"{\"version\":999,\"tasks\":[]}").unwrap();
    assert!(Store::open(&s.data).is_err());
    assert!(std::fs::read_to_string(&path).unwrap().contains("999"));
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"{}").unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(Store::open(&s.data).is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "{}");
    }
}

#[tokio::test]
async fn worker_panic_is_supervised_and_archives_an_incomplete_failed_report() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let a = prepare(&s, &t).await;
    s.batches.panic_next_catalog_worker();
    s.batch_execution_start(a.clone()).await.unwrap();
    let result = terminal(&s, &a.execution_id).await;
    assert_eq!(result["status"], "failed");
    assert_eq!(result["archive_status"], "archived");
    assert_eq!(result["complete"], false);
    let reopened = Store::open(&s.data).unwrap();
    assert_eq!(
        reopened.execution(&a.execution_id).unwrap().unwrap().status,
        "failed"
    );
    s.batches.settle().await.unwrap();
}
#[tokio::test]
async fn checkpoint_failure_is_visible_and_terminal_report_wins_over_stale_pending() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (t, _) = create(&s, &url).await;
    let a = prepare(&s, &t).await;
    s.batch_execution_start(a.clone()).await.unwrap();
    let (conn, _) = listener.accept().await.unwrap();
    let pending = s
        .data
        .join("batch-tasks")
        .join(format!("{}.pending.json", a.execution_id));
    std::fs::remove_file(&pending).unwrap();
    std::fs::create_dir(&pending).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1150)).await;
    assert!(
        s.batch_tasks
            .store
            .execution(&a.execution_id)
            .unwrap()
            .unwrap()
            .checkpoint_error
            .is_some()
    );
    std::fs::remove_dir(&pending).unwrap();
    s.batch_execution_stop(ExecutionRequest {
        execution_id: a.execution_id.clone(),
    })
    .await
    .unwrap();
    drop(conn);
    std::fs::write(&pending, b"corrupt stale pending").unwrap();
    let recovered = Store::open(&s.data)
        .unwrap()
        .execution(&a.execution_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.archive_status, "archived");
    assert!(!pending.exists());
}
#[tokio::test]
async fn report_capacity_and_oversize_are_refused_before_network_or_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let a = prepare(&s, &t).await;
    let task = s.batch_tasks.store.task(&t.task_id).unwrap();
    let c = s.site_catalog.snapshot();
    let mut r = task.config.request(&a.execution_id, c.revision);
    r.site_ids = vec!["task-fixture-site".into()];
    let (mut snapshot, _) =
        crate::catalog_batch::freeze(&r, &c, &s.permits.read().await, None).unwrap();
    snapshot.status = "preparing".into();
    let base = crate::batch_task_store::Execution {
        version: 1,
        execution_id: a.execution_id,
        task_id: task.task_id.clone(),
        task_revision: 1,
        task,
        snapshot,
        status: "preparing".into(),
        archive_status: "pending".into(),
        archive_error: None,
        checkpoint_error: None,
        complete: false,
        accepted_at: crate::batch_task_store::now(),
        started_at: None,
        ended_at: None,
        preflight_token: a.preflight_token,
        confirmed_authorized: true,
        reservation: 16 * 1024 * 1024,
    };
    for _ in 0..3 {
        let mut e = base.clone();
        let id = uuid::Uuid::new_v4().to_string();
        e.execution_id = id.clone();
        e.snapshot.request_id = id.clone();
        e.snapshot.config.request_id = id;
        s.batch_tasks.store.reserve(e).unwrap();
    }
    assert!(s.batch_tasks.store.check_capacity(5 * 1024 * 1024).is_err());
    assert!(
        s.batches
            .catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .is_none()
    );
    let mut oversized = base;
    oversized.reservation = crate::batch_task_store::REPORT_BYTES + 1;
    assert!(s.batch_tasks.store.reserve(oversized).is_err());
}

#[tokio::test]
async fn route_change_invalidates_token_and_successful_cas_preserves_execution_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, r) = create(&s, "http://127.0.0.1:2/").await;
    let mut public = r.config.clone();
    public.mode = Mode::PublicHttp;
    public.permit_bindings.clear();
    public.start_rate = 1;
    let c = s
        .site_catalog
        .save(catalog(
            s.site_catalog.snapshot().revision,
            "http://example.com/",
        ))
        .unwrap();
    s.batch_tasks_update(UpdateRequest {
        task_id: t.task_id.clone(),
        expected_revision: 1,
        name: "Public fixture definition".into(),
        catalog_revision: c.revision,
        config: public.clone(),
    })
    .await
    .unwrap();
    let a = prepare(&s, &t).await;
    s.proxy_settings
        .save(crate::proxy_settings::SaveRequest {
            expected_revision: s.proxy_settings.snapshot().revision,
            active_id: Some("fixture-route".into()),
            profiles: vec![crate::proxy_settings::Profile {
                id: "fixture-route".into(),
                name: "fixture route".into(),
                url: "http://127.0.0.1:18000/".into(),
            }],
        })
        .unwrap();
    let mut a = a;
    a.confirmed_authorized = false;
    assert!(
        s.batch_execution_start(a)
            .await
            .unwrap_err()
            .contains("preflight")
    );
    assert!(
        s.batches
            .catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .is_none()
    );
    let updated = s
        .batch_tasks_update(UpdateRequest {
            task_id: t.task_id.clone(),
            expected_revision: 2,
            name: "CAS update".into(),
            catalog_revision: c.revision,
            config: public,
        })
        .await
        .unwrap();
    assert_eq!(updated["task"]["revision"], 3);
    // Original create intent remains reconcilable even after later edits.
    assert_eq!(
        s.batch_tasks_create(r).await.unwrap()["task"]["revision"],
        3
    );
}
#[tokio::test]
async fn selected_application_detach_and_alpn_changes_are_explicit_scope_blockers() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let mut c = catalog(s.site_catalog.snapshot().revision, "http://127.0.0.1:2/");
    c.sites[0].protocol = Protocol::Tcp;
    c.sites[0].address = "127.0.0.1".into();
    c.sites[0].port = Some(2);
    s.site_catalog.save(c).unwrap();
    let p = s
        .batch_tasks_preflight(PreflightRequest {
            task_id: t.task_id.clone(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    assert_eq!(p["can_execute"], false);
    let mut c = catalog(s.site_catalog.snapshot().revision, "http://127.0.0.1:2/");
    c.applications[0].site_ids.clear();
    s.site_catalog.save(c).unwrap();
    let p = s
        .batch_tasks_preflight(PreflightRequest {
            task_id: t.task_id,
            expected_revision: 1,
        })
        .await
        .unwrap();
    assert_eq!(p["can_execute"], false);
    assert!(p["blockers"][0].as_str().unwrap().contains("association"));
}
#[tokio::test]
async fn dns_draining_on_shutdown_keeps_pending_for_interrupted_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let dns = std::sync::Arc::new(crate::catalog_batch_worker::TestDns::default());
    s.batches.set_test_dns(dns.clone());
    let a = prepare(&s, &t).await;
    s.batch_execution_start(a.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while dns.pending.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    s.shutdown.cancel();
    s.batches.shutdown_and_wait().await;
    let e = s
        .batch_tasks
        .store
        .execution(&a.execution_id)
        .unwrap()
        .unwrap();
    assert_eq!(e.snapshot.status, "draining");
    assert!(!e.complete);
    assert_eq!(e.archive_status, "pending");
    let recovered = Store::open(&s.data)
        .unwrap()
        .execution(&a.execution_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert!(!recovered.complete);
    dns.release();
}

#[tokio::test]
async fn cancelled_registration_is_terminal_and_legacy_start_cannot_bypass_orphan_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let s = std::sync::Arc::new(service(dir.path()).await);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (t, _) = create(&s, &format!("http://{}/", listener.local_addr().unwrap())).await;
    let a = prepare(&s, &t).await;
    let entered = CancellationToken::new();
    let release = CancellationToken::new();
    *s.batch_tasks.registration_pause.lock().unwrap() = Some((entered.clone(), release));
    let runner = s.clone();
    let request = a.clone();
    let caller = tokio::spawn(async move { runner.batch_execution_start(request).await });
    entered.cancelled().await;
    let mut old = s
        .batch_tasks
        .store
        .task(&t.task_id)
        .unwrap()
        .config
        .request(
            &uuid::Uuid::new_v4().to_string(),
            s.site_catalog.snapshot().revision,
        );
    old.site_ids = vec![];
    let report = s
        .data
        .join("batch-tasks")
        .join(format!("{}.report.json", a.execution_id));
    std::fs::create_dir(&report).unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let e = terminal(&s, &a.execution_id).await;
    assert_eq!(e["status"], "failed");
    assert_eq!(e["archive_status"], "failed");
    assert!(
        s.start_catalog_batch(old)
            .await
            .unwrap_err()
            .contains("archive")
    );
    let legacy = crate::batch_service::RunBatchRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        list_ids: vec!["missing".into()],
        target_ids: vec![],
        concurrency: 1,
        rounds: 1,
        timeout_secs: 1,
        route: crate::batch_service::BatchRoute::Direct,
        proxy: None,
    };
    assert!(s.start_batch(legacy).await.unwrap_err().contains("archive"));
    std::fs::remove_dir(&report).unwrap();
    s.batch_execution_retry_archive(ExecutionRequest {
        execution_id: a.execution_id.clone(),
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    assert!(
        !s.batch_tasks
            .store
            .execution(&a.execution_id)
            .unwrap()
            .unwrap()
            .snapshot
            .active()
    );
    assert!(
        s.batches
            .catalog_state(None)
            .await
            .unwrap()
            .snapshot
            .is_none()
    );
}
#[tokio::test]
async fn terminal_archive_failure_survives_restart_and_retry_writes_no_network() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (t, _) = create(&s, &format!("http://{}/", listener.local_addr().unwrap())).await;
    let a = prepare(&s, &t).await;
    let report = s
        .data
        .join("batch-tasks")
        .join(format!("{}.report.json", a.execution_id));
    std::fs::create_dir(&report).unwrap();
    s.batch_execution_start(a.clone()).await.unwrap();
    let (mut conn, _) = listener.accept().await.unwrap();
    let mut buf = [0; 4096];
    let _ = conn.read(&mut buf).await.unwrap();
    conn.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
    terminal(&s, &a.execution_id).await;
    std::fs::remove_dir(&report).unwrap();
    let reopened = service(dir.path()).await;
    let recovered = reopened
        .batch_tasks
        .store
        .execution(&a.execution_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.status, "completed");
    assert_eq!(recovered.archive_status, "failed");
    assert!(recovered.complete);
    assert_eq!(recovered.snapshot.sites[0].responses, 1);
    let blocked = reopened
        .start_batch(crate::batch_service::RunBatchRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            list_ids: vec!["missing".into()],
            target_ids: vec![],
            concurrency: 1,
            rounds: 1,
            timeout_secs: 1,
            route: crate::batch_service::BatchRoute::Direct,
            proxy: None,
        })
        .await
        .unwrap_err();
    assert!(blocked.contains("archive"));
    let saved = reopened
        .batch_execution_retry_archive(ExecutionRequest {
            execution_id: a.execution_id,
        })
        .await
        .unwrap();
    assert_eq!(saved["execution"]["archive_status"], "archived");
    assert_eq!(saved["execution"]["snapshot"]["sites"][0]["responses"], 1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn unchanged_failed_checkpoint_retries_and_cleanup_residual_is_accounted() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (t, r) = create(&s, &format!("http://{}/", listener.local_addr().unwrap())).await;
    let mut config = r.config;
    config.request_timeout_secs = 10;
    config.batch_timeout_secs = 20;
    s.batch_tasks_update(UpdateRequest {
        task_id: t.task_id.clone(),
        expected_revision: 1,
        name: "checkpoint retry".into(),
        catalog_revision: s.site_catalog.snapshot().revision,
        config,
    })
    .await
    .unwrap();
    let a = prepare(&s, &t).await;
    s.batch_tasks
        .store
        .fail_cleanup
        .store(true, std::sync::atomic::Ordering::SeqCst);
    s.batch_execution_start(a.clone()).await.unwrap();
    let (conn, _) = listener.accept().await.unwrap();
    let pending = s
        .data
        .join("batch-tasks")
        .join(format!("{}.pending.json", a.execution_id));
    std::fs::remove_file(&pending).unwrap();
    std::fs::create_dir(&pending).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1150)).await;
    assert!(
        s.batch_tasks
            .store
            .execution(&a.execution_id)
            .unwrap()
            .unwrap()
            .checkpoint_error
            .is_some()
    );
    std::fs::remove_dir(&pending).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !pending.is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await
        }
    })
    .await
    .unwrap();
    s.batch_execution_stop(ExecutionRequest {
        execution_id: a.execution_id.clone(),
    })
    .await
    .unwrap();
    drop(conn);
    let report = s
        .data
        .join("batch-tasks")
        .join(format!("{}.report.json", a.execution_id));
    let bytes = std::fs::read(&report).unwrap();
    let expected = bytes.len() + std::fs::metadata(&pending).unwrap().len() as usize;
    assert_eq!(s.batch_tasks.store.usage_bytes(), expected);
    s.batch_tasks
        .store
        .fail_cleanup
        .store(false, std::sync::atomic::Ordering::SeqCst);
    s.batch_execution_retry_archive(ExecutionRequest {
        execution_id: a.execution_id,
    })
    .await
    .unwrap();
    assert!(!pending.exists());
    assert_eq!(std::fs::read(&report).unwrap(), bytes);
    assert_eq!(s.batch_tasks.store.usage_bytes(), bytes.len());
}
#[tokio::test]
async fn panic_dns_supervisor_observes_shutdown_instead_of_waiting_forever() {
    let dir = tempfile::tempdir().unwrap();
    let s = service(dir.path()).await;
    let (t, _) = create(&s, "http://127.0.0.1:2/").await;
    let a = prepare(&s, &t).await;
    let dns = std::sync::Arc::new(crate::catalog_batch_worker::TestDns::default());
    dns.pending.store(1, std::sync::atomic::Ordering::SeqCst);
    s.batches.set_test_dns(dns.clone());
    s.batches.panic_next_catalog_worker();
    s.batch_execution_start(a.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = s
                .batches
                .catalog_state(Some(&a.execution_id))
                .await
                .unwrap()
                .snapshot
                .unwrap();
            if snapshot.status == "draining" {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    s.shutdown.cancel();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        s.batches.shutdown_and_wait(),
    )
    .await
    .unwrap();
    let e = s
        .batch_tasks
        .store
        .execution(&a.execution_id)
        .unwrap()
        .unwrap();
    assert_eq!(e.status, "draining");
    assert_eq!(e.archive_status, "pending");
    assert!(!e.complete);
    dns.pending.store(0, std::sync::atomic::Ordering::SeqCst);
}
