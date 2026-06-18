//! 驱动外部编码 Agent 会话：构造 CLI 命令 + spawn 子进程发一轮消息。
//!
//! 设计要点（见 plan.md §3 / 任务硬约束）：
//! - **命令构造是纯函数**（[`codex_argv`] / [`claude_argv`]），可脱离真进程单测。
//! - **输出解析是纯函数**（[`parse_codex_stdout`] / [`parse_claude_stdout`]），用 fixture 单测。
//! - 真正 spawn 的 [`send`] 只是把以上两步接到 `tokio::process::Command`，**不在单测里跑**
//!   （会真实消耗 codex / claude 额度）。
//!
//! 固化命令形态（已于 2026-06-15 真机实跑核实，见 runbook §5）：
//! - Codex：`codex exec -s workspace-write -c approval_policy="never" --cd <repo> resume --json <id> <prompt>`
//!   stdout 是事件 JSONL：`thread.started` / `turn.started` / `item.completed`{item} / `turn.completed`{usage}。
//!   回复 = 末个 `item.completed` 且 `item.type=="agent_message"` 的 `item.text`。
//!   ⚠️ Windows 下 stdout 可能混入非 UTF-8（GBK）噪声行（如 taskkill 的「成功: 已终止 PID…」）——
//!   `run_capture` 用 `from_utf8_lossy` 兜底，噪声行变替换字符后作为非 JSON 行跳过。
//! - Claude：`claude -p <prompt> --resume <id> --permission-mode acceptEdits`，
//!   `Command::current_dir(cwd)`，stdout 即纯文本回复（实测干净 UTF-8）。

use crate::{Provider, SessionRef, TurnResult};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// stdout 末段保留长度（排障用），避免把超长输出整段塞进 TurnResult。
const RAW_TAIL_MAX: usize = 4096;

/// 单轮 CLI 执行硬超时：codex / claude 网络卡死、等交互、子进程不退出时兜底，
/// 防止任务永久 running。超时后 kill 子进程并返回基础设施错（→ 任务 failed）。
/// 取较宽松值（含 agent 真实改文件的耗时），仅防真正的 hang。
const TURN_TIMEOUT: Duration = Duration::from_secs(1200);

/// 构造 Codex `exec resume` 的 argv（不含 `codex` 程序名本身）。
///
/// `repo_root` 用于 `--cd`：Codex `exec resume` 虽不挑当前目录，但传入工作树根更稳妥。
pub fn codex_argv(repo_root: &Path, session_id: &str, prompt: &str) -> Vec<String> {
    vec![
        "exec".to_string(),
        "-s".to_string(),
        "workspace-write".to_string(),
        "-c".to_string(),
        "approval_policy=\"never\"".to_string(),
        "--cd".to_string(),
        repo_root.to_string_lossy().to_string(),
        "resume".to_string(),
        "--json".to_string(),
        session_id.to_string(),
        prompt.to_string(),
    ]
}

/// 构造 Codex `exec`（**新建会话**，不带 `resume`）的 argv（不含 `codex` 程序名本身）。
///
/// 与 [`codex_argv`] 同形但省去 `resume <id>`：codex 会创建一个新会话，其 id 由首个
/// `thread.started` 事件的 `thread_id` 给出（见 [`parse_codex_thread_id`]）。
pub fn codex_create_argv(repo_root: &Path, prompt: &str) -> Vec<String> {
    vec![
        "exec".to_string(),
        "-s".to_string(),
        "workspace-write".to_string(),
        "-c".to_string(),
        "approval_policy=\"never\"".to_string(),
        "--cd".to_string(),
        repo_root.to_string_lossy().to_string(),
        "--json".to_string(),
        prompt.to_string(),
    ]
}

/// 从 codex `exec --json` stdout 解析新建会话 id：首个 `thread.started` 事件的 `thread_id`。
pub fn parse_codex_thread_id(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("thread.started") {
            if let Some(id) = v.get("thread_id").and_then(Value::as_str) {
                if !id.is_empty() {
                    return Some(id.to_string());
                }
            }
        }
    }
    None
}

