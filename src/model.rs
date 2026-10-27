use serde::{Deserialize, Serialize};

pub const MAX_CONCURRENCY: usize = 1000;
pub const MAX_RPS: u32 = 1000;
pub const MAX_DURATION: u64 = 300;
pub const MAX_TIMEOUT: u64 = 30;
pub const PUBLIC_CONCURRENCY: usize = 4;
pub const PUBLIC_RPS: u32 = 2;
pub const PUBLIC_DURATION: u64 = 120;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub checked_at: String,
    #[serde(default = "yes")]
    pub probe_only: bool,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteList {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub builtin: bool,
    pub targets: Vec<Target>,
}

/// Reference data only: never a SiteList, Permit, or executable RunConfig.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionPreset {
    pub id: String,
    pub provider: String,
    pub product: String,
    pub aliases: Vec<String>,
    pub access_type: String,
    pub environment: Option<String>,
    pub version: Option<String>,
    pub status: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub missing: Vec<String>,
    pub endpoints: Vec<ConnectionEndpoint>,
    pub sources: Vec<ConnectionSource>,
    /// Explicit vendor-owned domains, not inferred from shared infrastructure rules.
    pub protected_domains: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionEndpointKind {
    Url,
    Host,
    Wildcard,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionEndpoint {
    pub kind: ConnectionEndpointKind,
    pub address: Option<String>,
    pub protocol: Option<String>,
    pub port: Option<u16>,
    pub purpose: String,
    pub evidence: String,
    pub source_ids: Vec<String>,
    pub notes: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSource {
    pub id: String,
    pub title: String,
    pub url: String,
    pub checked_at: String,
    pub published_at: Option<String>,
    pub scope: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyPermit {
    pub url: String,
    pub allowed_ips: Vec<ipnet::IpNet>,
    pub authorization: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permit {
    pub id: String,
    pub name: String,
    pub url: String,
    pub modes: Vec<String>,
    pub allowed_ips: Vec<ipnet::IpNet>,
    pub max_concurrency: usize,
    pub max_rps: u32,
    pub max_duration: u64,
    pub authorization: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy_routes: Vec<ProxyPermit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub mode: String,
    #[serde(default)]
    pub list_ids: Vec<String>,
    #[serde(default)]
    pub target_ids: Vec<String>,
    #[serde(default)]
    pub authorized_ids: Vec<String>,
    pub concurrency: usize,
    pub rps: u32,
    pub duration_secs: u64,
    pub timeout_secs: u64,
    #[serde(default)]
    pub ramp_secs: u64,
    #[serde(default)]
    pub fresh_connections: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<crate::proxy::ProxyConfig>,
}

impl RunConfig {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(proxy) = &self.proxy {
            crate::proxy::normalize_proxy_config(proxy, None).map_err(|e| e.to_string())?;
        }
        if !["probe", "http", "tcp"].contains(&self.mode.as_str()) {
            return Err("未知测试模式".into());
        }
        if !(1..=MAX_CONCURRENCY).contains(&self.concurrency)
            || !(1..=MAX_RPS).contains(&self.rps)
            || !(1..=MAX_DURATION).contains(&self.duration_secs)
            || !(1..=MAX_TIMEOUT).contains(&self.timeout_secs)
            || self.ramp_secs > self.duration_secs
        {
            return Err(
                "并发/速率范围 1–1000，时长 1–300 秒，超时 1–30 秒，爬升不能超过时长".into(),
            );
        }
        if self.mode == "probe"
            && (self.concurrency > PUBLIC_CONCURRENCY
                || self.rps > PUBLIC_RPS
                || self.duration_secs > PUBLIC_DURATION)
        {
            return Err("公共探测限制：最多 4 并发、2 RPS、120 秒".into());
        }
        if self.list_ids.len() > 100
            || self.target_ids.len() > 1000
            || self.authorized_ids.len() > 100
        {
            return Err("选择目标过多".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TargetStats {
    pub id: String,
    pub name: String,
    pub url: String,
    pub attempted: u64,
    pub completed: u64,
    pub responses: u64,
    pub good_status: u64,
    pub errors: u64,
    pub cancelled: u64,
    pub active: usize,
    pub connected: usize,
    pub last_status: Option<u16>,
    pub last_error: Option<String>,
    pub last_latency_ms: Option<f64>,
    pub location: Option<String>,
    pub bytes: u64,
    pub truncated: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub elapsed_secs: f64,
    pub rps: f64,
    pub p95_ms: Option<f64>,
    pub active: usize,
    pub connected: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub status: String,
    pub mode: String,
    pub started_at: String,
    pub elapsed_secs: f64,
    pub config: RunConfig,
    pub reason: String,
    pub attempted: u64,
    pub completed: u64,
    pub responses: u64,
    pub good_status: u64,
    pub errors: u64,
    pub cancelled: u64,
    pub active: usize,
    pub peak_active: usize,
    pub connected: usize,
    pub peak_connected: usize,
    pub bytes: u64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub rps: f64,
    pub series: Vec<Sample>,
    pub targets: Vec<TargetStats>,
    pub sequence: u64,
}
impl Run {
    pub fn active(&self) -> bool {
        self.status == "running" || self.status == "stopping"
    }
}
