//! Codeloop 模块：Codex⇄Claude Code 跨会话复核循环的桌面内嵌实现。
//!
//! 采集层直接用 `agent-session`（读会话 / 起 CLI / 等空闲），协议层用 `codeloop-core`
//! （prompt 模板 / verdict 解析 / 三方校验）。循环跑在 zero-desktop 自己进程里，本机无需
//! 任何额外进程（不依赖 toolkit-server）。设计见
//! `docs/toolkit-rfc/2026-06-15-cross-session-review-loop/plan.md` 与本仓 plan。
//!
//! 与 toolkit-server 版（`crates/toolkit-server/src/codeloop/kind.rs`）的差异：
//! - 进度上报：`report_progress` 写 DB → 改 `app.emit("codeloop://progress")` 推前端 + 内存快照。
//! - ASK_USER 挂起：`codeloop_io` 表 + 2s 轮询 → 同进程 `oneshot` channel（拿得到 AppState，更干净）。
//! - 任务引擎：`impl TaskKind` → `tokio::spawn` 的后台任务，句柄存 `CodeloopState`。
//! - 通知回调（推微信）本期不做。

pub mod db;
pub mod runtime;
pub mod smoke;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_session::store::Store;
use agent_session::{driver, watch, MessagesPage, Provider, SessionRef, SessionSummary};
use anyhow::Result;
use codeloop_core::parse::{self, Verdict};
use codeloop_core::prompt::{self, EntryKind, ReviewMode, TargetRole, TargetSpec};
use codeloop_core::validate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, State};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::app_state::AppState;
use crate::shared::workspace::codeloop_db_path;
use runtime::{
    AskUserRequest, ConfirmPolicy, ConfirmRequest, Gate, LoopEvents, Pending, PendingConfirm,
    TauriLoopEvents, UiConfirmPolicy,
};

/// 等待 Claude 当前轮空闲的超时（对应 wait_for_claude_idle）。
const CLAUDE_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// ASK_USER 挂起等待用户回答的上限。
const ANSWER_TIMEOUT: Duration = Duration::from_secs(1800);
/// 连续解析失败到此轮数 → AbortedParse。
const MAX_PARSE_FAILS: u32 = 2;

// ------------------------- 模块状态 -------------------------

/// Codeloop 模块状态：允许多个复核循环并发在跑（按 loop_id 索引）；记录持久化到 SQLite。
/// 并发安全约束：同一对会话（claude/codex session_id）同一时刻只允许一个循环占用，
/// 启动时校验冲突（见 [`codeloop_start`]），避免两个循环驱动同一 CLI 会话互相踩踏。
pub struct CodeloopState {
    inner: Mutex<HashMap<i64, RunningLoop>>,
    db: Arc<db::Db>,
}

impl CodeloopState {
    pub fn new(workspace: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Mutex::new(HashMap::new()),
            db: Arc::new(db::Db::open(&codeloop_db_path(workspace))?),
        })
    }
}

/// 一个运行中（或刚结束）的循环。
struct RunningLoop {
    handle: JoinHandle<()>,
    /// 该循环在 `loops` 表的记录 id（供 stop 显式 finalize）。
    loop_id: i64,
    /// 占用的两端会话 id（启动时冲突校验：同一会话不可并发跑多个循环）。
    claude_session: String,
    codex_session: String,
    /// 最近一次上报的进度快照（供 `codeloop_status` 兜底读取）。
    progress: Arc<Mutex<Value>>,
    /// ASK_USER 挂起态（非 None = 正等用户回答）。与 UiConfirmPolicy 共享 Arc。
    pending: Arc<Mutex<Option<Pending>>>,
    /// 逐步确认门挂起态（非 None = 正等用户确认/否决某次传递）。与 UiConfirmPolicy 共享 Arc。
    pending_confirm: Arc<Mutex<Option<PendingConfirm>>>,
    /// 逐步确认开关（运行时可翻转：true=每步确认 / false=全自动）。与 UiConfirmPolicy 共享。
    step_confirm: Arc<AtomicBool>,
}

// ------------------------- 输入契约 -------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SessionRefDto {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StartInput {
    pub claude: SessionRefDto,
    pub codex: SessionRefDto,
    /// `target_path` 在多入口里的语义：
    ///
    /// - `DocReview` / `Implement` / `ReviewSeed`：必填，按入口语义解释（待复核文档 / 规格文档 / 代码根）。
    /// - `Continuation`：留空——既有 session 已经讨论过对象，prompt 不再引用 target。
    ///
    /// 用 `Option<String>` 接收前端缺省；空串视同 None。
    #[serde(default)]
    pub target_path: Option<String>,
    #[serde(default)]
    pub target_label: Option<String>,
    /// `Continuation` 入口 mode 无意义（不渲染 SCOPE），可缺省。其余入口必须给。
    #[serde(default = "default_mode")]
    pub mode: ReviewMode,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
    #[serde(default)]
    pub wait_for_claude_idle: bool,
    /// 逐步确认（手动）：每次跨会话传递前弹窗等用户拍板；关则全自动。默认开。
    #[serde(default = "default_true")]
    pub step_confirm: bool,
    /// worktree 模式：让 Claude 自己用 git worktree + 子 agent 隔离实现，后端解析其回报的
    /// worktree 路径后把 Codex 复核重定位过去。默认关（向后兼容）。
    #[serde(default)]
    pub use_worktree: bool,
    /// 两阶段血缘：本循环承接自哪条记录（design→implementation）。新建独立循环时为 None。
    #[serde(default)]
    pub parent_loop_id: Option<i64>,
    /// 从已完成的 worktree 继续复核：跳过 implementation 模式的 Claude 实现首步，直接让 Codex
    /// 在该 worktree 内复核。用于恢复「Zero Desktop 已停止跟踪，但 Claude 后续完成」的记录。
    #[serde(default)]
    pub resume_worktree_path: Option<String>,
    /// 首轮预热：哪些端已在外部（预览台）发过首轮说明块，循环首轮即跳过其 STANDING_BLOCK。
    /// 按 provider 分开（预览台一次只热一端，另一端仍需照发）。
    #[serde(default)]
    pub established: Established,
    // ----- 多入口（codeloop-multi-entry-design.md §6.1）-----
    /// 多入口标记；未带时按 `mode` 推断（`design ⇒ DocReview` / `implementation ⇒ Implement`）。
    /// 只发 mode 永远不会被推断为 ReviewSeed，避免误升级。
    #[serde(default)]
    pub entry_kind: Option<EntryKind>,
    /// 仅 `ReviewSeed(mode=implementation)` 可用：规格依据文档路径（绝对或相对 target 仓根）。
    #[serde(default)]
    pub design_doc_path: Option<String>,
    /// `ReviewSeed`：seed 文件路径（与 `seed_review_inline` 二选一）。
    #[serde(default)]
    pub seed_review_path: Option<String>,
    /// `ReviewSeed`：直接粘贴的 review 文本（与 `seed_review_path` 二选一）。
    #[serde(default)]
    pub seed_review_inline: Option<String>,
    /// 评估方案最优性（仅 Design 系入口 / Implementation 忽略）：true 切到
    /// `DESIGN_SCOPE_WITH_ALTERNATIVES`，多一条"评估所选方案 vs 替代方案"维度。
    /// 较慢、易发散，仅对"方案尚未定稿"的文档有用。默认 false。
    #[serde(default)]
    pub evaluate_alternatives: bool,
}

/// 首轮预热状态（按 provider 分开）。见 docs/codeloop-cli-resilience-design.md §5。
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Established {
    #[serde(default)]
    pub codex: bool,
    #[serde(default)]
    pub claude: bool,
}

fn default_max_rounds() -> u32 {
    10
}

fn default_mode() -> ReviewMode {
    ReviewMode::Design
}

fn default_true() -> bool {
    true
}

/// 业务终态（对齐 toolkit-server 版 FinalVerdict 语义）。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FinalVerdict {
    Pass,
    MaxRounds,
    AbortedTimeout,
    AbortedParse,
    /// 用户在逐步确认弹窗里否决了某次跨会话传递 → 主动中止。
    AbortedByUser,
}

// ------------------------- 多入口（codeloop-multi-entry-design.md §6.1）-------------------------

/// `entry_kind` → 落库字符串（`loops.entry_kind` 列）。
fn entry_kind_str(k: EntryKind) -> &'static str {
    match k {
        EntryKind::DocReview => "doc_review",
        EntryKind::Implement => "implement",
        EntryKind::ReviewSeed => "review_seed",
        EntryKind::Continuation => "continuation",
    }
}

/// 未带 `entry_kind` 时按 `mode` 推断（§6.1）；只发 mode 永远不会被推断为 ReviewSeed。
fn infer_entry_kind(explicit: Option<EntryKind>, mode: ReviewMode) -> EntryKind {
    if let Some(k) = explicit {
        return k;
    }
    match mode {
        ReviewMode::Design => EntryKind::DocReview,
        ReviewMode::Implementation => EntryKind::Implement,
    }
}

/// `StartInput` / `RunLoopInput` 共用的入口字段校验（§6.1 校验表）。
///
/// `design_doc_path` / `seed_review_path` / `seed_review_inline` 仅做"字段层校验"
/// （是否给了、是否互斥），路径存在性 / 仓根归属 / 大小阈值由 `validate_review_seed_inputs`
/// 做（preflight + start 均调）。
fn validate_entry_fields(
    entry_kind: EntryKind,
    mode: ReviewMode,
    design_doc_path: Option<&str>,
    seed_review_path: Option<&str>,
    seed_review_inline: Option<&str>,
) -> std::result::Result<(), String> {
    let has_seed_path = seed_review_path.map(|s| !s.is_empty()).unwrap_or(false);
    let has_seed_inline = seed_review_inline.map(|s| !s.is_empty()).unwrap_or(false);
    let has_design_doc = design_doc_path.map(|s| !s.is_empty()).unwrap_or(false);
    match entry_kind {
        EntryKind::DocReview => {
            if mode != ReviewMode::Design {
                return Err("DocReview 入口仅支持 mode=design".into());
            }
            if has_design_doc {
                return Err("DocReview 入口不接受 design_doc_path".into());
            }
            if has_seed_path || has_seed_inline {
                return Err("DocReview 入口不接受 seed_review_*".into());
            }
        }
        EntryKind::Implement => {
            if mode != ReviewMode::Implementation {
                return Err("Implement 入口仅支持 mode=implementation".into());
            }
            if has_design_doc {
                return Err(
                    "Implement 入口不接受 design_doc_path（target_path 即规格文档）".into(),
                );
            }
            if has_seed_path || has_seed_inline {
                return Err("Implement 入口不接受 seed_review_*".into());
            }
        }
        EntryKind::ReviewSeed => {
            if has_seed_path == has_seed_inline {
                return Err(
                    "ReviewSeed 入口要求 seed_review_path / seed_review_inline 恰好一项有值".into(),
                );
            }
            if mode == ReviewMode::Design && has_design_doc {
                return Err(
                    "ReviewSeed(mode=design) 拒绝 design_doc_path（target_path 即修订对象）".into(),
                );
            }
            // mode=implementation 时 design_doc_path 可缺省（合法）。
        }
        EntryKind::Continuation => {
            // 续跑入口不接受任何 target / seed / 规格依据字段——
            // 既有 session 已经携带上下文，prompt 不会引用这些。
            if has_seed_path || has_seed_inline {
                return Err(
                    "Continuation 入口不接受 seed_review_*（既有 session 已携带上下文）".into(),
                );
            }
            if has_design_doc {
                return Err("Continuation 入口不接受 design_doc_path".into());
            }
            // mode 字段在 Continuation 下无意义（不渲染 SCOPE），不校验。
        }
    }
    Ok(())
}

/// `ReviewSeed` 的 seed 输入校验（路径存在 / 可读 / 大小阈值）。`preflight` 与 `start` 都调。
const SEED_MAX_BYTES: u64 = 1024 * 1024; // 1 MiB

fn validate_review_seed_inputs(
    seed_review_path: Option<&str>,
    seed_review_inline: Option<&str>,
) -> std::result::Result<(), String> {
    if let Some(p) = seed_review_path.filter(|s| !s.is_empty()) {
        let path = Path::new(p);
        let meta = std::fs::metadata(path)
            .map_err(|e| format!("seed_review_path 不可读：{}（{e}）", p))?;
        if !meta.is_file() {
            return Err(format!("seed_review_path 不是常规文件：{}", p));
        }
        if meta.len() > SEED_MAX_BYTES {
            return Err(format!(
                "seed_review_path 超过 {}KiB（实际 {} 字节）",
                SEED_MAX_BYTES / 1024,
                meta.len()
            ));
        }
    }
    if let Some(s) = seed_review_inline.filter(|s| !s.is_empty()) {
        if s.len() as u64 > SEED_MAX_BYTES {
            return Err(format!(
                "seed_review_inline 超过 {}KiB",
                SEED_MAX_BYTES / 1024
            ));
        }
    }
    Ok(())
}