/// 构造 Claude `-p --resume` 的 argv（不含 `claude` 程序名本身）。
///
/// 注意：Claude 必须在会话原始 cwd 下 spawn（由调用方 `current_dir` 设置），argv 不含目录。
pub fn claude_argv(session_id: &str, prompt: &str) -> Vec<String> {
    vec![
        "-p".to_string(),
        prompt.to_string(),
        "--resume".to_string(),
        session_id.to_string(),
        "--permission-mode".to_string(),
        "acceptEdits".to_string(),
    ]
}

/// 解析 Codex `exec --json` 的 stdout（事件 JSONL）。
///
/// 优先按真机固化 schema：取末个 `item.completed` 且 `item.type=="agent_message"` 的 `item.text`。
/// 退化兼容旧/rollout 形态：`task_complete.last_agent_message`、`agent_message.message`。
/// 都没有时回复为空（调用方据 raw_tail 排障）。坏行 / 非 UTF-8 噪声行（lossy 后非 JSON）跳过。
pub fn parse_codex_stdout(stdout: &str) -> TurnResult {
    let mut last_item: Option<String> = None;
    let mut last_complete: Option<String> = None;
    let mut last_agent: Option<String> = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        // 真机固化 schema：codex exec --json 顶层事件 item.completed{item}。
        if v.get("type").and_then(Value::as_str) == Some("item.completed") {
            if let Some(item) = v.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                    if let Some(t) = item.get("text").and_then(Value::as_str) {
                        last_item = Some(t.trim().to_string());
                    }
                }
            }
            continue;
        }
        // 退化：rollout / 旧形态（type 可能平铺顶层或在 payload 下）。
        let payload = v.get("payload").unwrap_or(&v);
        match payload.get("type").and_then(Value::as_str) {
            Some("task_complete") => {
                if let Some(m) = payload.get("last_agent_message").and_then(Value::as_str) {
                    last_complete = Some(m.trim().to_string());
                }
            }
            Some("agent_message") => {
                if let Some(m) = payload.get("message").and_then(Value::as_str) {
                    last_agent = Some(m.trim().to_string());
                }
            }
            _ => {}
        }
    }
    let reply_text = last_item
        .or(last_complete)
        .or(last_agent)
        .unwrap_or_default();
    TurnResult {
        reply_text,
        raw_tail: tail(stdout),
    }
}

/// 解析 Claude `-p` 的 stdout（纯文本回复）。
pub fn parse_claude_stdout(stdout: &str) -> TurnResult {
    TurnResult {
        reply_text: stdout.trim().to_string(),
        raw_tail: tail(stdout),
    }
}

fn tail(s: &str) -> String {
    if s.len() <= RAW_TAIL_MAX {
        return s.to_string();
    }
    // 退到最近的 char 边界，避免切断多字节字符。
    let mut start = s.len() - RAW_TAIL_MAX;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…(truncated){}", &s[start..])
}

/// 发一轮消息到指定会话，阻塞至本轮完成，返回解析后的回复。
///
/// **会真实调用 codex / claude CLI 并消耗额度**——不要在单测中调用。
pub async fn send(s: &SessionRef, prompt: &str) -> Result<TurnResult> {
    let turn = match s.provider {
        Provider::Codex => {
            let argv = codex_argv(&s.cwd, &s.session_id, prompt);
            let stdout = run_capture("codex", &argv, None).await?;
            parse_codex_stdout(&stdout)
        }
        Provider::Claude => {
            let argv = claude_argv(&s.session_id, prompt);
            // Claude resume 必须在会话原始 cwd 下执行。
            let stdout = run_capture("claude", &argv, Some(&s.cwd)).await?;
            parse_claude_stdout(&stdout)
        }
    };
    // 空回复 = 显式错误，不静默返回空串让上层当正常回复继续空转。
    // 子进程退出 0 却没解析出任何已知 schema 的回复文本，最可能是 CLI 版本变更导致
    // 输出格式不兼容（见 docs/codeloop-cli-resilience-design.md §3）。带 raw_tail 末段
    // 冒泡到上层 → 循环 emit error → 前端直接显示原始片段，便于判断契约是否破裂。
    if turn.reply_text.trim().is_empty() {
        return Err(anyhow!(
            "{} CLI 退出 0 但未解析出任何回复文本（可能 CLI 版本变更导致 stdout 格式不兼容）。原始输出末段：\n{}",
            s.provider.as_str(),
            turn.raw_tail.trim()
        ));
    }
    Ok(turn)
}

