//! Task/execution bridge; uses the existing catalog worker and global admission gate.
use crate::{
    batch_task_store::{self as store, Config, EndpointIdentity, Execution, Store, Task},
    catalog_batch::{self, Snapshot},
    service::AppService,
    site_catalog,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub task_id: String,
    pub create_id: String,
    pub name: String,
    pub catalog_revision: u64,
    pub config: Config,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    pub task_id: String,
    pub expected_revision: u64,
    pub name: String,
    pub catalog_revision: u64,
    pub config: Config,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRequest {
    pub task_id: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightRequest {
    pub task_id: String,
    pub expected_revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub task_id: String,
    pub task_revision: u64,
    pub execution_id: String,
    pub preflight_token: String,
    pub confirmed_authorized: bool,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRequest {
    pub execution_id: String,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyRequest {}
// A cancelled HTTP/IPC caller must never strand a durable accepted execution.
struct RegistrationGuard {
    store: Store,
    id: String,
    armed: bool,
}
impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        if self.armed
            && let Ok(Some(mut e)) = self.store.execution(&self.id)
        {
            e.snapshot.status = "failed".into();
            e.snapshot.error=Some("execution registration cancelled before worker ownership transfer; no network started".into());
            let _ = self.store.finish(&self.id, e.snapshot);
        }
    }
}
struct Token {
    task_id: String,
    revision: u64,
    expires: Instant,
    intent: Vec<u8>,
}
pub struct Tasks {
    pub store: Store,
    tokens: Mutex<HashMap<String, Token>>,
    #[cfg(test)]
    pub(crate) registration_pause: Mutex<
        Option<(
            tokio_util::sync::CancellationToken,
            tokio_util::sync::CancellationToken,
        )>,
    >,
}
impl Tasks {
    pub fn open(data: &std::path::Path) -> Result<Self, String> {
        Ok(Self {
            store: Store::open(data)?,
            tokens: Mutex::new(HashMap::new()),
            #[cfg(test)]
            registration_pause: Mutex::new(None),
        })
    }
}
fn identities(
    snapshot: &Snapshot,
    catalog: &site_catalog::Snapshot,
) -> Result<Vec<EndpointIdentity>, String> {
    let mut result: Vec<_> = snapshot
        .sites
        .iter()
        .map(|s| {
            let site = &catalog
                .sites
                .iter()
                .find(|r| r.site.id == s.site_id)
                .ok_or("fixed endpoint removed")?
                .site;
            let mut applications: Vec<_> = s.applications.iter().map(|a| a.id.clone()).collect();
            applications.sort();
            let mut groups = s.group_ids.clone();
            groups.sort();
            Ok(EndpointIdentity {
                site_id: s.site_id.clone(),
                endpoint: s.endpoint.clone(),
                transport: site.transport,
                protocol: site.protocol,
                alpn: site.alpn.clone(),
                associations: groups
                    .iter()
                    .flat_map(|g| {
                        let group = catalog.groups.iter().find(|r| r.group.id == *g);
                        applications.iter().filter_map(move |a| {
                            group
                                .filter(|r| r.group.application_ids.contains(a))
                                .filter(|_| {
                                    snapshot.config.resource_selections.as_ref().is_none_or(
                                        |selections| {
                                            selections
                                                .iter()
                                                .any(|s| s.group_id == *g && s.resource_id == *a)
                                        },
                                    )
                                })
                                .map(|_| (g.clone(), a.clone()))
                        })
                    })
                    .collect(),
                applications,
                groups,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    result.sort_by(|a, b| a.site_id.cmp(&b.site_id));
    Ok(result)
}
fn fixed_catalog(
    task: &Task,
    catalog: &site_catalog::Snapshot,
) -> Result<site_catalog::Snapshot, String> {
    let mut fixed = catalog.clone();
    let ids: HashSet<_> = task.identities.iter().map(|i| i.site_id.as_str()).collect();
    for i in &task.identities {
        if !catalog.sites.iter().any(|r| r.site.id == i.site_id) {
            return Err(format!(
                "{}: fixed endpoint removed; edit task scope",
                i.site_id
            ));
        }
        for app in &i.applications {
            let app = catalog
                .applications
                .iter()
                .find(|r| r.application.id == *app)
                .ok_or_else(|| format!("{app}: selected application removed"))?;
            if !app.application.site_ids.contains(&i.site_id) {
                return Err(format!(
                    "{}: endpoint/application association changed",
                    i.site_id
                ));
            }
        }
        for group in &i.groups {
            let g = catalog
                .groups
                .iter()
                .find(|g| g.group.id == *group)
                .ok_or_else(|| format!("{group}: selected group removed"))?;
            if !i
                .associations
                .iter()
                .filter(|(id, _)| id == group)
                .all(|(_, a)| g.group.application_ids.contains(a))
            {
                return Err(format!("{group}: application membership changed"));
            }
        }
    }
    let apps: HashSet<_> = task
        .identities
        .iter()
        .flat_map(|i| i.applications.iter().map(String::as_str))
        .collect();
    fixed.sites.retain(|r| ids.contains(r.site.id.as_str()));
    fixed
        .applications
        .retain(|r| apps.contains(r.application.id.as_str()));
    for a in &mut fixed.applications {
        a.application.site_ids.retain(|id| {
            task.identities
                .iter()
                .any(|i| i.site_id == *id && i.applications.contains(&a.application.id))
        });
    }
    fixed
        .groups
        .retain(|g| task.config.group_ids.contains(&g.group.id));
    for g in &mut fixed.groups {
        g.group.application_ids.retain(|a| {
            task.identities
                .iter()
                .any(|i| i.associations.contains(&(g.group.id.clone(), a.clone())))
        });
    }
    Ok(fixed)
}
impl AppService {
    pub fn batch_tasks_list(&self) -> Value {
        let tasks = self.batch_tasks.store.tasks();
        let last_executions: std::collections::BTreeMap<_, _> = tasks
            .iter()
            .map(|t| {
                (
                    t.task_id.clone(),
                    self.batch_tasks
                        .store
                        .summaries(&t.task_id)
                        .first()
                        .cloned(),
                )
            })
            .collect();
        let count: usize = tasks
            .iter()
            .map(|t| self.batch_tasks.store.summaries(&t.task_id).len())
            .sum();
        json!({"counts":{"tasks":tasks.len(),"executions":count},"tasks":tasks,"last_executions":last_executions})
    }
    pub fn batch_tasks_get(&self, r: TaskRequest) -> Result<Value, String> {
        let task = self.batch_tasks.store.task(&r.task_id)?;
        Ok(json!({"executions":self.batch_tasks.store.summaries(&r.task_id),"task":task}))
    }
    pub fn batch_execution_list(&self, r: TaskRequest) -> Result<Value, String> {
        self.batch_tasks.store.task(&r.task_id)?;
        Ok(json!({"executions":self.batch_tasks.store.summaries(&r.task_id)}))
    }
    pub async fn batch_tasks_create(&self, mut r: CreateRequest) -> Result<Value, String> {
        catalog_batch::canonical_id(&r.task_id)?;
        catalog_batch::canonical_id(&r.create_id)?;
        r.config.normalize()?;
        let intent = serde_json::to_string(&r).map_err(|e| e.to_string())?;
        let _gate = self.mutation.lock().await;
        if let Ok(old) = self.batch_tasks.store.task(&r.task_id) {
            if old.create_id == r.create_id && old.create_intent == intent {
                return self.batch_tasks_get(TaskRequest { task_id: r.task_id });
            }
            return Err("task_id belongs to another create intent".into());
        }
        if self.shutdown.is_cancelled() {
            return Err("application is shutting down".into());
        }
        let catalog = self.site_catalog.snapshot();
        let request = r.config.request(&r.task_id, r.catalog_revision);
        let snapshot = catalog_batch::freeze_definition(&request, &catalog, self.control_port)?;
        let task = Task {
            task_id: r.task_id.clone(),
            create_id: r.create_id,
            revision: 1,
            name: r.name.trim().into(),
            created_at: store::now(),
            updated_at: store::now(),
            config: r.config,
            identities: identities(&snapshot, &catalog)?,
            scope: snapshot.sites,
            create_intent: intent,
        };
        self.batch_tasks.store.save_task(task, None)?;
        self.batch_tasks_get(TaskRequest { task_id: r.task_id })
    }
    pub async fn batch_tasks_update(&self, mut r: UpdateRequest) -> Result<Value, String> {
        let _gate = self.mutation.lock().await;
        r.config.normalize()?;
        if self.shutdown.is_cancelled() {
            return Err("application is shutting down".into());
        }
        let mut task = self.batch_tasks.store.task(&r.task_id)?;
        if self
            .batch_tasks
            .store
            .active()
            .is_some_and(|e| e.task_id == r.task_id)
        {
            return Err("task has an active execution or unsaved report".into());
        }
        if task.revision != r.expected_revision {
            return Err("task revision conflict".into());
        }
        let catalog = self.site_catalog.snapshot();
        let snapshot = catalog_batch::freeze_definition(
            &r.config.request(&r.task_id, r.catalog_revision),
            &catalog,
            self.control_port,
        )?;
        task.name = r.name.trim().into();
        task.revision = task.revision.checked_add(1).ok_or("revision overflow")?;
        task.updated_at = store::now();
        task.config = r.config;
        task.identities = identities(&snapshot, &catalog)?;
        task.scope = snapshot.sites;
        self.batch_tasks
            .store
            .save_task(task, Some(r.expected_revision))?;
        self.batch_tasks_get(TaskRequest { task_id: r.task_id })
    }
    async fn task_preview(
        &self,
        task: &Task,
        id: &str,
    ) -> Result<(Snapshot, site_catalog::Snapshot, Vec<u8>, usize), String> {
        let catalog = self.site_catalog.snapshot();
        let fixed = fixed_catalog(task, &catalog)?;
        let mut request = task.execution_request(id, catalog.revision);
        request.normalize()?;
        if request.mode != catalog_batch::Mode::PublicHttp
            && let Some(err) = self.auth_error.read().await.as_ref()
        {
            return Err(format!(
                "authorization configuration requires repair: {err}"
            ));
        }
        let permits = self.permits.read().await.clone();
        let (mut snapshot, targets) =
            catalog_batch::freeze(&request, &fixed, &permits, self.control_port)?;
        if identities(&snapshot, &fixed)? != task.identities {
            return Err("fixed endpoint semantics changed (address/port/transport/protocol/ALPN/association); edit task scope".into());
        }
        snapshot.route = self.proxy_settings.route();
        for target in &targets {
            if let (Some(p), Some(proxy)) = (&target.permit, &snapshot.route.proxy) {
                crate::policy::proxy_route(p, proxy, self.control_port)?;
            }
        }
        let intent=serde_json::to_vec(&json!({"identities":task.identities,"config":task.config,"route":snapshot.route,"permits":targets.iter().filter_map(|t|t.permit.as_ref()).collect::<Vec<_>>()})).map_err(|e|e.to_string())?;
        // Account for worst-case escaped errors and all optional peer/timing fields.
        let base = serde_json::to_vec(&task)
            .map_err(|e| e.to_string())?
            .len()
            .saturating_add(
                serde_json::to_vec(&snapshot)
                    .map_err(|e| e.to_string())?
                    .len(),
            );
        let reservation = base
            .saturating_add(snapshot.counts.planned.saturating_mul(7500))
            .saturating_add(32768);
        if reservation > store::REPORT_BYTES {
            return Err("worst-case report exceeds 16 MiB; reduce endpoints/attempts".into());
        }
        self.batch_tasks.store.check_capacity(reservation)?;
        Ok((snapshot, fixed, intent, reservation))
    }
    pub async fn batch_tasks_preflight(&self, r: PreflightRequest) -> Result<Value, String> {
        let _gate = self.mutation.lock().await;
        let task = self.batch_tasks.store.task(&r.task_id)?;
        let preview = if task.revision != r.expected_revision {
            Err("task revision conflict".into())
        } else {
            self.task_preview(&task, &task.task_id).await
        };
        let (snapshot, _, intent, _) = match preview {
            Ok(v) => v,
            Err(e) => {
                return Ok(
                    json!({"task_id":r.task_id,"task_revision":task.revision,"can_execute":false,"blockers":[e],"token":null,"expires_at":null,"preview":null,"route_changed":false}),
                );
            }
        };
        let token = uuid::Uuid::new_v4().to_string();
        let mut tokens = self.batch_tasks.tokens.lock().unwrap();
        tokens.retain(|_, t| t.expires > Instant::now());
        if tokens.len() >= 100 {
            return Err("preflight capacity reached".into());
        }
        tokens.insert(
            token.clone(),
            Token {
                task_id: r.task_id.clone(),
                revision: task.revision,
                expires: Instant::now() + Duration::from_secs(30),
                intent,
            },
        );
        let previous = self
            .batch_tasks
            .store
            .summaries(&r.task_id)
            .first()
            .and_then(|s| {
                self.batch_tasks
                    .store
                    .execution(&s.execution_id)
                    .ok()
                    .flatten()
            });
        let changed = previous.is_some_and(|e| e.snapshot.route != snapshot.route);
        Ok(
            json!({"task_id":r.task_id,"task_revision":task.revision,"can_execute":true,"blockers":[],"token":token,"expires_at":(chrono::Utc::now()+chrono::Duration::seconds(30)).to_rfc3339(),"preview":snapshot,"route_changed":changed}),
        )
    }
    async fn execution_response(&self, id: Option<&str>) -> Result<Value, String> {
        let execution = match id {
            Some(id) => self.batch_tasks.store.execution(id)?,
            None => self.batch_tasks.store.active(),
        };
        let external_busy_sources = self.catalog_batch_external_busy_sources().await;
        Ok(
            json!({"execution_id":id.map(str::to_owned).or_else(||execution.as_ref().map(|e|e.execution_id.clone())),"accepted":execution.is_some(),"execution":execution,"external_busy":!external_busy_sources.is_empty(),"external_busy_sources":external_busy_sources}),
        )
    }
    pub async fn batch_execution_get(&self, r: ExecutionRequest) -> Result<Value, String> {
        self.execution_response(Some(&r.execution_id)).await
    }
    pub async fn batch_execution_active(&self) -> Result<Value, String> {
        self.execution_response(None).await
    }
    pub async fn batch_execution_start(&self, r: StartRequest) -> Result<Value, String> {
        catalog_batch::canonical_id(&r.execution_id)?;
        catalog_batch::canonical_id(&r.task_id)?;
        let _gate = self.mutation.lock().await;
        // Durable identity is checked BEFORE task revision, token lifetime or mutable catalog.
        if let Some(e) = self.batch_tasks.store.execution(&r.execution_id)? {
            if e.task_id != r.task_id
                || e.task_revision != r.task_revision
                || e.preflight_token != r.preflight_token
                || e.confirmed_authorized != r.confirmed_authorized
            {
                return Err("execution_id belongs to a different execution intent".into());
            }
            return self.execution_response(Some(&r.execution_id)).await;
        }
        self.ensure_mutable().await?;
        if self
            .batches
            .catalog_state(Some(&r.execution_id))
            .await?
            .accepted
        {
            return Err("execution_id already belongs to a legacy catalog batch".into());
        }
        let task = self.batch_tasks.store.task(&r.task_id)?;
        if task.revision != r.task_revision {
            return Err("task revision conflict".into());
        }
        if r.confirmed_authorized != (task.config.mode != catalog_batch::Mode::PublicHttp) {
            return Err("explicit per-execution authorization confirmation required only in authorized modes".into());
        }
        let (mut snapshot, catalog, intent, reservation) =
            self.task_preview(&task, &r.execution_id).await?;
        {
            let tokens = self.batch_tasks.tokens.lock().unwrap();
            let token = tokens
                .get(&r.preflight_token)
                .ok_or("preflight token unknown; preflight again")?;
            if token.task_id != r.task_id
                || token.revision != r.task_revision
                || token.expires <= Instant::now()
                || token.intent != intent
            {
                return Err(
                    "preflight expired or endpoint/permit/route changed; preflight again".into(),
                );
            }
        }
        snapshot.status = "preparing".into();
        let request = snapshot.config.clone();
        let execution = Execution {
            version: 1,
            execution_id: r.execution_id.clone(),
            task_id: r.task_id,
            task_revision: r.task_revision,
            task,
            snapshot,
            status: "preparing".into(),
            archive_status: "pending".into(),
            archive_error: None,
            checkpoint_error: None,
            complete: false,
            accepted_at: store::now(),
            started_at: None,
            ended_at: None,
            preflight_token: r.preflight_token,
            confirmed_authorized: r.confirmed_authorized,
            reservation,
        };
        let permits = self.permits.read().await.clone();
        self.batch_tasks.store.reserve(execution)?;
        let mut registration = RegistrationGuard {
            store: self.batch_tasks.store.clone(),
            id: r.execution_id.clone(),
            armed: true,
        };
        #[cfg(test)]
        {
            let pause = self.batch_tasks.registration_pause.lock().unwrap().take();
            if let Some((entered, release)) = pause {
                entered.cancel();
                release.cancelled().await;
            }
        }
        self.proxy_health.pause();
        let result = self
            .batches
            .start_catalog_archived(
                request,
                &catalog,
                &permits,
                self.proxy_settings.route(),
                self.batch_tasks.store.clone(),
            )
            .await;
        registration.armed = false;
        if let Err(error) = result {
            let mut e = self
                .batch_tasks
                .store
                .execution(&r.execution_id)?
                .ok_or("accepted execution missing")?
                .snapshot;
            e.status = "failed".into();
            e.error = Some(error);
            let _ = self.batch_tasks.store.finish(&r.execution_id, e);
        }
        self.execution_response(Some(&r.execution_id)).await
    }
    pub async fn batch_execution_stop(&self, r: ExecutionRequest) -> Result<Value, String> {
        if let Some(e) = self.batch_tasks.store.execution(&r.execution_id)? {
            if e.snapshot.active() {
                self.batches.stop_catalog(&r.execution_id).await?;
            }
        } else {
            return Err("execution not found".into());
        }
        self.execution_response(Some(&r.execution_id)).await
    }
    pub async fn batch_execution_retry_archive(
        &self,
        r: ExecutionRequest,
    ) -> Result<Value, String> {
        let _gate = self.mutation.lock().await;
        self.batch_tasks.store.retry_archive(&r.execution_id)?;
        self.execution_response(Some(&r.execution_id)).await
    }
}