/// 把 `design_doc_path` 解析为 `target` 所在仓根之内的绝对路径（canonicalize + 同仓根校验）。
/// `repo_root` 由调用方提供（已 canonicalize / display_path）。
pub(super) fn resolve_design_doc(
    repo_root: &Path,
    design_doc_path: &str,
) -> std::result::Result<PathBuf, String> {
    let p = Path::new(design_doc_path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_root.join(p)
    };
    let canon = std::fs::canonicalize(&abs)
        .map_err(|e| format!("design_doc_path 解析失败 {}：{e}", abs.display()))?;
    let meta = std::fs::metadata(&canon)
        .map_err(|e| format!("design_doc_path 不可读 {}：{e}", canon.display()))?;
    if !meta.is_file() {
        return Err(format!("design_doc_path 不是常规文件：{}", canon.display()));
    }
    if meta.len() == 0 {
        return Err(format!("design_doc_path 为空文件：{}", canon.display()));
    }
    let repo_canon = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    if !canon.starts_with(&repo_canon) {
        return Err(format!(
            "design_doc_path 跨仓：{} 不在仓根 {} 内",
            canon.display(),
            repo_canon.display()
        ));
    }
    Ok(canon)
}

/// 读 seed 文本：`inline` 优先；否则从 `path` 读全文。
///
/// 目前 `dispatch_review_seed` 直接从 ctx.seed_review_inline / DB 行的 seed_review_path
/// 读，未走此函数；保留供未来桌面端「预览 seed」/「在 preflight 提前读出 seed 文本」时复用。
#[allow(dead_code)]
fn load_seed_text(
    seed_review_path: Option<&str>,
    seed_review_inline: Option<&str>,
) -> std::result::Result<String, String> {
    if let Some(s) = seed_review_inline.filter(|s| !s.is_empty()) {
        return Ok(s.to_string());
    }
    if let Some(p) = seed_review_path.filter(|s| !s.is_empty()) {
        return std::fs::read_to_string(p)
            .map_err(|e| format!("读 seed_review_path 失败 {}：{e}", p));
    }
    Err("缺少 seed_review_path / seed_review_inline".into())
}

/// inline seed 的短哈希前缀（sha256 前 16 字符 hex），供排错与去重。
pub(super) fn seed_inline_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    let out = h.finalize();
    let hex: String = out.iter().take(8).map(|b| format!("{b:02x}")).collect();
    hex
}

// ------------------------- 循环上下文 -------------------------

/// 运行期上下文：解析好的两端 SessionRef + target 定位 + 配置 + 共享句柄。
struct LoopCtx {
    events: Arc<dyn LoopEvents>,
    confirm: Arc<dyn ConfirmPolicy>,
    store: Store,
    db: Arc<db::Db>,
    loop_id: i64,
    claude: SessionRef,
    codex: SessionRef,
    target: TargetSpec,
    mode: ReviewMode,
    max_rounds: u32,
    wait_for_claude_idle: bool,
    use_worktree: bool,
    /// true 表示实现已由历史 worktree 完成，本循环从 Codex 复核阶段开始。
    resume_from_worktree: bool,
    /// 首轮预热：对应端已在外部建立，循环首轮跳过其 STANDING_BLOCK。
    established_codex: bool,
    established_claude: bool,
    progress: Arc<Mutex<Value>>,
    seq: Arc<AtomicI64>,
    // ----- 多入口（codeloop-multi-entry-design.md §6.3）-----
    /// 入口类型（决定 round 1 在 `loop_main` 之前做什么）。
    entry_kind: EntryKind,
    /// Codex / Claude prompt 渲染时的 Locator 措辞角色。
    target_role: TargetRole,
    /// 仅 `RevisionCode` 用：规格依据文档绝对路径。
    spec_doc: Option<PathBuf>,
    /// `ReviewSeed` 入口的 inline seed 原文（与 DB 的 `seed_review_path` 互斥）。
    /// DB 只存 hash 不存原文，故 inline 走通需 LoopCtx 透传。
    seed_review_inline: Option<String>,
    /// 续跑模型：true = 这是用户点了"继续"再次发起的任务。
    /// 影响：implementation prompt 用 RESUME 模板；worktree_path 已落库时附 WORKTREE_REUSE_NOTICE
    /// 而非 WORKTREE_INSTRUCTION（见 codeloop-attempt-model-design.md §4.6）。
    is_continue: bool,
    /// 评估方案最优性（仅 Design 系入口生效）：true → render_codex_prompt 切 ALTERNATIVES SCOPE。
    /// 不持久化（续跑时回落默认）；见 prompt::DESIGN_SCOPE_WITH_ALTERNATIVES。
    evaluate_alternatives: bool,
}

impl LoopCtx {
    /// 写进度快照 + 通过 events 适配器外发（Tauri emit / 控制台打印 / JSONL）。
    async fn report(&self, v: Value) {
        *self.progress.lock().await = v.clone();
        self.events.progress(self.loop_id, v);
    }

    /// 追加一条逐轮消息到记录（失败仅记日志，不影响循环）。
    fn log_msg(&self, round: u32, kind: &str, verdict: Option<&str>, content: &str) {
        if let Err(e) = self
            .db
            .append_message(self.loop_id, round as i64, kind, verdict, content)
        {
            log::warn!("[codeloop] 写消息记录失败：{e:#}");
        }
    }

    /// 续跑模型：写 last_phase（失败仅记日志，不影响循环）。
    /// 调用点见 docs/codeloop-attempt-model-design.md §3.3。
    fn set_phase(&self, phase: &str) {
        if let Err(e) = self.db.set_last_phase(self.loop_id, phase) {
            log::warn!("[codeloop] 写 last_phase={phase} 失败：{e:#}");
        }
    }

    /// 终态收尾：finalize 记录（幂等）+ 上报 done。
    async fn finish(&self, final_verdict: FinalVerdict, total_rounds: u32) {
        let (status, fv) = match final_verdict {
            FinalVerdict::Pass => ("done", "pass"),
            FinalVerdict::MaxRounds => ("done", "max_rounds"),
            FinalVerdict::AbortedTimeout => ("aborted", "aborted_timeout"),
            FinalVerdict::AbortedParse => ("aborted", "aborted_parse"),
            FinalVerdict::AbortedByUser => ("aborted", "aborted_by_user"),
        };
        self.set_phase("finalized");
        if let Err(e) = self
            .db
            .finalize(self.loop_id, status, Some(fv), total_rounds as i64, None)
        {
            log::warn!("[codeloop] finalize 记录失败：{e:#}");
        }
        self.report(json!({
            "phase": "done", "final_verdict": final_verdict, "total_rounds": total_rounds,
        }))
        .await;
    }
}

/// send_and_resolve 的结果：拿到回复，或等用户答超时。
enum Resolved {
    Reply(String),
    Timeout,
}

/// 发一轮 → 若含 ASK_USER 则挂起等用户答（同进程 oneshot）→ 把答案发回同一会话 →
/// 直到不再 ASK_USER。基础设施错（CLI 缺失 / spawn 失败）→ Err。
async fn send_and_resolve(
    ctx: &LoopCtx,
    session: &SessionRef,
    prompt_text: &str,
) -> Result<Resolved> {
    send_and_resolve_with(ctx, session, prompt_text, None).await
}

/// 同 [`send_and_resolve`]，但可指定单轮超时；同时自动把 CLI stdout 流式 emit 到前端。
/// 用于 implementation 首步等需要长超时 + 实时心跳的场景（见 design doc §4.4 / §5）。
async fn send_and_resolve_with(
    ctx: &LoopCtx,
    session: &SessionRef,
    prompt_text: &str,
    timeout_override: Option<std::time::Duration>,
) -> Result<Resolved> {
    let mut current = prompt_text.to_string();
    loop {
        log::info!(
            "[codeloop] → 发往 {} 会话 {}（prompt {} 字符），等待回复…",
            session.provider.as_str(),
            session.session_id,
            current.chars().count(),
        );
        let events_for_stream = ctx.events.clone();
        let loop_id_for_stream = ctx.loop_id;
        let source_for_stream = session.provider.as_str();
        let on_line: driver::LineCallback = Box::new(move |line: &str| {
            events_for_stream.stream_line(loop_id_for_stream, source_for_stream, line);
        });
        let mut opts = driver::SendOpts::default().with_on_line(on_line);
        if let Some(t) = timeout_override {
            opts = opts.with_timeout(t);
        }
        let turn = driver::send_with(session, &current, opts).await?;
        log::info!(
            "[codeloop] ← {} 回复 {} 字符",
            session.provider.as_str(),
            turn.reply_text.chars().count(),
        );
        let Some(q) = parse::parse_ask_user(&turn.reply_text) else {
            return Ok(Resolved::Reply(turn.reply_text));
        };
        log::info!(
            "[codeloop] {} 触发 ASK_USER，挂起等用户作答",
            session.provider.as_str()
        );

        let seq = ctx.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let req = AskUserRequest {
            loop_id: ctx.loop_id,
            seq,
            asked_by: session.provider.as_str(),
            question: &q.question,
        };
        // ConfirmPolicy 决定怎么拿答案：UI 走 oneshot 弹窗；headless 走 answer map / 中止。
        match ctx.confirm.answer_user(req, ctx.events.as_ref()).await {
            Some(answer) => current = format!("用户答复：{answer}"),
            None => return Ok(Resolved::Timeout),
        }
    }
}

/// 跨会话传递前的人工确认门：通过 ConfirmPolicy 路由（UI 走弹窗、headless 走全自动）。
async fn confirm_gate(ctx: &LoopCtx, direction: &str, title: &str, content: &str) -> Gate {
    let seq = ctx.seq.fetch_add(1, Ordering::SeqCst) + 1;
    let req = ConfirmRequest {
        loop_id: ctx.loop_id,
        seq,
        direction,
        title,
        content,
    };
    ctx.confirm.confirm(req, ctx.events.as_ref()).await
}

/// worktree 模式：尝试从 Claude 回复解析 worktree 路径，命中且校验通过则把 Codex 重定位过去
/// （改局部 `target`/`codex`，置 `worktree_established`，下一轮 Codex 强制重发新定位）。`round`
/// 仅用于 system 消息归属。实现首步与每轮修订共用此逻辑。
fn try_relocate_worktree(
    ctx: &LoopCtx,
    reply: &str,
    round: u32,
    target: &mut TargetSpec,
    codex: &mut SessionRef,
    worktree_established: &mut bool,
    force_locator: &mut bool,
) {
    let Some(wt) = parse::parse_worktree_path(reply) else {
        return;
    };
    // 用 ctx.codex.cwd（原始未重定位的 codex cwd）作为同仓校验锚点；它一直是 loop 的原 repo 根。
    match relocate_to_worktree(&wt, &target.repo_rel, &ctx.codex.cwd) {
        Ok((new_target, new_cwd)) => {
            *target = new_target;
            codex.cwd = new_cwd;
            *worktree_established = true;
            *force_locator = true; // 下一轮 Codex 强制重发新定位
            if let Err(e) = ctx.db.set_worktree(ctx.loop_id, &wt) {
                log::warn!("[codeloop] set_worktree 失败：{e:#}");
            }
            ctx.log_msg(
                round,
                "system",
                None,
                &format!("已切换到 worktree：{wt}（后续 Codex 在此工作树复核）"),
            );
            log::info!("[codeloop] worktree 重定位成功 → {wt}");
        }
        Err(e) => {
            ctx.log_msg(
                round,
                "system",
                None,
                &format!("worktree 路径校验失败（{e}），继续在原仓库复核"),
            );
            log::warn!("[codeloop] worktree 校验失败：{e}");
        }
    }
}

/// 复核↔修订主循环。基础设施错 → Err（上层 emit error）；业务终态正常收尾。
///
/// `start_round`：循环主体的起点轮次。`DocReview` / `Implement` 入口传 1（首轮 = Codex 复核 round
/// 1）；`ReviewSeed` 入口传 2（round 1 已由入口分发段以 seed + Claude 修订 round 1 顶替）。
/// 见 docs/codeloop-multi-entry-design.md §6.2。
async fn loop_main(ctx: &LoopCtx, start_round: u32) -> Result<()> {
    // 临时别名，便于在过渡期保留既有 implement 首步逻辑可继续编译。
    drive(ctx, start_round).await
}

