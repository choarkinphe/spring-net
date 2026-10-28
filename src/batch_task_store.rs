//! Bounded, versioned task definitions and immutable execution reports.
use crate::{
    catalog_batch::{self, SiteResult, Snapshot},
    storage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const TASK_BYTES: usize = 8 * 1024 * 1024;
pub const REPORT_BYTES: usize = 16 * 1024 * 1024;
const TOTAL_REPORT_BYTES: usize = 100 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub group_ids: Vec<String>,
    pub site_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_selections: Option<Vec<catalog_batch::ResourceSelection>>,
    pub mode: catalog_batch::Mode,
    pub permit_bindings: Vec<catalog_batch::PermitBinding>,
    pub concurrency: usize,
    pub connection_limit: usize,
    pub queue_capacity: usize,
    pub attempts_per_site: usize,
    pub start_rate: u32,
    pub request_timeout_secs: u64,
    pub batch_timeout_secs: u64,
    #[serde(
        default,
        deserialize_with = "crate::catalog_batch::present_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub per_site_connections: Option<usize>,
    #[serde(
        default,
        deserialize_with = "crate::catalog_batch::present_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub tcp_hold_secs: Option<usize>,
}
impl Config {
    pub fn request(&self, id: &str, revision: u64) -> catalog_batch::Request {
        catalog_batch::Request {
            request_id: id.into(),
            catalog_revision: revision,
            group_ids: self.group_ids.clone(),
            site_ids: self.site_ids.clone(),
            resource_selections: self.resource_selections.clone(),
            mode: self.mode,
            permit_bindings: self.permit_bindings.clone(),
            concurrency: self.concurrency,
            connection_limit: self.connection_limit,
            queue_capacity: self.queue_capacity,
            attempts_per_site: self.attempts_per_site,
            start_rate: self.start_rate,
            request_timeout_secs: self.request_timeout_secs,
            batch_timeout_secs: self.batch_timeout_secs,
            per_site_connections: self.per_site_connections,
            tcp_hold_secs: self.tcp_hold_secs,
            confirmed_authorized: self.mode != catalog_batch::Mode::PublicHttp,
        }
    }
    pub fn normalize(&mut self) -> Result<(), String> {
        let mut r = self.request("00000000-0000-0000-0000-000000000000", 1);
        r.normalize()?;
        for id in r.group_ids.iter().chain(&r.site_ids).chain(
            r.permit_bindings
                .iter()
                .flat_map(|b| [&b.site_id, &b.permit_id]),
        ) {
            if id.is_empty() || id.len() > 200 {
                return Err("invalid selection identity".into());
            }
        }
        self.group_ids = r.group_ids;
        self.site_ids = r.site_ids;
        self.resource_selections = r.resource_selections;
        self.permit_bindings = r.permit_bindings;
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointIdentity {
    pub site_id: String,
    pub endpoint: String,
    pub transport: crate::site_catalog::Transport,
    pub protocol: crate::site_catalog::Protocol,
    pub alpn: Option<String>,
    pub applications: Vec<String>,
    pub groups: Vec<String>,
    pub associations: Vec<(String, String)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub task_id: String,
    pub create_id: String,
    pub revision: u64,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub config: Config,
    pub scope: Vec<SiteResult>,
    pub(crate) identities: Vec<EndpointIdentity>,
    pub(crate) create_intent: String,
}
impl Task {
    pub(crate) fn execution_request(&self, id: &str, revision: u64) -> catalog_batch::Request {
        let mut request = self.config.request(id, revision);
        request.site_ids = self.scope.iter().map(|s| s.site_id.clone()).collect();
        request.group_ids = self
            .identities
            .iter()
            .flat_map(|i| i.groups.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if request.resource_selections.is_some() {
            request.resource_selections = Some(
                self.identities
                    .iter()
                    .flat_map(|i| i.associations.iter())
                    .map(|(group_id, resource_id)| catalog_batch::ResourceSelection {
                        group_id: group_id.clone(),
                        resource_id: resource_id.clone(),
                    })
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            );
        }
        request
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub version: u32,
    pub execution_id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub task: Task,
    pub snapshot: Snapshot,
    pub status: String,
    pub archive_status: String,
    pub archive_error: Option<String>,
    pub checkpoint_error: Option<String>,
    pub complete: bool,
    pub accepted_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub(crate) preflight_token: String,
    pub(crate) confirmed_authorized: bool,
    pub(crate) reservation: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub execution_id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub status: String,
    pub archive_status: String,
    pub accepted_at: String,
    pub ended_at: Option<String>,
    pub complete: bool,
}
impl From<&Execution> for Summary {
    fn from(e: &Execution) -> Self {
        Self {
            execution_id: e.execution_id.clone(),
            task_id: e.task_id.clone(),
            task_revision: e.task_revision,
            status: e.status.clone(),
            archive_status: e.archive_status.clone(),
            accepted_at: e.accepted_at.clone(),
            ended_at: e.ended_at.clone(),
            complete: e.complete,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tasks {
    version: u32,
    tasks: Vec<Task>,
}
struct State {
    tasks: BTreeMap<String, Task>,
    executions: BTreeMap<String, Execution>,
}
#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
    state: Arc<Mutex<State>>,
    #[cfg(test)]
    pub(crate) fail_cleanup: Arc<std::sync::atomic::AtomicBool>,
}
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn safe(path: &Path, directory: bool) -> Result<(), String> {
    // Every existing ancestor is checked too; never follow a configured symlink root.
    for ancestor in path.ancestors() {
        // macOS exposes its real temporary directory through these system aliases.
        #[cfg(target_os = "macos")]
        if ancestor == Path::new("/var") || ancestor == Path::new("/tmp") {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err("batch storage symlinks forbidden".into());
            }
            Ok(m)
                if ancestor == path
                    && ((directory && !m.is_dir()) || (!directory && !m.is_file())) =>
            {
                return Err("batch storage requires regular files/directories".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
fn self_cleanup(path: &Path) -> Result<(), String> {
    safe(path, false)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
fn read<T: serde::de::DeserializeOwned>(path: &Path, max: usize) -> Result<T, String> {
    safe(path, false)?;
    serde_json::from_slice(&storage::read_bounded(path, max as u64)?)
        .map_err(|e| format!("invalid batch storage (original preserved): {e}"))
}
fn write(path: &Path, value: &impl Serialize, max: usize) -> Result<(), String> {
    safe(path, false)?;
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > max {
        return Err("batch storage byte limit exceeded".into());
    }
    storage::atomic_bytes(path, &bytes)
}
fn valid_site(s: &SiteResult) -> Result<(), String> {
    if s.site_id.is_empty()
        || s.site_id.len() > 200
        || s.name.len() > 200
        || s.address.len() > 2048
        || s.endpoint.len() > 4096
        || s.group_ids.len() > 100
        || s.applications.len() > 1000
        || s.planned > 100
        || s.attempted > s.planned
        || s.responses > s.attempted
        || s.last_error.as_ref().is_some_and(|v| v.len() > 2048)
        || [s.average_latency_ms, s.last_latency_ms]
            .into_iter()
            .flatten()
            .any(|n| !n.is_finite() || n < 0.0)
        || s.applications
            .iter()
            .any(|a| a.id.len() > 200 || a.name.len() > 200)
    {
        return Err("invalid site report bounds".into());
    }
    Ok(())
}
fn valid_task(t: &Task) -> Result<(), String> {
    catalog_batch::canonical_id(&t.task_id)?;
    catalog_batch::canonical_id(&t.create_id)?;
    if t.revision == 0
        || t.name.trim().is_empty()
        || t.name.len() > 200
        || t.scope.is_empty()
        || t.scope.len() > 1000
        || t.identities.len() != t.scope.len()
        || t.create_intent.len() > TASK_BYTES
    {
        return Err("invalid stored task bounds".into());
    }
    let mut config = t.config.clone();
    config.normalize()?;
    if config != t.config {
        return Err("stored task config is not normalized".into());
    }
    let mut ids = std::collections::HashSet::new();
    for site in &t.scope {
        valid_site(site)?;
        if !ids.insert(&site.site_id) || site.planned != t.config.attempts_per_site {
            return Err("invalid fixed task site".into());
        }
    }
    let mut identities = std::collections::HashSet::new();
    for i in &t.identities {
        if !identities.insert(&i.site_id)
            || i.associations.len() > 10000
            || i.alpn.as_ref().is_some_and(|v| v.len() > 100)
        {
            return Err("invalid identity bounds".into());
        }
    }
    if t.identities.iter().any(|i| {
        let Some(s) = t.scope.iter().find(|s| s.site_id == i.site_id) else {
            return true;
        };
        i.site_id != s.site_id
            || i.endpoint != s.endpoint
            || i.applications.len() > 1000
            || i.groups.len() > 100
    }) {
        return Err("invalid task identity snapshot".into());
    }
    for timestamp in [&t.created_at, &t.updated_at] {
        chrono::DateTime::parse_from_rfc3339(timestamp).map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn valid_execution(e: &Execution) -> Result<(), String> {
    valid_task(&e.task)?;
    catalog_batch::canonical_id(&e.execution_id)?;
    if e.version != 1
        || e.task_id != e.task.task_id
        || e.task_revision != e.task.revision
        || e.snapshot.request_id != e.execution_id
        || e.snapshot.config.request_id != e.execution_id
        || e.snapshot.sites.len() > 1000
        || e.snapshot.attempts.len() > 10000
        || e.snapshot.counts.planned > 10000
        || e.reservation > REPORT_BYTES
        || e.snapshot.sites.len() != e.task.scope.len()
    {
        return Err("invalid execution schema/bounds".into());
    }
    e.snapshot.config.clone().normalize()?;
    let mut expected = e
        .task
        .execution_request(&e.execution_id, e.snapshot.config.catalog_revision);
    expected.normalize()?;
    if e.snapshot.config != expected {
        return Err("execution parameters mismatch".into());
    }
    if e.status != e.snapshot.status
        || e.snapshot.counts.planned != e.task.scope.len() * e.task.config.attempts_per_site
        || e.reservation < 32768
        || e.preflight_token.len() > 200
        || e.snapshot.error.as_ref().is_some_and(|v| v.len() > 65536)
        || e.archive_error.as_ref().is_some_and(|v| v.len() > 65536)
        || e.checkpoint_error.as_ref().is_some_and(|v| v.len() > 65536)
    {
        return Err("invalid execution state bounds".into());
    }
    for timestamp in std::iter::once(&e.accepted_at)
        .chain(e.started_at.iter())
        .chain(e.ended_at.iter())
    {
        chrono::DateTime::parse_from_rfc3339(timestamp).map_err(|err| err.to_string())?;
    }
    for s in &e.snapshot.sites {
        valid_site(s)?;
        if !e
            .task
            .scope
            .iter()
            .any(|f| f.site_id == s.site_id && f.endpoint == s.endpoint)
        {
            return Err("execution endpoint mismatch".into());
        }
    }
    for (index, a) in e.snapshot.attempts.iter().enumerate() {
        if a.sequence != index + 1
            || a.attempt == 0
            || a.attempt > e.task.config.attempts_per_site
            || a.phase.len() > 100
            || a.latency_kind.len() > 100
            || a.outcome.as_ref().is_some_and(|v| v.len() > 100)
            || [
                &a.connected_ip,
                &a.target_ip,
                &a.proxy_ip,
                &a.started_at,
                &a.ended_at,
            ]
            .into_iter()
            .flatten()
            .any(|v| v.len() > 100)
            || [a.proxy_tcp_ms, a.tunnel_ms, a.latency_ms, a.hold_ms]
                .into_iter()
                .flatten()
                .any(|n| !n.is_finite() || n < 0.0)
            || a.sequence > 10000
            || a.error.as_ref().is_some_and(|s| s.len() > 2048)
            || !e.task.scope.iter().any(|s| s.site_id == a.site_id)
        {
            return Err("invalid attempt snapshot".into());
        }
    }
    if !["pending", "archived", "failed", "interrupted"].contains(&e.archive_status.as_str())
        || ![
            "preparing",
            "running",
            "stopping",
            "draining",
            "completed",
            "cancelled",
            "failed",
            "timed_out",
            "interrupted",
        ]
        .contains(&e.status.as_str())
    {
        return Err("unknown execution state".into());
    }
    Ok(())
}
impl Store {
    pub fn open(data: &Path) -> Result<Self, String> {
        let dir = data.join("batch-tasks");
        safe(&dir, true)?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let tasks_file = dir.join("tasks.json");
        let mut tasks = BTreeMap::new();
        if tasks_file.try_exists().map_err(|e| e.to_string())? {
            let saved: Tasks = read(&tasks_file, TASK_BYTES)?;
            if saved.version != 1 || saved.tasks.len() > 100 {
                return Err("unknown task schema or task limit".into());
            }
            let mut creates = std::collections::HashSet::new();
            for t in saved.tasks {
                valid_task(&t)?;
                if !creates.insert(t.create_id.clone())
                    || tasks.insert(t.task_id.clone(), t).is_some()
                {
                    return Err("duplicate task identity".into());
                }
            }
        }
        let mut executions = BTreeMap::new();
        let mut pending_paths = Vec::new();
        let mut file_count = 0usize;
        let mut total = 0usize;
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            safe(&path, false)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "tasks.json" || (name.starts_with('.') && name.ends_with(".tmp")) {
                continue;
            }
            let (id, terminal) = if let Some(id) = name.strip_suffix(".report.json") {
                (id, true)
            } else if let Some(id) = name.strip_suffix(".pending.json") {
                (id, false)
            } else {
                return Err("unknown batch storage file".into());
            };
            catalog_batch::canonical_id(id)?;
            file_count += 1;
            if file_count > 200 {
                return Err("execution storage file count exceeded".into());
            }
            if !terminal {
                pending_paths.push((id.to_owned(), path));
                continue;
            }
            let file_bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len() as usize;
            if file_bytes > REPORT_BYTES || total.saturating_add(file_bytes) > TOTAL_REPORT_BYTES {
                return Err("report storage byte capacity exceeded".into());
            }
            let e: Execution = read(&path, REPORT_BYTES)?;
            valid_execution(&e)?;
            if e.execution_id != id {
                return Err("execution filename identity mismatch".into());
            }
            if terminal {
                if e.archive_status != "archived" || e.snapshot.active() {
                    return Err("invalid terminal report".into());
                }
                total = total.saturating_add(file_bytes);
                if executions.len() >= 100 {
                    return Err("report count capacity exceeded".into());
                }
                executions.insert(id.to_owned(), e);
            }
        }
        for (id, path) in pending_paths {
            if executions.contains_key(&id) {
                let e = executions.get_mut(&id).unwrap();
                if let Err(err) = self_cleanup(&path) {
                    let bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len() as usize;
                    total = total.saturating_add(bytes);
                    e.checkpoint_error = Some(format!(
                        "terminal report recovered; stale pending cleanup failed: {err}"
                    ));
                }
                continue;
            }
            let mut e: Execution = read(&path, REPORT_BYTES)?;
            valid_execution(&e)?;
            if e.execution_id != id {
                return Err("pending filename identity mismatch".into());
            }
            if !e.snapshot.active() && e.ended_at.is_some() && e.status != "interrupted" {
                e.archive_status = "failed".into();
                e.archive_error = Some(e.archive_error.unwrap_or_else(|| {
                    "terminal pending recovered; retry report archive only".into()
                }));
            } else {
                e.status = "interrupted".into();
                e.snapshot.status = "interrupted".into();
                e.archive_status = "interrupted".into();
                e.complete = false;
                e.ended_at = Some(now());
            }
            total = total.saturating_add(e.reservation.saturating_mul(2));
            if total > TOTAL_REPORT_BYTES || executions.len() >= 100 {
                return Err("execution storage capacity exceeded".into());
            }
            executions.insert(e.execution_id.clone(), e);
        }
        if executions.len() > 100 || total > TOTAL_REPORT_BYTES {
            return Err("execution storage capacity exceeded".into());
        }
        Ok(Self {
            dir,
            state: Arc::new(Mutex::new(State { tasks, executions })),
            #[cfg(test)]
            fail_cleanup: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
    fn cleanup(&self, path: &Path) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_cleanup.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("fixture pending cleanup failed".into());
        }
        self_cleanup(path)
    }
    #[cfg(test)]
    pub(crate) fn usage_bytes(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .executions
            .values()
            .map(|e| self.retained_bytes(e).unwrap())
            .sum()
    }
    fn path(&self, id: &str, terminal: bool) -> PathBuf {
        self.dir.join(format!(
            "{id}.{}.json",
            if terminal { "report" } else { "pending" }
        ))
    }
    pub fn tasks(&self) -> Vec<Task> {
        self.state.lock().unwrap().tasks.values().cloned().collect()
    }
    pub fn task(&self, id: &str) -> Result<Task, String> {
        catalog_batch::canonical_id(id)?;
        self.state
            .lock()
            .unwrap()
            .tasks
            .get(id)
            .cloned()
            .ok_or_else(|| "task not found".into())
    }
    pub fn save_task(&self, t: Task, expected: Option<u64>) -> Result<Task, String> {
        valid_task(&t)?;
        let mut s = self.state.lock().unwrap();
        match expected {
            Some(rev) => {
                if s.tasks.get(&t.task_id).map(|t| t.revision) != Some(rev) {
                    return Err("task revision conflict".into());
                }
            }
            None => {
                if let Some(old) = s.tasks.get(&t.task_id) {
                    if old.create_id == t.create_id && old.create_intent == t.create_intent {
                        return Ok(old.clone());
                    }
                    return Err("task_id already used with different create intent".into());
                }
                if s.tasks.len() >= 100 || s.tasks.values().any(|x| x.create_id == t.create_id) {
                    return Err("task capacity or create_id conflict".into());
                }
            }
        }
        let mut tasks = s.tasks.clone();
        tasks.insert(t.task_id.clone(), t.clone());
        write(
            &self.dir.join("tasks.json"),
            &Tasks {
                version: 1,
                tasks: tasks.values().cloned().collect(),
            },
            TASK_BYTES,
        )?;
        s.tasks = tasks;
        Ok(t)
    }
    pub fn execution(&self, id: &str) -> Result<Option<Execution>, String> {
        catalog_batch::canonical_id(id)?;
        Ok(self.state.lock().unwrap().executions.get(id).cloned())
    }
    pub fn summaries(&self, task: &str) -> Vec<Summary> {
        let s = self.state.lock().unwrap();
        let mut out: Vec<_> = s
            .executions
            .values()
            .filter(|e| e.task_id == task)
            .map(Summary::from)
            .collect();
        out.sort_by(|a, b| b.accepted_at.cmp(&a.accepted_at));
        out
    }
    pub fn active(&self) -> Option<Execution> {
        self.state
            .lock()
            .unwrap()
            .executions
            .values()
            .filter(|e| {
                e.snapshot.active() || e.archive_status == "failed" || e.archive_status == "pending"
            })
            .max_by(|a, b| a.accepted_at.cmp(&b.accepted_at))
            .cloned()
    }
    fn retained_bytes(&self, e: &Execution) -> Result<usize, String> {
        if e.archive_status == "archived" {
            let report = self.path(&e.execution_id, true);
            safe(&report, false)?;
            let bytes = std::fs::metadata(&report)
                .map_err(|err| err.to_string())?
                .len() as usize;
            let pending = self.path(&e.execution_id, false);
            safe(&pending, false)?;
            let residual = match std::fs::metadata(&pending) {
                Ok(m) => m.len() as usize,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
                Err(err) => return Err(err.to_string()),
            };
            Ok(bytes.saturating_add(residual))
        } else {
            Ok(e.reservation.saturating_mul(2))
        }
    }
    pub fn check_capacity(&self, reservation: usize) -> Result<(), String> {
        let s = self.state.lock().unwrap();
        let used = s.executions.values().try_fold(0usize, |n, e| {
            self.retained_bytes(e).map(|b| n.saturating_add(b))
        })?;
        if s.executions.len() >= 100
            || reservation > REPORT_BYTES
            || used.saturating_add(reservation.saturating_mul(2)) > TOTAL_REPORT_BYTES
        {
            return Err("report capacity full; no reports were deleted".into());
        }
        Ok(())
    }
    pub fn reserve(&self, e: Execution) -> Result<(), String> {
        valid_execution(&e)?;
        let mut s = self.state.lock().unwrap();
        if s.executions.contains_key(&e.execution_id) {
            return Err("execution already reserved".into());
        }
        let used = s.executions.values().try_fold(0usize, |n, e| {
            self.retained_bytes(e).map(|b| n.saturating_add(b))
        })?;
        if s.executions.len() >= 100
            || used.saturating_add(e.reservation.saturating_mul(2)) > TOTAL_REPORT_BYTES
            || e.reservation > REPORT_BYTES
        {
            return Err("report capacity full; no reports were deleted".into());
        }
        write(&self.path(&e.execution_id, false), &e, REPORT_BYTES)?;
        s.executions.insert(e.execution_id.clone(), e);
        Ok(())
    }
    pub fn checkpoint(&self, id: &str, snapshot: Snapshot) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        let e = s.executions.get_mut(id).ok_or("execution not found")?;
        e.status = snapshot.status.clone();
        e.snapshot = snapshot;
        if e.started_at.is_none() {
            e.started_at = Some(now())
        }
        if let Err(err) = write(&self.path(id, false), e, REPORT_BYTES) {
            e.checkpoint_error = Some(err.clone());
            return Err(err);
        }
        Ok(())
    }
    pub fn finish(&self, id: &str, snapshot: Snapshot) -> Result<(), String> {
        {
            let mut s = self.state.lock().unwrap();
            let e = s.executions.get_mut(id).ok_or("execution not found")?;
            e.status = snapshot.status.clone();
            e.snapshot = snapshot;
            e.complete = !e.snapshot.active()
                && !e
                    .snapshot
                    .error
                    .as_ref()
                    .is_some_and(|err| err.starts_with("worker join failed"));
            e.ended_at = Some(now());
        }
        self.retry_archive(id).map(|_| ())
    }
    pub fn retry_archive(&self, id: &str) -> Result<Execution, String> {
        catalog_batch::canonical_id(id)?;
        let mut s = self.state.lock().unwrap();
        let e = s.executions.get_mut(id).ok_or("execution not found")?;
        if e.archive_status == "archived" {
            match self.cleanup(&self.path(id, false)) {
                Ok(()) => {
                    e.checkpoint_error = None;
                }
                Err(err) => {
                    e.checkpoint_error = Some(format!("report saved; pending cleanup: {err}"));
                    return Err(err);
                }
            }
            return Ok(e.clone());
        }
        if e.snapshot.active() || e.archive_status == "interrupted" {
            return Err("execution has no complete in-memory terminal report".into());
        }
        let mut report = e.clone();
        report.archive_status = "archived".into();
        report.archive_error = None;
        let path = self.path(id, true);
        let result: Result<(), String> = (|| {
            valid_execution(&report)?;
            if serde_json::to_vec(&report)
                .map_err(|e| e.to_string())?
                .len()
                > report.reservation
            {
                return Err("report exceeded its pre-reserved byte budget".into());
            }
            safe(&path, false)?;
            if path.try_exists().map_err(|e| e.to_string())? {
                let old: Execution = read(&path, REPORT_BYTES)?;
                if serde_json::to_vec(&old).map_err(|e| e.to_string())?
                    != serde_json::to_vec(&report).map_err(|e| e.to_string())?
                {
                    return Err("immutable report already exists".into());
                }
            } else {
                write(&path, &report, REPORT_BYTES)?
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                *e = report;
                let pending = self.path(id, false);
                if let Err(err) = self.cleanup(&pending) {
                    e.checkpoint_error = Some(format!("report saved; pending cleanup: {err}"));
                }
                Ok(e.clone())
            }
            Err(err) => {
                e.archive_status = "failed".into();
                e.archive_error = Some(err.clone());
                if let Err(pending_error) = write(&self.path(id, false), e, REPORT_BYTES) {
                    e.checkpoint_error = Some(format!(
                        "terminal pending persistence failed: {pending_error}"
                    ));
                }
                Err(err)
            }
        }
    }
    pub fn archived(&self, id: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .executions
            .get(id)
            .is_some_and(|e| e.archive_status == "archived")
    }
}
