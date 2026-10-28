//! Strict v2 application catalog. Reference rules are never expanded or probed.
use crate::{
    model::{ConnectionEndpointKind, ConnectionPreset, SiteList},
    storage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::Mutex,
};

const VERSION: u32 = 2;
const MAX_BYTES: usize = 1_048_576;
const MAX_SITES: usize = 1000;
const MAX_APPLICATIONS: usize = 1000;
const MAX_GROUPS: usize = 50;
const MAX_ITEMS: usize = 100;
const MAX_REFS: usize = 10_000;
const FILE_NAME: &str = "site-catalog.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Tcp,
    Udp,
    Unspecified,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Http,
    Https,
    Tcp,
    Tls,
    UdpDns,
    Quic,
    Other,
    Unspecified,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationCategory {
    Web,
    Gds,
    Communication,
    Network,
    Custom,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    Host,
    Url,
    Wildcard,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: Option<u16>,
    pub transport: Transport,
    pub protocol: Protocol,
    pub purpose: String,
    pub alpn: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRule {
    pub id: String,
    pub kind: RuleKind,
    pub address: Option<String>,
    pub transport: Option<Transport>,
    pub protocol: Option<Protocol>,
    /// Human-readable single port, range, dynamic range, or unresolved port description.
    pub ports: String,
    pub purpose: String,
    pub evidence: String,
    pub notes: String,
    pub source_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Application {
    pub id: String,
    pub name: String,
    pub category: ApplicationCategory,
    pub provider: String,
    pub product: String,
    pub aliases: Vec<String>,
    pub access_type: String,
    pub site_ids: Vec<String>,
    pub reference_rules: Vec<ReferenceRule>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub description: String,
    pub application_ids: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
    pub schema_version: u32,
    pub expected_revision: u64,
    pub sites: Vec<Site>,
    pub applications: Vec<Application>,
    pub groups: Vec<Group>,
}
#[derive(Clone, Debug, Serialize)]
pub struct CatalogSite {
    #[serde(flatten)]
    pub site: Site,
    pub builtin: bool,
    pub source: String,
    pub checked_at: String,
    pub capabilities: Vec<String>,
    pub unavailable_reasons: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct CatalogApplication {
    #[serde(flatten)]
    pub application: Application,
    pub builtin: bool,
    pub sources: Vec<crate::model::ConnectionSource>,
    pub missing: Vec<String>,
    pub limitations: Vec<String>,
    pub summary: String,
    pub status: String,
    pub environment: Option<String>,
    pub version: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct CatalogGroup {
    #[serde(flatten)]
    pub group: Group,
    pub builtin: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub version: u32,
    pub revision: u64,
    pub sites: Vec<CatalogSite>,
    pub applications: Vec<CatalogApplication>,
    pub groups: Vec<CatalogGroup>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedCatalog {
    version: u32,
    revision: u64,
    sites: Vec<Site>,
    applications: Vec<Application>,
    groups: Vec<Group>,
}
struct State {
    saved: SavedCatalog,
    source: Option<Vec<u8>>,
}
pub struct Manager {
    path: PathBuf,
    builtin_sites: Vec<CatalogSite>,
    builtin_applications: Vec<CatalogApplication>,
    builtin_groups: Vec<CatalogGroup>,
    state: Mutex<State>,
}
impl Manager {
    /// Backwards-friendly constructor: migrates only legacy site lists; reference products are optional.
    pub fn new(data_dir: impl AsRef<Path>, lists: &[SiteList]) -> Result<Self, String> {
        Self::load(data_dir.as_ref(), lists, &[], false)
    }
    /// Read-only initialization. Missing v2 storage is assembled in memory, never written on read.
    pub fn new_with_connections(
        data_dir: impl AsRef<Path>,
        lists: &[SiteList],
        connections: &[ConnectionPreset],
    ) -> Result<Self, String> {
        Self::load(data_dir.as_ref(), lists, connections, true)
    }
    fn load(
        data_dir: &Path,
        lists: &[SiteList],
        connections: &[ConnectionPreset],
        placeholders: bool,
    ) -> Result<Self, String> {
        let (mut builtin_sites, mut builtin_applications, builtin_groups) = list_builtins(lists)?;
        let (ref_sites, mut ref_apps) = reference_builtins(connections)?;
        if placeholders {
            ref_apps.extend(communication_placeholders());
            let existing: HashSet<_> = builtin_applications
                .iter()
                .map(|a| a.application.id.clone())
                .collect();
            if ref_apps
                .iter()
                .any(|a| existing.contains(&a.application.id))
            {
                return Err("内置应用 ID 冲突".into());
            }
            let ref_site_ids: HashSet<_> = ref_sites.iter().map(|s| s.site.id.clone()).collect();
            if builtin_sites
                .iter()
                .any(|s| ref_site_ids.contains(&s.site.id))
            {
                return Err("内置端点 ID 冲突".into());
            }
            builtin_applications.extend(ref_apps);
            builtin_sites.extend(ref_sites);
        }
        // The legacy web taxonomy stays at seven groups; GDS and communication applications
        // are discoverable by category and do not invent new user-visible groups.
        // Groups must not shadow each other, even if an input preset is malicious.
        let mut group_ids = HashSet::new();
        if builtin_groups
            .iter()
            .any(|g| !group_ids.insert(g.group.id.clone()))
        {
            return Err("内置分组 ID 重复".into());
        }
        let path = data_dir.join(FILE_NAME);
        let source = read_source(&path)?;
        let saved = match &source {
            Some(bytes) => decode_saved(bytes, &builtin_sites)?,
            None => migrate(lists)?,
        };
        if saved.version != VERSION {
            return Err("不支持的应用目录版本；未覆盖原文件".into());
        }
        validate(
            &saved,
            &builtin_sites,
            &builtin_applications,
            &builtin_groups,
        )
        .map_err(|e| format!("应用目录无法加载，未写入任何文件：{e}"))?;
        serialize(&saved)?;
        Ok(Self {
            path,
            builtin_sites,
            builtin_applications,
            builtin_groups,
            state: Mutex::new(State { saved, source }),
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.make_snapshot(&state.saved)
    }
    pub fn save(&self, request: SaveRequest) -> Result<Snapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "应用目录锁异常，请重启应用")?;
        if request.schema_version != VERSION {
            return Err("只接受 schema_version=2；旧版保存请求被拒绝".into());
        }
        if request.expected_revision != state.saved.revision {
            return Err("应用目录版本冲突：数据已更新，请重新读取后再保存".into());
        }
        let revision = state
            .saved
            .revision
            .checked_add(1)
            .ok_or("应用目录修订号已耗尽")?;
        let saved = SavedCatalog {
            version: VERSION,
            revision,
            sites: request.sites,
            applications: request.applications,
            groups: request.groups,
        };
        validate(
            &saved,
            &self.builtin_sites,
            &self.builtin_applications,
            &self.builtin_groups,
        )?;
        let bytes = serialize(&saved)?;
        if read_source(&self.path)? != state.source {
            return Err(
                "应用目录文件已被外部修改、创建或删除；为避免覆盖，请备份并重启后重新读取".into(),
            );
        }
        storage::atomic_bytes(&self.path, &bytes)
            .map_err(|e| format!("应用目录保存失败；请重新读取核对，不要自动重试：{e}"))?;
        state.saved = saved;
        state.source = Some(bytes);
        Ok(self.make_snapshot(&state.saved))
    }
    fn make_snapshot(&self, saved: &SavedCatalog) -> Snapshot {
        let sites = self
            .builtin_sites
            .iter()
            .cloned()
            .chain(saved.sites.iter().cloned().map(|site| {
                let (capabilities, unavailable_reasons) = capabilities(&site);
                CatalogSite {
                    site,
                    builtin: false,
                    source: String::new(),
                    checked_at: String::new(),
                    capabilities,
                    unavailable_reasons,
                }
            }))
            .collect();
        Snapshot {
            version: VERSION,
            revision: saved.revision,
            sites,
            applications: self
                .builtin_applications
                .iter()
                .cloned()
                .chain(
                    saved
                        .applications
                        .iter()
                        .cloned()
                        .map(|application| CatalogApplication {
                            application,
                            builtin: false,
                            sources: vec![],
                            missing: vec![],
                            limitations: vec![],
                            summary: String::new(),
                            status: "custom".into(),
                            environment: None,
                            version: None,
                        }),
                )
                .collect(),
            groups: self
                .builtin_groups
                .iter()
                .cloned()
                .chain(saved.groups.iter().cloned().map(|group| CatalogGroup {
                    group,
                    builtin: false,
                }))
                .collect(),
        }
    }
}
// A dedicated strict legacy DTO prevents v2 fields/provenance from being accepted as v1.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Site {
    id: String,
    name: String,
    address: String,
    port: Option<u16>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Group {
    id: String,
    name: String,
    description: String,
    site_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct V1Catalog {
    version: u32,
    revision: u64,
    sites: Vec<V1Site>,
    groups: Vec<V1Group>,
}
#[derive(Deserialize)]
struct VersionHeader {
    version: u32,
}
fn decode_saved(bytes: &[u8], builtin_sites: &[CatalogSite]) -> Result<SavedCatalog, String> {
    let header: VersionHeader =
        serde_json::from_slice(bytes).map_err(|e| format!("应用目录损坏，未覆盖原文件：{e}"))?;
    match header.version {
        2 => {
            serde_json::from_slice(bytes).map_err(|e| format!("v2 应用目录损坏，未覆盖原文件：{e}"))
        }
        1 => {
            let old: V1Catalog = serde_json::from_slice(bytes)
                .map_err(|e| format!("v1 应用目录损坏，未覆盖原文件：{e}"))?;
            if old.version != 1 {
                return Err("旧目录版本无效".into());
            }
            let mut ids: HashSet<String> =
                builtin_sites.iter().map(|s| s.site.id.clone()).collect();
            let mut applications = Vec::new();
            let mut sites = Vec::new();
            let mut site_to_app: HashMap<String, String> = builtin_sites
                .iter()
                .map(|s| (s.site.id.clone(), stable_id("builtin-app", &[&s.site.id])))
                .collect();
            for s in old.sites {
                custom_id(&s.id, &mut ids, "旧端点")?;
                let address = normalize_address(&s.address, s.port)?;
                let mut site = site_from_old(&s.id, &s.name, &address, "")?;
                site.port = s.port;
                let app_id = stable_id("legacy-app", &[&s.id]);
                site_to_app.insert(s.id.clone(), app_id.clone());
                applications.push(single_site_application(
                    app_id,
                    &site,
                    ApplicationCategory::Custom,
                ));
                sites.push(site);
            }
            let mut groups = Vec::new();
            for g in old.groups {
                custom_id(&g.id, &mut ids, "旧分组")?;
                let mut members = HashSet::new();
                let mut application_ids = Vec::new();
                for sid in g.site_ids {
                    if !members.insert(sid.clone()) {
                        return Err("旧分组重复引用同一端点".into());
                    }
                    application_ids.push(
                        site_to_app
                            .get(&sid)
                            .ok_or_else(|| format!("旧分组引用不存在的端点 {sid}"))?
                            .clone(),
                    );
                }
                groups.push(Group {
                    id: g.id,
                    name: g.name,
                    description: g.description,
                    application_ids,
                });
            }
            Ok(SavedCatalog {
                version: 2,
                revision: old.revision,
                sites,
                applications,
                groups,
            })
        }
        _ => Err("不支持的应用目录版本；未覆盖原文件".into()),
    }
}
fn single_site_application(id: String, site: &Site, category: ApplicationCategory) -> Application {
    Application {
        id,
        name: site.name.clone(),
        category,
        provider: String::new(),
        product: site.name.clone(),
        aliases: vec![],
        access_type: if matches!(site.protocol, Protocol::Http | Protocol::Https) {
            "browser"
        } else {
            "unconfirmed"
        }
        .into(),
        site_ids: vec![site.id.clone()],
        reference_rules: vec![],
    }
}
fn communication_placeholders() -> Vec<CatalogApplication> {
    [
        ("wechat", "微信", "Tencent", vec!["WeChat", "微信"]),
        ("qq", "QQ", "Tencent", vec!["QQ"]),
        ("dingtalk", "钉钉", "Alibaba", vec!["DingTalk", "钉钉"]),
    ]
    .into_iter()
    .map(|(key, name, provider, aliases)| {
        let rules = [
            ("login", "登录"),
            ("messaging", "消息"),
            ("files", "文件"),
            ("audio-video", "音视频"),
        ]
        .into_iter()
        .map(|(purpose_key, purpose)| ReferenceRule {
            id: stable_id("builtin-rule", &[key, purpose_key]),
            kind: RuleKind::Unknown,
            address: None,
            transport: None,
            protocol: None,
            ports: String::new(),
            purpose: format!("{name}{purpose}连接配置（尚未证实）"),
            evidence: "仅记录产品身份及配置用途，未核实主机/端口/协议".into(),
            notes: "需用户提供本机构适用的厂商资料；不套用其他产品白名单、不猜测 IP 或端口".into(),
            source_ids: vec![],
        })
        .collect();
        CatalogApplication {
            application: Application {
                id: stable_id("builtin-app", &["communication", key]),
                name: name.into(),
                category: ApplicationCategory::Communication,
                provider: provider.into(),
                product: name.into(),
                aliases: aliases.into_iter().map(str::to_owned).collect(),
                access_type: "unconfirmed".into(),
                site_ids: vec![],
                reference_rules: rules,
            },
            builtin: true,
            sources: vec![],
            missing: vec![
                "实际客户端版本、接入环境和机构适用网络文档".into(),
                "登录、消息、文件、音视频的具体主机、端口和协议".into(),
            ],
            limitations: vec![
                if key == "dingtalk" {
                    "钉钉网络文档入口仅取得壳页，未取得可核实端点矩阵；不声明已核实"
                } else {
                    "尚无已核实网络端点资料；此占位不构成厂商白名单"
                }
                .into(),
            ],
            summary: format!("{name}应用身份与配置用途占位；没有可执行端点"),
            status: "incomplete".into(),
            environment: None,
            version: None,
        }
    })
    .collect()
}
fn read_source(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            Err("应用目录必须为普通文件，不能是符号链接或目录；未覆盖原文件".into())
        }
        Ok(_) => storage::read_bounded(path, MAX_BYTES as u64)
            .map(Some)
            .map_err(|e| format!("无法读取应用目录：{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("无法检查应用目录文件：{e}")),
    }
}
fn serialize(saved: &SavedCatalog) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(saved).map_err(|e| format!("应用目录序列化失败：{e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err("应用目录最终 JSON 不能超过 1 MiB".into());
    }
    Ok(bytes)
}
fn stable_id(prefix: &str, parts: &[&str]) -> String {
    let mut hash: u128 = 0x6c62272e07bb014262b821756295c58d;
    for part in parts {
        for byte in (part.len() as u64)
            .to_le_bytes()
            .iter()
            .chain(part.as_bytes())
        {
            hash ^= *byte as u128;
            hash = hash.wrapping_mul(0x0000000001000000000000000000013b);
        }
    }
    format!("{prefix}-{hash:032x}")
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn text(s: &str, max: usize, required: bool, field: &str) -> Result<(), String> {
    if s.len() > max
        || (required && s.trim().is_empty())
        || s.chars()
            .any(|c| c.is_control() && (required || !matches!(c, '\n' | '\r' | '\t')))
    {
        return Err(format!(
            "{field}不能为空、包含控制字符或超过 {max} 个 UTF-8 字节"
        ));
    }
    Ok(())
}
fn custom_id(id: &str, ids: &mut HashSet<String>, kind: &str) -> Result<(), String> {
    if !valid_id(id) || id.starts_with("builtin-") || !ids.insert(id.into()) {
        return Err(format!("{kind} ID 无效、重复或试图修改内置数据"));
    }
    Ok(())
}
fn validate(
    saved: &SavedCatalog,
    builtin_sites: &[CatalogSite],
    builtin_apps: &[CatalogApplication],
    builtin_groups: &[CatalogGroup],
) -> Result<(), String> {
    if saved.sites.len() > MAX_SITES
        || saved.applications.len() > MAX_APPLICATIONS
        || saved.groups.len() > MAX_GROUPS
    {
        return Err("自定义端点/应用最多 1000 个，分组最多 50 个".into());
    }
    let mut ids: HashSet<String> = builtin_sites
        .iter()
        .map(|s| s.site.id.clone())
        .chain(builtin_apps.iter().map(|a| a.application.id.clone()))
        .chain(builtin_groups.iter().map(|g| g.group.id.clone()))
        .collect();
    let mut site_ids: HashSet<String> = builtin_sites.iter().map(|s| s.site.id.clone()).collect();
    for s in &saved.sites {
        custom_id(&s.id, &mut ids, "端点")?;
        validate_site(s)?;
        site_ids.insert(s.id.clone());
    }
    let mut app_ids: HashSet<String> = builtin_apps
        .iter()
        .map(|a| a.application.id.clone())
        .collect();
    let mut refs = 0usize;
    for a in &saved.applications {
        custom_id(&a.id, &mut ids, "应用")?;
        app_ids.insert(a.id.clone());
        text(&a.name, 200, true, "应用名称")?;
        text(&a.provider, 200, false, "厂商")?;
        text(&a.product, 200, false, "产品")?;
        text(&a.access_type, 100, false, "接入方式")?;
        if a.aliases.len() > 20 {
            return Err("应用别名最多 20 条".into());
        }
        let mut aliases = HashSet::new();
        for alias in &a.aliases {
            text(alias, 200, true, "应用别名")?;
            if !aliases.insert(alias) {
                return Err("应用别名不能重复".into());
            }
        }
        if a.site_ids.len() > MAX_ITEMS || a.reference_rules.len() > MAX_ITEMS {
            return Err("每个应用最多引用 100 个端点、包含 100 条规则".into());
        }
        refs += a.site_ids.len() + a.reference_rules.len();
        if refs > MAX_REFS {
            return Err("应用和分组引用总数不能超过 10000".into());
        }
        let mut seen = HashSet::new();
        for id in &a.site_ids {
            if !site_ids.contains(id) {
                return Err(format!("应用「{}」引用了不存在的端点 {id}", a.name));
            }
            if !seen.insert(id) {
                return Err("应用不能重复引用端点".into());
            }
        }
        let mut rules = HashSet::new();
        for r in &a.reference_rules {
            custom_id(&r.id, &mut ids, "规则")?;
            validate_rule(r, false)?;
            if !rules.insert(&r.id) {
                return Err("规则 ID 重复".into());
            }
            if !r.source_ids.is_empty() {
                return Err("自定义规则不得伪造内置来源引用".into());
            }
        }
    }
    for g in &saved.groups {
        custom_id(&g.id, &mut ids, "分组")?;
        text(&g.name, 200, true, "分组名称")?;
        text(&g.description, 1000, false, "分组说明")?;
        refs += g.application_ids.len();
        if refs > MAX_REFS {
            return Err("应用和分组引用总数不能超过 10000".into());
        }
        let mut members = HashSet::new();
        for id in &g.application_ids {
            if !app_ids.contains(id) {
                return Err(format!("分组「{}」引用了不存在的应用 {id}", g.name));
            }
            if !members.insert(id) {
                return Err("分组不能重复引用同一应用".into());
            }
        }
    }
    Ok(())
}
fn validate_site(s: &Site) -> Result<(), String> {
    text(&s.name, 200, true, "端点名称")?;
    text(&s.purpose, 1000, false, "端点用途")?;
    normalize_address(&s.address, s.port)?;
    if let Some(a) = &s.alpn {
        if a.is_empty()
            || a.len() > 255
            || !a.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b',')
        {
            return Err("ALPN 必须是 1–255 字节单个 ASCII 协议标识".into());
        }
        if s.protocol != Protocol::Quic {
            return Err("只有 QUIC 端点可填写 ALPN".into());
        }
    }
    match s.protocol {
        Protocol::Http | Protocol::Https => {
            let u = url::Url::parse(&s.address).map_err(|_| "HTTP(S) 端点必须是完整 URL")?;
            if u.scheme()
                != if s.protocol == Protocol::Http {
                    "http"
                } else {
                    "https"
                }
            {
                return Err("端点 URL 与显式协议不一致".into());
            }
            if s.transport != Transport::Tcp {
                return Err("HTTP(S) 必须使用 TCP 传输".into());
            }
        }
        _ => {
            if s.address.contains("://") {
                return Err("非 HTTP(S) 端点只允许单个 host/IP".into());
            }
        }
    }
    match s.protocol {
        Protocol::Tcp | Protocol::Tls if s.transport != Transport::Tcp => {
            return Err("TCP/TLS 端点必须使用 TCP 传输".into());
        }
        Protocol::Quic | Protocol::UdpDns if s.transport != Transport::Udp => {
            return Err("QUIC/UDP DNS 端点必须使用 UDP 传输".into());
        }
        _ => {}
    }
    Ok(())
}
fn validate_rule(r: &ReferenceRule, builtin: bool) -> Result<(), String> {
    text(&r.ports, 200, false, "端口规则")?;
    text(&r.purpose, 1000, true, "规则用途")?;
    text(&r.evidence, 2000, false, "规则证据")?;
    text(&r.notes, 2000, false, "规则备注")?;
    if r.source_ids.len() > 32 || (!builtin && !r.source_ids.is_empty()) {
        return Err("规则来源引用无效".into());
    }
    if let Some(address) = &r.address {
        text(address, 2048, true, "规则地址")?;
    }
    match (r.kind, &r.address) {
        (RuleKind::Unknown, None) => {}
        (RuleKind::Unknown, Some(_)) => return Err("unknown 规则地址必须为 null".into()),
        (RuleKind::Url, Some(v)) => {
            normalize_address(v, None)?;
            let u = url::Url::parse(v).map_err(|_| "URL规则格式无效")?;
            if !matches!(u.scheme(), "http" | "https")
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                return Err("URL规则只允许无凭据的 HTTP(S) URL".into());
            }
        }
        (RuleKind::Host, Some(v)) => {
            normalize_host(v)?;
        }
        (RuleKind::Wildcard, Some(v)) => {
            normalize_host(v.strip_prefix("*.").ok_or("通配规则必须以 *. 开头")?)?;
        }
        _ => return Err("规则类型与地址不匹配".into()),
    }
    Ok(())
}
fn capabilities(s: &Site) -> (Vec<String>, Vec<String>) {
    let mut caps = vec!["ping".into(), "traceroute".into(), "dns".into()];
    let mut reasons = Vec::new();
    let explicit = s.protocol != Protocol::Unspecified && s.transport != Transport::Unspecified;
    if !explicit {
        reasons.push("协议或传输未知；仅允许主机层 Ping/Traceroute/系统 DNS".into());
        return (caps, reasons);
    }
    match (s.transport, s.protocol) {
        (Transport::Tcp, Protocol::Tcp) if s.port.is_some() => caps.push("tcp".into()),
        (Transport::Tcp, Protocol::Tls) if s.port.is_some() => {
            caps.extend(["tcp".into(), "tls".into()])
        }
        (Transport::Tcp, Protocol::Http) => caps.extend(["tcp".into(), "http".into()]),
        (Transport::Tcp, Protocol::Https) => {
            caps.extend(["tcp".into(), "tls".into(), "http".into()])
        }
        (Transport::Tcp, Protocol::Other) if s.port.is_some() => {
            caps.push("tcp".into());
            reasons.push("仅 TCP 传输层建连，不代表自定义业务协议成功".into());
        }
        (Transport::Udp, Protocol::UdpDns) if s.port.is_some() => caps.push("udp_dns".into()),
        (Transport::Udp, Protocol::Quic) if s.port.is_some() && s.alpn.is_some() => {
            caps.push("quic".into())
        }
        (Transport::Udp, Protocol::Quic) => reasons.push("QUIC 需要明确填写 ALPN".into()),
        (Transport::Udp, _) => reasons.push("协议未映射到安全的单项检测工具".into()),
        _ => reasons.push("端点缺少明确协议/传输/单端口，不能连接检测".into()),
    }
    (caps, reasons)
}
type BuiltinCatalog = (Vec<CatalogSite>, Vec<CatalogApplication>, Vec<CatalogGroup>);
fn list_builtins(lists: &[SiteList]) -> Result<BuiltinCatalog, String> {
    let mut sites = Vec::new();
    let mut apps = Vec::new();
    let mut groups = Vec::new();
    let mut address_ids: HashMap<String, String> = HashMap::new();
    for list in lists.iter().filter(|l| l.builtin) {
        let mut app_ids = Vec::new();
        for t in &list.targets {
            let site = site_from_old(
                &stable_id("builtin-site", &[&canonical_old_address(&t.url)?]),
                &t.name,
                &t.url,
                &t.region,
            )?;
            let key = canonical_old_address(&site.address)?;
            let id = if let Some(id) = address_ids.get(&key) {
                id.clone()
            } else {
                let id = site.id.clone();
                let (capabilities, unavailable_reasons) = capabilities(&site);
                sites.push(CatalogSite {
                    site,
                    builtin: true,
                    source: t.source.clone(),
                    checked_at: t.checked_at.clone(),
                    capabilities,
                    unavailable_reasons,
                });
                address_ids.insert(key, id.clone());
                id
            };
            let app_id = stable_id("builtin-app", &[&id]);
            if !apps
                .iter()
                .any(|a: &CatalogApplication| a.application.id == app_id)
            {
                apps.push(CatalogApplication {
                    application: Application {
                        id: app_id.clone(),
                        name: t.name.clone(),
                        category: ApplicationCategory::Web,
                        provider: String::new(),
                        product: t.name.clone(),
                        aliases: vec![],
                        access_type: "browser".into(),
                        site_ids: vec![id.clone()],
                        reference_rules: vec![],
                    },
                    builtin: true,
                    sources: vec![],
                    missing: vec![],
                    limitations: vec![],
                    summary: String::new(),
                    status: "reference_only".into(),
                    environment: None,
                    version: None,
                });
            }
            if !app_ids.contains(&app_id) {
                app_ids.push(app_id);
            }
        }
        groups.push(CatalogGroup {
            group: Group {
                id: stable_id("builtin-group", &[&list.id]),
                name: list.name.clone(),
                description: list.description.clone(),
                application_ids: app_ids,
            },
            builtin: true,
        });
    }
    Ok((sites, apps, groups))
}
fn canonical_old_address(value: &str) -> Result<String, String> {
    let normalized = normalize_address(value, None)?;
    Ok(if normalized.contains("://") {
        url::Url::parse(&normalized)
            .map_err(|e| e.to_string())?
            .to_string()
    } else {
        normalized
    })
}
fn site_from_old(id: &str, name: &str, address: &str, purpose: &str) -> Result<Site, String> {
    let address = normalize_address(address, None)?;
    let protocol = if address.starts_with("https://") {
        Protocol::Https
    } else if address.starts_with("http://") {
        Protocol::Http
    } else {
        Protocol::Unspecified
    };
    Ok(Site {
        id: id.into(),
        name: name.into(),
        address,
        port: None,
        transport: if protocol == Protocol::Unspecified {
            Transport::Unspecified
        } else {
            Transport::Tcp
        },
        protocol,
        purpose: purpose.into(),
        alpn: None,
    })
}
fn migrate(lists: &[SiteList]) -> Result<SavedCatalog, String> {
    let mut saved = SavedCatalog {
        version: VERSION,
        revision: 0,
        sites: vec![],
        applications: vec![],
        groups: vec![],
    };
    let mut same: HashMap<(String, String), String> = HashMap::new();
    for list in lists.iter().filter(|l| !l.builtin) {
        let mut members = vec![];
        let mut targets = HashSet::new();
        for t in &list.targets {
            if !targets.insert(&t.id) {
                return Err("旧列表目标 ID 重复，无法无损迁移".into());
            }
            let addr = canonical_old_address(&t.url)?;
            let key = (t.name.clone(), addr.clone());
            let app_id = if let Some(id) = same.get(&key) {
                id.clone()
            } else {
                let site_id = stable_id("legacy-site", &[&list.id, &t.id]);
                let app_id = stable_id("legacy-app", &[&site_id]);
                let site = site_from_old(&site_id, &t.name, &addr, &t.region)?;
                saved.sites.push(site);
                saved.applications.push(Application {
                    id: app_id.clone(),
                    name: t.name.clone(),
                    category: ApplicationCategory::Web,
                    provider: String::new(),
                    product: t.name.clone(),
                    aliases: vec![],
                    access_type: "unconfirmed".into(),
                    site_ids: vec![site_id],
                    reference_rules: vec![],
                });
                same.insert(key, app_id.clone());
                app_id
            };
            if !members.contains(&app_id) {
                members.push(app_id)
            }
        }
        saved.groups.push(Group {
            id: stable_id("legacy-group", &[&list.id]),
            name: list.name.clone(),
            description: list.description.clone(),
            application_ids: members,
        });
    }
    Ok(saved)
}
fn reference_builtins(
    presets: &[ConnectionPreset],
) -> Result<(Vec<CatalogSite>, Vec<CatalogApplication>), String> {
    crate::connections::validate(presets)?;
    let mut sites = vec![];
    let mut apps = vec![];
    let mut site_ids = HashSet::new();
    let mut app_ids = HashSet::new();
    for p in presets {
        let app_id = stable_id("builtin-app", &[&p.id]);
        if !app_ids.insert(app_id.clone()) {
            return Err("连接参考应用 ID 冲突".into());
        }
        let source_map: HashMap<_, _> = p.sources.iter().map(|s| (s.id.as_str(), s)).collect();
        let mut rules = vec![];
        let mut refs = vec![];
        if p.endpoints.len() > MAX_ITEMS {
            return Err("每个内置应用最多 100 条参考规则".into());
        }
        for ep in &p.endpoints {
            let kind = match ep.kind {
                ConnectionEndpointKind::Url => RuleKind::Url,
                ConnectionEndpointKind::Host => {
                    if ep.address.is_some() {
                        RuleKind::Host
                    } else {
                        RuleKind::Unknown
                    }
                }
                ConnectionEndpointKind::Wildcard => RuleKind::Wildcard,
            };
            let proto = ep.protocol.as_deref().and_then(parse_protocol);
            let transport = proto.map(protocol_transport);
            let ports = ep.port.map(|v| v.to_string()).unwrap_or_default();
            let rule_id = stable_id(
                "builtin-rule",
                &[
                    &p.id,
                    &format!("{:?}", kind),
                    ep.address.as_deref().unwrap_or(""),
                    ep.protocol.as_deref().unwrap_or(""),
                    &ports,
                    &ep.purpose,
                ],
            );
            let rule_id_for_site = rule_id.clone();
            rules.push(ReferenceRule {
                id: rule_id,
                kind,
                address: ep.address.clone(),
                transport,
                protocol: proto,
                ports: ports.clone(),
                purpose: ep.purpose.clone(),
                evidence: ep.evidence.clone(),
                notes: ep.notes.clone(),
                source_ids: ep.source_ids.clone(),
            });
            if matches!(ep.kind, ConnectionEndpointKind::Url)
                && ep
                    .address
                    .as_deref()
                    .is_some_and(|u| u.starts_with("https://") || u.starts_with("http://"))
                && proto.is_some_and(|v| matches!(v, Protocol::Http | Protocol::Https))
            {
                let sid = stable_id("builtin-site", &[&p.id, &rule_id_for_site]);
                if site_ids.insert(sid.clone()) {
                    let site = site_from_old(
                        &sid,
                        &ep.purpose,
                        ep.address.as_deref().unwrap(),
                        &ep.purpose,
                    )?;
                    let (caps, reasons) = capabilities(&site);
                    let source = ep
                        .source_ids
                        .first()
                        .and_then(|id| source_map.get(id.as_str()))
                        .map(|s| s.url.clone())
                        .unwrap_or_default();
                    let checked = ep
                        .source_ids
                        .iter()
                        .filter_map(|id| source_map.get(id.as_str()).map(|s| s.checked_at.as_str()))
                        .max()
                        .unwrap_or("")
                        .to_owned();
                    sites.push(CatalogSite {
                        site,
                        builtin: true,
                        source,
                        checked_at: checked,
                        capabilities: caps,
                        unavailable_reasons: reasons,
                    });
                    refs.push(sid);
                }
            }
        }
        let category = if p.provider.eq_ignore_ascii_case("Travelport")
            || p.provider.eq_ignore_ascii_case("Sabre")
            || p.provider.eq_ignore_ascii_case("Amadeus")
            || p.provider.eq_ignore_ascii_case("TravelSky")
        {
            ApplicationCategory::Gds
        } else {
            ApplicationCategory::Custom
        };
        apps.push(CatalogApplication {
            application: Application {
                id: app_id,
                name: p.product.clone(),
                category,
                provider: p.provider.clone(),
                product: p.product.clone(),
                aliases: p.aliases.clone(),
                access_type: p.access_type.clone(),
                site_ids: refs,
                reference_rules: rules,
            },
            builtin: true,
            sources: p.sources.clone(),
            missing: p.missing.clone(),
            limitations: p.limitations.clone(),
            summary: p.summary.clone(),
            status: p.status.clone(),
            environment: p.environment.clone(),
            version: p.version.clone(),
        });
    }
    Ok((sites, apps))
}
fn parse_protocol(p: &str) -> Option<Protocol> {
    match p.to_ascii_lowercase().as_str() {
        "http" => Some(Protocol::Http),
        "https" => Some(Protocol::Https),
        "tcp" => Some(Protocol::Tcp),
        "tls" => Some(Protocol::Tls),
        "udp_dns" | "dns" => Some(Protocol::UdpDns),
        "quic" => Some(Protocol::Quic),
        "other" => Some(Protocol::Other),
        _ => None,
    }
}
fn protocol_transport(p: Protocol) -> Transport {
    match p {
        Protocol::UdpDns | Protocol::Quic => Transport::Udp,
        Protocol::Http | Protocol::Https | Protocol::Tcp | Protocol::Tls => Transport::Tcp,
        _ => Transport::Unspecified,
    }
}
/// No DNS, network I/O, private-address policy, or permit checks occur here.
fn normalize_address(value: &str, default_port: Option<u16>) -> Result<String, String> {
    if default_port == Some(0) {
        return Err("默认端口必须在 1–65535 之间".into());
    }
    if value.is_empty()
        || value.len() > 2048
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
        || value.contains(['\\', '*', ',', ';', '|', '?', '#'])
    {
        return Err("站点地址无效：最多 2048 个 UTF-8 字节，不允许空白、控制字符、反斜线、通配符、多目标、查询或片段".into());
    }
    if let Some((scheme, rest)) = value.split_once("://") {
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err("站点网址只允许 HTTP(S)".into());
        }
        let authority = rest.split('/').next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') {
            return Err("站点网址必须有主机且不能包含凭据".into());
        }
        let (host, explicit_port) = authority_parts(authority)?;
        if let (Some(a), Some(b)) = (explicit_port, default_port)
            && a != b
        {
            return Err("网址显式端口与站点默认端口冲突".into());
        }
        let normalized_host = normalize_host(host)?;
        let mut parsed = url::Url::parse(value).map_err(|e| format!("站点网址语法无效：{e}"))?;
        if parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("站点网址不能包含凭据、查询或片段".into());
        }
        parsed
            .set_host(Some(&normalized_host))
            .map_err(|_| "站点主机无效")?;
        let normalized_authority = match explicit_port {
            Some(port) => format!("{normalized_host}:{port}"),
            None => normalized_host,
        };
        let result = format!(
            "{}://{}{}",
            parsed.scheme(),
            normalized_authority,
            &parsed[url::Position::BeforePath..]
        );
        if result.len() > 2048 {
            return Err("规范化后的站点地址超过 2048 字节".into());
        }
        Ok(result)
    } else {
        if value.contains(['/', '@', '%']) {
            return Err(
                "裸地址只能填写单个域名或 IP；路径和端口请使用 HTTP(S) 网址或独立端口字段".into(),
            );
        }
        normalize_host(value)
    }
}

fn authority_parts(authority: &str) -> Result<(&str, Option<u16>), String> {
    let (host, suffix) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or("IPv6 地址缺少右方括号")?;
        (&authority[..=end], &authority[end + 1..])
    } else if let Some((host, _)) = authority.split_once(':') {
        (host, &authority[host.len()..])
    } else {
        (authority, "")
    };
    if suffix.is_empty() {
        return Ok((host, None));
    }
    let port = suffix.strip_prefix(':').ok_or("网址端口格式无效")?;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err("网址端口必须是 1–65535 的十进制整数".into());
    }
    let port: u16 = port.parse().map_err(|_| "网址端口必须在 1–65535 之间")?;
    if port == 0 {
        return Err("网址端口必须在 1–65535 之间".into());
    }
    Ok((host, Some(port)))
}