/// 复核↔修订主循环（旧名）。`start_round` 决定首个 Codex 复核轮次。
async fn drive(ctx: &LoopCtx, start_round: u32) -> Result<()> {
    if ctx.wait_for_claude_idle {
        log::info!("[codeloop] 先等 Claude 当前轮空闲（超时 {CLAUDE_IDLE_TIMEOUT:?}）…");
        if let Err(e) = watch::wait_for_idle(&ctx.store, &ctx.claude, CLAUDE_IDLE_TIMEOUT).await {
            log::warn!("[codeloop] wait_for_claude_idle 超时/失败，按 AbortedTimeout 处理: {e:#}");
            ctx.finish(FinalVerdict::AbortedTimeout, 0).await;
            return Ok(());
        }
        log::info!("[codeloop] Claude 已空闲，开始复核循环");
    }

    // 本地可变副本：worktree 重定位时只改局部（Codex 端 --cd / target 迁到 worktree），
    // 不给 LoopCtx 加锁。Claude 端始终用 ctx.claude（其会话只能在原 cwd resume）。
    let mut target = ctx.target.clone();
    let mut codex = ctx.codex.clone();
    let mut worktree_established = false;
    let mut force_locator = false;
    let mut consecutive_parse_fail = 0u32;
    let mut last_claude_reply = String::new();
    // Claude 端常驻说明块是否已发过（实现首步会发，之后修订轮不再重发）。
    let mut claude_standing_sent = ctx.established_claude;

    // implementation 模式：先让 Claude Code 按设计文档把功能代码全部实现，再交 Codex 复核。
    // （design 模式无此步：复核对象是已存在的文档，直接进入 Codex 复核。）
    // worktree 模式下，worktree 即在这一步由 Claude 建立并回报路径。
    // Continuation 入口跳过此步——既有 session 已经讨论过实现，直接进入 Codex 复核。
    if ctx.entry_kind != EntryKind::Continuation
        && ctx.mode == ReviewMode::Implementation
        && !ctx.resume_from_worktree
    {
        log::info!("[codeloop] implementation 模式：先让 Claude 按文档实现功能代码…");
        // 续跑模型：is_continue → 用 RESUME 模板（"上次中断，继续完成未做的部分"）；
        // 首次 → 沿用 DEFAULT 模板（"按规格全部实现"）。
        let impl_template = if ctx.is_continue {
            prompt::DEFAULT_CLAUDE_IMPLEMENT_RESUME_TEMPLATE
        } else {
            prompt::DEFAULT_CLAUDE_IMPLEMENT_TEMPLATE
        };
        let mut implement_prompt =
            prompt::render_claude_implement_prompt(impl_template, &target, !claude_standing_sent);
        if ctx.use_worktree && !worktree_established {
            // worktree_path 已落库 → 用 REUSE_NOTICE，让 Claude 在原 worktree 内继续；
            // 否则发 INSTRUCTION 让 Claude 自己 git worktree add 一棵新的。
            let existing_wt = ctx
                .db
                .get_loop(ctx.loop_id)
                .ok()
                .flatten()
                .and_then(|r| r.worktree_path);
            if let Some(wt) = existing_wt {
                implement_prompt.push_str(&prompt::render_worktree_reuse_notice(&wt));
            } else {
                implement_prompt.push_str(prompt::WORKTREE_INSTRUCTION);
            }
        }
        ctx.set_phase("implementing");
        ctx.log_msg(
            0,
            "system",
            None,
            &format!(
                "已进入实现阶段，正在向 Claude 发送实现命令，并等待其按设计文档落地功能代码。{}",
                if ctx.use_worktree {
                    "已启用 worktree 模式，Claude 完成后应回报 WORKTREE 路径。"
                } else {
                    "未启用 worktree 模式，Claude 将在原会话工作树内实现。"
                }
            ),
        );
        ctx.report(json!({ "round": 0, "phase": "implementing" }))
            .await;
        // implementation 首步用长超时（TURN_TIMEOUT_IMPL=3600s）；worktree+多文件+编译验证
        // 常态 30–60min，默认 1200s 会被切断（loop 12 即此种死法）。
        let reply = match send_and_resolve_with(
            ctx,
            &ctx.claude,
            &implement_prompt,
            Some(driver::TURN_TIMEOUT_IMPL),
        )
        .await?
        {
            Resolved::Reply(r) => r,
            Resolved::Timeout => {
                ctx.finish(FinalVerdict::AbortedTimeout, 0).await;
                return Ok(());
            }
        };
        claude_standing_sent = true;
        if ctx.use_worktree && !worktree_established {
            try_relocate_worktree(
                ctx,
                &reply,
                0,
                &mut target,
                &mut codex,
                &mut worktree_established,
                &mut force_locator,
            );
        }
        ctx.log_msg(0, "claude_implement", None, &reply);
        last_claude_reply = reply;
        log::info!("[codeloop] Claude 首步实现完成，进入 Codex 复核循环");
        ctx.set_phase("implemented");
        ctx.report(json!({ "round": 0, "phase": "implemented" }))
            .await;
    } else if ctx.entry_kind != EntryKind::Continuation && ctx.mode == ReviewMode::Implementation {
        log::info!("[codeloop] 从已存在 worktree 继续：跳过 Claude 实现首步，直接进入 Codex 复核");
        worktree_established = true;
        ctx.set_phase("implemented");
        ctx.report(json!({ "round": 0, "phase": "implemented" }))
            .await;
    }

    for n in start_round..=ctx.max_rounds {
        // 0. 二轮起：让 Codex 基于上一轮 Claude 修订重新审核前，先确认（展示 Claude 本轮回复）。
        if n > 1 {
            match confirm_gate(
                ctx,
                "claude_to_codex",
                "让 Codex 基于 Claude 本轮修订重新审核？",
                &last_claude_reply,
            )
            .await
            {
                Gate::Approve => {}
                Gate::Reject => {
                    ctx.finish(FinalVerdict::AbortedByUser, n - 1).await;
                    return Ok(());
                }
                Gate::Timeout => {
                    ctx.finish(FinalVerdict::AbortedTimeout, n - 1).await;
                    return Ok(());
                }
            }
        }

        // 1. Codex 复核（含 ASK_USER 挂起）。
        ctx.set_phase("codex_review");
        log::info!(
            "[codeloop] === 第 {n}/{} 轮：发起 Codex 复核 ===",
            ctx.max_rounds
        );
        // first_turn = n==1：常驻说明块（定位 + ASK_USER 协议）只在持续会话首轮发一次，
        // 后续轮依赖会话历史，不再重发（避免每条消息末尾重复刷屏/占 token）。
        // 若该端已在外部预热（established_codex），首轮也跳过（说明块已在预览台发过）。
        // first_turn = 首轮 或 worktree 重定位后强制重发一次（让 Codex 知道目标已迁到新工作树）。
        let codex_first_turn =
            (n == 1 && !ctx.established_codex) || std::mem::take(&mut force_locator);
        let codex_prompt = if ctx.entry_kind == EntryKind::Continuation {
            // Continuation：会话历史已含上下文 → 用 continuation 模板（无 LABEL / 无 locator）。
            // 但 ASK_USER / VERDICT 输出契约对既有"自然语言审查"的 session 仍是新东西，
            // 首轮（未预热时）追加协议段；预热勾上则跳过。
            prompt::render_codex_continuation_prompt(
                prompt::DEFAULT_CODEX_CONTINUATION_TEMPLATE,
                n,
                codex_first_turn,
            )
        } else {
            prompt::render_codex_prompt(
                prompt::DEFAULT_CODEX_TEMPLATE,
                &target,
                ctx.mode,
                n,
                codex_first_turn,
                ctx.target_role,
                ctx.spec_doc.as_deref(),
                ctx.evaluate_alternatives,
            )
        };
        let review = match send_and_resolve(ctx, &codex, &codex_prompt).await? {
            Resolved::Reply(r) => r,
            Resolved::Timeout => {
                ctx.finish(FinalVerdict::AbortedTimeout, n - 1).await;
                return Ok(());
            }
        };

        // 2. 解析 VERDICT，并把本轮 Codex 复核全文记入记录。
        let parsed = parse::parse_verdict(&review);
        let verdict_str = match parsed {
            Some(Verdict::Pass) => "pass",
            Some(Verdict::NeedsWork) => "needs_work",
            None => "parse_failed",
        };
        ctx.log_msg(n, "codex_review", Some(verdict_str), &review);
        let verdict = match parsed {
            Some(v) => {
                consecutive_parse_fail = 0;
                v
            }
            None => {
                consecutive_parse_fail += 1;
                if consecutive_parse_fail >= MAX_PARSE_FAILS {
                    ctx.report(json!({
                        "round": n, "phase": "reviewed", "verdict": "parse_failed",
                        "consecutive_parse_fail": consecutive_parse_fail,
                    }))
                    .await;
                    ctx.finish(FinalVerdict::AbortedParse, n - 1).await;
                    return Ok(());
                }
                Verdict::NeedsWork
            }
        };
        log::info!("[codeloop] 第 {n} 轮 Codex 判定：{verdict:?}");
        ctx.report(json!({ "round": n, "phase": "reviewed", "verdict": verdict }))
            .await;

        // 3. PASS → 终止。
        if verdict == Verdict::Pass {
            log::info!("[codeloop] PASS，循环通过收尾");
            ctx.finish(FinalVerdict::Pass, n).await;
            return Ok(());
        }

        // 4. 把 Codex 审核意见发给 Claude 修订前，先确认（展示意见全文）。
        match confirm_gate(
            ctx,
            "codex_to_claude",
            "把 Codex 审核意见发给 Claude Code 修订？",
            &review,
        )
        .await
        {
            Gate::Approve => {}
            Gate::Reject => {
                ctx.finish(FinalVerdict::AbortedByUser, n - 1).await;
                return Ok(());
            }
            Gate::Timeout => {
                ctx.finish(FinalVerdict::AbortedTimeout, n - 1).await;
                return Ok(());
            }
        }

        // 5. Claude 据意见修订（含 ASK_USER 挂起）。
        ctx.set_phase("claude_revise");
        // Claude 仅在 NEEDS_WORK 时被发起，其首次发送恒为第 1 轮 → n==1 即首轮。
        // 若 Claude 端已外部预热（established_claude），首轮也跳过 STANDING_BLOCK。
        // impl 模式首步已给 Claude 发过常驻块；仅在尚未发过时（design 模式首次修订）补发。
        let claude_first_turn = !claude_standing_sent;
        let mut claude_prompt = if ctx.entry_kind == EntryKind::Continuation {
            prompt::render_claude_continuation_prompt(
                prompt::DEFAULT_CLAUDE_CONTINUATION_TEMPLATE,
                &review,
                claude_first_turn,
            )
        } else {
            prompt::render_claude_prompt(
                prompt::DEFAULT_CLAUDE_TEMPLATE,
                &target,
                &review,
                claude_first_turn,
                ctx.target_role,
                ctx.spec_doc.as_deref(),
            )
        };
        // worktree 模式且尚未建立：追加指令，让 Claude 自己用 worktree + 子 agent 实现并回报路径。
        // Continuation 不接 worktree 指令——续跑场景下 worktree 决定权交回用户/Claude。
        if ctx.entry_kind != EntryKind::Continuation && ctx.use_worktree && !worktree_established {
            claude_prompt.push_str(prompt::WORKTREE_INSTRUCTION);
        }
        let claude_reply = match send_and_resolve(ctx, &ctx.claude, &claude_prompt).await? {
            Resolved::Reply(r) => r,
            Resolved::Timeout => {
                ctx.finish(FinalVerdict::AbortedTimeout, n).await;
                return Ok(());
            }
        };
        claude_standing_sent = true;

        // worktree 模式：从 Claude 回复解析 worktree 路径，命中且校验通过则把 Codex 重定位过去。
        if ctx.use_worktree && !worktree_established {
            try_relocate_worktree(
                ctx,
                &claude_reply,
                n,
                &mut target,
                &mut codex,
                &mut worktree_established,
                &mut force_locator,
            );
        }

        ctx.log_msg(n, "claude_revise", None, &claude_reply);
        last_claude_reply = claude_reply;
        log::info!("[codeloop] 第 {n} 轮 Claude 修订完成");
        ctx.report(json!({ "round": n, "phase": "revised" })).await;
    }

    // 跑满未 PASS。
    log::info!(
        "[codeloop] 跑满 {} 轮仍未 PASS，按 MaxRounds 收尾",
        ctx.max_rounds
    );
    ctx.finish(FinalVerdict::MaxRounds, ctx.max_rounds).await;
    Ok(())
}