/// 新建一个 Codex 会话：在 `cwd` 下跑一轮 `codex exec`（无 resume），返回新会话 id。
///
/// **会真实调用 codex CLI 并消耗额度**——不要在单测中调用。`prompt` 是建立会话的种子提示词。
pub async fn create_codex_session(cwd: &Path, prompt: &str) -> Result<String> {
    let argv = codex_create_argv(cwd, prompt);
    let stdout = run_capture("codex", &argv, Some(cwd)).await?;
    parse_codex_thread_id(&stdout).ok_or_else(|| {
        anyhow!(
            "codex 新建会话未返回 thread_id；stdout 末段：{}",
            tail(&stdout)
        )
    })
}

/// 探针：取 CLI 版本号（`<program> --version`）。验证命令在 PATH、Windows npm shim 解析、
/// 可正常 spawn（即 C4/C5）。返回 stdout 首行（去空白）。
pub async fn probe_version(program: &str) -> Result<String> {
    let argv = vec!["--version".to_string()];
    let stdout = run_capture(program, &argv, None).await?;
    Ok(stdout.lines().next().unwrap_or("").trim().to_string())
}

/// 构造 Codex 合成探针的 argv：`codex exec --json -s read-only`（新建会话，只读沙箱）。
///
/// 与 [`codex_create_argv`] 同形但沙箱降为 `read-only`——探针只验链路，不应改文件。
pub fn codex_probe_argv(repo_root: &Path, prompt: &str) -> Vec<String> {
    vec![
        "exec".to_string(),
        "-s".to_string(),
        "read-only".to_string(),
        "-c".to_string(),
        "approval_policy=\"never\"".to_string(),
        "--cd".to_string(),
        repo_root.to_string_lossy().to_string(),
        "--json".to_string(),
        prompt.to_string(),
    ]
}

/// 构造 Claude 合成探针的 argv：`claude -p <prompt> --permission-mode plan`（**不带 resume**，
/// 新建临时会话；plan 为只读权限模式，探针不应改文件）。
pub fn claude_probe_argv(prompt: &str) -> Vec<String> {
    vec![
        "-p".to_string(),
        prompt.to_string(),
        "--permission-mode".to_string(),
        "plan".to_string(),
    ]
}

/// 实发合成探针（Codex 端，**新建一次性会话 + 只读沙箱**，不污染真实会话）。
///
/// 用 [`codex_probe_argv`] 跑一轮 `prompt`，返回解析出的回复文本。覆盖新建路径 argv +
/// `item.completed` 输出解析（C3/O3/O1），**不**覆盖 resume 路径 C1。**会真实调用 codex CLI 并消耗额度。**
pub async fn probe_codex_synthetic(cwd: &Path, prompt: &str) -> Result<String> {
    let argv = codex_probe_argv(cwd, prompt);
    let stdout = run_capture("codex", &argv, Some(cwd)).await?;
    Ok(parse_codex_stdout(&stdout).reply_text)
}

/// 实发合成探针（Claude 端，**不带 `--resume` 的一次性会话 + plan 只读权限**，不污染真实会话）。
///
/// 用 `claude -p <prompt> --permission-mode plan` 跑一轮，返回纯文本回复。覆盖新建调用 + 纯文本
/// 输出解析（O4），**不**覆盖 resume argv C2 与原 cwd 约束 C6。**会真实调用 claude CLI 并消耗额度。**
pub async fn probe_claude_synthetic(cwd: &Path, prompt: &str) -> Result<String> {
    let argv = claude_probe_argv(prompt);
    let stdout = run_capture("claude", &argv, Some(cwd)).await?;
    Ok(parse_claude_stdout(&stdout).reply_text)
}