fn normalize_host(value: &str) -> Result<String, String> {
    let unbracketed = if value.starts_with('[') && value.ends_with(']') {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        return Ok(match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        });
    }
    if value.contains([':', '[', ']', '%']) {
        return Err("主机必须是标准 IP 或域名，不接受端口、IPv6 区域标识或编码主机".into());
    }
    match url::Host::parse(value).map_err(|e| format!("主机语法无效：{e}"))? {
        url::Host::Ipv4(_) => {
            // WHATWG also accepts 127.1, 2130706433, octal and hex. Do not reinterpret them.
            value
                .parse::<Ipv4Addr>()
                .map(|ip| ip.to_string())
                .map_err(|_| "IPv4 必须使用标准四段十进制格式".into())
        }
        url::Host::Ipv6(ip) => Ok(format!("[{ip}]")),
        url::Host::Domain(domain) => {
            if domain.ends_with('.') {
                return Err("域名不接受末尾点号，请明确填写检测主机".into());
            }
            if domain.is_empty()
                || domain.len() > 253
                || domain.split('.').any(|label| {
                    label.is_empty()
                        || label.len() > 63
                        || label.starts_with('-')
                        || label.ends_with('-')
                        || !label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
            {
                return Err("域名标签无效，必须使用合法域名或标准 IP".into());
            }
            Ok(domain.to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Target;
    fn list(id: &str, builtin: bool, targets: &[(&str, &str, &str)]) -> SiteList {
        SiteList {
            id: id.into(),
            name: id.into(),
            description: "legacy".into(),
            builtin,
            targets: targets
                .iter()
                .map(|(id, name, url)| Target {
                    id: (*id).into(),
                    name: (*name).into(),
                    url: (*url).into(),
                    region: String::new(),
                    source: "https://source.example/".into(),
                    checked_at: "2026-10-27".into(),
                    probe_only: true,
                })
                .collect(),
        }
    }
    fn req(
        revision: u64,
        sites: Vec<Site>,
        applications: Vec<Application>,
        groups: Vec<Group>,
    ) -> SaveRequest {
        SaveRequest {
            schema_version: 2,
            expected_revision: revision,
            sites,
            applications,
            groups,
        }
    }
    #[test]
    fn builtins_keep_55_endpoints_and_seven_groups() {
        let lists =
            storage::load_presets(&Path::new(env!("CARGO_MANIFEST_DIR")).join("presets")).unwrap();
        let m = Manager::new(tempfile::tempdir().unwrap().path(), &lists).unwrap();
        let s = m.snapshot();
        assert_eq!((s.sites.len(), s.groups.len()), (55, 7));
        assert_eq!(s.applications.len(), 55);
        assert!(!s.sites.iter().any(|x| x.capabilities.is_empty()));
    }
    #[test]
    fn old_lists_migrate_read_only_with_stable_shared_app_and_unknown_protocol() {
        let lists = vec![
            list("a", false, &[("x", "x", "example.com")]),
            list("b", false, &[("y", "x", "example.com")]),
        ];
        let dir = tempfile::tempdir().unwrap();
        let m = Manager::new(dir.path(), &lists).unwrap();
        let s = m.snapshot();
        assert_eq!(s.sites.len(), 1);
        assert_eq!(s.applications.len(), 1);
        assert_eq!(
            s.groups[0].group.application_ids,
            s.groups[1].group.application_ids
        );
        assert_eq!(s.sites[0].site.protocol, Protocol::Unspecified);
        assert_eq!(s.sites[0].capabilities, ["ping", "traceroute", "dns"]);
        assert!(!dir.path().join(FILE_NAME).exists());
    }
    #[test]
    fn strict_v2_save_cas_and_unknown_host_never_gains_connect_capability() {
        let dir = tempfile::tempdir().unwrap();
        let m = Manager::new(dir.path(), &[]).unwrap();
        let site = Site {
            id: "s".into(),
            name: "host".into(),
            address: "example.com".into(),
            port: None,
            transport: Transport::Unspecified,
            protocol: Protocol::Unspecified,
            purpose: String::new(),
            alpn: None,
        };
        let app = Application {
            id: "a".into(),
            name: "app".into(),
            category: ApplicationCategory::Custom,
            provider: String::new(),
            product: String::new(),
            aliases: vec![],
            access_type: String::new(),
            site_ids: vec!["s".into()],
            reference_rules: vec![],
        };
        let out = m.save(req(0, vec![site], vec![app], vec![])).unwrap();
        assert_eq!(out.sites[0].capabilities, ["ping", "traceroute", "dns"]);
        assert!(
            m.save(req(0, vec![], vec![], vec![]))
                .unwrap_err()
                .contains("冲突")
        );
        let disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(disk["version"], 2);
    }
    #[test]
    fn rejects_v1_save_payload_and_computed_fields() {
        let raw = serde_json::json!({"expected_revision":0,"sites":[],"groups":[]});
        assert!(serde_json::from_value::<SaveRequest>(raw).is_err());
        let mut s = serde_json::to_value(Site {
            id: "s".into(),
            name: "n".into(),
            address: "host.example".into(),
            port: None,
            transport: Transport::Tcp,
            protocol: Protocol::Tcp,
            purpose: String::new(),
            alpn: None,
        })
        .unwrap();
        s["builtin"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Site>(s).is_err());
    }
}

#[cfg(test)]
mod legacy_protection_tests {
    use super::*;
    use crate::model::Target;

    fn site(id: &str, address: &str) -> Site {
        Site {
            id: id.into(),
            name: "测试站点".into(),
            address: address.into(),
            port: None,
            transport: if address.contains("://") {
                Transport::Tcp
            } else {
                Transport::Unspecified
            },
            protocol: if address.starts_with("https://") {
                Protocol::Https
            } else if address.starts_with("http://") {
                Protocol::Http
            } else {
                Protocol::Unspecified
            },
            purpose: String::new(),
            alpn: None,
        }
    }
    fn group(id: &str, refs: &[&str]) -> Group {
        Group {
            id: id.into(),
            name: "组合".into(),
            description: String::new(),
            application_ids: refs
                .iter()
                .map(|id| {
                    stable_id(
                        if id.starts_with("builtin-") {
                            "builtin-app"
                        } else {
                            "test-app"
                        },
                        &[id],
                    )
                })
                .collect(),
        }
    }
    fn request(revision: u64, sites: Vec<Site>, groups: Vec<Group>) -> SaveRequest {
        SaveRequest {
            schema_version: 2,
            expected_revision: revision,
            applications: sites
                .iter()
                .map(|s| {
                    single_site_application(
                        stable_id("test-app", &[&s.id]),
                        s,
                        ApplicationCategory::Custom,
                    )
                })
                .collect(),
            sites,
            groups,
        }
    }
    fn list(id: &str, builtin: bool, targets: &[(&str, &str, &str)]) -> SiteList {
        SiteList {
            id: id.into(),
            name: id.into(),
            description: "说明".into(),
            builtin,
            targets: targets
                .iter()
                .map(|(id, name, url)| Target {
                    id: (*id).into(),
                    name: (*name).into(),
                    url: (*url).into(),
                    region: String::new(),
                    source: "https://official.example/".into(),
                    checked_at: "2026-10-26".into(),
                    probe_only: true,
                })
                .collect(),
        }
    }

    #[test]
    fn strict_dtos_reject_provenance_and_invalid_ports() {
        let value = serde_json::json!({"expected_revision":0,"sites":[{"id":"s","name":"n","address":"example.com","port":null,"builtin":true}],"groups":[]});
        assert!(serde_json::from_value::<SaveRequest>(value).is_err());
        for key in ["builtin", "source", "checked_at"] {
            let mut s = serde_json::to_value(site("s", "example.com")).unwrap();
            s[key] = serde_json::json!("spoof");
            assert!(serde_json::from_value::<Site>(s).is_err());
        }
        assert!(
            serde_json::from_str::<Site>(
                r#"{"id":"s","name":"n","address":"example.com","port":65536}"#
            )
            .is_err()
        );
        let mut g = serde_json::to_value(group("g", &[])).unwrap();
        g["builtin"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Group>(g).is_err());
    }

    #[test]
    fn syntax_normalizes_idn_paths_and_retains_explicit_default_port() {
        assert_eq!(
            normalize_address("HTTPS://例子.测试:443/a/b%20c", Some(443)).unwrap(),
            "https://xn--fsqu00a.xn--0zwm56d:443/a/b%20c"
        );
        assert!(normalize_address("https://example.com:443/a", Some(8443)).is_err());
        assert!(normalize_address("http://example.com:80", Some(443)).is_err());
        assert_eq!(
            normalize_address("https://Example.COM/a", Some(8443)).unwrap(),
            "https://example.com/a"
        );
        assert_eq!(
            normalize_address("例子.测试", None).unwrap(),
            "xn--fsqu00a.xn--0zwm56d"
        );
        assert_eq!(
            normalize_address("2001:0db8::1", None).unwrap(),
            "[2001:db8::1]"
        );
        assert_eq!(
            normalize_address("http://[::1]:8080/path", None).unwrap(),
            "http://[::1]:8080/path"
        );
        // Storage does not apply runtime network policy or attempt DNS resolution.
        assert!(normalize_address("http://127.0.0.1/private", None).is_ok());
        assert!(normalize_address("unresolvable.invalid", None).is_ok());
    }

    #[test]
    fn syntax_rejects_ambiguous_unsafe_and_multi_target_addresses() {
        for address in [
            "",
            " https://example.com",
            "https://example.com\n",
            "https://user@example.com/",
            "https://@example.com/",
            "https://example.com/?q=1",
            "https://example.com/#x",
            "javascript:alert(1)",
            "ftp://example.com",
            "https:\\example.com",
            "https:///example.com",
            "*.example.com",
            "127.0.0.0/8",
            "example.com,other.com",
            "example.com;other.com",
            "https://127.1/",
            "https://0x7f000001/",
            "https://2130706433/",
            "0177.0.0.1",
            "https://%65xample.com/",
            "example.com:443",
            "example.com.",
            "https://example.com./",
            "https://example.com:",
            "http://example.com:0",
            "http://example.com:65536",
            "https://[::1]:a",
            "https://bad_host/",
            "http://[fe80::1%25en0]/",
        ] {
            assert!(
                normalize_address(address, None).is_err(),
                "accepted {address}"
            );
        }
        assert!(normalize_address("example.com", Some(0)).is_err());
    }

    #[test]
    fn group_description_accepts_multiline_text_but_names_reject_controls() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), &[]).unwrap();
        let mut g = group("multiline", &[]);
        g.description = "第一行\n第二行".into();
        assert!(manager.save(request(0, vec![], vec![g])).is_ok());
        let mut s = site("invalid-name", "example.invalid");
        s.name = "名称\n换行".into();
        assert!(manager.save(request(1, vec![s], vec![])).is_err());
    }

    #[test]
    fn actual_presets_have_seven_groups_and_fifty_five_sites() {
        let presets =
            storage::load_presets(&Path::new(env!("CARGO_MANIFEST_DIR")).join("presets")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), &presets).unwrap();
        let snapshot = manager.snapshot();
        assert_eq!((snapshot.groups.len(), snapshot.sites.len()), (7, 55));
        assert_eq!(
            snapshot
                .groups
                .iter()
                .map(|g| g.group.application_ids.len())
                .sum::<usize>(),
            55
        );
        assert!(
            snapshot
                .sites
                .iter()
                .all(|s| s.builtin && !s.source.is_empty() && !s.checked_at.is_empty())
        );
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap(),
            serde_json::to_value(Manager::new(dir.path(), &presets).unwrap().snapshot()).unwrap()
        );
        assert!(!dir.path().join(FILE_NAME).exists());
    }

    #[test]
    fn builtins_dedup_and_legacy_migration_preserve_names_and_namespaces() {
        let lists = vec![
            list(
                "builtin-a",
                true,
                &[("one", "官方", "https://EXAMPLE.com:443/")],
            ),
            list(
                "builtin-b",
                true,
                &[("two", "另一名称", "https://example.com/")],
            ),
            list(
                "old-a",
                false,
                &[
                    ("same-id", "甲", "https://example.com/"),
                    ("same-site", "甲", "https://example.com/"),
                ],
            ),
            list(
                "old-b",
                false,
                &[
                    ("same-id", "乙", "https://example.com/"),
                    ("merge", "甲", "https://example.com/"),
                ],
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), &lists).unwrap();
        let snap = manager.snapshot();
        assert_eq!(snap.sites.len(), 3);
        assert_eq!(
            snap.groups[0].group.application_ids,
            snap.groups[1].group.application_ids
        );
        assert_eq!(snap.groups[2].group.application_ids.len(), 1);
        assert_eq!(snap.groups[3].group.application_ids.len(), 2);
        assert_ne!(
            snap.groups[2].group.application_ids[0],
            snap.groups[3].group.application_ids[0]
        );
        assert!(
            snap.sites
                .iter()
                .filter(|s| !s.builtin)
                .all(|s| s.source.is_empty() && s.checked_at.is_empty())
        );
        assert!(!dir.path().join(FILE_NAME).exists());
        manager.save(request(0, vec![], vec![])).unwrap();
        assert_eq!(
            Manager::new(dir.path(), &lists)
                .unwrap()
                .snapshot()
                .sites
                .len(),
            1
        );
    }

    #[test]
    fn save_cas_shared_references_independent_copy_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let lists = vec![list(
            "builtin",
            true,
            &[("one", "官网", "https://example.com/")],
        )];
        let manager = Manager::new(dir.path(), &lists).unwrap();
        let builtin = manager.snapshot().sites[0].site.id.clone();
        let mut custom = site("custom", "https://example.com/");
        let groups = vec![
            group("a", &[&builtin, "custom"]),
            group("b", &["custom"]),
            group("empty", &[]),
        ];
        let first = manager
            .save(request(0, vec![custom.clone()], groups.clone()))
            .unwrap();
        assert_eq!(
            (first.revision, first.sites.len(), first.groups.len()),
            (1, 2, 4)
        );
        custom.name = "更新名称".into();
        custom.address = "other.example".into();
        custom.transport = Transport::Unspecified;
        custom.protocol = Protocol::Unspecified;
        manager
            .save(request(1, vec![custom.clone()], groups.clone()))
            .unwrap();
        assert!(
            manager
                .save(request(1, vec![], vec![]))
                .unwrap_err()
                .contains("版本冲突")
        );
        assert!(manager.save(request(2, vec![], groups)).is_err());
        let reloaded = Manager::new(dir.path(), &lists).unwrap().snapshot();
        assert_eq!(reloaded.revision, 2);
        assert_eq!(reloaded.sites[1].site, custom);
        let disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(disk["sites"].as_array().unwrap().len(), 1);
        assert!(disk["sites"][0].get("builtin").is_none());
        assert!(
            manager
                .save(request(2, vec![site(&builtin, "example.com")], vec![]))
                .is_err()
        );
    }

    #[test]
    fn rejects_duplicate_ids_refs_unknown_refs_and_utf8_limits() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), &[]).unwrap();
        for req in [
            request(
                0,
                vec![site("s", "example.com"), site("s", "other.example")],
                vec![],
            ),
            request(0, vec![site("s", "example.com")], vec![group("s", &[])]),
            request(
                0,
                vec![site("s", "example.com")],
                vec![group("g", &["s", "s"])],
            ),
            request(0, vec![], vec![group("g", &["missing"])]),
            request(0, vec![site("builtin-site-spoof", "example.com")], vec![]),
            request(0, vec![site("非ASCII", "example.com")], vec![]),
        ] {
            assert!(manager.save(req).is_err());
        }
        let mut s = site("s", "example.com");
        s.name = "中".repeat(67);
        assert!(manager.save(request(0, vec![s], vec![])).is_err());
        let mut g = group("g", &[]);
        g.description = "中".repeat(334);
        assert!(manager.save(request(0, vec![], vec![g])).is_err());
        assert_eq!(manager.snapshot().revision, 0);
        assert!(!dir.path().join(FILE_NAME).exists());
    }

    #[test]
    fn bounds_counts_refs_and_final_serialized_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), &[]).unwrap();
        assert!(
            manager
                .save(request(
                    0,
                    (0..1001)
                        .map(|i| site(&format!("s{i}"), "example.com"))
                        .collect(),
                    vec![]
                ))
                .is_err()
        );
        assert!(
            manager
                .save(request(
                    0,
                    vec![],
                    (0..51).map(|i| group(&format!("g{i}"), &[])).collect()
                ))
                .is_err()
        );
        let sites: Vec<_> = (0..1000)
            .map(|i| site(&format!("s{i}"), "example.com"))
            .collect();
        let refs: Vec<_> = sites.iter().map(|s| s.id.as_str()).collect();
        assert!(
            manager
                .save(request(
                    0,
                    sites.clone(),
                    (0..11).map(|i| group(&format!("g{i}"), &refs)).collect()
                ))
                .unwrap_err()
                .contains("10000")
        );
        manager.save(request(0, vec![], vec![])).unwrap();
        let original = std::fs::read(dir.path().join(FILE_NAME)).unwrap();
        let huge = (0..1000)
            .map(|i| {
                let mut s = site(
                    &format!("s{i}"),
                    &format!("https://example.com/{}", "a".repeat(750)),
                );
                s.name = "\"".repeat(200); // JSON escaping contributes to the final byte limit.
                s
            })
            .collect();
        assert!(
            manager
                .save(request(1, huge, vec![]))
                .unwrap_err()
                .contains("1 MiB")
        );
        assert_eq!(manager.snapshot().revision, 1);
        assert_eq!(std::fs::read(dir.path().join(FILE_NAME)).unwrap(), original);
    }

    #[test]
    fn corruption_oversize_external_changes_and_failed_write_never_replace_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let manager = Manager::new(dir.path(), &[]).unwrap();
        std::fs::write(&path, b"external").unwrap();
        assert!(
            manager
                .save(request(0, vec![], vec![]))
                .unwrap_err()
                .contains("外部")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        assert!(Manager::new(dir.path(), &[]).is_err());
        std::fs::write(&path, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(Manager::new(dir.path(), &[]).is_err());
        std::fs::remove_file(&path).unwrap();
        manager.save(request(0, vec![], vec![])).unwrap();
        std::fs::write(&path, b"damage").unwrap();
        assert!(manager.save(request(1, vec![], vec![])).is_err());
        assert_eq!(manager.snapshot().revision, 1);
        std::fs::remove_file(&path).unwrap();
        assert!(manager.save(request(1, vec![], vec![])).is_err());
        assert_eq!(manager.snapshot().revision, 1);
        let blocked = dir.path().join("not-a-directory");
        let failed = Manager::new(&blocked, &[]).unwrap();
        std::fs::write(&blocked, b"block").unwrap();
        assert!(failed.save(request(0, vec![], vec![])).is_err());
        assert_eq!(failed.snapshot().revision, 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_rejected_on_load_and_save() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), &[]).unwrap();
        let target = dir.path().join("other.json");
        std::fs::write(&target, b"do not touch").unwrap();
        symlink(&target, dir.path().join(FILE_NAME)).unwrap();
        assert!(Manager::new(dir.path(), &[]).is_err());
        assert!(manager.save(request(0, vec![], vec![])).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");
    }

    #[test]
    fn concurrent_cas_allows_only_one_writer() {
        let dir = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(Manager::new(dir.path(), &[]).unwrap());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let manager = manager.clone();
                std::thread::spawn(move || manager.save(request(0, vec![], vec![])).is_ok())
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            1
        );
        assert_eq!(
            Manager::new(dir.path(), &[]).unwrap().snapshot().revision,
            1
        );
    }
}

