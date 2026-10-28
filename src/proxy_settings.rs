//! Application-wide proxy profiles. Validation and persistence never resolve hosts or connect.
use crate::{
    proxy::{ProxyConfig, normalize_proxy_config},
    storage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Mutex,
};

const VERSION: u32 = 1;
const FILE_NAME: &str = "proxy-settings.json";
const MAX_BYTES: usize = 64 * 1024;
const MAX_PROFILES: usize = 30;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub url: String,
}

/// Only editable fields are accepted, never a client-supplied version or provenance.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
    pub expected_revision: u64,
    pub profiles: Vec<Profile>,
    #[serde(deserialize_with = "required_active_id")]
    pub active_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub revision: u64,
    pub profiles: Vec<Profile>,
    #[serde(deserialize_with = "required_active_id")]
    pub active_id: Option<String>,
}

// Unlike serde's implicit Option default, omission must not accidentally disable a proxy.
fn required_active_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

/// Frozen at task admission; historical results keep this value after settings change.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteSnapshot {
    pub settings_revision: u64,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    pub proxy: Option<ProxyConfig>,
    pub local_direct: bool,
}

impl RouteSnapshot {
    /// Ping/traceroute/DNS use their ordinary local path, not the configured proxy.
    pub fn local(self) -> Self {
        Self {
            settings_revision: self.settings_revision,
            local_direct: true,
            ..Self::default()
        }
    }
}

struct State {
    saved: Snapshot,
    // Exact bytes detect external edits/deletion/creation, including during memory-only migration.
    source: Option<Vec<u8>>,
}

pub struct Manager {
    path: PathBuf,
    control_port: Option<u16>,
    state: Mutex<State>,
}

impl Manager {
    /// Read-only initialization. Errors must be surfaced, not replaced by a direct-route default.
    pub fn new(data_dir: impl AsRef<Path>, control_port: Option<u16>) -> Result<Self, String> {
        let path = data_dir.as_ref().join(FILE_NAME);
        let source = read_source(&path, MAX_BYTES)?;
        let mut saved = match &source {
            Some(bytes) => serde_json::from_slice::<Snapshot>(bytes).map_err(|e| {
                format!("proxy-settings.json 损坏，未覆盖原文件；请备份并修复后重启：{e}")
            })?,
            None => migrate(data_dir.as_ref())?,
        };
        if saved.version != VERSION {
            return Err("不支持的代理设置版本，未覆盖原文件；请升级应用或恢复兼容备份".into());
        }
        validate(&mut saved, control_port)
            .map_err(|e| format!("代理设置无法加载，未写入任何文件：{e}"))?;
        serialize(&saved)?;
        Ok(Self {
            path,
            control_port,
            state: Mutex::new(State { saved, source }),
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        // Read back the actual validated state even after poisoning; never invent direct state.
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .saved
            .clone()
    }

    pub fn route(&self) -> RouteSnapshot {
        let saved = self.snapshot();
        let mut route = RouteSnapshot {
            settings_revision: saved.revision,
            ..RouteSnapshot::default()
        };
        if let Some(id) = saved.active_id {
            let profile = saved
                .profiles
                .into_iter()
                .find(|p| p.id == id)
                .expect("validated active proxy profile must exist");
            route.profile_id = Some(profile.id);
            route.profile_name = Some(profile.name);
            route.proxy = Some(ProxyConfig { url: profile.url });
        }
        route
    }

    pub fn save(&self, request: SaveRequest) -> Result<Snapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "代理设置锁异常，请重启应用")?;
        if request.expected_revision != state.saved.revision {
            return Err("代理设置版本冲突：数据已更新，请重新读取后再保存".into());
        }
        let revision = state
            .saved
            .revision
            .checked_add(1)
            .ok_or("代理设置修订号已耗尽")?;
        let mut saved = Snapshot {
            version: VERSION,
            revision,
            profiles: request.profiles,
            active_id: request.active_id,
        };
        validate(&mut saved, self.control_port)?;
        let bytes = serialize(&saved)?;
        if read_source(&self.path, MAX_BYTES)? != state.source {
            return Err(
                "代理设置文件已被外部修改、创建或删除；为避免覆盖，请备份并重启后重新读取".into(),
            );
        }
        // Like site_catalog: mutex/service mutation gate serialize application writers. This
        // byte guard is not an interprocess lock; do not share the data directory across processes.
        storage::atomic_bytes(&self.path, &bytes)
            .map_err(|e| format!("代理设置保存失败；请重新读取核对，不要自动重试：{e}"))?;
        state.saved = saved;
        state.source = Some(bytes);
        Ok(state.saved.clone())
    }
}

