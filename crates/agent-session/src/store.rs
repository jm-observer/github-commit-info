//! 只读解析 Codex / Claude Code 的本机磁盘会话存储（移植自 `probe_sessions.py`）。
//!
//! 存储布局：
//!
//! - Codex 清单：`<home>/.codex/session_index.jsonl`（`{id, thread_name, updated_at}`）。
//! - Codex 事件流：`<home>/.codex/sessions/<年>/<月>/<日>/rollout-*-<id>.jsonl`；归档目录 `<home>/.codex/archived_sessions/` 同样递归扫描。
//! - Claude 事件流：`<home>/.claude/projects/<编码 cwd>/<sessionId>.jsonl`。
//!
//! 所有 JSONL 逐行解析，坏行跳过（内部格式会漂移，必须容错）。

use crate::{
    MessagesPage, Provider, SessionMessage, SessionSnapshot, SessionStatus, SessionSummary,
};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 会话存储根：默认指向用户 home，下挂 `.codex` / `.claude`。
/// 测试通过 [`Store::with_home`] 注入 fixture 目录，避免读真实用户数据。
#[derive(Debug, Clone)]
pub struct Store {
    home: PathBuf,
}

impl Store {
    /// 用显式 home 根构造（测试 / 自定义部署）。
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// 从环境定位 home（`HOME`，Windows 回退 `USERPROFILE`）。
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .context("HOME / USERPROFILE 均未设置，无法定位会话存储")?;
        Ok(Self::with_home(PathBuf::from(home)))
    }

    fn codex_index(&self) -> PathBuf {
        self.home.join(".codex").join("session_index.jsonl")
    }

    fn codex_sessions_root(&self) -> PathBuf {
        self.home.join(".codex").join("sessions")
    }

    fn codex_archived_root(&self) -> PathBuf {
        self.home.join(".codex").join("archived_sessions")
    }

    fn claude_projects_root(&self) -> PathBuf {
        self.home.join(".claude").join("projects")
    }

    // ---------------- 公开 API ----------------

    /// 列出两边会话清单（id/title/status/updated_at），按 updated_at 倒序，每边各取 `limit` 条。
    pub fn list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let mut out = self.codex_list(limit)?;
        out.extend(self.claude_list(limit)?);
        Ok(out)
    }

    /// 按 id 全盘定位会话事件文件。
    pub fn locate(&self, provider: Provider, session_id: &str) -> Result<Option<PathBuf>> {
        match provider {
            Provider::Codex => self.codex_locate(session_id),
            Provider::Claude => self.claude_locate(session_id),
        }
    }

    /// 解析事件文件为 JSON 行（坏行跳过）。
    pub fn parse_events(path: &Path) -> Result<Vec<Value>> {
        read_jsonl(path)
    }

    /// 单会话快照：状态 + 最近回复 + cwd + 标题。
    pub fn snapshot(&self, provider: Provider, session_id: &str) -> Result<SessionSnapshot> {
        let path = self
            .locate(provider, session_id)?
            .ok_or_else(|| anyhow!("未找到 {} 会话: {session_id}", provider.as_str()))?;
        let events = read_jsonl(&path)?;
        let (status, latest_reply, cwd, title, updated_at) = match provider {
            Provider::Codex => (
                codex_status(&events),
                codex_latest_reply(&events),
                codex_cwd(&events),
                codex_title(&events, session_id),
                codex_updated_at(&events),
            ),
            Provider::Claude => (
                claude_status(&events),
                claude_latest_reply(&events),
                claude_cwd(&events),
                claude_title(&events),
                claude_updated_at(&events),
            ),
        };
        Ok(SessionSnapshot {
            provider,
            id: session_id.to_string(),
            title,
            status,
            latest_reply,
            updated_at,
            cwd,
        })
    }

    /// 增量取消息：跳过前 `after` 行（已读行数游标），返回新增消息 + 新游标。
    pub fn messages(
        &self,
        provider: Provider,
        session_id: &str,
        after: usize,
    ) -> Result<MessagesPage> {
        let path = self
            .locate(provider, session_id)?
            .ok_or_else(|| anyhow!("未找到 {} 会话: {session_id}", provider.as_str()))?;
        let lines = read_lines(&path)?;
        let total = lines.len();
        let start = after.min(total);
        let slice = &lines[start..];
        // Codex 同一轮把 user/assistant 同时写成 `response_item` 与 `event_msg`（双写）。
        // 择一权威源：行片含 `response_item/message`(role=user|assistant) → 用新源
        // `codex_response_item_to_msg`（逐轮完整、含 resume 用户轮）；否则回退旧源 `event_msg`。
        // 同一轮的两族事件总在同一增量页成对出现 → 不会重复、不会割裂（见设计 §4/§5.1）。
        let codex_new_source = provider == Provider::Codex && codex_slice_uses_response_item(slice);
        let mut messages = Vec::new();
        for raw in slice {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue; // 坏行跳过
            };
            let msg = match provider {
                Provider::Codex if codex_new_source => codex_response_item_to_msg(&value),
                Provider::Codex => codex_message_of(&value),
                Provider::Claude => claude_message_of(&value),
            };
            if let Some(m) = msg {
                messages.push(m);
            }
        }
        Ok(MessagesPage {
            messages,
            cursor: total,
        })
    }

    // ---------------- Codex ----------------

    fn codex_locate(&self, session_id: &str) -> Result<Option<PathBuf>> {
        for root in [self.codex_sessions_root(), self.codex_archived_root()] {
            if let Some(hit) = find_jsonl_containing(&root, session_id)? {
                return Ok(Some(hit));
            }
        }
        Ok(None)
    }

    /// 仅在 active 目录里定位 rollout jsonl；已归档（搬到 archived_sessions/）的 → None。
    /// 用于 codex_list 过滤——session_index.jsonl 不区分活/归档，需用文件系统位置判定。
    fn codex_locate_active(&self, session_id: &str) -> Result<Option<PathBuf>> {
        find_jsonl_containing(&self.codex_sessions_root(), session_id)
    }

    fn codex_list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let mut rows = read_jsonl(&self.codex_index())?;
        // 按 updated_at 倒序。
        rows.sort_by_key(|r| std::cmp::Reverse(str_field(r, "updated_at")));
        // session_index.jsonl 保留已归档（`codex archive`）的条目；rollout jsonl 已被搬到
        // ~/.codex/archived_sessions/。这些 session 不适合再驱动新一轮交互——下拉列表里
        // 直接过滤掉，不消耗 limit 配额。
        let mut out = Vec::new();
        for row in rows.into_iter() {
            if out.len() >= limit {
                break;
            }
            let id = str_field(&row, "id");
            // 只在 active 目录里查；查不到即视为已归档（或文件已删），跳过。
            let Some(path) = self.codex_locate_active(&id)? else {
                continue;
            };
            let events = read_jsonl(&path)?;
            out.push(SessionSummary {
                provider: Provider::Codex,
                id,
                title: str_field(&row, "thread_name"),
                preview: codex_first_user(&events),
                cwd: codex_cwd(&events).to_string_lossy().into_owned(),
                status: codex_status(&events),
                updated_at: str_field(&row, "updated_at"),
            });
        }
        Ok(out)
    }

    // ---------------- Claude ----------------

    fn claude_locate(&self, session_id: &str) -> Result<Option<PathBuf>> {
        let root = self.claude_projects_root();
        let target = format!("{session_id}.jsonl");
        for project in read_dir_sorted(&root)? {
            if !project.is_dir() {
                continue;
            }
            let candidate = project.join(&target);
            if candidate.is_file() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    fn claude_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let root = self.claude_projects_root();
        for project in read_dir_sorted(&root)? {
            if !project.is_dir() {
                continue;
            }
            for entry in read_dir_sorted(&project)? {
                if entry.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(entry);
                }
            }
        }
        Ok(files)
    }

    fn claude_list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let mut files = self.claude_files()?;
        // 按文件 mtime 倒序（与 probe 一致）。
        files.sort_by_key(|p| std::cmp::Reverse(file_mtime(p)));
        let mut out = Vec::new();
        for path in files.into_iter().take(limit) {
            let events = read_jsonl(&path)?;
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            out.push(SessionSummary {
                provider: Provider::Claude,
                id,
                title: claude_title(&events),
                preview: claude_first_user(&events),
                cwd: claude_cwd(&events).to_string_lossy().into_owned(),
                status: claude_status(&events),
                updated_at: claude_updated_at(&events),
            });
        }
        Ok(out)
    }
}

