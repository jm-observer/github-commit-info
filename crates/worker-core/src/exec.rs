//! PowerShell 执行内核：写临时脚本 → 起进程 → 有界捕获 stdout/stderr → 超时杀进程树。
//!
//! 见 `docs/remote-exec-design.md` 第一期 §6。核心取舍：
//! - 临时目录名直接用 `req.id`（controller 生成、不可复用），但**必须先校验只含
//!   `[A-Za-z0-9_-]`**，防止调用方 payload 被当路径拼接（路径穿越）。
//! - `user.ps1` 带 UTF-8 BOM 写入，保证 `param()` / `#requires` 仍在脚本首部生效；
//!   `wrapper.ps1` 只负责设 UTF-8 输出编码后转发到 `user.ps1 @Args`，用
//!   `Join-Path $PSScriptRoot 'user.ps1'` 而非字符串拼接反斜杠，跨平台/带空格路径更稳。
//! - 全程用 `tokio::process::Command` 逐个 `.arg()`，不拼 shell 字符串。
//! - 有界捕获：stdout/stderr 各自起一个 tokio task 并发读，读满上限后继续排空管道
//!   （防止子进程写阻塞），只是不再往累积 buffer 里塞字节。
//! - 超时用 `tokio::time::timeout` 包住 `child.wait()`；超时后 Windows 用
//!   `taskkill /T /F /PID` 杀整棵树，Unix 用 `kill -9 <pid>`（未引入 libc 依赖，
//!   足够第一期需求；进程组/子孙进程的更强收敛留给未来迭代）。杀树是阻塞子进程调用，
//!   放进 `spawn_blocking` 避免占用 tokio 工作线程。
//! - 任务级 pid 登记表只在 `run` 期间存活，供 `kill_all`（Ctrl+C 退出前）遍历杀树；
//!   `Child` 本身仍由 `run` 独占，登记表不持有它。

use crate::audit::AuditLog;
use crate::proto::{self, ExecRequest, ExecResponse, ExecState};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::Duration;

/// 执行器：持有临时目录根、本地审计与「在跑任务 id → pid」登记表。
///
/// 登记表让 worker 在收到 Ctrl+C 时能杀掉整棵进程树（[`Executor::kill_all`]）；
/// `Child` 本身仍由执行任务独占，登记表只存 pid。
pub struct Executor {
    tmp_root: PathBuf,
    audit: AuditLog,
    running: Mutex<HashMap<String, u32>>,
}

impl Executor {
    /// `root` 为 worker 的工作根目录；临时脚本落 `<root>/remote-exec/tmp/<id>/`，
    /// 本地审计落 `<root>/remote-exec/audit/`。构造时清扫崩溃遗留的临时目录。
    pub fn new(root: &Path) -> Result<Self> {
        let tmp_root = root.join("remote-exec").join("tmp");
        std::fs::create_dir_all(&tmp_root).context("create remote-exec tmp root failed")?;
        set_private_dir(&tmp_root);

        let audit_dir = root.join("remote-exec").join("audit");
        let audit = AuditLog::new(&audit_dir).context("init exec audit log failed")?;

        // 启动时清扫上一次进程崩溃残留的临时目录（幂等，不影响本次运行）。
        cleanup_stale_tmp(root);

        Ok(Self {
            tmp_root,
            audit,
            running: Mutex::new(HashMap::new()),
        })
    }

    /// 跑一条请求，任何失败都归一为 [`ExecResponse`]（不抛）。
    pub async fn run(&self, req: &ExecRequest) -> ExecResponse {
        let start = Instant::now();

        // exec_start：只记元信息（含脚本 SM3 短哈希），绝不记正文。
        self.audit.record(
            "exec_start",
            serde_json::json!({
                "id": req.id,
                "operator": req.operator,
                "shell": req.shell,
                "cwd": req.cwd,
                "script_hash": proto::script_hash(&req.script),
                "timeout_secs": req.timeout_secs,
            }),
        );

        let resp = self.run_inner(req, start).await;

        // 任务结束（含失败），从登记表摘除。
        self.running.lock().unwrap().remove(&req.id);

        self.audit.record(
            "exec_end",
            serde_json::json!({
                "id": resp.id,
                "state": resp.state.as_str(),
                "exit_code": resp.exit_code,
                "stdout_bytes": resp.stdout.len(),
                "stderr_bytes": resp.stderr.len(),
                "stdout_truncated": resp.stdout_truncated,
                "stderr_truncated": resp.stderr_truncated,
                "duration_ms": resp.duration_ms,
            }),
        );

        resp
    }

