use crate::{model::Run, storage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HISTORY_FILES: usize = 100;
const MAX_HISTORY_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone)]
pub struct History {
    dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    run: Run,
}

impl History {
    pub fn open(data: &Path) -> Result<Self, String> {
        let dir = data.join("history");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建运行历史目录失败：{e}"))?;
        let history = Self {
            dir,
            lock: Arc::new(Mutex::new(())),
        };
        {
            let _guard = history.lock.lock().map_err(|_| "运行历史锁已损坏")?;
            history.recover_pending()?;
            history.enforce_retention()?;
        }
        Ok(history)
    }

    pub fn begin(&self, run: &Run) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|_| "运行历史锁已损坏")?;
        let id = canonical_id(&run.id)?;
        let pending = self.pending_path(&id);
        let complete = self.run_path(&id);
        if checked_metadata(&complete)?.is_some() {
            return Err(format!("运行历史 {id} 已存在，拒绝覆盖"));
        }
        if checked_metadata(&pending)?.is_some() {
            return Err(format!("运行历史标记 {id} 已存在，拒绝覆盖"));
        }
        let bytes = serde_json::to_vec(run).map_err(|e| format!("序列化运行历史失败：{e}"))?;
        check_size(bytes.len() as u64)?;
        storage::atomic_bytes(&pending, &bytes)
    }

    pub fn finish(&self, run: &Run) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|_| "运行历史锁已损坏")?;
        let id = canonical_id(&run.id)?;
        if run.active()
            || run.active != 0
            || run.connected != 0
            || run
                .targets
                .iter()
                .any(|t| t.active != 0 || t.connected != 0)
        {
            return Err("不能完成仍在运行或连接尚未清空的运行历史".into());
        }
        let path = self.run_path(&id);
        let pending = self.pending_path(&id);
        let has_pending = checked_metadata(&pending)?.is_some();
        if checked_metadata(&path)?.is_some() {
            // Validate rather than ever replacing an existing (possibly corrupt) record.
            self.read_envelope(&path)?;
            return Err(format!("运行历史 {id} 已存在，拒绝覆盖"));
        }
        let envelope = Envelope {
            version: 1,
            run: run.clone(),
        };
        let bytes =
            serde_json::to_vec(&envelope).map_err(|e| format!("序列化运行历史失败：{e}"))?;
        check_size(bytes.len() as u64)?;
        storage::atomic_bytes(&path, &bytes)?;
        if has_pending {
            std::fs::remove_file(pending).map_err(|e| format!("删除运行标记失败：{e}"))?;
        }
        self.enforce_retention()
    }

    pub fn list(&self) -> Result<Vec<Value>, String> {
        let _guard = self.lock.lock().map_err(|_| "运行历史锁已损坏")?;
        let mut entries = self.completed_files()?;
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        entries
            .into_iter()
            .map(|(id, _)| {
                let run = self.read_envelope(&self.run_path(&id))?.run;
                serde_json::to_value(serde_json::json!({
                    "id": run.id,
                    "status": run.status,
                    "mode": run.mode,
                    "started_at": run.started_at,
                    "elapsed_secs": run.elapsed_secs,
                    "attempted": run.attempted,
                    "errors": run.errors,
                }))
                .map_err(|e| format!("生成运行历史摘要失败：{e}"))
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Result<Run, String> {
        let _guard = self.lock.lock().map_err(|_| "运行历史锁已损坏")?;
        let id = canonical_id(id)?;
        Ok(self.read_envelope(&self.run_path(&id))?.run)
    }

    pub fn clear(&self) -> Result<(), String> {
        let _guard = self.lock.lock().map_err(|_| "运行历史锁已损坏")?;
        for entry in std::fs::read_dir(&self.dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if owned_name(&path, ".json") || owned_name(&path, ".pending.json") {
                checked_metadata(&path)?;
                std::fs::remove_file(path).map_err(|e| format!("清除运行历史失败：{e}"))?;
            }
        }
        Ok(())
    }

    fn recover_pending(&self) -> Result<(), String> {
        for entry in std::fs::read_dir(&self.dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if !owned_name(&path, ".pending.json") {
                continue;
            }
            checked_metadata(&path)?;
            let id = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".pending.json"))
                .ok_or("无效运行标记名称")?;
            let id = canonical_id(id)?;
            let complete = self.run_path(&id);
            if checked_metadata(&complete)?.is_some() {
                self.read_envelope(&complete)?;
                std::fs::remove_file(path).map_err(|e| format!("删除冗余运行标记失败：{e}"))?;
                continue;
            }
            let bytes = storage::read_bounded(&path, MAX_FILE_BYTES)
                .map_err(|e| format!("运行标记 {id} 损坏，未覆盖原文件：{e}"))?;
            let mut run: Run = serde_json::from_slice(&bytes)
                .map_err(|e| format!("运行标记 {id} 损坏，未覆盖原文件：{e}"))?;
            if run.id != id {
                return Err(format!("运行标记 {id} 内部 ID 不匹配，未覆盖原文件"));
            }
            run.status = "interrupted".into();
            run.reason = "data incomplete".into();
            run.active = 0;
            run.connected = 0;
            for target in &mut run.targets {
                target.active = 0;
                target.connected = 0;
            }
            let envelope = Envelope { version: 1, run };
            let encoded = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
            check_size(encoded.len() as u64)?;
            storage::atomic_bytes(&complete, &encoded)?;
            std::fs::remove_file(path).map_err(|e| format!("删除已恢复运行标记失败：{e}"))?;
        }
        Ok(())
    }

    fn enforce_retention(&self) -> Result<(), String> {
        let mut entries = self.completed_files()?;
        entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let mut total = entries.iter().try_fold(0u64, |sum, (id, _)| {
            let path = self.run_path(id);
            let size = checked_metadata(&path)?.ok_or("运行历史文件已消失")?.len();
            check_size(size)?;
            Ok::<u64, String>(sum.saturating_add(size))
        })?;
        while entries.len() > MAX_HISTORY_FILES || total > MAX_HISTORY_BYTES {
            let (id, _) = entries.remove(0);
            let path = self.run_path(&id);
            let size = checked_metadata(&path)?.ok_or("运行历史文件已消失")?.len();
            std::fs::remove_file(path).map_err(|e| format!("清理过期运行历史失败：{e}"))?;
            total = total.saturating_sub(size);
        }
        Ok(())
    }

    fn completed_files(&self) -> Result<Vec<(String, std::time::SystemTime)>, String> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&self.dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if owned_name(&path, ".pending.json") {
                checked_metadata(&path)?;
            }
            if !owned_name(&path, ".json") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("无效运行历史名称")?;
            let id = canonical_id(id)?;
            // Parse each owned file before retention/listing; corruption is reported, never discarded.
            self.read_envelope(&path)?;
            let modified = checked_metadata(&path)?
                .ok_or("运行历史文件已消失")?
                .modified()
                .map_err(|e| e.to_string())?;
            files.push((id, modified));
        }
        Ok(files)
    }

    fn read_envelope(&self, path: &Path) -> Result<Envelope, String> {
        checked_metadata(path)?;
        let bytes = storage::read_bounded(path, MAX_FILE_BYTES)
            .map_err(|e| format!("读取运行历史 {} 失败：{e}", path.display()))?;
        let envelope: Envelope = serde_json::from_slice(&bytes)
            .map_err(|e| format!("运行历史 {} 损坏，未覆盖原文件：{e}", path.display()))?;
        if envelope.version != 1 {
            return Err(format!(
                "运行历史 {} 版本不受支持，未覆盖原文件",
                path.display()
            ));
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("无效运行历史名称")?;
        if envelope.run.id != id {
            return Err(format!(
                "运行历史 {} 内部 ID 不匹配，未覆盖原文件",
                path.display()
            ));
        }
        Ok(envelope)
    }

    fn run_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn pending_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.pending.json"))
    }
}