// ----------------------------- JSONL 读取 ----------------------------- //

fn read_lines(path: &Path) -> Result<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().map(|l| l.to_string()).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("读取 {}", path.display())),
    }
}

/// 逐行解析 JSONL，跳过坏行 / 空行。文件不存在视为空。
fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for line in read_lines(path)? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            out.push(v);
        }
    }
    Ok(out)
}

// ----------------------------- 文件系统辅助 ----------------------------- //

/// 列目录条目，缺失目录返回空，结果按路径排序（稳定）。
fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("读取目录 {}", dir.display())),
    };
    for entry in rd {
        out.push(
            entry
                .with_context(|| format!("遍历目录 {}", dir.display()))?
                .path(),
        );
    }
    out.sort();
    Ok(out)
}

fn file_mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// 递归扫描 `root`，返回第一个文件名含 `session_id` 的 `.jsonl` 文件。
fn find_jsonl_containing(root: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    if !root.exists() {
        return Ok(None);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for path in read_dir_sorted(&dir)? {
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if name.ends_with(".jsonl") && name.contains(session_id) {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

// ----------------------------- 字段取值辅助 ----------------------------- //

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn clip(text: &str, n: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() <= n {
        normalized
    } else {
        let head: String = chars[..n.saturating_sub(1)].iter().collect();
        format!("{head}…")
    }
}

// ----------------------------- Codex 解析 ----------------------------- //

/// 状态：统计 `event_msg` 的 `task_started` / `task_complete`，有未闭合 turn 即 Generating。
fn codex_status(events: &[Value]) -> SessionStatus {
    if events.is_empty() {
        return SessionStatus::Unknown;
    }
    let mut started = 0i64;
    let mut completed = 0i64;
    for e in events {
        if str_field(e, "type") == "event_msg" {
            match payload_type(e).as_deref() {
                Some("task_started") => started += 1,
                Some("task_complete") => completed += 1,
                _ => {}
            }
        }
    }
    if started > completed {
        SessionStatus::Generating
    } else {
        SessionStatus::Idle
    }
}

fn payload_type(e: &Value) -> Option<String> {
    e.get("payload")
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

/// cwd：优先 `session_meta` 事件的 `payload.cwd`，回退 `turn_context.cwd`。
fn codex_cwd(events: &[Value]) -> PathBuf {
    for e in events {
        let p = e.get("payload");
        let ty = str_field(e, "type");
        if ty == "session_meta" {
            if let Some(cwd) = p.and_then(|p| p.get("cwd")).and_then(Value::as_str) {
                return PathBuf::from(cwd);
            }
        }
        if ty == "turn_context" {
            if let Some(cwd) = p.and_then(|p| p.get("cwd")).and_then(Value::as_str) {
                return PathBuf::from(cwd);
            }
        }
        // 某些 rollout 把 meta 平铺在顶层而非 payload，兜底再扫一遍。
        if let Some(cwd) = e
            .get("session_meta")
            .and_then(|m| m.get("cwd"))
            .and_then(Value::as_str)
        {
            return PathBuf::from(cwd);
        }
    }
    PathBuf::new()
}

fn codex_title(events: &[Value], session_id: &str) -> String {
    // rollout 内未必有标题，回退首条 user_message，再回退 session_id。
    for e in events {
        if str_field(e, "type") == "event_msg" && payload_type(e).as_deref() == Some("user_message")
        {
            let msg = e
                .get("payload")
                .and_then(|p| p.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !msg.is_empty() {
                return clip(msg, 50);
            }
        }
    }
    session_id.to_string()
}

/// 首条用户消息（codex `user_message` 事件正文）的前若干字符；无则空串。
fn codex_first_user(events: &[Value]) -> String {
    for e in events {
        if str_field(e, "type") == "event_msg" && payload_type(e).as_deref() == Some("user_message")
        {
            let msg = e
                .get("payload")
                .and_then(|p| p.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !msg.is_empty() {
                return clip(msg, 40);
            }
        }
    }
    String::new()
}

fn codex_updated_at(events: &[Value]) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| e.get("timestamp").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

/// 最近回复：优先末个 `task_complete.last_agent_message`，回退末个 `agent_message`。
fn codex_latest_reply(events: &[Value]) -> String {
    for e in events.iter().rev() {
        if str_field(e, "type") == "event_msg"
            && payload_type(e).as_deref() == Some("task_complete")
        {
            if let Some(msg) = e
                .get("payload")
                .and_then(|p| p.get("last_agent_message"))
                .and_then(Value::as_str)
            {
                return msg.trim().to_string();
            }
        }
    }
    for e in events.iter().rev() {
        if str_field(e, "type") == "event_msg"
            && payload_type(e).as_deref() == Some("agent_message")
        {
            if let Some(msg) = e
                .get("payload")
                .and_then(|p| p.get("message"))
                .and_then(Value::as_str)
            {
                return msg.trim().to_string();
            }
        }
    }
    String::new()
}

/// 把单行 Codex 事件转成消息（仅 user_message / agent_message）。
fn codex_message_of(e: &Value) -> Option<SessionMessage> {
    if str_field(e, "type") != "event_msg" {
        return None;
    }
    let ts = str_field(e, "timestamp");
    let p = e.get("payload")?;
    let pt = p.get("type").and_then(Value::as_str)?;
    let role = match pt {
        "user_message" => "user",
        "agent_message" => "assistant",
        _ => return None,
    };
    let text = p.get("message").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return None;
    }
    Some(SessionMessage {
        role: role.to_string(),
        text: text.to_string(),
        detail: None,
        timestamp: ts,
    })
}

/// 行片是否含 `response_item/message`(role=user|assistant)——决定 Codex 取新源还是旧源。
fn codex_slice_uses_response_item(lines: &[String]) -> bool {
    for raw in lines {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if str_field(&v, "type") != "response_item" {
            continue;
        }
        let Some(p) = v.get("payload") else { continue };
        if p.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        match p.get("role").and_then(Value::as_str) {
            Some("user") | Some("assistant") => return true,
            _ => {}
        }
    }
    false
}

/// 取 `response_item.payload.content[]` 中指定 block 类型（如 `input_text` / `output_text`）的文本。
/// content 偶有直接为字符串的形态，一并兜底。
fn codex_content_text(payload: &Value, block_type: &str) -> String {
    let Some(content) = payload.get("content") else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some(block_type))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// 把 `reasoning` 的 `summary` / `content`（数组的 `text`）拼成思考正文；偶有顶层 `text`。
fn codex_reasoning_text(payload: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["summary", "content"] {
        if let Some(arr) = payload.get(key).and_then(Value::as_array) {
            for b in arr {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
        }
    }
    if parts.is_empty() {
        if let Some(s) = payload.get("text").and_then(Value::as_str) {
            if !s.is_empty() {
                parts.push(s.to_string());
            }
        }
    }
    parts.join("\n")
}

/// `arguments` / `output` 常是 JSON 字符串，直接用；否则 pretty 序列化。
fn codex_detail_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// 把单行 Codex `response_item` 事件转成消息（新源，权威）。
///
/// 覆盖：`message`(user/assistant，跳过 developer 与每轮注入的 `<environment_context>`)、
/// `reasoning`→`[thinking]`、`function_call`→`[tool_use: name]`、`function_call_output`→`[tool_result]`，
/// 真实内容（思考正文 / 入参 / 返回体）汇入 `detail` 供前端折叠。非 `response_item` 行返回 `None`
/// （新源模式下 `event_msg` 双写被天然跳过，避免重复）。
fn codex_response_item_to_msg(e: &Value) -> Option<SessionMessage> {
    if str_field(e, "type") != "response_item" {
        return None;
    }
    let ts = str_field(e, "timestamp");
    let p = e.get("payload")?;
    let pt = p.get("type").and_then(Value::as_str)?;
    let (role, text, detail): (&str, String, String) = match pt {
        "message" => match p.get("role").and_then(Value::as_str).unwrap_or("") {
            "developer" => return None, // 系统/权限说明，不展示
            "user" => {
                let body = codex_content_text(p, "input_text");
                // 每轮注入的环境上下文块不是真实用户输入。
                if body.trim_start().starts_with("<environment_context>") {
                    return None;
                }
                ("user", body, String::new())
            }
            "assistant" => (
                "assistant",
                codex_content_text(p, "output_text"),
                String::new(),
            ),
            _ => return None,
        },
        "reasoning" => {
            let think = codex_reasoning_text(p);
            let detail = if think.is_empty() {
                String::new()
            } else {
                format!("[thinking]\n{think}")
            };
            ("assistant", "[thinking]".to_string(), detail)
        }
        "function_call" => {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("");
            let args = p
                .get("arguments")
                .map(codex_detail_text)
                .unwrap_or_default();
            let detail = if args.is_empty() {
                String::new()
            } else {
                format!("[tool_use: {name}] 入参\n{args}")
            };
            ("assistant", format!("[tool_use: {name}]"), detail)
        }
        "function_call_output" => {
            let out = p.get("output").map(codex_detail_text).unwrap_or_default();
            let detail = if out.is_empty() {
                String::new()
            } else {
                format!("[tool_result]\n{out}")
            };
            ("assistant", "[tool_result]".to_string(), detail)
        }
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(SessionMessage {
        role: role.to_string(),
        text,
        detail: (!detail.is_empty()).then_some(detail),
        timestamp: ts,
    })
}

// ----------------------------- Claude 解析 ----------------------------- //

/// cwd：取首个带 `cwd` 字段的事件。
fn claude_cwd(events: &[Value]) -> PathBuf {
    for e in events {
        if let Some(cwd) = e.get("cwd").and_then(Value::as_str) {
            if !cwd.is_empty() {
                return PathBuf::from(cwd);
            }
        }
    }
    PathBuf::new()
}

fn claude_title(events: &[Value]) -> String {
    // 优先末条 ai-title 事件的 aiTitle，回退首条 user 消息文本。
    for e in events.iter().rev() {
        if str_field(e, "type") == "ai-title" {
            let t = str_field(e, "aiTitle");
            if !t.is_empty() {
                return t;
            }
        }
    }
    for e in events {
        if str_field(e, "type") == "user" {
            if let Some(m) = claude_message_of(e) {
                return clip(&m.text, 50);
            }
        }
    }
    String::new()
}

/// 首条用户消息正文的前若干字符；无则空串。
fn claude_first_user(events: &[Value]) -> String {
    for e in events {
        if str_field(e, "type") == "user" {
            if let Some(m) = claude_message_of(e) {
                if !m.text.is_empty() {
                    return clip(&m.text, 40);
                }
            }
        }
    }
    String::new()
}

fn claude_updated_at(events: &[Value]) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| e.get("timestamp").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

/// 状态：看末条 assistant 的 `message.stop_reason`；末条是 user 则 Processing。
fn claude_status(events: &[Value]) -> SessionStatus {
    let last_real = events
        .iter()
        .rev()
        .find(|e| matches!(str_field(e, "type").as_str(), "assistant" | "user"));
    if let Some(e) = last_real {
        if str_field(e, "type") == "user" {
            return SessionStatus::Processing;
        }
    } else {
        return SessionStatus::Unknown;
    }
    let last_assistant = events
        .iter()
        .rev()
        .find(|e| str_field(e, "type") == "assistant");
    match last_assistant {
        Some(e) => {
            let sr = e
                .get("message")
                .and_then(|m| m.get("stop_reason"))
                .and_then(Value::as_str);
            match sr {
                Some("end_turn") | Some("stop_sequence") | Some("stop") => SessionStatus::Idle,
                Some("tool_use") => SessionStatus::Generating,
                _ => SessionStatus::Unknown,
            }
        }
        None => SessionStatus::Unknown,
    }
}

/// 把 Claude `tool_result` 的 `content`（字符串或 `[{type:text,text}]`）抽成纯文本。
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(v) if v.is_string() => v.as_str().unwrap_or("").to_string(),
        Some(v) => v
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// 把 Claude 的 `message.content`（字符串或 block 列表）渲染为「可读正文 + 可展开详情」。
///
/// 正文里 thinking/tool_use/tool_result 仍是标记（`[thinking]` 等），但各自的真实内容
/// （思考正文 / 入参 JSON / 返回体）汇入 detail，供前端折叠展开。无可展开内容时 detail 为空串。
fn claude_content_to_text(content: &Value) -> (String, String) {
    if let Some(s) = content.as_str() {
        return (s.to_string(), String::new());
    }
    let Some(blocks) = content.as_array() else {
        return (String::new(), String::new());
    };
    let mut parts = Vec::new();
    let mut details = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
            Some("thinking") => {
                parts.push("[thinking]".to_string());
                let think = b
                    .get("thinking")
                    .or_else(|| b.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !think.is_empty() {
                    details.push(format!("[thinking]\n{think}"));
                }
            }
            Some("tool_use") => {
                let name = b.get("name").and_then(Value::as_str).unwrap_or("");
                parts.push(format!("[tool_use: {name}]"));
                if let Some(input) = b.get("input") {
                    let pretty =
                        serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
                    details.push(format!("[tool_use: {name}] 入参\n{pretty}"));
                }
            }
            Some("tool_result") => {
                parts.push("[tool_result]".to_string());
                let body = tool_result_text(b.get("content"));
                if !body.is_empty() {
                    details.push(format!("[tool_result]\n{body}"));
                }
            }
            // 贴图/截图：仅标记 `[image]`（不把 base64 塞进 detail），避免纯图消息正文为空被丢弃。
            Some("image") => parts.push("[image]".to_string()),
            _ => {}
        }
    }
    (parts.join(" "), details.join("\n\n"))
}

/// 把单行 Claude 事件转成消息（user / assistant，正文非空）。
fn claude_message_of(e: &Value) -> Option<SessionMessage> {
    let ty = str_field(e, "type");
    if ty != "user" && ty != "assistant" {
        return None;
    }
    let content = e.get("message").and_then(|m| m.get("content"))?;
    let (text, detail) = claude_content_to_text(content);
    if text.trim().is_empty() {
        return None;
    }
    Some(SessionMessage {
        role: ty,
        text,
        detail: (!detail.is_empty()).then_some(detail),
        timestamp: str_field(e, "timestamp"),
    })
}

/// 最近回复：末条含自然语言 text block 的 assistant，跳过纯 tool_use / thinking。
fn claude_latest_reply(events: &[Value]) -> String {
    for e in events.iter().rev() {
        if str_field(e, "type") != "assistant" {
            continue;
        }
        let Some(blocks) = e
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let joined = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let trimmed = joined.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    String::new()
}
