//! 远程执行（remote-exec）集中审计：JSONL 追加写 `<workspace>/remote-exec/audit/exec-YYYY-MM-DD.jsonl`。
//!
//! **绝不记 script/args/env/stdout/stderr 正文**（可能含凭据），只记元信息：operator /
//! worker_id / server id / 起止时间 / shell / cwd / `script_hash` / state / exit_code /
//! 输出字节数 / 截断标记 / duration，以及鉴权失败、领取失败、结果归属失败、worker 失联等
//! 异常事件。按天轮转，默认保留 30 天（每次写入顺带清理过期文件）。见
//! `docs/remote-exec-design.md` 第一期 §7。
//!
//! 文件 I/O 全部经 [`tokio::task::spawn_blocking`]：异步上下文禁同步阻塞 I/O
//! （见仓库 `CLAUDE.md`「编码约定」）。

use serde::Serialize;
use serde_json::json;
use std::path::{Path, PathBuf};

const RETENTION_DAYS: i64 = 30;

/// 单次执行终态的审计记录（成功路径：`completed`/`timed_out`/`spawn_failed`）。
#[derive(Debug, Clone, Serialize)]
pub struct ExecAuditRecord {
    pub operator: String,
    pub worker_id: String,
    pub id: String,
    pub shell: String,
    pub cwd: Option<String>,
    pub script_hash: String,
    pub state: String,
    pub exit_code: Option<i32>,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub duration_ms: u64,
    pub started_at: String,
    pub finished_at: String,
}

/// 异常事件：鉴权失败 / 领取失败(`not_picked_up`) / 结果归属失败 / worker 失联(`unknown`) 等。
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    pub event: String,
    pub worker_id: Option<String>,
    pub id: Option<String>,
    pub reason: String,
}

/// 审计写入句柄。只持有目录路径，克隆代价极低（供 `spawn_blocking` 闭包捕获）。
#[derive(Clone)]
pub struct AuditLog {
    dir: PathBuf,
}

impl AuditLog {
    pub fn new(workspace: &Path) -> Self {
        Self {
            dir: workspace.join("remote-exec").join("audit"),
        }
    }

    /// 记一条执行终态。写失败只记 warn 日志，不影响主流程（审计不应拖垮业务）。
    pub async fn record_exec(&self, rec: ExecAuditRecord) {
        let mut v = serde_json::to_value(&rec).unwrap_or_else(|_| json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("ts".into(), json!(chrono::Utc::now().to_rfc3339()));
            obj.insert("event".into(), json!("exec"));
        }
        self.append_blocking(v).await;
    }

    /// 记一条异常事件（鉴权失败 / 领取失败 / 结果归属失败 / worker 失联）。
    pub async fn record_event(
        &self,
        event: impl Into<String>,
        worker_id: Option<String>,
        id: Option<String>,
        reason: impl Into<String>,
    ) {
        let evt = AuditEvent {
            event: event.into(),
            worker_id,
            id,
            reason: reason.into(),
        };
        let mut v = serde_json::to_value(&evt).unwrap_or_else(|_| json!({}));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("ts".into(), json!(chrono::Utc::now().to_rfc3339()));
        }
        self.append_blocking(v).await;
    }

    async fn append_blocking(&self, value: serde_json::Value) {
        let dir = self.dir.clone();
        let res = tokio::task::spawn_blocking(move || append_and_prune(&dir, value)).await;
        match res {
            Ok(Err(e)) => log::warn!("remote-exec audit write failed: {e:#}"),
            Err(e) => log::warn!("remote-exec audit task panicked: {e}"),
            Ok(Ok(())) => {}
        }
    }
}

fn append_and_prune(dir: &Path, value: serde_json::Value) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::Write;

    std::fs::create_dir_all(dir).context("create remote-exec/audit dir")?;
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let path = dir.join(format!("exec-{today}.jsonl"));
    let mut line = serde_json::to_string(&value).context("serialize audit record")?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open audit file {}", path.display()))?;
    f.write_all(line.as_bytes()).context("write audit line")?;

    prune_expired(dir);
    Ok(())
}

/// 删除超过 [`RETENTION_DAYS`] 天的审计文件。目录/文件名解析失败一律跳过，不致命。
fn prune_expired(dir: &Path) {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(date_str) = name
            .strip_prefix("exec-")
            .and_then(|s| s.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
            continue;
        };
        let Some(midnight) = date.and_hms_opt(0, 0, 0) else {
            continue;
        };
        if midnight.and_utc() < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_exec_and_event_append_jsonl_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let log = AuditLog::new(tmp.path());

        log.record_exec(ExecAuditRecord {
            operator: "alice".into(),
            worker_id: "w1".into(),
            id: "id-1".into(),
            shell: "powershell".into(),
            cwd: None,
            script_hash: "deadbeef".into(),
            state: "completed".into(),
            exit_code: Some(0),
            stdout_bytes: 2,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            duration_ms: 5,
            started_at: "2026-01-01T00:00:00Z".into(),
            finished_at: "2026-01-01T00:00:01Z".into(),
        })
        .await;
        log.record_event(
            "not_picked_up",
            Some("w1".into()),
            Some("id-2".into()),
            "pickup timeout",
        )
        .await;

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let path = tmp
            .path()
            .join("remote-exec")
            .join("audit")
            .join(format!("exec-{today}.jsonl"));
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["event"], "exec");
        assert_eq!(first["operator"], "alice");
        assert_eq!(first["script_hash"], "deadbeef");
        // 绝不记正文。
        assert!(first.get("script").is_none());
        assert!(first.get("stdout").is_none());
        assert!(first.get("stderr").is_none());

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["event"], "not_picked_up");
        assert_eq!(second["worker_id"], "w1");
        assert_eq!(second["id"], "id-2");
    }

    #[test]
    fn prune_removes_old_files_keeps_recent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path()).unwrap();
        let old_name = format!(
            "exec-{}.jsonl",
            (chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS + 5)).format("%Y-%m-%d")
        );
        let recent_name = format!("exec-{}.jsonl", chrono::Utc::now().format("%Y-%m-%d"));
        std::fs::write(tmp.path().join(&old_name), "{}\n").unwrap();
        std::fs::write(tmp.path().join(&recent_name), "{}\n").unwrap();

        prune_expired(tmp.path());

        assert!(!tmp.path().join(&old_name).exists());
        assert!(tmp.path().join(&recent_name).exists());
    }
}
