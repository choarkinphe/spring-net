use crate::proxy::ProxyConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticConfig {
    pub request_id: String,
    pub target: String,
    pub port: u16,
    pub tls: bool,
    #[serde(default)]
    pub allow_lan: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticStage {
    pub status: String,
    pub elapsed_ms: Option<u64>,
    pub error: Option<String>,
    pub observations: Value,
}
impl Default for DiagnosticStage {
    fn default() -> Self {
        Self {
            status: "pending".into(),
            elapsed_ms: None,
            error: None,
            observations: json!({}),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct DiagnosticStages {
    pub dns: DiagnosticStage,
    pub route: DiagnosticStage,
    pub tcp: DiagnosticStage,
    pub tls: DiagnosticStage,
}
#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticSnapshot {
    pub id: String,
    pub request_id: String,
    pub status: String,
    pub config: DiagnosticConfig,
    pub route: crate::proxy_settings::RouteSnapshot,
    pub stages: DiagnosticStages,
    pub selected_ip: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub warnings: Vec<String>,
    pub limitations: Vec<String>,
}
impl DiagnosticSnapshot {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "running" | "stopping" | "draining")
    }
    pub(super) fn new(config: DiagnosticConfig) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(), request_id: config.request_id.clone(),
            route: crate::proxy_settings::RouteSnapshot { proxy: config.proxy.clone(), ..Default::default() },
            config, status: "running".into(), stages: DiagnosticStages::default(),
            selected_ip: None, started_at: now.clone(), updated_at: now, finished_at: None,
            warnings: vec![], limitations: vec![
                "仅诊断单个目标，只建立一次 TCP 连接；不重试、不发送 HTTP 或业务载荷。连接成功不代表登录、订座或出票成功。".into(),
                "DNS 是系统解析视图（含 hosts 与缓存），不是权威 DNS 请求计时。全部答案均检查，后续仅测试第一个固定 IP，其余地址未测。".into(),
                "路由探测协议与业务 TCP 可能不同。星号仅表示未及时收到预期回复，不证明路由器故障；最后响应跳点不是故障边界（RFC1812/RFC5508）。".into(),
                "TLS 使用随包 Mozilla 根证书，不使用操作系统或企业根；保留证书链与身份校验，不自动获取 AIA/OCSP。".into(),
                "预算：DNS 5 秒，路由 30 秒／20 跳／合计 64KiB，TCP 5 秒，TLS 5 秒。系统 DNS 不可强制中断，其收尾可超出执行预算。".into(),
                "LAN 范围只增加 RFC1918 与 IPv6 ULA，不授予承载测试权限；不推断本地子网掩码，无法据此识别所有定向广播地址。".into(),
            ],
        }
    }
}