fn canonical_id(id: &str) -> Result<String, String> {
    let parsed = uuid::Uuid::parse_str(id).map_err(|_| "运行 ID 必须是规范 UUID")?;
    let canonical = parsed.hyphenated().to_string();
    if id != canonical {
        return Err("运行 ID 必须是规范 UUID".into());
    }
    Ok(canonical)
}

fn checked_metadata(path: &Path) -> Result<Option<std::fs::Metadata>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("拒绝访问符号链接运行历史：{}", path.display()))
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("读取运行历史元数据失败：{error}")),
    }
}

fn owned_name(path: &Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(suffix))
        .is_some_and(|stem| canonical_id(stem).is_ok())
}

fn check_size(size: u64) -> Result<(), String> {
    if size > MAX_FILE_BYTES {
        Err("单条运行历史不能超过 8 MiB".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RunConfig, Sample};

    fn run(id: &str) -> Run {
        Run {
            id: id.into(),
            status: "completed".into(),
            mode: "probe".into(),
            started_at: "2025-01-01T00:00:00Z".into(),
            elapsed_secs: 1.5,
            config: RunConfig {
                mode: "probe".into(),
                list_ids: vec![],
                target_ids: vec![],
                authorized_ids: vec![],
                concurrency: 1,
                rps: 1,
                duration_secs: 1,
                timeout_secs: 1,
                ramp_secs: 0,
                fresh_connections: false,
                proxy: None,
            },
            reason: String::new(),
            attempted: 12,
            completed: 10,
            responses: 10,
            good_status: 9,
            errors: 2,
            cancelled: 0,
            active: 0,
            peak_active: 4,
            connected: 0,
            peak_connected: 6,
            bytes: 20,
            p50_ms: Some(1.0),
            p95_ms: Some(2.0),
            rps: 8.0,
            series: vec![Sample {
                elapsed_secs: 1.0,
                rps: 8.0,
                p95_ms: None,
                active: 2,
                connected: 3,
            }],
            targets: vec![],
            sequence: 1,
        }
    }

    #[test]
    fn begin_finish_list_get_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut entry = run(&id);
        history.begin(&entry).unwrap();
        assert!(history.dir.join(format!("{id}.pending.json")).exists());
        entry.status = "completed".into();
        history.finish(&entry).unwrap();
        assert_eq!(history.get(&id).unwrap().attempted, 12);
        let list = history.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], id);
        assert!(!history.dir.join(format!("{id}.pending.json")).exists());
        history.clear().unwrap();
        assert!(history.list().unwrap().is_empty());
    }

    #[test]
    fn recovers_pending_as_interrupted_without_changing_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut entry = run(&id);
        entry.status = "running".into();
        entry.active = 3;
        entry.connected = 5;
        entry.targets.push(crate::model::TargetStats {
            id: "target".into(),
            active: 2,
            connected: 4,
            attempted: 9,
            errors: 1,
            ..Default::default()
        });
        history.begin(&entry).unwrap();
        drop(history);
        let reopened = History::open(dir.path()).unwrap();
        let recovered = reopened.get(&id).unwrap();
        assert_eq!(recovered.status, "interrupted");
        assert_eq!(recovered.reason, "data incomplete");
        assert_eq!((recovered.active, recovered.connected), (0, 0));
        assert_eq!((recovered.attempted, recovered.errors), (12, 2));
        assert_eq!(
            (recovered.targets[0].active, recovered.targets[0].connected),
            (0, 0)
        );
        assert_eq!(
            (recovered.targets[0].attempted, recovered.targets[0].errors),
            (9, 1)
        );
        assert_eq!(recovered.series[0].active, 2);
    }

    #[test]
    fn finish_rejects_live_snapshots_and_preserves_pending_marker() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut entry = run(&id);
        history.begin(&entry).unwrap();
        for status in ["running", "stopping"] {
            entry.status = status.into();
            assert!(history.finish(&entry).is_err());
        }
        entry.status = "completed".into();
        entry.active = 1;
        assert!(history.finish(&entry).is_err());
        entry.active = 0;
        entry.connected = 1;
        assert!(history.finish(&entry).is_err());
        entry.connected = 0;
        entry.targets.push(crate::model::TargetStats {
            active: 1,
            ..Default::default()
        });
        assert!(history.finish(&entry).is_err());
        entry.targets[0].active = 0;
        entry.targets[0].connected = 1;
        assert!(history.finish(&entry).is_err());
        assert!(!history.run_path(&id).exists());
        assert!(history.pending_path(&id).exists());
    }

    #[test]
    fn duplicate_finish_leaves_original_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut entry = run(&id);
        history.finish(&entry).unwrap();
        let original = std::fs::read(history.run_path(&id)).unwrap();
        entry.attempted = 900;
        assert!(history.finish(&entry).is_err());
        assert_eq!(std::fs::read(history.run_path(&id)).unwrap(), original);
        assert_eq!(history.get(&id).unwrap().attempted, 12);
    }

    #[test]
    fn oversized_begin_and_finish_do_not_write_files() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut entry = run(&id);
        entry.reason = "x".repeat(MAX_FILE_BYTES as usize);
        assert!(history.begin(&entry).unwrap_err().contains("8 MiB"));
        assert!(history.finish(&entry).unwrap_err().contains("8 MiB"));
        assert!(!history.run_path(&id).exists());
        assert!(!history.pending_path(&id).exists());
    }

    #[test]
    fn retention_keeps_100_owned_records_and_preserves_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        let unrelated = history.dir.join("notes.json");
        std::fs::write(&unrelated, "unrelated").unwrap();
        let pending_id = uuid::Uuid::new_v4().to_string();
        history.begin(&run(&pending_id)).unwrap();
        // Equal timestamps are harmless: UUID tie-breaking makes eviction deterministic.
        for _ in 0..101 {
            let id = uuid::Uuid::new_v4().to_string();
            history.finish(&run(&id)).unwrap();
        }
        assert_eq!(history.list().unwrap().len(), 100);
        assert_eq!(std::fs::read_to_string(unrelated).unwrap(), "unrelated");
        assert!(history.pending_path(&pending_id).exists());
    }

    #[cfg(unix)]
    #[test]
    fn owned_symlinks_are_explicit_errors_and_never_followed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let outside = dir.path().join("outside.json");
        std::fs::write(&outside, "do not touch").unwrap();
        let link = history.run_path(&id);
        symlink(&outside, &link).unwrap();
        assert!(history.get(&id).unwrap_err().contains("符号链接"));
        assert!(history.list().unwrap_err().contains("符号链接"));
        assert!(history.clear().unwrap_err().contains("符号链接"));
        assert!(
            history
                .enforce_retention()
                .unwrap_err()
                .contains("符号链接")
        );
        assert!(history.finish(&run(&id)).unwrap_err().contains("符号链接"));
        assert!(History::open(dir.path()).is_err());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(link).unwrap();
        let pending = history.pending_path(&id);
        // A dangling link must not evade existence checks either.
        symlink(dir.path().join("missing"), &pending).unwrap();
        assert!(history.begin(&run(&id)).unwrap_err().contains("符号链接"));
        assert!(history.finish(&run(&id)).unwrap_err().contains("符号链接"));
        assert!(history.clear().unwrap_err().contains("符号链接"));
        assert!(history.list().unwrap_err().contains("符号链接"));
        assert!(
            history
                .enforce_retention()
                .unwrap_err()
                .contains("符号链接")
        );
        assert!(History::open(dir.path()).is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "do not touch");
    }

    #[test]
    fn canonical_ids_and_corrupt_files_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path()).unwrap();
        assert!(history.get("../../etc/passwd").is_err());
        let id = uuid::Uuid::new_v4().to_string();
        let file = history.dir.join(format!("{id}.json"));
        std::fs::write(&file, "bad json").unwrap();
        assert!(history.list().unwrap_err().contains("损坏"));
        assert!(history.finish(&run(&id)).unwrap_err().contains("损坏"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bad json");
        assert!(History::open(dir.path()).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bad json");
        history.clear().unwrap();
        assert!(!file.exists());
        std::fs::write(history.dir.join("not-a-run.json"), "keep").unwrap();
        history.clear().unwrap();
        assert!(history.dir.join("not-a-run.json").exists());
    }
}