/// `ReviewSeed` 入口分发：
/// 1. 读 seed（inline 优先 → 否则文件）。
/// 2. wrap 成 EXTERNAL_REVIEW_SEED 区块 → 作为 round-1 `codex_review_seed`（verdict=needs_work）
///    写入 `loop_messages`。
/// 3. 立刻用包裹后的 seed 作为 `{REVIEW}` 渲染 Claude 修订模板 → 调 `send_and_resolve` 拿回复，
///    写 round-1 `claude_revise`；并尝试 `try_relocate_worktree`（与 Implement 路径同形）。
///
/// 见 docs/codeloop-multi-entry-design.md §3.3 / §6.2。
async fn dispatch_review_seed(ctx: &LoopCtx) -> Result<()> {
    // 入口分发段读取 seed：inline 优先（由 LoopCtx 透传，DB 不存原文只存 hash）→ 否则
    // 从 DB 行的 `seed_review_path` 读文件。
    let seed = if let Some(inline) = ctx.seed_review_inline.as_deref().filter(|s| !s.is_empty()) {
        inline.to_string()
    } else {
        let row = match ctx.db.get_loop(ctx.loop_id) {
            Ok(Some(r)) => r,
            Ok(None) => return Err(anyhow::anyhow!("loop {} 行丢失", ctx.loop_id)),
            Err(e) => return Err(anyhow::anyhow!("读取 loop 行失败：{e}")),
        };
        let path = row.seed_review_path.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "ReviewSeed 入口未提供 seed_review_path / seed_review_inline（loop_id={})",
                ctx.loop_id
            )
        })?;
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读 seed_review_path 失败 {}：{e}", path))?
    };
    let wrapped = prompt::wrap_seed_for_claude(&seed);

    // round-1 codex_review_seed（verdict=needs_work），与真实 Codex 复核分流但参与 round 计数。
    ctx.log_msg(1, "codex_review_seed", Some("needs_work"), &wrapped);
    ctx.report(json!({
        "round": 1, "phase": "reviewed", "verdict": "needs_work", "source": "seed",
    }))
    .await;

    // step-confirm 闸门：seed → Claude 第一次下发也走 codex_to_claude 同一闸门（§3.3）。
    match confirm_gate(
        ctx,
        "codex_to_claude",
        "把外部 seed 复核意见发给 Claude Code 修订？",
        &wrapped,
    )
    .await
    {
        Gate::Approve => {}
        Gate::Reject => {
            ctx.finish(FinalVerdict::AbortedByUser, 0).await;
            return Ok(());
        }
        Gate::Timeout => {
            ctx.finish(FinalVerdict::AbortedTimeout, 0).await;
            return Ok(());
        }
    }

    // round-1 claude_revise：渲染 Claude 修订模板（target_role 已由调用方设置好）。
    let mut target = ctx.target.clone();
    let mut codex = ctx.codex.clone();
    let mut worktree_established = ctx.resume_from_worktree;
    let mut force_locator = false;
    let claude_first_turn = !ctx.established_claude;
    let mut claude_prompt = prompt::render_claude_prompt(
        prompt::DEFAULT_CLAUDE_TEMPLATE,
        &target,
        &wrapped,
        claude_first_turn,
        ctx.target_role,
        ctx.spec_doc.as_deref(),
    );
    if ctx.use_worktree && !worktree_established {
        claude_prompt.push_str(prompt::WORKTREE_INSTRUCTION);
    }
    let claude_reply = match send_and_resolve(ctx, &ctx.claude, &claude_prompt).await? {
        Resolved::Reply(r) => r,
        Resolved::Timeout => {
            ctx.finish(FinalVerdict::AbortedTimeout, 0).await;
            return Ok(());
        }
    };
    if ctx.use_worktree && !worktree_established {
        try_relocate_worktree(
            ctx,
            &claude_reply,
            1,
            &mut target,
            &mut codex,
            &mut worktree_established,
            &mut force_locator,
        );
    }
    ctx.log_msg(1, "claude_revise", None, &claude_reply);
    ctx.report(json!({ "round": 1, "phase": "revised" })).await;
    Ok(())
}

/// 循环顶层：按 `entry_kind` 做入口分发（§6.2）→ 跑 `loop_main`，基础设施错时 emit error；
/// 业务终态由 `loop_main` 自身 `finish` 处理。
async fn run_loop(ctx: LoopCtx) {
    log::info!(
        "[codeloop] 循环任务启动：claude={} codex={} target={} mode={:?} max_rounds={} \
         wait_idle={} entry_kind={:?}",
        ctx.claude.session_id,
        ctx.codex.session_id,
        ctx.target.repo_rel,
        ctx.mode,
        ctx.max_rounds,
        ctx.wait_for_claude_idle,
        ctx.entry_kind,
    );
    let start_round = match ctx.entry_kind {
        // DocReview / Implement / Continuation：从 round 1 起 Codex 复核。Implement 首步在
        // loop_main 内由 mode=Implementation 分支负责，Continuation 不会进入该分支。
        EntryKind::DocReview | EntryKind::Implement | EntryKind::Continuation => 1,
        EntryKind::ReviewSeed => {
            // 入口分发段：把 seed 包成 round-1 的 codex_review_seed + Claude 修订 round 1。
            // 之后由 loop_main 从 round 2 起接管真正的 Codex 复核。
            match dispatch_review_seed(&ctx).await {
                Ok(()) => 2,
                Err(e) => {
                    log::warn!("[codeloop] ReviewSeed 入口分发失败：{e:#}");
                    ctx.report(json!({ "phase": "error", "error": format!("{e:#}") }))
                        .await;
                    return;
                }
            }
        }
    };
    if let Err(e) = loop_main(&ctx, start_round).await {
        log::warn!("[codeloop] 基础设施错误，循环终止：{e:#}");
        // drive 返回 Err 不经 finish → 在此 finalize 为 failed（幂等 WHERE status='running'）。
        let total_rounds = ctx.db.recorded_rounds(ctx.loop_id).unwrap_or(0);
        if let Err(fe) = ctx.db.finalize(
            ctx.loop_id,
            "failed",
            None,
            total_rounds,
            Some(&format!("{e:#}")),
        ) {
            log::warn!("[codeloop] finalize(failed) 失败：{fe:#}");
        }
        ctx.report(json!({ "phase": "error", "error": format!("{e:#}") }))
            .await;
    }
    log::info!("[codeloop] 循环任务结束");
}

/// 把 Claude 回报的 worktree 路径校验后转成新的 `(TargetSpec, Codex cwd)`。
///
/// 信任规则（防 Claude 回报任意路径导致 workspace-write 的 Codex 越界读写）：
/// 1. 路径存在 + 是 git 工作树（`find_repo_root` 命中）；
/// 2. **同仓派生**（`git rev-parse --git-common-dir` 与 `origin_repo_root` 一致）
///    **或** 落在用户 home 下——满足其一即接受。
///
/// 旧版只允许 home 之下，会误拒 `D:\git\toolkit.worktrees\*` 这类同仓 sibling worktree
/// （见 docs/codeloop-attempt-model-design.md §3.3.6）。
fn relocate_to_worktree(
    worktree: &str,
    repo_rel: &str,
    origin_repo_root: &Path,
) -> std::result::Result<(TargetSpec, PathBuf), String> {
    let wt = PathBuf::from(worktree);
    if !wt.exists() {
        return Err("路径不存在".into());
    }
    let root = validate::find_repo_root(&wt).ok_or("不是 git 工作树（未找到 .git）")?;
    let canon = std::fs::canonicalize(&root).map_err(|e| format!("canonicalize 失败：{e}"))?;
    let same_repo = is_same_git_repo(&canon, origin_repo_root);
    let in_home_dir = in_home(&canon);
    if !same_repo && !in_home_dir {
        return Err("worktree 既不属于本仓也不在用户目录下".into());
    }
    let root_disp = validate::display_path(&root);
    let abs = root_disp.join(repo_rel);
    let target = TargetSpec {
        label: format!("worktree {repo_rel}"),
        repo_root: root_disp.to_string_lossy().to_string(),
        repo_rel: repo_rel.to_string(),
        abs: abs.to_string_lossy().replace('\\', "/"),
    };
    Ok((target, root_disp))
}

/// `path` 是否落在用户 home 目录下（canonicalize 后比较）。
fn in_home(path: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let home_canon = std::fs::canonicalize(&home).unwrap_or(home);
    path.starts_with(&home_canon)
}

