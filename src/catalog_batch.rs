//! Strict, finite catalog batches. A catalog entry is never itself an authorization.
use crate::{
    model::{MAX_CONCURRENCY, MAX_RPS, Permit},
    policy, site_catalog,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    PublicHttp,
    AuthorizedHttp,
    Tcp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermitBinding {
    pub site_id: String,
    pub permit_id: String,
}

// Missing is None; explicit null is not an integer and is rejected. In particular,
// an HTTP request cannot smuggle a TCP-only parameter through as null.
fn present_integer<'de, D: Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    usize::deserialize(d).map(Some)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: String,
    pub catalog_revision: u64,
    pub group_ids: Vec<String>,
    pub site_ids: Vec<String>,
    pub mode: Mode,
    pub permit_bindings: Vec<PermitBinding>,
    pub concurrency: usize,
    pub connection_limit: usize,
    pub queue_capacity: usize,
    pub attempts_per_site: usize,
    pub start_rate: u32,
    pub request_timeout_secs: u64,
    pub batch_timeout_secs: u64,
    #[serde(
        default,
        deserialize_with = "present_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub per_site_connections: Option<usize>,
    #[serde(
        default,
        deserialize_with = "present_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub tcp_hold_secs: Option<usize>,
    pub confirmed_authorized: bool,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateRequest {
    pub request_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopRequest {
    pub request_id: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct StateResponse {
    pub request_id: Option<String>,
    pub accepted: bool,
    pub snapshot: Option<Snapshot>,
    pub external_busy: bool,
    pub external_busy_sources: Vec<String>,
}

pub fn canonical_id(id: &str) -> Result<(), String> {
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| "invalid request_id UUID")?;
    if uuid.to_string() != id {
        return Err("request_id must be a canonical UUID".into());
    }
    Ok(())
}
impl Request {
    pub fn normalize(&mut self) -> Result<(), String> {
        canonical_id(&self.request_id)?;
        let public = self.mode == Mode::PublicHttp;
        let max = if public { 4 } else { MAX_CONCURRENCY };
        if !(1..=max).contains(&self.concurrency)
            || !(1..=max).contains(&self.connection_limit)
            || !(1..=256).contains(&self.queue_capacity)
            || !(1..=if public { 3 } else { 100 }).contains(&self.attempts_per_site)
            || !(1..=if public { 2 } else { MAX_RPS }).contains(&self.start_rate)
            || !(1..=30).contains(&self.request_timeout_secs)
            || !(1..=if public { 120 } else { 300 }).contains(&self.batch_timeout_secs)
            || self.group_ids.is_empty()
            || self.group_ids.len() > 100
            || self.site_ids.len() > 1000
            || self.permit_bindings.len() > 1000
        {
            return Err("catalog batch limits exceeded (see catalog-batch/info)".into());
        }
        if self.confirmed_authorized == public {
            return Err("confirmed_authorized must be true only in authorized modes".into());
        }
        if public && !self.permit_bindings.is_empty() {
            return Err("public_http forbids permit_bindings".into());
        }
        if self.mode == Mode::Tcp {
            if !self
                .per_site_connections
                .is_some_and(|n| (1..=MAX_CONCURRENCY).contains(&n) && n <= self.connection_limit)
                || !self.tcp_hold_secs.is_some_and(|n| (1..=30).contains(&n))
            {
                return Err("tcp requires per_site_connections 1..1000 <= connection_limit and tcp_hold_secs 1..30".into());
            }
        } else if self.per_site_connections.is_some() || self.tcp_hold_secs.is_some() {
            return Err("HTTP modes forbid TCP-only parameters".into());
        }
        self.group_ids.sort();
        self.site_ids.sort();
        self.permit_bindings
            .sort_by(|a, b| a.site_id.cmp(&b.site_id));
        if self.group_ids.windows(2).any(|w| w[0] == w[1])
            || self.site_ids.windows(2).any(|w| w[0] == w[1])
            || self
                .permit_bindings
                .windows(2)
                .any(|w| w[0].site_id == w[1].site_id)
        {
            return Err("duplicate group/site/permit binding reference".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub url: Url,
    pub permit: Option<Permit>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counts {
    pub planned: usize,
    pub pending_generation: usize,
    pub queued: usize,
    pub waiting_admission: usize,
    pub active: usize,
    pub connecting: usize,
    pub connected: usize,
    pub peak_connected: usize,
    pub connection_permits: usize,
    pub peak_connection_permits: usize,
    pub terminal: usize,
    pub unexecuted: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct ApplicationIdentity {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct SiteResult {
    pub site_id: String,
    pub name: String,
    pub address: String,
    pub endpoint: String,
    pub group_ids: Vec<String>,
    /// Frozen identities, independent of later catalog edits. Shared endpoints run once.
    pub applications: Vec<ApplicationIdentity>,
    pub permit_id: Option<String>,
    pub planned: usize,
    pub attempted: usize,
    pub responses: usize,
    pub good_status: usize,
    pub bad_status: usize,
    pub network_errors: usize,
    pub timed_out: usize,
    pub cancelled: usize,
    pub tcp_connected: usize,
    pub tcp_failed: usize,
    pub connected: usize,
    pub peak_connected: usize,
    pub early_closed: usize,
    pub last_status: Option<u16>,
    pub last_error: Option<String>,
    pub average_latency_ms: Option<f64>,
    pub last_latency_ms: Option<f64>,
    #[serde(skip)]
    pub(crate) latency_samples: usize,
    #[serde(skip)]
    pub(crate) latency_total_ms: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Attempt {
    pub sequence: usize,
    pub site_id: String,
    pub attempt: usize,
    pub phase: String,
    pub attempted: bool,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub status: Option<u16>,
    /// Actual connected peer (proxy IP when tunneled), never the pinned target.
    pub connected_ip: Option<String>,
    pub target_ip: Option<String>,
    pub proxy_ip: Option<String>,
    pub proxy_tcp_ms: Option<f64>,
    pub tunnel_ms: Option<f64>,
    pub latency_kind: String,
    pub latency_ms: Option<f64>,
    pub hold_ms: Option<f64>,
    pub outcome: Option<String>,
    pub error: Option<String>,
    pub error_truncated: bool,
}
impl Attempt {
    pub(crate) fn queued(sequence: usize, site_id: String, attempt: usize) -> Self {
        Self {
            sequence,
            site_id,
            attempt,
            phase: "queued".into(),
            attempted: false,
            started_at: None,
            ended_at: None,
            status: None,
            connected_ip: None,
            target_ip: None,
            proxy_ip: None,
            proxy_tcp_ms: None,
            tunnel_ms: None,
            latency_kind: "attempt_elapsed".into(),
            latency_ms: None,
            hold_ms: None,
            outcome: None,
            error: None,
            error_truncated: false,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub request_id: String,
    pub config: Request,
    pub route: crate::proxy_settings::RouteSnapshot,
    pub status: String,
    pub sites: Vec<SiteResult>,
    pub attempts: Vec<Attempt>,
    pub counts: Counts,
    pub error: Option<String>,
    pub details_retained: bool,
}
impl Snapshot {
    pub fn active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "preparing" | "running" | "stopping" | "draining"
        )
    }
    pub(crate) fn compact(&mut self) {
        self.sites.clear();
        self.attempts.clear();
        self.details_retained = false;
    }
}

pub(crate) fn freeze(
    request: &Request,
    catalog: &site_catalog::Snapshot,
    permits: &[Permit],
    control_port: Option<u16>,
) -> Result<(Snapshot, Vec<Target>), String> {
    if catalog.revision != request.catalog_revision {
        return Err("catalog revision conflict; reload before starting".into());
    }
    if catalog.version != 2 {
        return Err("catalog schema v2 required".into());
    }
    let mut memberships: HashMap<&str, Vec<String>> = HashMap::new();
    let mut applications: HashMap<&str, Vec<ApplicationIdentity>> = HashMap::new();
    for id in &request.group_ids {
        let group = catalog
            .groups
            .iter()
            .find(|g| &g.group.id == id)
            .ok_or_else(|| format!("unknown group: {id}"))?;
        if group.group.application_ids.is_empty() {
            return Err(format!("empty group: {id}"));
        }
        for app_id in &group.group.application_ids {
            let app = &catalog
                .applications
                .iter()
                .find(|a| &a.application.id == app_id)
                .ok_or_else(|| format!("group {id} contains unknown application: {app_id}"))?
                .application;
            if app.site_ids.is_empty() {
                return Err(format!(
                    "application {app_id} has no concrete endpoints; reference rules cannot be probed"
                ));
            }
            for site_id in &app.site_ids {
                if !catalog.sites.iter().any(|s| &s.site.id == site_id) {
                    return Err(format!(
                        "application {app_id} contains unknown site: {site_id}"
                    ));
                }
                let groups = memberships.entry(site_id).or_default();
                if !groups.contains(id) {
                    groups.push(id.clone());
                }
                let identities = applications.entry(site_id).or_default();
                if !identities.iter().any(|a| a.id == *app_id) {
                    identities.push(ApplicationIdentity {
                        id: app_id.clone(),
                        name: app.name.clone(),
                    });
                }
            }
        }
    }
    let mut selected_ids = HashSet::new();
    for id in &request.site_ids {
        if !memberships.contains_key(id.as_str()) {
            return Err(format!("site is not in selected groups: {id}"));
        }
        selected_ids.insert(id.as_str());
    }
    if !selected_ids.is_empty() {
        memberships.retain(|id, _| selected_ids.contains(id));
        applications.retain(|id, _| selected_ids.contains(id));
    }
    let planned = memberships
        .len()
        .checked_mul(request.attempts_per_site)
        .ok_or("planned attempts overflow")?;
    if memberships.is_empty() || memberships.len() > 1000 || planned > 10000 {
        return Err("batch needs 1..1000 sites and at most 10000 attempts".into());
    }
    let bindings: HashMap<_, _> = request
        .permit_bindings
        .iter()
        .map(|b| (b.site_id.as_str(), b.permit_id.as_str()))
        .collect();
    if request.mode != Mode::PublicHttp
        && (bindings.len() != memberships.len()
            || bindings.keys().any(|id| !memberships.contains_key(id)))
    {
        return Err(
            "each selected site needs exactly one permit binding; unrelated bindings forbidden"
                .into(),
        );
    }
    let mut sites = Vec::new();
    let mut targets = Vec::new();
    let mut errors = Vec::new();
    for record in &catalog.sites {
        let site = &record.site;
        let Some(groups) = memberships.get(site.id.as_str()) else {
            continue;
        };
        let endpoint = (|| -> Result<Target, String> {
            use site_catalog::{Protocol, Transport};
            // These capabilities came from Manager::snapshot, never from the request DTO.
            // Keep the explicit protocol/transport gates too: no UDP/QUIC/unknown fallback.
            let capability = if request.mode == Mode::Tcp {
                "tcp"
            } else {
                "http"
            };
            let protocol_allowed = if request.mode == Mode::Tcp {
                matches!(
                    site.protocol,
                    Protocol::Http
                        | Protocol::Https
                        | Protocol::Tcp
                        | Protocol::Tls
                        | Protocol::Other
                )
            } else {
                matches!(site.protocol, Protocol::Http | Protocol::Https)
            };
            if site.transport != Transport::Tcp
                || !protocol_allowed
                || !record.capabilities.iter().any(|c| c == capability)
            {
                return Err(format!(
                    "endpoint does not support {capability} batch mode; explicit TCP transport and supported protocol required"
                ));
            }
            let mut url = if request.mode == Mode::Tcp && !site.address.contains("://") {
                let port = site.port.ok_or("bare TCP host requires site.port")?;
                policy::parse_url(&format!("http://{}:{port}/", site.address))?
            } else {
                policy::parse_url(&site.address)?
            };
            if request.mode == Mode::Tcp
                && let Some(port) = site.port
            {
                url.set_port(Some(port)).map_err(|_| "invalid TCP port")?;
            }
            if control_port == url.port_or_known_default() && control_port.is_some() {
                return Err("AoNet control port is forbidden".into());
            }
            let permit = if request.mode == Mode::PublicHttp {
                policy::check_public_url(&url)?;
                None
            } else {
                let id = bindings.get(site.id.as_str()).ok_or("missing permit")?;
                let p = permits
                    .iter()
                    .find(|p| p.id == *id)
                    .ok_or("unknown permit")?;
                let allowed = policy::parse_url(&p.url)?;
                let matches = if request.mode == Mode::Tcp {
                    policy::host(&allowed) == policy::host(&url)
                        && allowed.port_or_known_default() == url.port_or_known_default()
                } else {
                    allowed == url
                };
                let mode = if request.mode == Mode::Tcp {
                    "tcp"
                } else {
                    "http"
                };
                if !matches || !p.modes.iter().any(|m| m == mode) {
                    return Err("permit must match exact HTTP URL / TCP host+port and mode".into());
                }
                let expiry = chrono::DateTime::parse_from_rfc3339(&p.expires_at)
                    .map_err(|_| "invalid permit expiry")?;
                if expiry
                    <= chrono::Utc::now()
                        + chrono::Duration::seconds(request.batch_timeout_secs as i64)
                {
                    return Err("permit expiry must cover entire batch budget".into());
                }
                if request.concurrency > p.max_concurrency
                    || request.connection_limit > p.max_concurrency
                    || request.start_rate > p.max_rps
                    || request.batch_timeout_secs > p.max_duration
                {
                    return Err("batch exceeds bound permit limits".into());
                }
                Some(p.clone())
            };
            Ok(Target { url, permit })
        })();
        match endpoint {
            Err(e) => errors.push(format!("{}: {e}", site.id)),
            Ok(target) => {
                sites.push(SiteResult {
                    site_id: site.id.clone(),
                    name: site.name.clone(),
                    address: site.address.clone(),
                    endpoint: target.url.to_string(),
                    group_ids: groups.clone(),
                    applications: applications.remove(site.id.as_str()).unwrap_or_default(),
                    permit_id: target.permit.as_ref().map(|p| p.id.clone()),
                    planned: request.attempts_per_site,
                    ..Default::default()
                });
                targets.push(target);
            }
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "inapplicable sites (batch not started): {}",
            errors.into_iter().take(100).collect::<Vec<_>>().join("; ")
        ));
    }
    if sites.len() != memberships.len() {
        return Err("group contains unknown site".into());
    }
    policy::check_fd_budget(request.connection_limit, false)?;
    Ok((
        Snapshot {
            request_id: request.request_id.clone(),
            config: request.clone(),
            route: Default::default(),
            status: "running".into(),
            sites,
            attempts: Vec::new(),
            counts: Counts {
                planned,
                pending_generation: planned,
                ..Default::default()
            },
            error: None,
            details_retained: true,
        },
        targets,
    ))
}

pub fn limits() -> serde_json::Value {
    serde_json::json!({"max_sites":1000,"max_attempts":10000,"max_queue_capacity":256,"max_request_timeout_secs":30,"max_per_site_connections":MAX_CONCURRENCY,"max_tcp_hold_secs":30,
        "public_http":{"max_concurrency":4,"max_connections":4,"max_attempts_per_site":3,"max_start_rate":2,"max_batch_timeout_secs":120},
        "authorized":{"max_concurrency":MAX_CONCURRENCY,"max_connections":MAX_CONCURRENCY,"max_attempts_per_site":100,"max_start_rate":MAX_RPS,"max_batch_timeout_secs":300}})
}

#[cfg(test)]
#[path = "catalog_batch_tests.rs"]
mod tests;