#[cfg(test)]
mod v2_migration_tests {
    use super::*;
    #[test]
    fn v1_disk_migration_is_strict_stable_shared_and_read_only() {
        let lists =
            storage::load_presets(&Path::new(env!("CARGO_MANIFEST_DIR")).join("presets")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let old = serde_json::json!({"version":1,"revision":7,"sites":[{"id":"old-host","name":"host","address":"example.com","port":443},{"id":"old-url","name":"URL","address":"https://example.com:443/path","port":443}],"groups":[{"id":"old-a","name":"a","description":"","site_ids":["old-host","old-url"]},{"id":"old-b","name":"b","description":"","site_ids":["old-url"]}]});
        let bytes = serde_json::to_vec_pretty(&old).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let m = Manager::new(dir.path(), &lists).unwrap();
        let snap = m.snapshot();
        assert_eq!(snap.revision, 7);
        assert_eq!(snap.version, 2);
        let host = snap.sites.iter().find(|s| s.site.id == "old-host").unwrap();
        assert_eq!(host.site.port, Some(443));
        assert_eq!(host.site.protocol, Protocol::Unspecified);
        assert_eq!(host.capabilities, ["ping", "traceroute", "dns"]);
        let url = snap.sites.iter().find(|s| s.site.id == "old-url").unwrap();
        assert_eq!(url.site.address, "https://example.com:443/path");
        assert_eq!(url.site.protocol, Protocol::Https);
        let ga = snap.groups.iter().find(|g| g.group.id == "old-a").unwrap();
        let gb = snap.groups.iter().find(|g| g.group.id == "old-b").unwrap();
        assert_eq!(ga.group.application_ids[1], gb.group.application_ids[0]);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            serde_json::to_value(&snap).unwrap(),
            serde_json::to_value(Manager::new(dir.path(), &lists).unwrap().snapshot()).unwrap()
        );
        m.save(SaveRequest {
            schema_version: 2,
            expected_revision: 7,
            sites: snap
                .sites
                .into_iter()
                .filter(|s| !s.builtin)
                .map(|s| s.site)
                .collect(),
            applications: snap
                .applications
                .into_iter()
                .filter(|a| !a.builtin)
                .map(|a| a.application)
                .collect(),
            groups: snap
                .groups
                .into_iter()
                .filter(|g| !g.builtin)
                .map(|g| g.group)
                .collect(),
        })
        .unwrap();
        assert_eq!(
            Manager::new(dir.path(), &lists)
                .unwrap()
                .snapshot()
                .revision,
            8
        );
        let mut spoof = old;
        spoof["sites"][0]["protocol"] = serde_json::json!("https");
        std::fs::write(&path, serde_json::to_vec(&spoof).unwrap()).unwrap();
        assert!(Manager::new(dir.path(), &lists).is_err());
    }
    #[test]
    fn gds_products_rules_and_communication_placeholders_are_not_invented_endpoints() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let lists = storage::load_presets(&root.join("presets")).unwrap();
        let mut presets = crate::connections::load(&root.join("presets/connections")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let s = Manager::new_with_connections(dir.path(), &lists, &presets)
            .unwrap()
            .snapshot();
        assert_eq!(s.groups.len(), 7);
        assert_eq!(
            s.applications
                .iter()
                .filter(|a| a.application.category == ApplicationCategory::Gds)
                .count(),
            6
        );
        let chats: Vec<_> = s
            .applications
            .iter()
            .filter(|a| a.application.category == ApplicationCategory::Communication)
            .collect();
        assert_eq!(chats.len(), 3);
        assert!(chats.iter().all(|a| {
            a.application.site_ids.is_empty()
                && a.sources.is_empty()
                && a.application.reference_rules.len() == 4
                && a.application
                    .reference_rules
                    .iter()
                    .all(|r| r.address.is_none())
                && !a.missing.is_empty()
        }));
        let cloud = s
            .applications
            .iter()
            .find(|a| a.application.product == "Smartpoint Cloud")
            .unwrap();
        assert_eq!(cloud.application.reference_rules.len(), 24);
        assert_eq!(cloud.application.site_ids.len(), 1);
        assert_eq!(
            cloud
                .application
                .reference_rules
                .iter()
                .filter(|r| r.protocol.is_none())
                .count(),
            23
        );
        for p in &mut presets {
            p.endpoints.reverse();
        }
        presets.reverse();
        let reordered = Manager::new_with_connections(dir.path(), &lists, &presets)
            .unwrap()
            .snapshot();
        let mut ids: Vec<_> = s.sites.iter().map(|s| s.site.id.clone()).collect();
        let mut reordered_ids: Vec<_> = reordered.sites.iter().map(|s| s.site.id.clone()).collect();
        ids.sort();
        reordered_ids.sort();
        assert_eq!(ids, reordered_ids);
        for app in &s.applications {
            let other = reordered
                .applications
                .iter()
                .find(|a| a.application.id == app.application.id)
                .unwrap();
            let a: HashSet<_> = app
                .application
                .reference_rules
                .iter()
                .map(|r| &r.id)
                .collect();
            let b: HashSet<_> = other
                .application
                .reference_rules
                .iter()
                .map(|r| &r.id)
                .collect();
            assert_eq!(a, b);
        }
        assert!(!dir.path().join(FILE_NAME).exists());
    }
}