/// 两个路径是否同属一个 git 仓库（比较 `git --git-common-dir`）。
fn is_same_git_repo(a: &Path, b: &Path) -> bool {
    match (git_common_dir(a), git_common_dir(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// `git -C <at> rev-parse --git-common-dir` → canonicalized PathBuf；任何错误返回 None。
fn git_common_dir(at: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(at)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let raw_path = PathBuf::from(&raw);
    let absolute = if raw_path.is_absolute() {
        raw_path
    } else {
        at.join(raw_path)
    };
    std::fs::canonicalize(&absolute).ok()
}

/// 把 DTO 解析成 SessionRef：cwd 缺省时从会话存储 snapshot 补全。
fn resolve_ref(store: &Store, provider: Provider, dto: &SessionRefDto) -> Result<SessionRef> {
    let cwd = match &dto.cwd {
        Some(c) if !c.is_empty() => PathBuf::from(c),
        _ => store.snapshot(provider, &dto.session_id)?.cwd,
    };
    Ok(SessionRef {
        provider,
        session_id: dto.session_id.clone(),
        cwd,
    })
}

/// 历史恢复：循环被停止跟踪后，Claude Code 可能仍在原会话继续实现并最终回报 WORKTREE。
/// 读取详情消息时尝试把这条完成回复补录进 loop_messages，刷新后即可看到实现输出并获得继续复核入口。
///
/// 触发约束(三重护栏防误补录,见 codeloop-headless-smoke-runner-plan §sync 缺陷):
/// 1. **必须是被显式停跟踪的实现记录**(`status='aborted' && final_verdict='stopped_tracking'`),
///    其它状态(running / done / aborted_timeout / 用户尚未停跟踪的)都不补;
/// 2. **必须没有 worktree_path**:从 worktree 继续的代码评审记录创建时 worktree_path 即非空,
///    `mode='implementation'` 但本来就**故意**不带 `claude_implement`,不能让它被补录污染;
/// 3. **transcript 上的 WORKTREE 消息必须晚于本 loop 的 `created_at`**:防止抓到本 loop
///    创建之前 Claude 在同一会话里报过的旧 WORKTREE。
fn sync_implementation_from_claude(db: &db::Db, store: &Store, loop_id: i64) -> Result<()> {
    let Some(row) = db.get_loop(loop_id)? else {
        return Ok(());
    };
    if row.mode != "implementation" {
        return Ok(());
    }
    // 护栏 0（多入口 §5 兼容表）：触发条件加一层 entry_kind=implement 限定（NULL 时按今天行为
    // 推断为 implement）。entry_kind='review_seed' 显式跳过此条 stopped_tracking 补录扫描——
    // ReviewSeed 同 mode=implementation 也无 claude_implement,会被误命中并伪造消息。
    let entry_kind_db = row.entry_kind.as_deref();
    let is_implement_entry = matches!(entry_kind_db, None | Some("implement"));
    if !is_implement_entry {
        return Ok(());
    }
    if db.has_message_kind(loop_id, "claude_implement")? {
        return Ok(());
    }
    // 护栏 2:跳过「从 worktree 继续的评审记录」。
    if row.worktree_path.is_some() {
        return Ok(());
    }
    // 护栏 1:仅当用户显式停跟踪过本记录才补录。
    if !(row.status == "aborted" && row.final_verdict.as_deref() == Some("stopped_tracking")) {
        return Ok(());
    }

    let page = store.messages(Provider::Claude, &row.claude_session, 0)?;
    // 护栏 3:transcript 消息 timestamp 必须 >= loop.created_at。两侧都是 RFC3339 UTC;
    // 用 chrono 解析做时序比较(直接字符串比 `+00:00` vs `Z` 不安全)。
    let loop_created = chrono::DateTime::parse_from_rfc3339(&row.created_at).ok();
    let Some(reply) = page
        .messages
        .iter()
        .rev()
        .filter(|m| m.role == "assistant")
        .filter(|m| {
            match (
                loop_created,
                chrono::DateTime::parse_from_rfc3339(&m.timestamp),
            ) {
                (Some(lc), Ok(mt)) => mt >= lc,
                // loop 或消息时间戳解析失败 → 保守不补录(fail-closed)。
                _ => false,
            }
        })
        .find(|m| parse::parse_worktree_path(&m.text).is_some())
        .map(|m| m.text.clone())
    else {
        return Ok(());
    };
    let Some(wt) = parse::parse_worktree_path(&reply) else {
        return Ok(());
    };

    match relocate_to_worktree(&wt, &row.target_repo_rel, Path::new(&row.repo_root)) {
        Ok(_) => {
            db.set_worktree(loop_id, &wt)?;
            db.append_message(
                loop_id,
                0,
                "system",
                None,
                &format!(
                    "从 Claude transcript 补录实现完成结果；检测到 WORKTREE: {wt}。可从该 worktree 继续 Codex 复核。"
                ),
            )?;
            db.append_message(loop_id, 0, "claude_implement", None, &reply)?;
        }
        Err(e) => {
            db.append_message(
                loop_id,
                0,
                "system",
                None,
                &format!("从 Claude transcript 发现 WORKTREE: {wt}，但路径校验失败：{e}"),
            )?;
        }
    }
    Ok(())
}

// ------------------------- Tauri 命令 -------------------------

/// 列出本机 Codex / Claude 会话清单（供前端配对挑选）。
#[tauri::command]
pub async fn codeloop_list_sessions(limit: Option<usize>) -> Result<Vec<SessionSummary>, String> {
    let store = Store::from_env()
        .map_err(|e| format!("定位会话存储失败（~/.codex / ~/.claude）：{e:#}"))?;
    store
        .list(limit.unwrap_or(30))
        .map_err(|e| format!("{e:#}"))
}

/// 新建 Codex 会话的种子提示词（仅用于建立会话；真正的复核任务由循环后续发起）。
const NEW_CODEX_SEED: &str =
    "你好。这是一个用于跨会话复核的新会话，已就绪。请回复「已就绪」，等待后续复核任务。";

/// 新建 Codex 会话时嵌入的设计文档体积上限（256 KiB）。超出报错——既防止 prompt 爆炸，
/// 也保护 `codex exec` 单轮 stdin。要复核超大文档应改走 Continuation + 手动建会话。
const NEW_CODEX_DOC_MAX_BYTES: u64 = 256 * 1024;

/// 新建一个 Codex 会话：复用所选 Claude 会话的 cwd（同一仓库）跑一轮 `codex exec` 建会话，
/// 返回新会话 id（前端据此选中 + 刷新清单）。**消耗 codex 额度**。
///
/// `design_doc_path` 可选：
/// - `None`/空 → 用默认种子（仅"已就绪"，不带文档）。
/// - 提供路径 → 读文档原文，喂给 [`prompt::NEW_CODEX_WITH_DOC_TEMPLATE`]，同时在 establishing
///   阶段就声明 VERDICT / ASK_USER 输出契约，避免续跑首轮再发协议块。
///
/// 路径解释：绝对路径直接用；相对路径**相对 Claude 会话的 cwd**解析。读后做常规文件 +
/// 非空 + 体积阈值校验。
#[tauri::command]
pub async fn codeloop_new_codex_session(
    claude_session_id: String,
    #[allow(non_snake_case)] design_doc_path: Option<String>,
) -> Result<String, String> {
    let store = Store::from_env().map_err(|e| format!("定位会话存储失败：{e:#}"))?;
    let snap = store
        .snapshot(Provider::Claude, &claude_session_id)
        .map_err(|e| format!("读取所选 Claude 会话的仓库目录失败：{e:#}"))?;

    let establishing_prompt = match design_doc_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => NEW_CODEX_SEED.to_string(),
        Some(p) => {
            let raw = Path::new(p);
            let abs = if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                snap.cwd.join(raw)
            };
            let meta = std::fs::metadata(&abs)
                .map_err(|e| format!("design_doc_path 不可读 {}：{e}", abs.display()))?;
            if !meta.is_file() {
                return Err(format!("design_doc_path 不是常规文件：{}", abs.display()));
            }
            if meta.len() == 0 {
                return Err(format!("design_doc_path 为空文件：{}", abs.display()));
            }
            if meta.len() > NEW_CODEX_DOC_MAX_BYTES {
                return Err(format!(
                    "design_doc_path 超过 {}KiB（实际 {} 字节）；超大文档请改走 Continuation 入口",
                    NEW_CODEX_DOC_MAX_BYTES / 1024,
                    meta.len()
                ));
            }
            let content = std::fs::read_to_string(&abs)
                .map_err(|e| format!("读取 design_doc_path 失败 {}：{e}", abs.display()))?;
            prompt::render_new_codex_with_doc(&content)
        }
    };

    driver::create_codex_session(&snap.cwd, &establishing_prompt)
        .await
        .map_err(|e| format!("新建 Codex 会话失败（codex CLI 是否在 PATH？）：{e:#}"))
}

/// 增量取某会话消息（cursor = 已读行数）。
#[tauri::command]
pub async fn codeloop_session_messages(
    provider: String,
    session_id: String,
    after: usize,
) -> Result<MessagesPage, String> {
    let p =
        Provider::parse(&provider).ok_or_else(|| "provider 必须是 codex 或 claude".to_string())?;
    let store = Store::from_env().map_err(|e| format!("定位会话存储失败：{e:#}"))?;
    store
        .messages(p, &session_id, after)
        .map_err(|e| format!("{e:#}"))
}

/// 向单个会话发一轮消息（预览交互台 / 首轮预热）。返回回复文本。
///
/// **与循环同等权力**（Codex workspace-write / Claude acceptEdits），是手动驱动台而非只读预览。
/// 用于预热时由调用方预填首轮提示词；发完该端可在启动时勾选 established 跳过首轮重发。
#[tauri::command]
pub async fn codeloop_send_one(
    provider: String,
    session_id: String,
    text: String,
) -> Result<String, String> {
    let p =
        Provider::parse(&provider).ok_or_else(|| "provider 必须是 codex 或 claude".to_string())?;
    let store = Store::from_env().map_err(|e| format!("定位会话存储失败：{e:#}"))?;
    let session = resolve_ref(
        &store,
        p,
        &SessionRefDto {
            session_id,
            cwd: None,
        },
    )
    .map_err(|e| format!("解析会话失败：{e:#}"))?;
    let turn = driver::send(&session, &text)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(turn.reply_text)
}

/// 启动一对会话的复核循环。三方一致性校验通过后 spawn 后台循环。
#[tauri::command]
pub async fn codeloop_start(
    app: AppHandle,
    state: State<'_, AppState>,
    input: StartInput,
) -> Result<i64, String> {
    let cs = &state.codeloop;

    // ---- 多入口字段校验（§6.1）：失败直接返回错误，不写 loops 行 ----
    let entry_kind = infer_entry_kind(input.entry_kind, input.mode);
    validate_entry_fields(
        entry_kind,
        input.mode,
        input.design_doc_path.as_deref(),
        input.seed_review_path.as_deref(),
        input.seed_review_inline.as_deref(),
    )?;
    if entry_kind == EntryKind::ReviewSeed {
        validate_review_seed_inputs(
            input.seed_review_path.as_deref(),
            input.seed_review_inline.as_deref(),
        )?;
    }

    let store = Store::from_env()
        .map_err(|e| format!("定位会话存储失败（~/.codex / ~/.claude）：{e:#}"))?;
    let claude = resolve_ref(&store, Provider::Claude, &input.claude)
        .map_err(|e| format!("解析 Claude 会话失败：{e:#}"))?;
    let mut codex = resolve_ref(&store, Provider::Codex, &input.codex)
        .map_err(|e| format!("解析 Codex 会话失败：{e:#}"))?;

    // 并发安全：同一对会话同一时刻只允许一个循环占用（顺手清理已结束的句柄）。
    let claude_sid = claude.session_id.clone();
    let codex_sid = codex.session_id.clone();
    {
        let mut guard = cs.inner.lock().await;
        guard.retain(|_, rl| !rl.handle.is_finished());
        if let Some(rl) = guard
            .values()
            .find(|rl| rl.claude_session == claude_sid || rl.codex_session == codex_sid)
        {
            return Err(format!(
                "该会话已被复核循环 #{} 占用，请勿对同一会话并发跑多个循环",
                rl.loop_id
            ));
        }
    }

    // 仓库一致性校验：
    //   - Continuation 入口（target_path 缺省）→ 仅两端 cwd 同根校验，target 视同仓根本身。
    //   - 其它入口 → 沿用三方校验（claude.cwd / codex.cwd / target_path 三者同根 + target 落仓内）。
    let target_path_str = input
        .target_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if entry_kind != EntryKind::Continuation && target_path_str.is_none() {
        return Err("该入口需要指定 target_path（仅 Continuation 入口允许留空）".into());
    }
    let (repo_root, target_abs, repo_rel) = match target_path_str {
        Some(tp) => {
            let validated = validate::validate_three_way(&claude.cwd, &codex.cwd, tp)
                .map_err(|e| format!("{e:#}"))?;
            let repo_root = validate::display_path(&validated.repo_root);
            let target_abs = validate::display_path(&validated.target_abs);
            let repo_rel = validated
                .target_abs
                .strip_prefix(&validated.repo_root)
                .unwrap_or(&validated.target_abs)
                .to_string_lossy()
                .replace('\\', "/");
            (repo_root, target_abs, repo_rel)
        }
        None => {
            // Continuation：只校仓根一致；target 占位为仓根（prompt 不会引用）。
            let root = validate::validate_two_way(&claude.cwd, &codex.cwd)
                .map_err(|e| format!("{e:#}"))?;
            let repo_root = validate::display_path(&root);
            (repo_root.clone(), repo_root, String::new())
        }
    };

    // Codex `exec resume` 的 --cd 用工作树根，消除子目录相对路径歧义；Claude resume 保持原 cwd。
    codex.cwd = repo_root.clone();

    let label = input.target_label.unwrap_or_else(|| {
        if entry_kind == EntryKind::Continuation {
            // Continuation 不在 prompt 里用 label，仅用于列表/详情展示。
            let repo_name = std::path::Path::new(&repo_root)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "continuation".to_string());
            format!("继续讨论（{repo_name}）")
        } else {
            prompt::default_label(&repo_rel)
        }
    });
    let mut target = TargetSpec {
        label,
        repo_root: repo_root.to_string_lossy().to_string(),
        repo_rel,
        abs: target_abs.to_string_lossy().to_string(),
    };

    let resume_worktree_path = input
        .resume_worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    if resume_worktree_path.is_some() && input.mode != ReviewMode::Implementation {
        return Err("只有 implementation 模式支持从 worktree 继续复核".into());
    }
    if let Some(wt) = resume_worktree_path.as_deref() {
        let (new_target, new_cwd) = relocate_to_worktree(wt, &target.repo_rel, &codex.cwd)
            .map_err(|e| format!("worktree 路径校验失败：{e}"))?;
        target = new_target;
        codex.cwd = new_cwd;
    }

    // ---- 多入口（§6.3）：target_role / spec_doc / seed 计算 ----
    let spec_doc_abs: Option<PathBuf> =
        if entry_kind == EntryKind::ReviewSeed && input.mode == ReviewMode::Implementation {
            match input.design_doc_path.as_deref().filter(|s| !s.is_empty()) {
                Some(p) => Some(
                    resolve_design_doc(Path::new(&target.repo_root), p)
                        .map_err(|e| format!("design_doc_path 校验失败：{e}"))?,
                ),
                None => None,
            }
        } else {
            None
        };
    let target_role = match (entry_kind, input.mode) {
        (EntryKind::DocReview, _) => TargetRole::RevisionDoc,
        (EntryKind::Implement, _) => TargetRole::SpecDoc,
        (EntryKind::ReviewSeed, ReviewMode::Design) => TargetRole::RevisionDoc,
        (EntryKind::ReviewSeed, ReviewMode::Implementation) => TargetRole::RevisionCode,
        // Continuation 不会渲染 Locator（loop_main 里直接走 continuation 模板）；role 仅占位。
        (EntryKind::Continuation, _) => TargetRole::RevisionDoc,
    };
    let seed_inline_hash_val = input
        .seed_review_inline
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(seed_inline_hash);
    let design_doc_db = spec_doc_abs.as_ref().map(|p| {
        // 落库存仓内相对路径（正斜杠），UI 直观。
        let rel = p
            .strip_prefix(Path::new(&target.repo_root))
            .map(|q| q.to_path_buf())
            .unwrap_or_else(|_| p.clone());
        rel.to_string_lossy().replace('\\', "/")
    });

    // 持久化为一条 running 记录（前端列表/详情据此呈现），拿到 loop_id。
    let db = cs.db.clone();
    let mode_str = match input.mode {
        ReviewMode::Design => "design",
        ReviewMode::Implementation => "implementation",
    };
    let loop_id = db
        .insert_loop(&db::NewLoop {
            claude_session: claude.session_id.clone(),
            codex_session: codex.session_id.clone(),
            claude_cwd: claude.cwd.to_string_lossy().to_string(),
            codex_cwd: codex.cwd.to_string_lossy().to_string(),
            repo_root: target.repo_root.clone(),
            target_repo_rel: target.repo_rel.clone(),
            target_abs: target.abs.clone(),
            target_label: target.label.clone(),
            mode: mode_str.to_string(),
            max_rounds: input.max_rounds.max(1) as i64,
            wait_for_idle: input.wait_for_claude_idle,
            step_confirm: input.step_confirm,
            use_worktree: input.use_worktree || resume_worktree_path.is_some(),
            parent_loop_id: input.parent_loop_id,
            entry_kind: Some(entry_kind_str(entry_kind).to_string()),
            design_doc_path: design_doc_db,
            seed_review_path: input.seed_review_path.clone(),
            seed_review_inline_hash: seed_inline_hash_val,
        })
        .map_err(|e| format!("写入复核记录失败：{e:#}"))?;
    if let Some(wt) = resume_worktree_path.as_deref() {
        db.set_worktree(loop_id, wt)
            .map_err(|e| format!("写入 worktree 路径失败：{e:#}"))?;
        db.append_message(
            loop_id,
            0,
            "system",
            None,
            &format!(
                "从已完成 worktree 继续复核：{wt}。已跳过 Claude 实现首步，直接进入 Codex 复核。"
            ),
        )
        .map_err(|e| format!("写入继续复核记录失败：{e:#}"))?;
    }

    let progress = Arc::new(Mutex::new(json!({ "phase": "starting" })));
    let pending: Arc<Mutex<Option<Pending>>> = Arc::new(Mutex::new(None));
    let pending_confirm: Arc<Mutex<Option<PendingConfirm>>> = Arc::new(Mutex::new(None));
    let step_confirm = Arc::new(AtomicBool::new(input.step_confirm));
    let seq = Arc::new(AtomicI64::new(0));

    let events: Arc<dyn LoopEvents> = Arc::new(TauriLoopEvents::new(app.clone()));
    let confirm: Arc<dyn ConfirmPolicy> = Arc::new(UiConfirmPolicy {
        pending: pending.clone(),
        pending_confirm: pending_confirm.clone(),
        step_confirm: step_confirm.clone(),
        answer_timeout: ANSWER_TIMEOUT,
    });

    let ctx = LoopCtx {
        events,
        confirm,
        store,
        db: db.clone(),
        loop_id,
        claude,
        codex,
        target,
        mode: input.mode,
        max_rounds: input.max_rounds.max(1),
        wait_for_claude_idle: input.wait_for_claude_idle,
        use_worktree: input.use_worktree || resume_worktree_path.is_some(),
        resume_from_worktree: resume_worktree_path.is_some(),
        established_codex: input.established.codex,
        established_claude: input.established.claude,
        progress: progress.clone(),
        seq,
        entry_kind,
        target_role,
        spec_doc: spec_doc_abs,
        seed_review_inline: input.seed_review_inline.clone(),
        is_continue: false,
        evaluate_alternatives: input.evaluate_alternatives,
    };

    let handle = tokio::spawn(run_loop(ctx));
    cs.inner.lock().await.insert(
        loop_id,
        RunningLoop {
            handle,
            loop_id,
            claude_session: claude_sid,
            codex_session: codex_sid,
            progress,
            pending,
            pending_confirm,
            step_confirm,
        },
    );
    Ok(loop_id)
}