fn read_source(path: &Path, max: usize) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => Err(format!(
            "{} 必须为普通文件，不能是符号链接或目录；未覆盖原文件",
            path.display()
        )),
        Ok(_) => storage::read_bounded(path, max as u64)
            .map(Some)
            .map_err(|e| {
                format!(
                    "无法读取 {}，请检查权限、大小或文件是否已变化：{e}",
                    path.display()
                )
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("无法检查 {}：{e}", path.display())),
    }
}

fn serialize(saved: &Snapshot) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(saved).map_err(|e| format!("代理设置序列化失败：{e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err("代理设置保存后的数据不能超过 64 KiB，请减少配置或字段长度".into());
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

fn validate(saved: &mut Snapshot, control_port: Option<u16>) -> Result<(), String> {
    if saved.profiles.len() > MAX_PROFILES {
        return Err("代理配置最多 30 组".into());
    }
    let mut ids = HashSet::new();
    for profile in &mut saved.profiles {
        if !valid_id(&profile.id) || !ids.insert(profile.id.clone()) {
            return Err(
                "代理配置 ID 无效或重复：应为 1–100 个 ASCII 字母、数字、连字符或下划线".into(),
            );
        }
        if profile.name.trim().is_empty()
            || profile.name.len() > 200
            || profile.name.chars().any(char::is_control)
        {
            return Err("代理名称应为 1–200 UTF-8 字节且不能包含控制字符".into());
        }
        if profile.url.len() > 2048 {
            return Err("代理 URL 不能超过 2048 UTF-8 字节".into());
        }
        profile.url = normalize_proxy_config(
            &ProxyConfig {
                url: profile.url.clone(),
            },
            control_port,
        )
        .map_err(|e| format!("代理配置「{}」无效：{e}", profile.name))?
        .url;
        if profile.url.len() > 2048 {
            return Err("规范化后的代理 URL 不能超过 2048 UTF-8 字节".into());
        }
    }
    if saved.active_id.as_ref().is_some_and(|id| !ids.contains(id)) {
        return Err("启用的代理 ID 必须引用现有配置；删除当前代理时请明确关闭或选择另一组".into());
    }
    Ok(())
}

fn migrate(data_dir: &Path) -> Result<Snapshot, String> {
    let mut saved = Snapshot {
        version: VERSION,
        revision: 0,
        profiles: vec![],
        active_id: None,
    };
    let path = data_dir.join("settings.json");
    let Some(bytes) = read_source(&path, 1_048_576)? else {
        return Ok(saved);
    };
    // The legacy workspace envelope contains unrelated UI fields. Only the nested proxy DTO
    // is strict; do not copy UI fields or provenance into the new settings file.
    let legacy: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("settings.json 损坏，未覆盖原文件；无法迁移代理：{e}"))?;
    if legacy.get("version").and_then(serde_json::Value::as_u64) != Some(1)
        || !legacy
            .get("value")
            .is_some_and(serde_json::Value::is_object)
    {
        return Err("settings.json 数据版本或格式无效，未覆盖原文件；无法迁移代理".into());
    }
    if let Some(raw) = legacy["value"].get("http_proxy").filter(|v| !v.is_null()) {
        let proxy: ProxyConfig = serde_json::from_value(raw.clone())
            .map_err(|e| format!("旧版 http_proxy 格式错误，未覆盖原文件：{e}"))?;
        if !proxy.url.starts_with("http://") {
            return Err("旧版 http_proxy 必须是合法 HTTP 代理，未覆盖原文件".into());
        }
        saved.profiles.push(Profile {
            id: "legacy-http-proxy".into(),
            name: "旧版 HTTP 代理".into(),
            url: proxy.url,
        });
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn profile(id: &str) -> Profile {
        Profile {
            id: id.into(),
            name: format!("代理 {id}"),
            url: "http://LOCALHOST:8080/".into(),
        }
    }

    fn request(revision: u64, profiles: Vec<Profile>, active: Option<&str>) -> SaveRequest {
        SaveRequest {
            expected_revision: revision,
            profiles,
            active_id: active.map(str::to_owned),
        }
    }

    fn legacy(dir: &Path, proxy: serde_json::Value) -> Vec<u8> {
        let bytes =
            serde_json::to_vec(&json!({"version":1,"value":{"page":"console","http_proxy":proxy}}))
                .unwrap();
        std::fs::write(dir.join("settings.json"), &bytes).unwrap();
        bytes
    }

    #[test]
    fn strict_request_and_disk_schema_reject_extra_missing_and_duplicate_fields() {
        let valid = json!({"expected_revision":0,"profiles":[profile("p")],"active_id":null});
        assert!(serde_json::from_value::<SaveRequest>(valid.clone()).is_ok());
        for extra in ["version", "revision", "builtin", "source", "checked_at"] {
            let mut value = valid.clone();
            value[extra] = json!(1);
            assert!(serde_json::from_value::<SaveRequest>(value).is_err());
            let mut value = valid.clone();
            value["profiles"][0][extra] = json!(1);
            assert!(serde_json::from_value::<SaveRequest>(value).is_err());
        }
        for field in ["expected_revision", "profiles", "active_id"] {
            let mut value = valid.clone();
            value.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<SaveRequest>(value).is_err());
        }
        assert!(
            serde_json::from_str::<SaveRequest>(
                r#"{"expected_revision":0,"profiles":[],"active_id":null,"active_id":null}"#
            )
            .is_err()
        );
        let disk =
            json!({"version":1,"revision":0,"profiles":[],"active_id":null,"source":"client"});
        assert!(serde_json::from_value::<Snapshot>(disk).is_err());
    }

    #[test]
    fn first_install_is_read_only_and_direct() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("not-created");
        let manager = Manager::new(&data, None).unwrap();
        assert_eq!(
            manager.snapshot(),
            Snapshot {
                version: 1,
                revision: 0,
                profiles: vec![],
                active_id: None
            }
        );
        assert_eq!(manager.route(), RouteSnapshot::default());
        assert!(!data.exists());
    }

    #[test]
    fn save_normalizes_cas_roundtrips_and_freezes_route() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), Some(3000)).unwrap();
        let saved = manager
            .save(request(
                0,
                vec![profile("one"), profile("two")],
                Some("two"),
            ))
            .unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.profiles[0].url, "http://localhost:8080");
        let frozen = manager.route();
        assert_eq!(frozen.settings_revision, 1);
        assert_eq!(frozen.profile_id.as_deref(), Some("two"));
        assert_eq!(frozen.profile_name.as_deref(), Some("代理 two"));
        assert_eq!(frozen.proxy.as_ref().unwrap().url, "http://localhost:8080");
        assert!(!frozen.local_direct);
        assert!(
            manager
                .save(request(0, vec![], None))
                .unwrap_err()
                .contains("冲突")
        );
        assert_eq!(
            Manager::new(dir.path(), Some(3000)).unwrap().snapshot(),
            saved
        );
        manager
            .save(request(1, vec![profile("one")], None))
            .unwrap();
        assert_eq!(manager.route().proxy, None);
        assert_eq!(frozen.profile_id.as_deref(), Some("two"));
        let local = frozen.local();
        assert_eq!(
            local,
            RouteSnapshot {
                settings_revision: 1,
                local_direct: true,
                ..RouteSnapshot::default()
            }
        );
    }

    #[test]
    fn socks5_and_ipv6_profiles_use_shared_normalizer_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), None).unwrap();
        let mut socks = profile("socks");
        socks.url = "socks5://LOCALHOST:1080/".into();
        let mut ipv6 = profile("ipv6");
        ipv6.url = "http://[0:0:0:0:0:0:0:1]:8080/".into();
        let saved = manager
            .save(request(0, vec![socks, ipv6], Some("socks")))
            .unwrap();
        assert_eq!(saved.profiles[0].url, "socks5://localhost:1080");
        assert_eq!(saved.profiles[1].url, "http://[::1]:8080");
        assert_eq!(
            manager.route().proxy.unwrap().url,
            "socks5://localhost:1080"
        );
        assert_eq!(Manager::new(dir.path(), None).unwrap().snapshot(), saved);
    }

    #[test]
    fn concurrent_cas_only_one_writer_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let manager = std::sync::Arc::new(Manager::new(dir.path(), None).unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let writers: Vec<_> = (0..2)
            .map(|_| {
                let manager = manager.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    manager.save(request(0, vec![], None)).is_ok()
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(
            writers
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter(|ok| *ok)
                .count(),
            1
        );
        assert_eq!(manager.snapshot().revision, 1);
    }

    #[test]
    fn invalid_profiles_do_not_change_memory_or_disk() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), Some(3000)).unwrap();
        let original = manager
            .save(request(0, vec![profile("p")], Some("p")))
            .unwrap();
        let bytes = std::fs::read(dir.path().join(FILE_NAME)).unwrap();
        let mut invalid = vec![
            vec![profile("dup"), profile("dup")],
            (0..31).map(|i| profile(&format!("p{i}"))).collect(),
        ];
        for id in [
            "".into(),
            "x".repeat(101),
            "中文".into(),
            "with space".into(),
            "a/b".into(),
        ] {
            let mut p = profile("p");
            p.id = id;
            invalid.push(vec![p]);
        }
        for name in [
            "".into(),
            " \t".into(),
            "x".repeat(201),
            "中".repeat(67),
            "a\nb".into(),
            "a\u{7f}b".into(),
        ] {
            let mut p = profile("p");
            p.name = name;
            invalid.push(vec![p]);
        }
        for url in [
            "http://localhost:3000".into(),
            "http://localhost".into(),
            "http://u:p@localhost:8080".into(),
            "http://localhost:8080/path".into(),
            "http://localhost:8080?q=1".into(),
            "http://localhost:8080#x".into(),
            "https://localhost:8080".into(),
            "socks5h://localhost:8080".into(),
            "http://local host:8080".into(),
            "x".repeat(2049),
        ] {
            let mut p = profile("p");
            p.url = url;
            invalid.push(vec![p]);
        }
        for profiles in invalid {
            assert!(manager.save(request(1, profiles, None)).is_err());
            assert_eq!(manager.snapshot(), original);
            assert_eq!(std::fs::read(dir.path().join(FILE_NAME)).unwrap(), bytes);
        }
        assert!(manager.save(request(1, vec![], Some("p"))).is_err());
        assert!(
            manager
                .save(request(1, vec![profile("p")], Some("absent")))
                .is_err()
        );
        assert!(
            manager
                .save(request(1, vec![profile("p")], Some("")))
                .is_err()
        );
    }

    #[test]
    fn exact_id_name_and_profile_count_boundaries_are_accepted_without_dns() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), None).unwrap();
        let mut profiles: Vec<_> = (0..30).map(|i| profile(&format!("p{i}"))).collect();
        profiles[0].id = "a".repeat(100);
        profiles[0].name = "中".repeat(66) + "ab";
        // Reserved nonexistent domain: save must not attempt DNS or establish a connection.
        profiles[0].url = "http://no-network.invalid:8080".into();
        assert!(manager.save(request(0, profiles, Some("p1"))).is_ok());
    }

    #[test]
    fn corrupt_invalid_version_and_oversized_disk_fail_explicitly_without_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        for bytes in [
            b"broken".to_vec(),
            vec![b' '; MAX_BYTES + 1],
            serde_json::to_vec(&json!({"version":2,"revision":0,"profiles":[],"active_id":null}))
                .unwrap(),
            serde_json::to_vec(
                &json!({"version":1,"revision":0,"profiles":[],"active_id":"missing"}),
            )
            .unwrap(),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(Manager::new(dir.path(), None).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(Manager::new(dir.path(), None).is_err());
    }

    #[test]
    fn bounded_read_and_final_serialization_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, vec![b' '; MAX_BYTES]).unwrap();
        assert_eq!(
            read_source(&path, MAX_BYTES).unwrap().unwrap().len(),
            MAX_BYTES
        );
        std::fs::write(&path, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(read_source(&path, MAX_BYTES).is_err());
        let saved = Snapshot {
            version: 1,
            revision: 0,
            active_id: None,
            profiles: vec![Profile {
                id: "p".into(),
                name: "p".into(),
                url: "x".repeat(MAX_BYTES),
            }],
        };
        assert!(serialize(&saved).unwrap_err().contains("64 KiB"));
    }

    #[test]
    fn external_edits_deletion_and_creation_do_not_change_memory_or_get_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Manager::new(dir.path(), None).unwrap();
        let path = dir.path().join(FILE_NAME);
        // Creation after read-only initialization is also a conflict.
        std::fs::write(&path, b"external").unwrap();
        assert!(
            manager
                .save(request(0, vec![], None))
                .unwrap_err()
                .contains("外部")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        std::fs::remove_file(&path).unwrap();
        let original = manager
            .save(request(0, vec![profile("p")], Some("p")))
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // Even a semantically identical JSON file with different whitespace is an edit.
        let mut edited = bytes.clone();
        edited.push(b'\n');
        std::fs::write(&path, &edited).unwrap();
        assert!(manager.save(request(1, vec![], None)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), edited);
        std::fs::remove_file(&path).unwrap();
        assert!(manager.save(request(1, vec![], None)).is_err());
        assert!(!path.exists());
        assert_eq!(manager.snapshot(), original);
        assert_eq!(manager.route().profile_id.as_deref(), Some("p"));
    }

    #[test]
    fn failed_write_and_revision_overflow_leave_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let manager = Manager::new(&data, None).unwrap();
        std::fs::write(&data, "not a directory").unwrap();
        assert!(manager.save(request(0, vec![], None)).is_err());
        assert_eq!(manager.snapshot().revision, 0);
        assert_eq!(std::fs::read_to_string(&data).unwrap(), "not a directory");
        std::fs::remove_file(&data).unwrap();
        std::fs::create_dir(&data).unwrap();
        let saved = Snapshot {
            version: 1,
            revision: u64::MAX,
            profiles: vec![],
            active_id: None,
        };
        std::fs::write(data.join(FILE_NAME), serialize(&saved).unwrap()).unwrap();
        let manager = Manager::new(&data, None).unwrap();
        assert!(
            manager
                .save(request(u64::MAX, vec![], None))
                .unwrap_err()
                .contains("耗尽")
        );
        assert_eq!(manager.snapshot(), saved);
    }

    #[test]
    fn legacy_migration_stays_disabled_and_memory_only_until_explicit_save() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = legacy(dir.path(), json!({"url":"http://LOCALHOST:8080/"}));
        let manager = Manager::new(dir.path(), None).unwrap();
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.active_id, None);
        assert_eq!(snapshot.profiles.len(), 1);
        assert_eq!(snapshot.profiles[0].url, "http://localhost:8080");
        assert_eq!(manager.route(), RouteSnapshot::default());
        assert!(!dir.path().join(FILE_NAME).exists());
        assert_eq!(
            std::fs::read(dir.path().join("settings.json")).unwrap(),
            bytes
        );
        manager.save(request(0, snapshot.profiles, None)).unwrap();
        std::fs::write(dir.path().join("settings.json"), "broken").unwrap();
        // Once the new file exists it is the sole source; never resync the old settings.
        assert_eq!(
            Manager::new(dir.path(), None).unwrap().snapshot().revision,
            1
        );
    }

    #[test]
    fn invalid_legacy_proxy_and_corrupt_legacy_data_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        for raw in [
            json!({"url":"http://localhost"}),
            json!({"url":"socks5://localhost:8080"}),
            json!({"url":"http://localhost:8080","source":"extra"}),
            json!("http://localhost:8080"),
        ] {
            let bytes = legacy(dir.path(), raw);
            assert!(Manager::new(dir.path(), None).is_err());
            assert!(!dir.path().join(FILE_NAME).exists());
            assert_eq!(
                std::fs::read(dir.path().join("settings.json")).unwrap(),
                bytes
            );
        }
        legacy(dir.path(), json!({"url":"http://localhost:3000"}));
        assert!(Manager::new(dir.path(), Some(3000)).is_err());
        for raw in [
            "broken",
            "{}",
            r#"{"version":2,"value":{}}"#,
            r#"{"version":1,"value":null}"#,
        ] {
            std::fs::write(dir.path().join("settings.json"), raw).unwrap();
            assert!(Manager::new(dir.path(), None).is_err());
        }
        for raw in [
            r#"{"version":1,"value":{}}"#,
            r#"{"version":1,"value":{"http_proxy":null}}"#,
        ] {
            std::fs::write(dir.path().join("settings.json"), raw).unwrap();
            assert!(
                Manager::new(dir.path(), None)
                    .unwrap()
                    .snapshot()
                    .profiles
                    .is_empty()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_including_dangling_are_rejected_on_new_and_save() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let path = dir.path().join(FILE_NAME);
        let manager = Manager::new(dir.path(), None).unwrap();
        symlink(&target, &path).unwrap();
        assert!(Manager::new(dir.path(), None).is_err());
        assert!(manager.save(request(0, vec![], None)).is_err());
        assert!(!target.exists());
        std::fs::write(&target, "unchanged").unwrap();
        assert!(Manager::new(dir.path(), None).is_err());
        assert!(manager.save(request(0, vec![], None)).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
        std::fs::remove_file(&path).unwrap();
        symlink(&target, dir.path().join("settings.json")).unwrap();
        assert!(Manager::new(dir.path(), None).is_err());
    }
}
