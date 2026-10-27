use crate::{model::SiteList, policy::parse_url};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedLists {
    version: u32,
    lists: Vec<SiteList>,
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    atomic_bytes(
        path,
        &serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
}
pub fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("无效数据路径")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        drop(f);
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to).map_err(|e| e.to_string())
}
#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: buffers are valid NUL-terminated UTF-16 strings for the duration of the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

pub fn load_custom(dir: &Path) -> Result<Vec<SiteList>, String> {
    let path = dir.join("lists.json");
    if !path.exists() {
        return Ok(vec![]);
    }
    let data = read_bounded(&path, 1_048_576)?;
    let mut saved: SavedLists =
        serde_json::from_slice(&data).map_err(|e| format!("lists.json 损坏，未覆盖原文件：{e}"))?;
    if saved.version != 1 {
        return Err("不支持的列表数据版本".into());
    }
    validate_lists(&mut saved.lists)?;
    Ok(saved.lists)
}
pub fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > max {
        return Err(format!("配置文件超过 {max} 字节"));
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(size as usize);
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err(format!("配置文件超过 {max} 字节"));
    }
    Ok(bytes)
}
pub fn save_custom(dir: &Path, lists: Vec<SiteList>) -> Result<(), String> {
    let bytes =
        serde_json::to_vec_pretty(&SavedLists { version: 1, lists }).map_err(|e| e.to_string())?;
    if bytes.len() > 1_048_576 {
        return Err("列表保存后的数据不能超过 1 MiB".into());
    }
    atomic_bytes(&dir.join("lists.json"), &bytes)
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn validate_lists(lists: &mut [SiteList]) -> Result<(), String> {
    if lists.len() > 50 {
        return Err("最多 50 个自定义列表".into());
    }
    let mut ids = HashSet::new();
    let mut count = 0;
    for l in lists {
        if l.builtin
            || !valid_id(&l.id)
            || !ids.insert(l.id.clone())
            || l.name.trim().is_empty()
            || l.name.len() > 200
            || l.description.len() > 1000
        {
            return Err("列表 ID/名称无效、重复或试图修改内置列表".into());
        }
        count += l.targets.len();
        if count > 1000 {
            return Err("自定义目标总数不能超过 1000".into());
        }
        let mut targets = HashSet::new();
        for t in &mut l.targets {
            if !valid_id(&t.id)
                || !targets.insert(t.id.clone())
                || t.name.trim().is_empty()
                || t.name.len() > 200
            {
                return Err("目标名称/ID无效或重复".into());
            }
            t.url = parse_url(&t.url)?.to_string();
            t.probe_only = true;
            t.source.clear();
            t.checked_at.clear();
            t.region.clear();
        }
    }
    Ok(())
}
pub fn load_presets(root: &Path) -> Result<Vec<SiteList>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|s| s == "json"))
        .collect();
    paths.sort();
    let mut lists = vec![];
    for p in paths {
        let mut l: SiteList = serde_json::from_slice(&read_bounded(&p, 1_048_576)?)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        l.builtin = true;
        for t in &mut l.targets {
            t.url = parse_url(&t.url)?.to_string();
            t.probe_only = true;
        }
        lists.push(l);
    }
    Ok(lists)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_size_limit_checks_final_bytes_without_replacing_existing_data() {
        let dir = tempfile::tempdir().unwrap();
        save_custom(dir.path(), vec![]).unwrap();
        let original = std::fs::read(dir.path().join("lists.json")).unwrap();
        let targets = (0..1000)
            .map(|i| crate::model::Target {
                id: format!("target-{i}"),
                name: "n".repeat(200),
                url: format!("https://example.com/{}", "a".repeat(680)),
                region: String::new(),
                source: String::new(),
                checked_at: String::new(),
                probe_only: true,
            })
            .collect();
        let mut lists = vec![SiteList {
            id: "size-fixture".into(),
            name: "fixture".into(),
            description: String::new(),
            builtin: false,
            targets,
        }];
        validate_lists(&mut lists).unwrap();
        assert!(serde_json::to_vec(&lists).unwrap().len() < 1_048_576);
        assert!(
            save_custom(dir.path(), lists)
                .unwrap_err()
                .contains("1 MiB")
        );
        assert_eq!(
            std::fs::read(dir.path().join("lists.json")).unwrap(),
            original
        );
        assert!(load_custom(dir.path()).unwrap().is_empty());
    }
    #[test]
    fn atomic_roundtrip_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        save_custom(dir.path(), vec![]).unwrap();
        assert!(load_custom(dir.path()).unwrap().is_empty());
        std::fs::write(dir.path().join("lists.json"), "broken").unwrap();
        assert!(load_custom(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lists.json")).unwrap(),
            "broken"
        );
    }
}