/// 单个运行中循环的状态快照（供前端 mount 时重建并发态）。
#[derive(Debug, Clone, Serialize)]
pub struct RunningSnapshot {
    loop_id: i64,
    progress: Value,
    /// 当前逐步确认开关（运行时可翻转）；false = 已转全自动。
    step_confirm: bool,
}

/// 所有运行中循环的状态快照列表（顺手清理已结束的句柄）。
#[tauri::command]
pub async fn codeloop_status(state: State<'_, AppState>) -> Result<Vec<RunningSnapshot>, String> {
    let mut guard = state.codeloop.inner.lock().await;
    guard.retain(|_, rl| !rl.handle.is_finished());
    let mut out = Vec::with_capacity(guard.len());
    for rl in guard.values() {
        out.push(RunningSnapshot {
            loop_id: rl.loop_id,
            progress: rl.progress.lock().await.clone(),
            step_confirm: rl.step_confirm.load(Ordering::SeqCst),
        });
    }
    Ok(out)
}

// ------------------------- 启动前自检（preflight） -------------------------

/// 一条自检结果。`tier`：passive(被动) | version(版本探针) | live(实发往返)。
/// `status`：pass | fail | warn | skipped。`raw_excerpt`：失败/异常时的原始片段（排障）。
#[derive(Debug, Clone, Serialize)]
pub struct CheckRow {
    id: String,
    label: String,
    tier: String,
    status: String,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_excerpt: Option<String>,
}

fn check(id: &str, label: &str, tier: &str, status: &str, detail: String) -> CheckRow {
    CheckRow {
        id: id.into(),
        label: label.into(),
        tier: tier.into(),
        status: status.into(),
        detail,
        raw_excerpt: None,
    }
}

/// 启动前自检：被动检查（会话可定位 / 三方一致性）+ 版本探针（codex/claude --version）+
/// 可选实发往返（合成探针，一次性只读会话发 PROBE_OK 验全链路）。逐项跑、逐项返回，
/// 任一失败不阻断其余检查。见 docs/codeloop-cli-resilience-design.md §4。
#[tauri::command]
pub async fn codeloop_preflight(input: StartInput, live: bool) -> Result<Vec<CheckRow>, String> {
    let mut rows: Vec<CheckRow> = Vec::new();

    // 多入口字段校验（§6.4 第 2 条）：放在最前面，作为 passive 自检的一部分。
    let entry_kind = infer_entry_kind(input.entry_kind, input.mode);
    match validate_entry_fields(
        entry_kind,
        input.mode,
        input.design_doc_path.as_deref(),
        input.seed_review_path.as_deref(),
        input.seed_review_inline.as_deref(),
    ) {
        Ok(()) => rows.push(check(
            "entry_kind",
            "多入口字段一致性",
            "passive",
            "pass",
            format!("entry_kind={:?} mode={:?}", entry_kind, input.mode),
        )),
        Err(e) => rows.push(check(
            "entry_kind",
            "多入口字段一致性",
            "passive",
            "fail",
            e,
        )),
    }
    if entry_kind == EntryKind::ReviewSeed {
        match validate_review_seed_inputs(
            input.seed_review_path.as_deref(),
            input.seed_review_inline.as_deref(),
        ) {
            Ok(()) => rows.push(check(
                "review_seed",
                "ReviewSeed 输入可读且大小合规",
                "passive",
                "pass",
                "seed_review_path/inline 一项有值，体积合规".into(),
            )),
            Err(e) => rows.push(check(
                "review_seed",
                "ReviewSeed 输入可读且大小合规",
                "passive",
                "fail",
                e,
            )),
        }
    }

    let store = match Store::from_env() {
        Ok(s) => s,
        Err(e) => {
            rows.push(check(
                "store",
                "定位会话存储（~/.codex / ~/.claude）",
                "passive",
                "fail",
                format!("{e:#}"),
            ));
            return Ok(rows);
        }
    };

    // 被动：两端会话可定位 + 解出 cwd。
    let claude = resolve_ref(&store, Provider::Claude, &input.claude);
    match &claude {
        Ok(r) => rows.push(check(
            "claude_locate",
            "Claude 会话可定位",
            "passive",
            "pass",
            format!("cwd={}", r.cwd.display()),
        )),
        Err(e) => rows.push(check(
            "claude_locate",
            "Claude 会话可定位",
            "passive",
            "fail",
            format!("{e:#}"),
        )),
    }
    let codex = resolve_ref(&store, Provider::Codex, &input.codex);
    match &codex {
        Ok(r) => rows.push(check(
            "codex_locate",
            "Codex 会话可定位",
            "passive",
            "pass",
            format!("cwd={}", r.cwd.display()),
        )),
        Err(e) => rows.push(check(
            "codex_locate",
            "Codex 会话可定位",
            "passive",
            "fail",
            format!("{e:#}"),
        )),
    }

    // 被动：仓库一致性（两端都定位到才校验）。Continuation 入口仅校两端同根。
    let target_path_str = input
        .target_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (&claude, &codex) {
        (Ok(c), Ok(x)) => {
            let result = match target_path_str {
                Some(tp) => validate::validate_three_way(&c.cwd, &x.cwd, tp).map(|_| ()),
                None => validate::validate_two_way(&c.cwd, &x.cwd).map(|_| ()),
            };
            match result {
                Ok(()) => rows.push(check(
                    "three_way",
                    "仓库一致性",
                    "passive",
                    "pass",
                    if target_path_str.is_some() {
                        "claude / codex / target 同仓".into()
                    } else {
                        "claude / codex 同仓（Continuation：无 target）".into()
                    },
                )),
                Err(e) => rows.push(check(
                    "three_way",
                    "仓库一致性",
                    "passive",
                    "fail",
                    format!("{e:#}"),
                )),
            }
        }
        _ => rows.push(check(
            "three_way",
            "仓库一致性",
            "passive",
            "skipped",
            "会话未定位，跳过".into(),
        )),
    }

    // 版本探针：验证 PATH / npm shim 解析 / 可 spawn（C4/C5）。
    match driver::probe_version("codex").await {
        Ok(v) => rows.push(check(
            "codex_version",
            "codex --version",
            "version",
            "pass",
            v,
        )),
        Err(e) => rows.push(check(
            "codex_version",
            "codex --version",
            "version",
            "fail",
            format!("{e:#}"),
        )),
    }
    match driver::probe_version("claude").await {
        Ok(v) => rows.push(check(
            "claude_version",
            "claude --version",
            "version",
            "pass",
            v,
        )),
        Err(e) => rows.push(check(
            "claude_version",
            "claude --version",
            "version",
            "fail",
            format!("{e:#}"),
        )),
    }

    // 实发往返（合成探针）：仅在用户勾选时跑。一次性只读会话，不污染真实会话。
    const PROBE: &str = "只回复 PROBE_OK 这一个词，不要执行任何其它操作。";
    if live {
        match &codex {
            Ok(x) => {
                let row = match driver::probe_codex_synthetic(&x.cwd, PROBE).await {
                    Ok(reply) if reply.contains("PROBE_OK") => check(
                        "codex_live",
                        "Codex 实发往返（合成探针）",
                        "live",
                        "pass",
                        "回复含 PROBE_OK，新建路径全链路通".into(),
                    ),
                    Ok(reply) => {
                        let mut r = check(
                            "codex_live",
                            "Codex 实发往返（合成探针）",
                            "live",
                            "warn",
                            "已收到回复但未含 PROBE_OK（输出格式可能变更）".into(),
                        );
                        r.raw_excerpt = Some(reply.chars().take(500).collect());
                        r
                    }
                    Err(e) => check(
                        "codex_live",
                        "Codex 实发往返（合成探针）",
                        "live",
                        "fail",
                        format!("{e:#}"),
                    ),
                };
                rows.push(row);
            }
            Err(_) => rows.push(check(
                "codex_live",
                "Codex 实发往返（合成探针）",
                "live",
                "skipped",
                "会话未定位，跳过".into(),
            )),
        }
        match &claude {
            Ok(c) => {
                let row = match driver::probe_claude_synthetic(&c.cwd, PROBE).await {
                    Ok(reply) if reply.contains("PROBE_OK") => check(
                        "claude_live",
                        "Claude 实发往返（合成探针）",
                        "live",
                        "pass",
                        "回复含 PROBE_OK，新建路径全链路通".into(),
                    ),
                    Ok(reply) => {
                        let mut r = check(
                            "claude_live",
                            "Claude 实发往返（合成探针）",
                            "live",
                            "warn",
                            "已收到回复但未含 PROBE_OK（输出格式可能变更）".into(),
                        );
                        r.raw_excerpt = Some(reply.chars().take(500).collect());
                        r
                    }
                    Err(e) => check(
                        "claude_live",
                        "Claude 实发往返（合成探针）",
                        "live",
                        "fail",
                        format!("{e:#}"),
                    ),
                };
                rows.push(row);
            }
            Err(_) => rows.push(check(
                "claude_live",
                "Claude 实发往返（合成探针）",
                "live",
                "skipped",
                "会话未定位，跳过".into(),
            )),
        }
    } else {
        rows.push(check(
            "codex_live",
            "Codex 实发往返（合成探针）",
            "live",
            "skipped",
            "未勾选实发探针".into(),
        ));
        rows.push(check(
            "claude_live",
            "Claude 实发往返（合成探针）",
            "live",
            "skipped",
            "未勾选实发探针".into(),
        ));
    }

    Ok(rows)
}