/// 起子进程并捕获 stdout；非零退出码视为基础设施错误（`Err`）。
///
/// 带 [`TURN_TIMEOUT`] 硬超时：`stdin` 接 null（CLI 误等交互时立即 EOF），`kill_on_drop`
/// 保证超时丢弃 future 时子进程被杀，避免僵尸进程 / 任务永久 running。
async fn run_capture(program: &str, argv: &[String], cwd: Option<&Path>) -> Result<String> {
    let (resolved, prefix) = resolve_program(program);
    let mut cmd = tokio::process::Command::new(&resolved);
    cmd.args(&prefix)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    log::info!(
        "[driver] spawn {program}（resolved={:?}, prefix={} 项, argv={} 项, cwd={:?}）",
        resolved,
        prefix.len(),
        argv.len(),
        cwd,
    );
    let started = std::time::Instant::now();
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {program} 失败（CLI 是否已安装并在 PATH 中？）"))?;
    let output = match tokio::time::timeout(TURN_TIMEOUT, child.wait_with_output()).await {
        Ok(r) => r.with_context(|| format!("等待 {program} 子进程退出失败"))?,
        // 超时：child future 在此被丢弃，kill_on_drop 触发子进程终止。
        Err(_) => {
            log::warn!("[driver] {program} 单轮执行超时（{TURN_TIMEOUT:?}），已终止子进程",);
            return Err(anyhow!(
                "{program} 单轮执行超时（{TURN_TIMEOUT:?}），已终止子进程",
            ));
        }
    };
    let elapsed = started.elapsed();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!(
            "[driver] {program} 退出码 {:?}（耗时 {elapsed:?}）：{}",
            output.status.code(),
            stderr.trim(),
        );
        return Err(anyhow!(
            "{program} 退出码 {:?}：{}",
            output.status.code(),
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    log::info!(
        "[driver] {program} 正常退出（耗时 {elapsed:?}, stdout {} 字节）",
        stdout.len(),
    );
    Ok(stdout)
}

/// 解析裸命令名到实际可执行文件 + 需前置的参数。
///
/// 返回 `(program, prefix_args)`：`Command::new(program)` 后先 `args(prefix_args)` 再接真正 argv。
///
/// Windows 专属问题：npm 全局安装的 CLI（如 `codex`）落地为 `codex.cmd` / `codex.ps1` 垫片，
/// 而 `CreateProcessW` 只会给裸名自动补 `.exe`，故 `Command::new("codex")` 报「program not
/// found」。补扩展名指到 `codex.cmd` 后又撞上 Rust 的 CVE-2024-24576 防护：含换行的参数
/// （我们的多行 prompt）无法安全传给 `.cmd`（经 cmd.exe），报「batch file arguments are invalid」。
///
/// 解法：识别 npm cmd-shim（`codex.cmd` 内 `node "<…>\node_modules\…\xxx.js" %*`），**绕开
/// cmd.exe 直接 `node <脚本.js> <argv>`**——`node.exe` 是真 PE，多行参数照常透传。
///
/// 非 Windows、或命令名已带路径分隔符/扩展名、或解析不到 → 原样返回（`prefix_args` 为空），
/// 保留有意义的报错。
fn resolve_program(program: &str) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
    #[cfg(windows)]
    {
        // 已含路径分隔符或扩展名 → 调用方已给定，不再解析。
        if !program.contains(['\\', '/', '.']) {
            let pathext =
                std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
            let exts: Vec<String> = pathext.split(';').map(|e| e.trim().to_string()).collect();
            if let Some(paths) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&paths) {
                    for ext in &exts {
                        let candidate = dir.join(format!("{program}{ext}"));
                        if !candidate.is_file() {
                            continue;
                        }
                        // npm 的 .cmd/.bat 垫片 → 改为直接 node 跑底层 .js，绕开 cmd.exe。
                        if matches!(ext.to_ascii_uppercase().as_str(), ".CMD" | ".BAT") {
                            if let Some(node_call) = npm_shim_to_node(&candidate) {
                                return node_call;
                            }
                        }
                        return (candidate.into_os_string(), Vec::new());
                    }
                }
            }
        }
    }
    (program.into(), Vec::new())
}