    async fn run_inner(&self, req: &ExecRequest, start: Instant) -> ExecResponse {
        // 1) 双端共用字段上限校验（controller 已校验过一次，这里是 worker 侧兜底）。
        if let Err(e) = proto::validate(req) {
            return ExecResponse::spawn_failed(req.id.clone(), e.to_string(), elapsed_ms(start));
        }
        // 2) id 只允许 [A-Za-z0-9_-]，绝不用调用方输入直接拼路径。
        if let Err(e) = validate_id(&req.id) {
            return ExecResponse::spawn_failed(req.id.clone(), e, elapsed_ms(start));
        }

        let task_dir = self.tmp_root.join(&req.id);
        if let Err(e) = tokio::fs::create_dir_all(&task_dir).await {
            return ExecResponse::spawn_failed(
                req.id.clone(),
                format!("create tmp dir failed: {e}"),
                elapsed_ms(start),
            );
        }
        set_private_dir(&task_dir);

        // 3) 写 user.ps1（带 BOM，保证 param()/#requires 仍在首部）。
        let user_script_path = task_dir.join("user.ps1");
        if let Err(e) = write_ps1_with_bom(&user_script_path, &req.script).await {
            let _ = tokio::fs::remove_dir_all(&task_dir).await;
            return ExecResponse::spawn_failed(
                req.id.clone(),
                format!("write user.ps1 failed: {e}"),
                elapsed_ms(start),
            );
        }

        // 4) 写 wrapper.ps1：只设 UTF-8 输出编码 + 转发到 user.ps1。
        let wrapper_path = task_dir.join("wrapper.ps1");
        // 末行 `exit $LASTEXITCODE` 不可省：用 `&` 调起的脚本里 `exit N` 只结束 user.ps1,
        // wrapper 自身仍以 0 退出 —— 不显式回传就会把用户脚本的退出码整个吞掉(实测
        // `exit 3` 会变成 0)。补上后:`exit N` → N,未捕获异常 → 1,正常结束 → 0
        // ($LASTEXITCODE 为 $null 时 `exit $null` 即 0)。
        let wrapper_content = "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\r\n\
             $OutputEncoding = [System.Text.Encoding]::UTF8\r\n\
             & (Join-Path $PSScriptRoot 'user.ps1') @Args\r\n\
             exit $LASTEXITCODE\r\n";
        if let Err(e) = write_ps1_with_bom(&wrapper_path, wrapper_content).await {
            let _ = tokio::fs::remove_dir_all(&task_dir).await;
            return ExecResponse::spawn_failed(
                req.id.clone(),
                format!("write wrapper.ps1 failed: {e}"),
                elapsed_ms(start),
            );
        }

        // 5) cwd 存在性提前校验，给出比裸 spawn 失败更明确的 error 文案。
        if let Some(cwd) = &req.cwd {
            if !Path::new(cwd).is_dir() {
                let _ = tokio::fs::remove_dir_all(&task_dir).await;
                return ExecResponse::spawn_failed(
                    req.id.clone(),
                    format!("cwd does not exist: {cwd}"),
                    elapsed_ms(start),
                );
            }
        }

        // 6) 组装并 spawn 进程：逐个 .arg()，绝不拼 shell 字符串。
        let mut cmd = Command::new("powershell.exe");
        cmd.arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&wrapper_path);
        for a in &req.args {
            cmd.arg(a);
        }
        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }
        // env 是追加，不清空父环境（tokio::Command 默认继承父进程 env）。
        for (k, v) in &req.env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&task_dir).await;
                return ExecResponse::spawn_failed(
                    req.id.clone(),
                    format!("spawn failed: {e}"),
                    elapsed_ms(start),
                );
            }
        };

        let pid = child.id();
        if let Some(pid) = pid {
            self.running.lock().unwrap().insert(req.id.clone(), pid);
        }

        // 7) stdout/stderr 并发有界读取，避免管道写满导致子进程/本进程互相卡死。
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_limit = req.stdout_limit_bytes;
        let stderr_limit = req.stderr_limit_bytes;

        let stdout_task = tokio::spawn(async move {
            match stdout {
                Some(s) => read_bounded(s, stdout_limit).await,
                None => (Vec::new(), false),
            }
        });
        let stderr_task = tokio::spawn(async move {
            match stderr {
                Some(s) => read_bounded(s, stderr_limit).await,
                None => (Vec::new(), false),
            }
        });

        // 8) 超时包住 wait()；超时则杀整棵进程树后继续 wait() 回收。
        let wait_result =
            tokio::time::timeout(Duration::from_secs(req.timeout_secs), child.wait()).await;

        let (state, exit_code) = match wait_result {
            Ok(Ok(status)) => (ExecState::Completed, status.code()),
            Ok(Err(e)) => {
                let _ = tokio::fs::remove_dir_all(&task_dir).await;
                return ExecResponse::spawn_failed(
                    req.id.clone(),
                    format!("wait failed: {e}"),
                    elapsed_ms(start),
                );
            }
            Err(_) => {
                if let Some(pid) = pid {
                    kill_tree_async(pid).await;
                }
                // kill 完成后仍需 wait() 回收，避免僵尸/悬挂句柄。
                let _ = child.wait().await;
                (ExecState::TimedOut, None)
            }
        };

        let (stdout_buf, stdout_truncated) = stdout_task.await.unwrap_or((Vec::new(), false));
        let (stderr_buf, stderr_truncated) = stderr_task.await.unwrap_or((Vec::new(), false));

        let _ = tokio::fs::remove_dir_all(&task_dir).await;

        ExecResponse {
            id: req.id.clone(),
            state,
            exit_code,
            stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
            stdout_truncated,
            stderr_truncated,
            duration_ms: elapsed_ms(start),
            error: None,
        }
    }

    /// 杀掉当前所有在跑的进程树（Ctrl+C 退出前调用）。同步接口，内部对每个 pid
    /// 做一次阻塞的 taskkill/kill 调用——只在进程退出路径上跑一次，可接受。
    pub fn kill_all(&self) {
        let ids: Vec<(String, u32)> = self
            .running
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        for (id, pid) in ids {
            kill_tree(pid);
            log::warn!("kill_all: killed process tree for task {id} (pid={pid})");
        }
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

/// 校验 id 只含 `[A-Za-z0-9_-]`，防止把调用方可控输入当路径片段拼接。
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("id is empty".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("invalid id (only [A-Za-z0-9_-] allowed): {id}"));
    }
    Ok(())
}

