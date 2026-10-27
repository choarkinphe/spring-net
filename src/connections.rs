//! Bounded, read-only reference catalog. Nothing here resolves hosts or creates targets.
use crate::{
    model::{ConnectionEndpointKind, ConnectionPreset},
    policy, storage,
};
use std::{collections::HashSet, path::Path};

const MAX_FILES: usize = 32;
const MAX_FILE_BYTES: u64 = 262_144;
const MAX_TOTAL_BYTES: usize = 4_194_304;
const MAX_PRESETS: usize = 64;

fn text(value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(|c| c.is_control()) {
        return Err("连接参考文本为空、过长或含控制字符".into());
    }
    Ok(())
}
fn id(value: &str) -> Result<(), String> {
    text(value, 100)?;
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("连接参考 ID 无效".into());
    }
    Ok(())
}
fn texts(values: &[String], max_count: usize, max_len: usize) -> Result<(), String> {
    if values.len() > max_count {
        return Err("连接参考条目过多".into());
    }
    for value in values {
        text(value, max_len)?;
    }
    Ok(())
}
fn hostname(value: &str) -> Result<(), String> {
    if value.len() > 253
        || !value.contains('.')
        || value.parse::<std::net::IpAddr>().is_ok()
        || value.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        || value == "localhost"
        || value.ends_with(".localhost")
        || value.ends_with(".local")
    {
        return Err("连接参考主机名无效".into());
    }
    Ok(())
}
fn safe_url(value: &str) -> Result<url::Url, String> {
    let url = policy::parse_url(value)?;
    if url.scheme() != "https" {
        return Err("连接参考链接必须为 HTTPS".into());
    }
    hostname(url.host_str().ok_or("连接参考链接缺少主机")?)?;
    Ok(url)
}
fn date(value: &str) -> Result<(), String> {
    if value.len() != 10 || chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() {
        return Err("连接参考日期必须为 YYYY-MM-DD".into());
    }
    Ok(())
}

pub fn validate(presets: &[ConnectionPreset]) -> Result<(), String> {
    if presets.len() > MAX_PRESETS {
        return Err("连接参考产品过多".into());
    }
    let mut ids = HashSet::new();
    for preset in presets {
        id(&preset.id)?;
        if !ids.insert(&preset.id) {
            return Err(format!("连接参考 ID 重复：{}", preset.id));
        }
        text(&preset.provider, 200)?;
        text(&preset.product, 200)?;
        texts(&preset.aliases, 20, 200)?;
        if ![
            "browser",
            "desktop",
            "api",
            "desktop_browser",
            "unconfirmed",
        ]
        .contains(&preset.access_type.as_str())
            || !["reference_only", "incomplete"].contains(&preset.status.as_str())
        {
            return Err("连接参考访问类型或状态无效".into());
        }
        for value in [&preset.environment, &preset.version].into_iter().flatten() {
            text(value, 200)?;
        }
        text(&preset.summary, 4000)?;
        texts(&preset.limitations, 32, 2000)?;
        texts(&preset.missing, 32, 2000)?;
        if preset.sources.is_empty() || preset.sources.len() > 32 || preset.endpoints.len() > 128 {
            return Err("连接参考来源/端点数量无效".into());
        }
        let mut source_ids = HashSet::new();
        for source in &preset.sources {
            id(&source.id)?;
            if !source_ids.insert(&source.id) {
                return Err("连接参考来源 ID 重复".into());
            }
            text(&source.title, 400)?;
            safe_url(&source.url)?;
            date(&source.checked_at)?;
            if let Some(published) = &source.published_at {
                date(published)?;
                if published > &source.checked_at {
                    return Err("连接参考发布日期晚于核对日期".into());
                }
            }
            text(&source.scope, 2000)?;
        }
        let mut endpoints = HashSet::new();
        for endpoint in &preset.endpoints {
            text(&endpoint.purpose, 1000)?;
            text(&endpoint.evidence, 2000)?;
            if endpoint.notes.len() > 4000 || endpoint.notes.chars().any(|c| c.is_control()) {
                return Err("连接参考端点备注无效".into());
            }
            if endpoint.source_ids.is_empty() || endpoint.source_ids.len() > 32 {
                return Err("连接参考端点缺少来源".into());
            }
            let mut refs = HashSet::new();
            for source_id in &endpoint.source_ids {
                if !source_ids.contains(source_id) || !refs.insert(source_id) {
                    return Err("连接参考端点引用未知或重复来源".into());
                }
            }
            if let Some(protocol) = &endpoint.protocol {
                text(protocol, 100)?;
            }
            if endpoint.port == Some(0) {
                return Err("连接参考端口不能为 0".into());
            }
            if let Some(address) = &endpoint.address {
                match endpoint.kind {
                    ConnectionEndpointKind::Url => {
                        let url = safe_url(address)?;
                        if endpoint
                            .port
                            .is_some_and(|port| Some(port) != url.port_or_known_default())
                        {
                            return Err("连接参考 URL 与端口不一致".into());
                        }
                    }
                    ConnectionEndpointKind::Host => hostname(address)?,
                    ConnectionEndpointKind::Wildcard => {
                        hostname(address.strip_prefix("*.").ok_or("通配规则必须以 *. 开头")?)?
                    }
                }
                let kind = match endpoint.kind {
                    ConnectionEndpointKind::Url => 0,
                    ConnectionEndpointKind::Host => 1,
                    ConnectionEndpointKind::Wildcard => 2,
                };
                if !endpoints.insert((kind, address, endpoint.port, &endpoint.protocol)) {
                    return Err("连接参考端点重复".into());
                }
            } else if endpoint.protocol.is_some() || endpoint.port.is_some() {
                return Err("未知地址不能带推测的协议或端口".into());
            }
        }
        texts(&preset.protected_domains, 32, 253)?;
        let mut domains = HashSet::new();
        for domain in &preset.protected_domains {
            hostname(domain)?;
            if !domains.insert(domain) {
                return Err("连接参考保护域名重复".into());
            }
        }
    }
    Ok(())
}

/// Missing directories are empty for legacy roots; malformed existing catalogs fail closed.
pub fn load(root: &Path) -> Result<Vec<ConnectionPreset>, String> {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err("连接参考目录必须为真实目录".into());
        }
        Ok(_) => {}
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("连接参考 JSON 必须为普通文件，不能是符号链接".into());
            }
            paths.push(path);
            if paths.len() > MAX_FILES {
                return Err("连接参考文件过多".into());
            }
        }
    }
    paths.sort();
    let mut presets = Vec::new();
    let mut bytes_read = 0;
    for path in paths {
        let bytes = storage::read_bounded(&path, MAX_FILE_BYTES)?;
        bytes_read += bytes.len();
        if bytes_read > MAX_TOTAL_BYTES {
            return Err("连接参考总数据超过 4 MiB".into());
        }
        let entries: Vec<ConnectionPreset> =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        presets.extend(entries);
        if presets.len() > MAX_PRESETS {
            return Err("连接参考产品过多".into());
        }
    }
    validate(&presets)?;
    Ok(presets)
}

/// Only explicit, reviewed vendor ownership is protected; wildcard dependency rules
/// (Azure, Google, CDNs, analytics) must never become blanket authorization bans.
pub fn protected_domain_keys(presets: &[ConnectionPreset]) -> Result<HashSet<String>, String> {
    presets
        .iter()
        .flat_map(|preset| &preset.protected_domains)
        .map(|domain| {
            hostname(domain)?;
            Ok(policy::domain_key(&policy::parse_url(&format!(
                "https://{domain}/"
            ))?))
        })
        .collect()
}
