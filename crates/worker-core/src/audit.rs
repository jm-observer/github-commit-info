//! 本地审计：JSONL 追加写，**默认不记录脚本/args/env/stdout/stderr 正文**
//! （其中可能含凭据），只记元信息 + 脚本 SM3 短哈希。见设计 §7。
//!
//! 取舍：
//! - 按天分文件（`exec-YYYY-MM-DD.jsonl`），构造时顺手删掉 30 天前的文件，不引入
//!   独立的轮转/压缩机制。
//! - 写失败只 `log::warn!`，绝不让审计影响主执行路径（`record` 不返回 `Result`）。
//! - 用 `std::fs`（阻塞）而非 tokio::fs：单行 JSON 追加写足够小，仓库约定
//!   「写小文件可接受」；`record` 本身也不是 `async fn`，调用方（`Executor::run`）
//!   直接同步调用即可，不必为此专门切一次 `.await`。

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 审计文件默认保留天数。
const RETAIN_DAYS: i64 = 30;

/// 追加式审计日志（按天分文件、保留 30 天）。
pub struct AuditLog {
    dir: PathBuf,
}

impl AuditLog {
    /// `dir` 一般是 `<root>/remote-exec/audit/`；不存在则创建，权限限制为当前用户。
    pub fn new(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).context("create audit dir failed")?;
        set_private_dir(dir);
        purge_old_files(dir, RETAIN_DAYS);
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// 追加一条审计记录：`event` 为事件名（如 `exec_start` / `exec_end` / `auth_failed`），
    /// `fields` 为附加字段（只放元信息，不放正文）。写失败只 warn，不影响执行。
    pub fn record(&self, event: &str, fields: serde_json::Value) {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let path = self.dir.join(format!("exec-{today}.jsonl"));

        let mut obj = serde_json::Map::new();
        obj.insert(
            "ts".to_string(),
            serde_json::Value::String(chrono::Local::now().to_rfc3339()),
        );
        obj.insert(
            "event".to_string(),
            serde_json::Value::String(event.to_string()),
        );
        match fields {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    // event/ts 是审计记录的保留字段，附加字段不应覆盖它们。
                    if k != "ts" && k != "event" {
                        obj.insert(k, v);
                    }
                }
            }
            serde_json::Value::Null => {}
            other => {
                obj.insert("fields".to_string(), other);
            }
        }

        let line = match serde_json::to_string(&serde_json::Value::Object(obj)) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("audit: serialize record failed: {e}");
                return;
            }
        };

        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{line}") {
                    log::warn!("audit: write to {path:?} failed: {e}");
                } else {
                    set_private_file(&path);
                }
            }
            Err(e) => log::warn!("audit: open {path:?} failed: {e}"),
        }
    }
}

/// 删除超过 `keep_days` 天的 `exec-YYYY-MM-DD.jsonl` 审计文件。
fn purge_old_files(dir: &Path, keep_days: i64) {
    let cutoff = chrono::Local::now().date_naive() - chrono::Duration::days(keep_days);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(date_str) = name
            .strip_prefix("exec-")
            .and_then(|s| s.strip_suffix(".jsonl"))
        else {
            continue;
        };
        if let Ok(d) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            if d < cutoff {
                if let Err(e) = std::fs::remove_file(&path) {
                    log::warn!("audit: purge old file {path:?} failed: {e}");
                }
            }
        }
    }
}

#[cfg(unix)]
fn set_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_private_dir(_path: &Path) {}

#[cfg(unix)]
fn set_private_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_private_file(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("worker-core-audit-test-{tag}-{nanos}"))
    }

    #[test]
    fn record_appends_jsonl_line_without_body_fields() {
        let dir = tmp_dir("append");
        let log = AuditLog::new(&dir).unwrap();

        log.record(
            "exec_start",
            serde_json::json!({
                "id": "server-1",
                "operator": "alice",
                "script_hash": "deadbeefdeadbeef",
            }),
        );
        log.record(
            "exec_end",
            serde_json::json!({
                "id": "server-1",
                "state": "completed",
                "exit_code": 0,
            }),
        );

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let path = dir.join(format!("exec-{today}.jsonl"));
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("ts").is_some());
            assert!(v.get("event").is_some());
            // 明确不应出现的正文字段
            assert!(v.get("script").is_none());
            assert!(v.get("stdout").is_none());
            assert!(v.get("stderr").is_none());
            assert!(v.get("args").is_none());
            assert!(v.get("env").is_none());
        }

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["event"], "exec_start");
        assert_eq!(first["id"], "server-1");
        assert_eq!(first["script_hash"], "deadbeefdeadbeef");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn purge_old_files_removes_expired_and_keeps_recent() {
        let dir = tmp_dir("purge");
        std::fs::create_dir_all(&dir).unwrap();

        let old_name = "exec-2000-01-01.jsonl";
        let recent_name = format!("exec-{}.jsonl", chrono::Local::now().format("%Y-%m-%d"));
        std::fs::write(dir.join(old_name), "{}\n").unwrap();
        std::fs::write(dir.join(&recent_name), "{}\n").unwrap();

        purge_old_files(&dir, RETAIN_DAYS);

        assert!(
            !dir.join(old_name).exists(),
            "old audit file should be purged"
        );
        assert!(
            dir.join(&recent_name).exists(),
            "recent audit file should be kept"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_creates_dir_idempotently() {
        let dir = tmp_dir("idempotent");
        let _log1 = AuditLog::new(&dir).unwrap();
        let _log2 = AuditLog::new(&dir).unwrap();
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