/// 运行时翻转自动确认：`enabled=true` 关掉逐步确认转全自动；`false` 恢复逐步确认。
///
/// 转自动那一刻若正好有确认门挂着，顺手以 Approve 放行（否则循环卡在那一步等再也不会来的点击）。
/// **只动 pending_confirm，绝不碰 pending（ASK_USER）**——ASK_USER 是真需人拍板的岔路，
/// 即便全自动也必须照停（见 docs/codeloop-cli-resilience-design.md §6）。
#[tauri::command]
pub async fn codeloop_set_auto_confirm(
    state: State<'_, AppState>,
    loop_id: i64,
    enabled: bool,
) -> Result<(), String> {
    let guard = state.codeloop.inner.lock().await;
    let Some(rl) = guard.get(&loop_id) else {
        return Err("没有运行中的复核循环".into());
    };
    // enabled=自动 → step_confirm=false。
    rl.step_confirm.store(!enabled, Ordering::SeqCst);
    if enabled {
        // 放行当前挂着的确认门（若有），避免卡死。
        if let Some(p) = rl.pending_confirm.lock().await.take() {
            let _ = p.decide_tx.send(true);
        }
    }
    Ok(())
}

/// 回答挂起的 ASK_USER：唤醒循环。
#[tauri::command]
pub async fn codeloop_answer(
    state: State<'_, AppState>,
    loop_id: i64,
    seq: i64,
    text: String,
) -> Result<(), String> {
    let guard = state.codeloop.inner.lock().await;
    let Some(rl) = guard.get(&loop_id) else {
        return Err("没有运行中的复核循环".into());
    };
    let mut pending = rl.pending.lock().await;
    match pending.take() {
        Some(p) if p.seq == seq => p
            .answer_tx
            .send(text)
            .map_err(|_| "循环已不在等待该回答".to_string()),
        Some(other) => {
            // seq 不匹配，放回。
            *pending = Some(other);
            Err("seq 与当前待答问题不匹配".into())
        }
        None => Err("当前没有待回答的问题".into()),
    }
}

/// 拍板挂起的逐步确认门：`approve=true` 放行传递，`false` 否决（→ 循环按用户中止收尾）。
#[tauri::command]
pub async fn codeloop_confirm(
    state: State<'_, AppState>,
    loop_id: i64,
    seq: i64,
    approve: bool,
) -> Result<(), String> {
    let guard = state.codeloop.inner.lock().await;
    let Some(rl) = guard.get(&loop_id) else {
        return Err("没有运行中的复核循环".into());
    };
    let mut pending = rl.pending_confirm.lock().await;
    match pending.take() {
        Some(p) if p.seq == seq => p
            .decide_tx
            .send(approve)
            .map_err(|_| "循环已不在等待该确认".to_string()),
        Some(other) => {
            *pending = Some(other);
            Err("seq 与当前待确认项不匹配".into())
        }
        None => Err("当前没有待确认的传递".into()),
    }
}

/// 停止指定循环（abort 后台任务，从并发表移除，记录终态置 stopped_tracking）。
#[tauri::command]
pub async fn codeloop_stop(state: State<'_, AppState>, loop_id: i64) -> Result<(), String> {
    let mut guard = state.codeloop.inner.lock().await;
    if let Some(rl) = guard.remove(&loop_id) {
        let progress = rl.progress.lock().await.clone();
        rl.handle.abort();
        // abort 只停止 Zero Desktop 侧的等待任务；已发给外部 CLI 的长任务可能仍在对应会话里继续。
        let phase = progress
            .get("phase")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let _ = state.codeloop.db.append_message(
            loop_id,
            0,
            "system",
            None,
            &format!(
                "已停止 Zero Desktop 侧跟踪（停止时 phase={phase}）。已发给 Claude/Codex CLI 的当前任务可能仍在对应会话继续运行；请以外部会话最新 transcript/worktree 为准。"
            ),
        );
        // abort 是硬终止，任务内 finish 不保证执行 → 在此显式 finalize（幂等）。
        let total_rounds = state.codeloop.db.recorded_rounds(loop_id).unwrap_or(0);
        let _ = state.codeloop.db.finalize(
            loop_id,
            "aborted",
            Some("stopped_tracking"),
            total_rounds,
            None,
        );
    }
    Ok(())
}

/// 「继续」一条已停 / 失败 / 完成的 loop：不新建记录，原地翻转 status 回 running、
/// attempts_count +1，并按 last_phase 决定是否跳过实现首步。
///
/// 见 docs/codeloop-attempt-model-design.md §4.2 / §4.6。
#[tauri::command]
pub async fn codeloop_continue(
    app: AppHandle,
    state: State<'_, AppState>,
    loop_id: i64,
) -> Result<(), String> {
    let cs = &state.codeloop;
    let db = cs.db.clone();

    // 防双发：当前已在跑就拒。
    {
        let guard = cs.inner.lock().await;
        if guard.contains_key(&loop_id) {
            return Err(format!("loop #{loop_id} 仍在跟踪中，请先停止再继续"));
        }
    }

    let row = db
        .get_loop(loop_id)
        .map_err(|e| format!("读取 loop 失败：{e:#}"))?
        .ok_or_else(|| format!("loop #{loop_id} 不存在"))?;

    let mode = match row.mode.as_str() {
        "design" => ReviewMode::Design,
        "implementation" => ReviewMode::Implementation,
        other => return Err(format!("loop #{loop_id} mode={other} 不支持续跑")),
    };
    let entry_kind = match row.entry_kind.as_deref().unwrap_or("") {
        "doc_review" => EntryKind::DocReview,
        "implement" => EntryKind::Implement,
        "review_seed" => EntryKind::ReviewSeed,
        "continuation" => EntryKind::Continuation,
        _ => match mode {
            ReviewMode::Design => EntryKind::DocReview,
            ReviewMode::Implementation => EntryKind::Implement,
        },
    };
    // ReviewSeed + inline seed：DB 只存 hash 不存原文，无法自动续跑（design doc §4.6 边界）。
    if entry_kind == EntryKind::ReviewSeed
        && row.seed_review_inline_hash.is_some()
        && row
            .seed_review_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_none()
    {
        return Err(
            "该 loop 使用 inline ReviewSeed，DB 未保存原文，无法自动继续；请新建一条".into(),
        );
    }
    let target_role = match (entry_kind, mode) {
        (EntryKind::DocReview, _) => TargetRole::RevisionDoc,
        (EntryKind::Implement, _) => TargetRole::SpecDoc,
        (EntryKind::ReviewSeed, ReviewMode::Design) => TargetRole::RevisionDoc,
        (EntryKind::ReviewSeed, ReviewMode::Implementation) => TargetRole::RevisionCode,
        (EntryKind::Continuation, _) => TargetRole::RevisionDoc,
    };

    let store = Store::from_env().map_err(|e| format!("定位会话存储失败：{e:#}"))?;
    let claude_dto = SessionRefDto {
        session_id: row.claude_session.clone(),
        cwd: None,
    };
    let codex_dto = SessionRefDto {
        session_id: row.codex_session.clone(),
        cwd: None,
    };
    let claude = resolve_ref(&store, Provider::Claude, &claude_dto)
        .map_err(|e| format!("解析 Claude session 失败：{e:#}"))?;
    let mut codex = resolve_ref(&store, Provider::Codex, &codex_dto)
        .map_err(|e| format!("解析 Codex session 失败：{e:#}"))?;

    let mut target = TargetSpec {
        label: row.target_label.clone(),
        repo_root: row.repo_root.clone(),
        repo_rel: row.target_repo_rel.clone(),
        abs: row.target_abs.clone(),
    };
    codex.cwd = PathBuf::from(&row.repo_root);

    // worktree_path 已落库 → 把 codex --cd / target 重定位到它（让 Codex 在 worktree 内复核）。
    if let Some(wt) = row.worktree_path.as_deref().filter(|s| !s.is_empty()) {
        match relocate_to_worktree(wt, &target.repo_rel, &codex.cwd) {
            Ok((new_target, new_cwd)) => {
                target = new_target;
                codex.cwd = new_cwd;
            }
            Err(e) => {
                log::warn!("[codeloop] 续跑时 worktree 校验失败（{e}），回退到 repo_root");
            }
        }
    }

    let spec_doc_abs: Option<PathBuf> = row
        .design_doc_path
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|p| resolve_design_doc(Path::new(&target.repo_root), p).ok());

    // 决定是否跳过实现首步：last_phase 至少到了 implemented 且 worktree_path 已落库。
    let last_phase = row.last_phase.clone().unwrap_or_default();
    let resume_from_wt = matches!(
        last_phase.as_str(),
        "implemented" | "codex_review" | "claude_revise" | "finalized"
    ) && row.worktree_path.is_some();

    // reset_for_continue 内部校验当前 status ∈ {aborted, failed, done}。
    let new_count = db
        .reset_for_continue(loop_id)
        .map_err(|e| format!("准备续跑失败：{e:#}"))?;
    let _ = db.append_message(
        loop_id,
        0,
        "system",
        None,
        &format!(
            "重新继续（第 {new_count} 次尝试，上次停在 last_phase={}）。{}",
            if last_phase.is_empty() {
                "未知"
            } else {
                last_phase.as_str()
            },
            if resume_from_wt {
                "已跳过实现首步，直接进入 Codex 复核。"
            } else {
                "将从实现首步继续。"
            }
        ),
    );

    let progress = Arc::new(Mutex::new(json!({ "phase": "resuming" })));
    let pending: Arc<Mutex<Option<Pending>>> = Arc::new(Mutex::new(None));
    let pending_confirm: Arc<Mutex<Option<PendingConfirm>>> = Arc::new(Mutex::new(None));
    let step_confirm = Arc::new(AtomicBool::new(row.step_confirm));
    let seq = Arc::new(AtomicI64::new(0));

    let events: Arc<dyn LoopEvents> = Arc::new(TauriLoopEvents::new(app.clone()));
    let confirm: Arc<dyn ConfirmPolicy> = Arc::new(UiConfirmPolicy {
        pending: pending.clone(),
        pending_confirm: pending_confirm.clone(),
        step_confirm: step_confirm.clone(),
        answer_timeout: ANSWER_TIMEOUT,
    });

    let claude_sid = claude.session_id.clone();
    let codex_sid = codex.session_id.clone();

    let ctx = LoopCtx {
        events,
        confirm,
        store,
        db: db.clone(),
        loop_id,
        claude,
        codex,
        target,
        mode,
        max_rounds: row.max_rounds.max(1) as u32,
        // 老记录没存 wait_for_idle，续跑时关掉预热（会话已稳定，不需要再 warm）。
        wait_for_claude_idle: false,
        use_worktree: row.use_worktree,
        resume_from_worktree: resume_from_wt,
        // 续跑：会话历史里 STANDING_BLOCK 已发过，双端都跳过。
        established_codex: true,
        established_claude: true,
        progress: progress.clone(),
        seq,
        entry_kind,
        target_role,
        spec_doc: spec_doc_abs,
        // inline seed 无法跨进程复现；上面已 reject 该路径。
        seed_review_inline: None,
        is_continue: true,
        // 续跑回落默认 SCOPE（不持久化），与设计一致。
        evaluate_alternatives: false,
    };

    let handle = tokio::spawn(run_loop(ctx));
    cs.inner.lock().await.insert(
        loop_id,
        RunningLoop {
            handle,
            loop_id,
            claude_session: claude_sid,
            codex_session: codex_sid,
            progress,
            pending,
            pending_confirm,
            step_confirm,
        },
    );
    Ok(())
}