/// 写入带 UTF-8 BOM 的 ps1 文件，权限限制为当前用户可访问。
async fn write_ps1_with_bom(path: &Path, content: &str) -> std::io::Result<()> {
    const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
    let mut bytes = Vec::with_capacity(BOM.len() + content.len());
    bytes.extend_from_slice(&BOM);
    bytes.extend_from_slice(content.as_bytes());
    tokio::fs::write(path, &bytes).await?;
    set_private_file(path);
    Ok(())
}

/// 纯累积逻辑：达到 `limit` 后不再写入 buffer，但仍记录后续还有数据到达（截断）。
/// 抽成独立结构体是为了脱离真实异步 IO 单测。
struct BoundedAccumulator {
    buf: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl BoundedAccumulator {
    fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if self.buf.len() < self.limit {
            let remaining = self.limit - self.buf.len();
            let take = remaining.min(chunk.len());
            self.buf.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                self.truncated = true;
            }
        } else {
            self.truncated = true;
        }
    }

    fn finish(self) -> (Vec<u8>, bool) {
        (self.buf, self.truncated)
    }
}

/// 有界读取一个异步流：达到 `limit` 后继续排空管道（防止子进程写阻塞），
/// 但不再往累积 buffer 里塞字节，最终返回 `(截至上限的字节, 是否发生截断)`。
async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R, limit: usize) -> (Vec<u8>, bool) {
    let mut acc = BoundedAccumulator::new(limit);
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => acc.push(&chunk[..n]),
            Err(_) => break,
        }
    }
    acc.finish()
}

/// 超时分支里用的异步杀树封装：taskkill/kill 是阻塞调用，丢进 `spawn_blocking`
/// 避免占住 tokio 工作线程。
async fn kill_tree_async(pid: u32) {
    let _ = tokio::task::spawn_blocking(move || kill_tree(pid)).await;
}

#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .output();
}

#[cfg(unix)]
fn kill_tree(pid: u32) {
    // 未引入 libc 依赖：借系统 kill(1) 发 SIGKILL。第一期不追求进程组级收敛，
    // 足以覆盖「脚本本身失控」的排查场景；更强的子孙进程收敛留给未来迭代。
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
}

#[cfg(not(any(windows, unix)))]
fn kill_tree(_pid: u32) {}

#[cfg(unix)]
fn set_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_private_dir(_path: &Path) {
    // Windows 上默认继承父目录 ACL（当前用户）即可，第一期不额外收紧 ACL。
}

#[cfg(unix)]
fn set_private_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_private_file(_path: &Path) {}

