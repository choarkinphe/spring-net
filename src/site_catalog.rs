//! Unified site catalog. Saving checks syntax only; execution policies remain separate.
use crate::{model::SiteList, storage};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::Mutex,
};

const VERSION: u32 = 1;
const MAX_BYTES: usize = 1_048_576;
const MAX_SITES: usize = 1000;
const MAX_GROUPS: usize = 50;
const MAX_REFS: usize = 10_000;
const FILE_NAME: &str = "site-catalog.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub description: String,
    pub site_ids: Vec<String>,
}

/// Only mutable custom fields are accepted; provenance is never accepted from clients.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
    pub expected_revision: u64,
    pub sites: Vec<Site>,
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CatalogSite {
    #[serde(flatten)]
    pub site: Site,
    pub builtin: bool,
    pub source: String,
    pub checked_at: String,
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
    pub groups: Vec<CatalogGroup>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedCatalog {
    version: u32,
    revision: u64,
    sites: Vec<Site>,
    groups: Vec<Group>,
}

struct State {
    saved: SavedCatalog,
    // Exact source bytes detect external edits, deletion, and creation after initial migration.
    source: Option<Vec<u8>>,
}

pub struct Manager {
    path: PathBuf,
    builtin_sites: Vec<CatalogSite>,
    builtin_groups: Vec<CatalogGroup>,
    state: Mutex<State>,
}

impl Manager {
    /// Read-only initialization: a missing new file migrates legacy lists in memory only.
    pub fn new(data_dir: impl AsRef<Path>, lists: &[SiteList]) -> Result<Self, String> {
        let (builtin_sites, builtin_groups) = builtins(lists)?;
        let path = data_dir.as_ref().join(FILE_NAME);
        let source = read_source(&path)?;
        let mut saved = match &source {
            Some(bytes) => serde_json::from_slice::<SavedCatalog>(bytes).map_err(|e| {
                format!("site-catalog.json 损坏，未覆盖原文件；请备份并修复后重启：{e}")
            })?,
            None => migrate(lists)?,
        };
        if saved.version != VERSION {
            return Err("不支持的站点库版本，请升级应用或恢复兼容备份；未覆盖原文件".into());
        }
        validate(&mut saved, &builtin_sites, &builtin_groups)
            .map_err(|e| format!("站点库无法加载，未写入任何文件：{e}"))?;
        serialize(&saved)?;
        Ok(Self {
            path,
            builtin_sites,
            builtin_groups,
            state: Mutex::new(State { saved, source }),
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        // A poisoned lock must not hide readable catalog data. Mutations still fail closed.
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.make_snapshot(&state.saved)
    }

    pub fn save(&self, request: SaveRequest) -> Result<Snapshot, String> {
        let mut state = self.state.lock().map_err(|_| "站点库锁异常，请重启应用")?;
        if request.expected_revision != state.saved.revision {
            return Err("站点库版本冲突：数据已更新，请重新读取后再保存".into());
        }
        let revision = state
            .saved
            .revision
            .checked_add(1)
            .ok_or("站点库修订号已耗尽")?;
        let mut saved = SavedCatalog {
            version: VERSION,
            revision,
            sites: request.sites,
            groups: request.groups,
        };
        validate(&mut saved, &self.builtin_sites, &self.builtin_groups)?;
        let bytes = serialize(&saved)?;
        if read_source(&self.path)? != state.source {
            return Err(
                "站点库文件已被外部修改、创建或删除；为避免覆盖，请备份并重启后重新读取".into(),
            );
        }
        // The service mutation gate and this mutex serialize application writers. This byte
        // guard is not an interprocess lock: another process must not share this data directory.
        storage::atomic_bytes(&self.path, &bytes)
            .map_err(|e| format!("站点库保存失败；请重新读取核对，不要自动重试：{e}"))?;
        state.saved = saved;
        state.source = Some(bytes);
        Ok(self.make_snapshot(&state.saved))
    }

    fn make_snapshot(&self, saved: &SavedCatalog) -> Snapshot {
        Snapshot {
            version: VERSION,
            revision: saved.revision,
            sites: self
                .builtin_sites
                .iter()
                .cloned()
                .chain(saved.sites.iter().cloned().map(|site| CatalogSite {
                    site,
                    builtin: false,
                    source: String::new(),
                    checked_at: String::new(),
                }))
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

fn read_source(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            Err("站点库必须为普通文件，不能是符号链接或目录；未覆盖原文件".into())
        }
        Ok(_) => storage::read_bounded(path, MAX_BYTES as u64)
            .map(Some)
            .map_err(|e| format!("无法读取站点库，请检查权限、大小或文件是否已变化：{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("无法检查站点库文件：{e}")),
    }
}

fn serialize(saved: &SavedCatalog) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(saved).map_err(|e| format!("站点库序列化失败：{e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err("站点库保存后的数据不能超过 1 MiB，请减少站点、组合或字段长度".into());
    }
    Ok(bytes)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn text(value: &str, max: usize, required: bool, field: &str) -> Result<(), String> {
    if value.len() > max
        || (required && value.trim().is_empty())
        || value
            .chars()
            .any(|c| c.is_control() && (required || !matches!(c, '\n' | '\r' | '\t')))
    {
        return Err(format!(
            "{field}不能为空、包含控制字符或超过 {max} 个 UTF-8 字节"
        ));
    }
    Ok(())
}

fn validate(
    saved: &mut SavedCatalog,
    builtin_sites: &[CatalogSite],
    builtin_groups: &[CatalogGroup],
) -> Result<(), String> {
    if saved.sites.len() > MAX_SITES || saved.groups.len() > MAX_GROUPS {
        return Err("自定义站点最多 1000 个，组合最多 50 个；请先备份并拆分数据".into());
    }
    let mut ids: HashSet<String> = builtin_sites
        .iter()
        .map(|s| s.site.id.clone())
        .chain(builtin_groups.iter().map(|g| g.group.id.clone()))
        .collect();
    let mut site_ids: HashSet<String> = builtin_sites.iter().map(|s| s.site.id.clone()).collect();
    for site in &mut saved.sites {
        custom_id(&site.id, &mut ids)?;
        text(&site.name, 200, true, "站点名称")?;
        site.address = normalize_address(&site.address, site.port)?;
        site_ids.insert(site.id.clone());
    }
    let mut refs = 0;
    for group in &saved.groups {
        custom_id(&group.id, &mut ids)?;
        text(&group.name, 200, true, "组合名称")?;
        text(&group.description, 1000, false, "组合说明")?;
        refs += group.site_ids.len();
        if refs > MAX_REFS {
            return Err("组合成员引用总数不能超过 10000".into());
        }
        let mut members = HashSet::new();
        for id in &group.site_ids {
            if !site_ids.contains(id) {
                return Err(format!(
                    "组合「{}」引用了不存在的站点 {id}；删除站点前请明确移除组合中的引用",
                    group.name
                ));
            }
            if !members.insert(id) {
                return Err(format!("组合「{}」不能重复引用同一站点", group.name));
            }
        }
    }
    Ok(())
}

fn custom_id(id: &str, ids: &mut HashSet<String>) -> Result<(), String> {
    if !valid_id(id) || id.starts_with("builtin-") || !ids.insert(id.to_owned()) {
        return Err("站点/组合 ID 无效、重复或试图修改内置数据（仅允许 1–100 位 ASCII 字母、数字、横线、下划线）".into());
    }
    Ok(())
}

/// A stable, length-bounded namespace hash, independent of randomized HashMap iteration.
/// Input components are length-prefixed, so (list, target) pairs cannot alias by concatenation.
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

fn builtins(lists: &[SiteList]) -> Result<(Vec<CatalogSite>, Vec<CatalogGroup>), String> {
    let mut sites = Vec::new();
    let mut groups = Vec::new();
    let mut addresses: HashMap<String, String> = HashMap::new();
    let mut ids = HashSet::new();
    for list in lists.iter().filter(|l| l.builtin) {
        let id = stable_id("builtin-group", &[&list.id]);
        if !ids.insert(id.clone()) {
            return Err("内置组合 ID 重复或冲突".into());
        }
        let mut site_ids = Vec::new();
        for target in &list.targets {
            let address = normalize_address(&target.url, None)
                .map_err(|e| format!("内置站点「{}」无效：{e}", target.name))?;
            // Explicit default ports are retained in addresses, but dedup uses URL equivalence.
            let key = url::Url::parse(&address)
                .map_err(|e| e.to_string())?
                .to_string();
            let site_id = if let Some(id) = addresses.get(&key) {
                id.clone()
            } else {
                let id = stable_id("builtin-site", &[&key]);
                if !ids.insert(id.clone()) {
                    return Err("内置站点 ID 冲突".into());
                }
                addresses.insert(key, id.clone());
                sites.push(CatalogSite {
                    site: Site {
                        id: id.clone(),
                        name: target.name.clone(),
                        address,
                        port: None,
                    },
                    builtin: true,
                    source: target.source.clone(),
                    checked_at: target.checked_at.clone(),
                });
                id
            };
            if !site_ids.contains(&site_id) {
                site_ids.push(site_id);
            }
        }
        groups.push(CatalogGroup {
            group: Group {
                id,
                name: list.name.clone(),
                description: list.description.clone(),
                site_ids,
            },
            builtin: true,
        });
    }
    Ok((sites, groups))
}

fn migrate(lists: &[SiteList]) -> Result<SavedCatalog, String> {
    let mut saved = SavedCatalog {
        version: VERSION,
        revision: 0,
        sites: Vec::new(),
        groups: Vec::new(),
    };
    let mut same: HashMap<(String, String), String> = HashMap::new();
    let mut ids = HashSet::new();
    for list in lists.iter().filter(|l| !l.builtin) {
        let id = stable_id("legacy-group", &[&list.id]);
        if !ids.insert(id.clone()) {
            return Err("旧列表 ID 重复，无法无损迁移；请先修复 lists.json".into());
        }
        let mut site_ids = Vec::new();
        let mut target_ids = HashSet::new();
        for target in &list.targets {
            if !target_ids.insert(&target.id) {
                return Err("旧列表中的目标 ID 重复，无法无损迁移；请先修复 lists.json".into());
            }
            let address = normalize_address(&target.url, None).map_err(|e| {
                format!(
                    "旧列表「{}」的站点「{}」无法迁移，请先修复 lists.json：{e}",
                    list.name, target.name
                )
            })?;
            let key = (target.name.clone(), address.clone());
            let site_id = if let Some(id) = same.get(&key) {
                id.clone()
            } else {
                let id = stable_id("legacy-site", &[&list.id, &target.id]);
                if !ids.insert(id.clone()) {
                    return Err("旧站点 ID 冲突，无法无损迁移；未写入文件".into());
                }
                same.insert(key, id.clone());
                saved.sites.push(Site {
                    id: id.clone(),
                    name: target.name.clone(),
                    address,
                    port: None,
                });
                id
            };
            if !site_ids.contains(&site_id) {
                site_ids.push(site_id);
            }
        }
        saved.groups.push(Group {
            id,
            name: list.name.clone(),
            description: list.description.clone(),
            site_ids,
        });
    }
    Ok(saved)
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

    fn site(id: &str, address: &str) -> Site {
        Site {
            id: id.into(),
            name: "测试站点".into(),
            address: address.into(),
            port: None,
        }
    }
    fn group(id: &str, refs: &[&str]) -> Group {
        Group {
            id: id.into(),
            name: "组合".into(),
            description: String::new(),
            site_ids: refs.iter().map(|s| (*s).into()).collect(),
        }
    }
    fn request(revision: u64, sites: Vec<Site>, groups: Vec<Group>) -> SaveRequest {
        SaveRequest {
            expected_revision: revision,
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
                .map(|g| g.group.site_ids.len())
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
        assert_eq!(snap.groups[0].group.site_ids, snap.groups[1].group.site_ids);
        assert_eq!(snap.groups[2].group.site_ids.len(), 1);
        assert_eq!(snap.groups[3].group.site_ids.len(), 2);
        assert_ne!(
            snap.groups[2].group.site_ids[0],
            snap.groups[3].group.site_ids[0]
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