/// 解析 npm cmd-shim，抽出其调用的 `node_modules\…\*.js` 脚本，返回 `("node", [脚本路径])`。
/// 非 npm shim / 脚本不存在 → `None`（调用方回退原 .cmd）。
#[cfg(windows)]
fn npm_shim_to_node(shim: &Path) -> Option<(std::ffi::OsString, Vec<std::ffi::OsString>)> {
    let text = std::fs::read_to_string(shim).ok()?;
    // shim 内形如 `"%dp0%\node_modules\@scope\pkg\bin\cli.js"`；取 node_modules→脚本扩展名一段。
    let start = text.find("node_modules")?;
    let rest = &text[start..];
    let end = [".js", ".cjs", ".mjs"]
        .iter()
        .filter_map(|ext| rest.find(ext).map(|i| i + ext.len()))
        .min()?;
    let rel = &rest[..end]; // node_modules\…\cli.js（分隔符可能是 \ 或 /，PathBuf::join 均可）
    let script = shim.parent()?.join(rel);
    if !script.is_file() {
        return None;
    }
    Some(("node".into(), vec![script.into_os_string()]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// npm cmd-shim 应被解析为「node + 底层 .js 脚本」，绕开 cmd.exe。
    #[cfg(windows)]
    #[test]
    fn npm_shim_resolves_to_node_script() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("node_modules/@openai/codex/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let js = bin.join("codex.js");
        std::fs::write(&js, b"// stub").unwrap();
        let shim = dir.path().join("codex.cmd");
        std::fs::write(
            &shim,
            "@ECHO off\r\n... & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n",
        )
        .unwrap();

        let (prog, prefix) = npm_shim_to_node(&shim).expect("应识别为 npm shim");
        assert_eq!(prog, std::ffi::OsString::from("node"));
        assert_eq!(prefix.len(), 1);
        // 比规范化路径，规避 \ 与 / 的字面差异。
        assert_eq!(
            std::fs::canonicalize(PathBuf::from(&prefix[0])).unwrap(),
            std::fs::canonicalize(&js).unwrap(),
        );
    }

    /// 脚本文件不存在时不应误判为 shim（回退原 .cmd）。
    #[cfg(windows)]
    #[test]
    fn npm_shim_missing_script_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("codex.cmd");
        std::fs::write(&shim, "\"%dp0%\\node_modules\\x\\bin\\cli.js\" %*").unwrap();
        assert!(npm_shim_to_node(&shim).is_none());
    }

    #[test]
    fn codex_argv_shape() {
        let argv = codex_argv(
            &PathBuf::from("D:/git/repo"),
            "11111111-aaaa",
            "请复核 docs/plan.md",
        );
        assert_eq!(
            argv,
            vec![
                "exec",
                "-s",
                "workspace-write",
                "-c",
                "approval_policy=\"never\"",
                "--cd",
                "D:/git/repo",
                "resume",
                "--json",
                "11111111-aaaa",
                "请复核 docs/plan.md",
            ]
        );
    }

    #[test]
    fn claude_argv_shape() {
        let argv = claude_argv("sess-1", "据意见修订");
        assert_eq!(
            argv,
            vec![
                "-p",
                "据意见修订",
                "--resume",
                "sess-1",
                "--permission-mode",
                "acceptEdits",
            ]
        );
    }

    #[test]
    fn codex_create_argv_has_no_resume() {
        let argv = codex_create_argv(&PathBuf::from("D:/git/repo"), "建立会话");
        assert_eq!(
            argv,
            vec![
                "exec",
                "-s",
                "workspace-write",
                "-c",
                "approval_policy=\"never\"",
                "--cd",
                "D:/git/repo",
                "--json",
                "建立会话",
            ]
        );
        assert!(!argv.iter().any(|a| a == "resume"));
    }

    /// 合成探针（Codex）：新建会话 + 只读沙箱，绝不带 resume，绝不 workspace-write。
    #[test]
    fn codex_probe_argv_is_readonly_and_no_resume() {
        let argv = codex_probe_argv(&PathBuf::from("D:/git/repo"), "只回复 PROBE_OK");
        assert_eq!(
            argv,
            vec![
                "exec",
                "-s",
                "read-only",
                "-c",
                "approval_policy=\"never\"",
                "--cd",
                "D:/git/repo",
                "--json",
                "只回复 PROBE_OK",
            ]
        );
        assert!(!argv.iter().any(|a| a == "resume"));
        assert!(!argv.iter().any(|a| a == "workspace-write"));
    }

    /// 合成探针（Claude）：不带 --resume（新建临时会话）+ plan 只读权限，绝不 acceptEdits。
    #[test]
    fn claude_probe_argv_is_plan_and_no_resume() {
        let argv = claude_probe_argv("只回复 PROBE_OK");
        assert_eq!(
            argv,
            vec!["-p", "只回复 PROBE_OK", "--permission-mode", "plan"]
        );
        assert!(!argv.iter().any(|a| a == "--resume"));
        assert!(!argv.iter().any(|a| a == "acceptEdits"));
    }

    #[test]
    fn parse_codex_thread_id_from_started_event() {
        let stdout = concat!(
            r#"{"type":"thread.started","thread_id":"019eca83-aaaa"}"#,
            "\n",
            r#"{"type":"turn.started"}"#,
            "\n",
        );
        assert_eq!(
            parse_codex_thread_id(stdout),
            Some("019eca83-aaaa".to_string())
        );
    }

    #[test]
    fn parse_codex_thread_id_none_when_absent() {
        let stdout = r#"{"type":"turn.started"}"#;
        assert_eq!(parse_codex_thread_id(stdout), None);
    }

    #[test]
    fn parse_codex_real_json_schema() {
        // 2026-06-15 真机 `codex exec --json` 实测序列（含一行 lossy 后的非 JSON 噪声）。
        let stdout = concat!(
            r#"{"type":"thread.started","thread_id":"019eca83"}"#,
            "\n",
            r#"{"type":"turn.started"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"OK\nVERDICT: PASS"}}"#,
            "\n",
            "\u{fffd}\u{fffd}\u{fffd}: \u{fffd}\u{fffd}ֹ PID 14008\n", // GBK 噪声 lossy 后
            r#"{"type":"turn.completed","usage":{"output_tokens":3187}}"#,
            "\n",
        );
        let r = parse_codex_stdout(stdout);
        assert_eq!(r.reply_text, "OK\nVERDICT: PASS");
    }

    #[test]
    fn parse_codex_item_completed_wins_over_legacy() {
        // 同时出现新旧两种形态时，以新 schema 的 item.completed 为准。
        let stdout = concat!(
            r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"旧形态"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"新形态"}}"#,
            "\n",
        );
        assert_eq!(parse_codex_stdout(stdout).reply_text, "新形态");
    }

    #[test]
    fn parse_codex_prefers_task_complete() {
        let stdout = concat!(
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"中间回复"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"最终回复\nVERDICT: PASS"}}"#,
            "\n",
        );
        let r = parse_codex_stdout(stdout);
        assert_eq!(r.reply_text, "最终回复\nVERDICT: PASS");
    }

    #[test]
    fn parse_codex_falls_back_to_agent_message() {
        let stdout = r#"{"type":"event_msg","payload":{"type":"agent_message","message":"只有 agent_message"}}"#;
        let r = parse_codex_stdout(stdout);
        assert_eq!(r.reply_text, "只有 agent_message");
    }

    #[test]
    fn parse_codex_skips_bad_lines_and_empty() {
        let stdout = concat!(
            "这是坏行\n",
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":" 带空白 "}}"#,
        );
        let r = parse_codex_stdout(stdout);
        assert_eq!(r.reply_text, "带空白");
    }

    #[test]
    fn parse_codex_handles_top_level_payload_shape() {
        // 某些版本把 type 平铺在顶层而非 payload。
        let stdout = r#"{"type":"task_complete","last_agent_message":"平铺形态"}"#;
        let r = parse_codex_stdout(stdout);
        assert_eq!(r.reply_text, "平铺形态");
    }

    #[test]
    fn parse_codex_empty_when_no_reply() {
        let r = parse_codex_stdout(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#);
        assert!(r.reply_text.is_empty());
    }

    #[test]
    fn parse_claude_trims_text() {
        let r = parse_claude_stdout("  已完成修订。\nVERDICT 不在这里  \n");
        assert_eq!(r.reply_text, "已完成修订。\nVERDICT 不在这里");
    }

    #[test]
    fn raw_tail_truncates_long_output() {
        let big = "a".repeat(RAW_TAIL_MAX + 100);
        let r = parse_claude_stdout(&big);
        assert!(r.raw_tail.starts_with("…(truncated)"));
        assert!(r.raw_tail.len() < big.len());
    }
}