/// 清扫 `<root>/remote-exec/tmp/` 下的遗留目录（进程崩溃后残留）。
pub fn cleanup_stale_tmp(root: &Path) {
    let tmp_root = root.join("remote-exec").join("tmp");
    let entries = match std::fs::read_dir(&tmp_root) {
        Ok(e) => e,
        Err(_) => return, // 目录不存在是正常情况（首次启动）
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                log::warn!("cleanup_stale_tmp: failed to remove {path:?}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_id_accepts_normal() {
        assert!(validate_id("server-uuid-123_abc").is_ok());
    }

    #[test]
    fn validate_id_rejects_path_traversal() {
        assert!(validate_id("../etc/passwd").is_err());
        assert!(validate_id("..\\..\\windows").is_err());
        assert!(validate_id("a/b").is_err());
        assert!(validate_id("a\\b").is_err());
        assert!(validate_id("").is_err());
        assert!(validate_id("with space").is_err());
        assert!(validate_id("C:\\abs").is_err());
    }

    #[test]
    fn bounded_accumulator_no_truncate_under_limit() {
        let mut acc = BoundedAccumulator::new(10);
        acc.push(b"hello");
        let (buf, truncated) = acc.finish();
        assert_eq!(buf, b"hello");
        assert!(!truncated);
    }

    #[test]
    fn bounded_accumulator_truncates_at_limit_across_chunks() {
        let mut acc = BoundedAccumulator::new(5);
        acc.push(b"hel"); // 3 bytes, under limit
        acc.push(b"lo world"); // crosses the limit mid-chunk
        acc.push(b"more"); // arrives after limit already hit
        let (buf, truncated) = acc.finish();
        assert_eq!(buf, b"hello");
        assert_eq!(buf.len(), 5);
        assert!(truncated);
    }

    #[test]
    fn bounded_accumulator_exact_limit_no_truncate() {
        let mut acc = BoundedAccumulator::new(5);
        acc.push(b"hello");
        let (buf, truncated) = acc.finish();
        assert_eq!(buf, b"hello");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn read_bounded_truncates_from_async_reader() {
        let data = b"0123456789".to_vec();
        let cursor = std::io::Cursor::new(data);
        let (buf, truncated) = read_bounded(cursor, 4).await;
        assert_eq!(buf, b"0123");
        assert!(truncated);
    }

    #[tokio::test]
    async fn read_bounded_no_truncate_when_under_limit() {
        let data = b"short".to_vec();
        let cursor = std::io::Cursor::new(data);
        let (buf, truncated) = read_bounded(cursor, 1024).await;
        assert_eq!(buf, b"short");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn executor_new_cleans_stale_tmp_and_creates_dirs() {
        let dir = std::env::temp_dir().join(format!("worker-core-test-{}", uuid_like()));
        let root = dir.as_path();
        // 预置一个「遗留」临时任务目录
        let stale = root.join("remote-exec").join("tmp").join("stale-task");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("user.ps1"), "leftover").unwrap();

        let executor = Executor::new(root).expect("Executor::new should succeed");
        assert!(!stale.exists(), "stale tmp dir should be cleaned on new()");
        // running 表应为空
        assert!(executor.running.lock().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    /// 无 uuid 依赖，凑一个够用的临时目录名后缀。
    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{nanos}")
    }

    /// 依赖真实 PowerShell，只在 Windows 上跑，且默认忽略（避免拖慢常规 `cargo test`）。
    #[cfg(windows)]
    #[tokio::test]
    #[ignore]
    async fn run_executes_simple_script() {
        let dir = std::env::temp_dir().join(format!("worker-core-exec-test-{}", uuid_like()));
        let executor = Executor::new(&dir).unwrap();
        let req = ExecRequest {
            id: "test-id-1".into(),
            operator: "tester".into(),
            shell: proto::SHELL_POWERSHELL.into(),
            script: "Write-Output 'hello'".into(),
            args: vec![],
            cwd: None,
            env: vec![],
            timeout_secs: 10,
            stdout_limit_bytes: 1024,
            stderr_limit_bytes: 1024,
        };
        let resp = executor.run(&req).await;
        assert_eq!(resp.state, ExecState::Completed);
        assert_eq!(resp.exit_code, Some(0));
        assert!(resp.stdout.contains("hello"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore]
    async fn run_times_out_and_kills_process() {
        let dir = std::env::temp_dir().join(format!("worker-core-exec-test-{}", uuid_like()));
        let executor = Executor::new(&dir).unwrap();
        let req = ExecRequest {
            id: "test-id-2".into(),
            operator: "tester".into(),
            shell: proto::SHELL_POWERSHELL.into(),
            script: "Start-Sleep -Seconds 999".into(),
            args: vec![],
            cwd: None,
            env: vec![],
            timeout_secs: 2,
            stdout_limit_bytes: 1024,
            stderr_limit_bytes: 1024,
        };
        let resp = executor.run(&req).await;
        assert_eq!(resp.state, ExecState::TimedOut);
        assert_eq!(resp.exit_code, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