/// 列出复核循环记录（按 id 倒序，最近优先）。
#[tauri::command]
pub async fn codeloop_list_loops(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<db::LoopRow>, String> {
    state
        .codeloop
        .db
        .list_loops(limit.unwrap_or(50))
        .map_err(|e| format!("{e:#}"))
}

/// 取某条记录的逐轮往返消息（codex_review / claude_revise / system）。
#[tauri::command]
pub async fn codeloop_loop_messages(
    state: State<'_, AppState>,
    loop_id: i64,
) -> Result<Vec<db::LoopMessageRow>, String> {
    if let Ok(store) = Store::from_env() {
        if let Err(e) = sync_implementation_from_claude(&state.codeloop.db, &store, loop_id) {
            log::warn!("[codeloop] 从 Claude transcript 补录实现消息失败：{e:#}");
        }
    }
    state
        .codeloop
        .db
        .loop_messages(loop_id)
        .map_err(|e| format!("{e:#}"))
}

/// 删除一条记录（连带其消息）。
#[tauri::command]
pub async fn codeloop_delete_loop(state: State<'_, AppState>, loop_id: i64) -> Result<(), String> {
    state
        .codeloop
        .db
        .delete_loop(loop_id)
        .map_err(|e| format!("{e:#}"))
}

/// 把记录关联的 worktree 合并回主仓库当前分支。
/// 流程：worktree 干净校验 → 仓库根干净 + 已知分支 → `git merge --no-ff <wt_commit>`。
/// 不自动切换分支：repo_root 当前 HEAD 是什么分支就合到哪个分支（通常 main）；用户需先 checkout。
/// 不删除 worktree，方便复查；用户可手动 `git worktree remove`。
#[tauri::command]
pub async fn codeloop_merge_worktree(
    state: State<'_, AppState>,
    loop_id: i64,
) -> Result<String, String> {
    let row = state
        .codeloop
        .db
        .get_loop(loop_id)
        .map_err(|e| format!("读取记录失败：{e:#}"))?
        .ok_or_else(|| "记录不存在".to_string())?;
    let worktree = row
        .worktree_path
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "该记录没有关联 worktree，无法合并".to_string())?;
    let repo_root = row.repo_root.as_str();
    let wt_path = std::path::PathBuf::from(worktree);
    if !wt_path.exists() {
        return Err(format!("worktree 路径不存在：{worktree}"));
    }
    let repo_path = std::path::PathBuf::from(repo_root);
    if !repo_path.exists() {
        return Err(format!("仓库根不存在：{repo_root}"));
    }
    if std::fs::canonicalize(&wt_path).ok() == std::fs::canonicalize(&repo_path).ok() {
        return Err("worktree 与仓库根是同一路径，没有需要合并的内容".into());
    }

    async fn git(cwd: &std::path::Path, args: &[&str]) -> std::result::Result<String, String> {
        let output = tokio::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .await
            .map_err(|e| format!("git {args:?} 启动失败：{e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(format!(
                "git {args:?} 失败：{}{}",
                stderr.trim(),
                if stdout.trim().is_empty() {
                    String::new()
                } else {
                    format!(" / {}", stdout.trim())
                }
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    let wt_commit = git(&wt_path, &["rev-parse", "HEAD"]).await?;
    let wt_branch = git(&wt_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .unwrap_or_else(|_| "(detached)".into());

    let wt_status = git(&wt_path, &["status", "--porcelain"]).await?;
    if !wt_status.is_empty() {
        return Err("worktree 有未提交改动，请先 commit 或 stash 后再合并".into());
    }
    let repo_status = git(&repo_path, &["status", "--porcelain"]).await?;
    if !repo_status.is_empty() {
        return Err("主仓库工作树非干净状态，请先 commit/stash 后再合并".into());
    }
    let repo_branch = git(&repo_path, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let repo_head = git(&repo_path, &["rev-parse", "HEAD"]).await?;

    if repo_head == wt_commit {
        return Ok(format!(
            "无需合并：{repo_branch} 已经是 {}",
            short(&wt_commit)
        ));
    }
    // is-ancestor 返回 0 表示是祖先（已被包含在 HEAD 历史里）；用 status 判断
    let is_anc = tokio::process::Command::new("git")
        .current_dir(&repo_path)
        .args(["merge-base", "--is-ancestor", &wt_commit, &repo_head])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);
    if is_anc {
        return Ok(format!(
            "无需合并：{} 已在 {} 历史中",
            short(&wt_commit),
            repo_branch
        ));
    }

    let msg = format!("merge worktree from codeloop #{loop_id} ({wt_branch})");
    git(&repo_path, &["merge", "--no-ff", "-m", &msg, &wt_commit]).await?;
    let new_head = git(&repo_path, &["rev-parse", "--short", "HEAD"])
        .await
        .unwrap_or_default();
    Ok(format!(
        "已合并 {wt_branch}({}) → {repo_branch}（新 HEAD：{new_head}）",
        short(&wt_commit)
    ))
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

// ============================================================================
// 共享引擎入口（run_codeloop）：UI 命令 codeloop_start 与 CLI codeloop-smoke 都调它。
// 不依赖 AppHandle / AppState，调用方注入 LoopEvents + ConfirmPolicy。
// ============================================================================

/// 共享引擎依赖：所有 IO 端点（DB / 会话存储 / 事件外发 / 用户拍板）显式注入。
pub struct RunLoopDeps {
    pub store: Store,
    pub db: Arc<db::Db>,
    pub events: Arc<dyn LoopEvents>,
    pub confirm: Arc<dyn ConfirmPolicy>,
}

/// 共享引擎输入：语义参数（与 `StartInput` 同构，但已是解析后的强类型）。
pub struct RunLoopInput {
    pub claude: SessionRef,
    pub codex: SessionRef,
    pub target: TargetSpec,
    pub mode: ReviewMode,
    pub max_rounds: u32,
    pub wait_for_claude_idle: bool,
    pub use_worktree: bool,
    pub parent_loop_id: Option<i64>,
    pub resume_worktree_path: Option<String>,
    pub established: Established,
    // ----- 多入口（codeloop-multi-entry-design.md §6.4 第 1 条）-----
    /// 多入口标记；由调用方按 §6.1 校验后传入。
    pub entry_kind: EntryKind,
    /// 仅 `ReviewSeed(mode=implementation)`：规格依据文档绝对路径（已 canonicalize 落仓内）。
    pub design_doc_path: Option<PathBuf>,
    /// `ReviewSeed`：seed 来源（文件路径 + 文本，两者其一恰好有值；inline 时 path=None）。
    pub seed_review_path: Option<String>,
    pub seed_review_inline: Option<String>,
    /// 评估方案最优性（仅 Design 系入口生效）；见 [`StartInput::evaluate_alternatives`]。
    pub evaluate_alternatives: bool,
}

/// 引擎终态：DB 已 finalize，调用方可直接据此判定下一步。
#[derive(Debug, Clone, Serialize)]
pub struct RunLoopResult {
    pub loop_id: i64,
    pub status: String,
    pub final_verdict: Option<String>,
    pub total_rounds: i64,
    pub worktree_path: Option<String>,
}

/// 同步运行一个复核循环到终态。建 DB 行 → 构造 LoopCtx → drive → 读回最终行。
///
/// `mode` / `parent_loop_id` / `resume_worktree_path` 的语义与 `codeloop_start` 一致。
/// 不做会话占用并发校验（headless smoke 串行跑三段，UI 路径仍在 `codeloop_start` 校验）。
pub async fn run_codeloop(deps: RunLoopDeps, input: RunLoopInput) -> Result<RunLoopResult> {
    let RunLoopDeps {
        store,
        db,
        events,
        confirm,
    } = deps;

    let mut target = input.target;
    let mut codex = input.codex;
    let claude = input.claude;

    // ---- 多入口字段校验（§6.4 第 1 / 2 条）：与 Tauri 路径同形 ----
    let entry_kind = input.entry_kind;
    let seed_path_str = input.seed_review_path.as_deref();
    let seed_inline_str = input.seed_review_inline.as_deref();
    let design_doc_str = input.design_doc_path.as_ref().map(|p| {
        // RunLoopInput.design_doc_path 已是 PathBuf,转字符串供字段校验复用。
        p.to_string_lossy().to_string()
    });
    validate_entry_fields(
        entry_kind,
        input.mode,
        design_doc_str.as_deref(),
        seed_path_str,
        seed_inline_str,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    if entry_kind == EntryKind::ReviewSeed {
        validate_review_seed_inputs(seed_path_str, seed_inline_str)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    // resume_worktree_path 已由调用方校验/规范化，这里只在非 None 时把 target/codex 迁过去。
    let resume_worktree_path = input
        .resume_worktree_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    if resume_worktree_path.is_some() && input.mode != ReviewMode::Implementation {
        return Err(anyhow::anyhow!(
            "只有 implementation 模式支持从 worktree 继续复核"
        ));
    }
    if let Some(wt) = resume_worktree_path.as_deref() {
        let (new_target, new_cwd) = relocate_to_worktree(wt, &target.repo_rel, &codex.cwd)
            .map_err(|e| anyhow::anyhow!("worktree 路径校验失败：{e}"))?;
        target = new_target;
        codex.cwd = new_cwd;
    }

    // target_role / spec_doc / 落库元数据计算（与 codeloop_start 同形）。
    let target_role = match (entry_kind, input.mode) {
        (EntryKind::DocReview, _) => TargetRole::RevisionDoc,
        (EntryKind::Implement, _) => TargetRole::SpecDoc,
        (EntryKind::ReviewSeed, ReviewMode::Design) => TargetRole::RevisionDoc,
        (EntryKind::ReviewSeed, ReviewMode::Implementation) => TargetRole::RevisionCode,
        (EntryKind::Continuation, _) => TargetRole::RevisionDoc,
    };
    let spec_doc_abs = input.design_doc_path.clone();
    let design_doc_db = spec_doc_abs.as_ref().map(|p| {
        let rel = p
            .strip_prefix(Path::new(&target.repo_root))
            .map(|q| q.to_path_buf())
            .unwrap_or_else(|_| p.clone());
        rel.to_string_lossy().replace('\\', "/")
    });
    let seed_inline_hash_val = seed_inline_str
        .filter(|s| !s.is_empty())
        .map(seed_inline_hash);

    let mode_str = match input.mode {
        ReviewMode::Design => "design",
        ReviewMode::Implementation => "implementation",
    };
    let loop_id = db.insert_loop(&db::NewLoop {
        claude_session: claude.session_id.clone(),
        codex_session: codex.session_id.clone(),
        claude_cwd: claude.cwd.to_string_lossy().to_string(),
        codex_cwd: codex.cwd.to_string_lossy().to_string(),
        repo_root: target.repo_root.clone(),
        target_repo_rel: target.repo_rel.clone(),
        target_abs: target.abs.clone(),
        target_label: target.label.clone(),
        mode: mode_str.to_string(),
        max_rounds: input.max_rounds.max(1) as i64,
        wait_for_idle: input.wait_for_claude_idle,
        // UI 的 step_confirm 已由 ConfirmPolicy 内部决策；DB 字段仅做记录，固定写 false。
        step_confirm: false,
        use_worktree: input.use_worktree || resume_worktree_path.is_some(),
        parent_loop_id: input.parent_loop_id,
        entry_kind: Some(entry_kind_str(entry_kind).to_string()),
        design_doc_path: design_doc_db,
        seed_review_path: input.seed_review_path.clone(),
        seed_review_inline_hash: seed_inline_hash_val,
    })?;
    if let Some(wt) = resume_worktree_path.as_deref() {
        db.set_worktree(loop_id, wt)?;
        db.append_message(
            loop_id,
            0,
            "system",
            None,
            &format!(
                "从已完成 worktree 继续复核：{wt}。已跳过 Claude 实现首步，直接进入 Codex 复核。"
            ),
        )?;
    }

    // loop_id 一就绪就发一次 `phase=starting` —— headless smoke 的 CurrentLoopTracker
    // 据此在「DB 行已建、首条业务 progress 尚未发」的窗口内也能捕获 loop_id,
    // 避免该窗口内被全局 timeout 击中时 DB 留 running 行。
    events.progress(loop_id, json!({ "phase": "starting", "mode": mode_str }));

    let progress = Arc::new(Mutex::new(json!({ "phase": "starting" })));
    let seq = Arc::new(AtomicI64::new(0));
    let ctx = LoopCtx {
        events: events.clone(),
        confirm: confirm.clone(),
        store,
        db: db.clone(),
        loop_id,
        claude,
        codex,
        target,
        mode: input.mode,
        max_rounds: input.max_rounds.max(1),
        wait_for_claude_idle: input.wait_for_claude_idle,
        use_worktree: input.use_worktree || resume_worktree_path.is_some(),
        resume_from_worktree: resume_worktree_path.is_some(),
        established_codex: input.established.codex,
        established_claude: input.established.claude,
        progress,
        seq,
        entry_kind,
        target_role,
        spec_doc: spec_doc_abs,
        seed_review_inline: input.seed_review_inline.clone(),
        is_continue: false,
        evaluate_alternatives: input.evaluate_alternatives,
    };

    // 直接 await，不 spawn —— smoke runner 串行编排，等本段终态再起下一段。
    run_loop(ctx).await;

    let row = db
        .get_loop(loop_id)?
        .ok_or_else(|| anyhow::anyhow!("插入循环记录后未能读回（loop_id={loop_id}）"))?;
    Ok(RunLoopResult {
        loop_id,
        status: row.status,
        final_verdict: row.final_verdict,
        total_rounds: row.total_rounds,
        worktree_path: row.worktree_path,
    })
}
