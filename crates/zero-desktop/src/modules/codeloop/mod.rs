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

mod db;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_session::store::Store;
use agent_session::{driver, watch, MessagesPage, Provider, SessionRef, SessionSummary};
use anyhow::Result;
use codeloop_core::parse::{self, Verdict};
use codeloop_core::prompt::{self, ReviewMode, TargetSpec};
use codeloop_core::validate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::app_state::AppState;
use crate::shared::workspace::codeloop_db_path;

/// 等待 Claude 当前轮空闲的超时（对应 wait_for_claude_idle）。
const CLAUDE_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// ASK_USER 挂起等待用户回答的上限。
const ANSWER_TIMEOUT: Duration = Duration::from_secs(1800);
/// 连续解析失败到此轮数 → AbortedParse。
const MAX_PARSE_FAILS: u32 = 2;

/// 进度事件名（前端 listen 它刷新状态条 / 触发 ASK_USER 弹窗）。
const EV_PROGRESS: &str = "codeloop_progress";

// ------------------------- 模块状态 -------------------------

/// Codeloop 模块状态：同一时刻只允许一个复核循环在跑；记录持久化到 SQLite。
pub struct CodeloopState {
    inner: Mutex<Option<RunningLoop>>,
    db: Arc<db::Db>,
}

impl CodeloopState {
    pub fn new(workspace: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Mutex::new(None),
            db: Arc::new(db::Db::open(&codeloop_db_path(workspace))?),
        })
    }
}

/// 一个运行中（或刚结束）的循环。
struct RunningLoop {
    handle: JoinHandle<()>,
    /// 该循环在 `loops` 表的记录 id（供 stop 显式 finalize）。
    loop_id: i64,
    /// 最近一次上报的进度快照（供 `codeloop_status` 兜底读取）。
    progress: Arc<Mutex<Value>>,
    /// ASK_USER 挂起态（非 None = 正等用户回答）。
    pending: Arc<Mutex<Option<Pending>>>,
    /// 逐步确认门挂起态（非 None = 正等用户确认/否决某次传递）。
    pending_confirm: Arc<Mutex<Option<PendingConfirm>>>,
    /// 逐步确认开关（运行时可翻转：true=每步确认 / false=全自动）。与 LoopCtx 共享。
    step_confirm: Arc<AtomicBool>,
}

/// 一个待用户回答的问题：seq + 唤醒循环的 oneshot 发送端。
struct Pending {
    seq: i64,
    answer_tx: oneshot::Sender<String>,
}

/// 一个待用户拍板的传递确认：seq + 决定（true=确认发送 / false=否决）的 oneshot 发送端。
struct PendingConfirm {
    seq: i64,
    decide_tx: oneshot::Sender<bool>,
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
    pub target_path: String,
    #[serde(default)]
    pub target_label: Option<String>,
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
    /// 首轮预热：哪些端已在外部（预览台）发过首轮说明块，循环首轮即跳过其 STANDING_BLOCK。
    /// 按 provider 分开（预览台一次只热一端，另一端仍需照发）。
    #[serde(default)]
    pub established: Established,
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
    5
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

// ------------------------- 循环上下文 -------------------------

/// 运行期上下文：解析好的两端 SessionRef + target 定位 + 配置 + 共享句柄。
struct LoopCtx {
    app: AppHandle,
    store: Store,
    db: Arc<db::Db>,
    loop_id: i64,
    claude: SessionRef,
    codex: SessionRef,
    target: TargetSpec,
    mode: ReviewMode,
    max_rounds: u32,
    wait_for_claude_idle: bool,
    /// 逐步确认开关（运行时可翻转，见 codeloop_set_auto_confirm）。confirm_gate 每次实时读。
    step_confirm: Arc<AtomicBool>,
    use_worktree: bool,
    /// 首轮预热：对应端已在外部建立，循环首轮跳过其 STANDING_BLOCK。
    established_codex: bool,
    established_claude: bool,
    progress: Arc<Mutex<Value>>,
    pending: Arc<Mutex<Option<Pending>>>,
    pending_confirm: Arc<Mutex<Option<PendingConfirm>>>,
    seq: Arc<AtomicI64>,
}

impl LoopCtx {
    /// 写进度快照 + emit 给前端。
    async fn report(&self, v: Value) {
        *self.progress.lock().await = v.clone();
        let _ = self.app.emit(EV_PROGRESS, v);
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

    /// 终态收尾：finalize 记录（幂等）+ 上报 done。
    async fn finish(&self, final_verdict: FinalVerdict, total_rounds: u32) {
        let (status, fv) = match final_verdict {
            FinalVerdict::Pass => ("done", "pass"),
            FinalVerdict::MaxRounds => ("done", "max_rounds"),
            FinalVerdict::AbortedTimeout => ("aborted", "aborted_timeout"),
            FinalVerdict::AbortedParse => ("aborted", "aborted_parse"),
            FinalVerdict::AbortedByUser => ("aborted", "aborted_by_user"),
        };
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
    let mut current = prompt_text.to_string();
    loop {
        log::info!(
            "[codeloop] → 发往 {} 会话 {}（prompt {} 字符），等待回复…",
            session.provider.as_str(),
            session.session_id,
            current.chars().count(),
        );
        let turn = driver::send(session, &current).await?;
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

        // 挂起：建 channel、存 pending、emit awaiting_input。
        let seq = ctx.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel::<String>();
        *ctx.pending.lock().await = Some(Pending { seq, answer_tx: tx });
        ctx.report(json!({
            "phase": "awaiting_input",
            "seq": seq,
            "asked_by": session.provider.as_str(),
            "question": q,
        }))
        .await;

        // 等用户回答（带超时）。超时 / 发送端被丢 → 业务终态 Timeout。
        match tokio::time::timeout(ANSWER_TIMEOUT, rx).await {
            Ok(Ok(answer)) => current = format!("用户答复：{answer}"),
            _ => {
                *ctx.pending.lock().await = None;
                return Ok(Resolved::Timeout);
            }
        }
    }
}

/// 逐步确认门的结果。
enum Gate {
    /// 用户确认 / 未开启逐步确认 → 放行。
    Approve,
    /// 用户否决 → 主动中止。
    Reject,
    /// 等待超时 → 按超时中止（保守：不自动发送）。
    Timeout,
}

/// 跨会话传递前的人工确认门：弹窗展示「即将发送的文本」，等用户拍板。
///
/// `step_confirm` 关时直接放行（全自动）。开时挂起：建 oneshot、存 pending_confirm、
/// emit `awaiting_confirm`（带 direction/title/content），等 `codeloop_confirm` 唤醒。
async fn confirm_gate(ctx: &LoopCtx, direction: &str, title: &str, content: &str) -> Gate {
    if !ctx.step_confirm.load(Ordering::SeqCst) {
        log::info!("[codeloop] 逐步确认关闭（自动），{direction} 直接放行");
        return Gate::Approve;
    }
    log::info!(
        "[codeloop] 逐步确认门 {direction}：弹窗等用户拍板（content {} 字符）",
        content.chars().count()
    );
    let seq = ctx.seq.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx, rx) = oneshot::channel::<bool>();
    *ctx.pending_confirm.lock().await = Some(PendingConfirm { seq, decide_tx: tx });
    ctx.report(json!({
        "phase": "awaiting_confirm",
        "seq": seq,
        "direction": direction,
        "title": title,
        "content": content,
    }))
    .await;

    match tokio::time::timeout(ANSWER_TIMEOUT, rx).await {
        Ok(Ok(true)) => Gate::Approve,
        Ok(Ok(false)) => Gate::Reject,
        // 发送端被丢 / 超时：清挂起态，按超时处理。
        _ => {
            *ctx.pending_confirm.lock().await = None;
            Gate::Timeout
        }
    }
}

/// 复核↔修订主循环。基础设施错 → Err（上层 emit error）；业务终态正常收尾。
async fn drive(ctx: &LoopCtx) -> Result<()> {
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
    for n in 1..=ctx.max_rounds {
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
        let codex_prompt = prompt::render_codex_prompt(
            prompt::DEFAULT_CODEX_TEMPLATE,
            &target,
            ctx.mode,
            n,
            codex_first_turn,
        );
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
        // Claude 仅在 NEEDS_WORK 时被发起，其首次发送恒为第 1 轮 → n==1 即首轮。
        // 若 Claude 端已外部预热（established_claude），首轮也跳过 STANDING_BLOCK。
        let claude_first_turn = n == 1 && !ctx.established_claude;
        let mut claude_prompt = prompt::render_claude_prompt(
            prompt::DEFAULT_CLAUDE_TEMPLATE,
            &target,
            &review,
            claude_first_turn,
        );
        // worktree 模式且尚未建立：追加指令，让 Claude 自己用 worktree + 子 agent 实现并回报路径。
        if ctx.use_worktree && !worktree_established {
            claude_prompt.push_str(prompt::WORKTREE_INSTRUCTION);
        }
        let claude_reply = match send_and_resolve(ctx, &ctx.claude, &claude_prompt).await? {
            Resolved::Reply(r) => r,
            Resolved::Timeout => {
                ctx.finish(FinalVerdict::AbortedTimeout, n).await;
                return Ok(());
            }
        };

        // worktree 模式：从 Claude 回复解析 worktree 路径，命中且校验通过则把 Codex 重定位过去。
        if ctx.use_worktree && !worktree_established {
            if let Some(wt) = parse::parse_worktree_path(&claude_reply) {
                match relocate_to_worktree(&wt, &target.repo_rel) {
                    Ok((new_target, new_cwd)) => {
                        target = new_target;
                        codex.cwd = new_cwd;
                        worktree_established = true;
                        force_locator = true; // 下一轮 Codex 强制重发新定位
                        if let Err(e) = ctx.db.set_worktree(ctx.loop_id, &wt) {
                            log::warn!("[codeloop] set_worktree 失败：{e:#}");
                        }
                        ctx.log_msg(
                            n,
                            "system",
                            None,
                            &format!("已切换到 worktree：{wt}（后续 Codex 在此工作树复核）"),
                        );
                        log::info!("[codeloop] worktree 重定位成功 → {wt}");
                    }
                    Err(e) => {
                        ctx.log_msg(
                            n,
                            "system",
                            None,
                            &format!("worktree 路径校验失败（{e}），继续在原仓库复核"),
                        );
                        log::warn!("[codeloop] worktree 校验失败：{e}");
                    }
                }
            }
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

/// 循环顶层：跑 drive，基础设施错时 emit error；收尾清 pending。
async fn run_loop(ctx: LoopCtx) {
    log::info!(
        "[codeloop] 循环任务启动：claude={} codex={} target={} mode={:?} max_rounds={} wait_idle={} step_confirm={}",
        ctx.claude.session_id,
        ctx.codex.session_id,
        ctx.target.repo_rel,
        ctx.mode,
        ctx.max_rounds,
        ctx.wait_for_claude_idle,
        ctx.step_confirm.load(Ordering::SeqCst),
    );
    if let Err(e) = drive(&ctx).await {
        log::warn!("[codeloop] 基础设施错误，循环终止：{e:#}");
        // drive 返回 Err 不经 finish → 在此 finalize 为 failed（幂等 WHERE status='running'）。
        if let Err(fe) = ctx
            .db
            .finalize(ctx.loop_id, "failed", None, 0, Some(&format!("{e:#}")))
        {
            log::warn!("[codeloop] finalize(failed) 失败：{fe:#}");
        }
        ctx.report(json!({ "phase": "error", "error": format!("{e:#}") }))
            .await;
    }
    log::info!("[codeloop] 循环任务结束");
    *ctx.pending.lock().await = None;
    *ctx.pending_confirm.lock().await = None;
}

/// 把 Claude 回报的 worktree 路径校验后转成新的 `(TargetSpec, Codex cwd)`。
///
/// 校验三关（防 Claude 回报任意路径导致 workspace-write 的 Codex 越界读写）：
/// 路径存在 + 是 git 工作树（`find_repo_root` 命中）+ 落在用户目录（home）之下。
/// repo_rel 在同仓另一检出里一致，故新 target 复用之，仅把 repo_root/abs 迁到 worktree。
fn relocate_to_worktree(
    worktree: &str,
    repo_rel: &str,
) -> std::result::Result<(TargetSpec, PathBuf), String> {
    let wt = PathBuf::from(worktree);
    if !wt.exists() {
        return Err("路径不存在".into());
    }
    let root = validate::find_repo_root(&wt).ok_or("不是 git 工作树（未找到 .git）")?;
    let canon = std::fs::canonicalize(&root).map_err(|e| format!("canonicalize 失败：{e}"))?;
    if let Some(home) = dirs::home_dir() {
        let home_canon = std::fs::canonicalize(&home).unwrap_or(home);
        if !canon.starts_with(&home_canon) {
            return Err("worktree 越出用户目录".into());
        }
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

/// 新建一个 Codex 会话：复用所选 Claude 会话的 cwd（同一仓库），用默认种子提示词跑一轮
/// `codex exec` 建会话，返回新会话 id（前端据此选中 + 刷新清单）。**消耗 codex 额度**。
#[tauri::command]
pub async fn codeloop_new_codex_session(claude_session_id: String) -> Result<String, String> {
    let store = Store::from_env().map_err(|e| format!("定位会话存储失败：{e:#}"))?;
    let snap = store
        .snapshot(Provider::Claude, &claude_session_id)
        .map_err(|e| format!("读取所选 Claude 会话的仓库目录失败：{e:#}"))?;
    driver::create_codex_session(&snap.cwd, NEW_CODEX_SEED)
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
    let session = resolve_ref(&store, p, &SessionRefDto { session_id, cwd: None })
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
) -> Result<(), String> {
    let cs = &state.codeloop;

    // 单写者：已有未结束的循环则拒。
    {
        let guard = cs.inner.lock().await;
        if let Some(rl) = guard.as_ref() {
            if !rl.handle.is_finished() {
                return Err("已有复核循环在运行，请先停止再启动".into());
            }
        }
    }

    let store = Store::from_env()
        .map_err(|e| format!("定位会话存储失败（~/.codex / ~/.claude）：{e:#}"))?;
    let claude = resolve_ref(&store, Provider::Claude, &input.claude)
        .map_err(|e| format!("解析 Claude 会话失败：{e:#}"))?;
    let mut codex = resolve_ref(&store, Provider::Codex, &input.codex)
        .map_err(|e| format!("解析 Codex 会话失败：{e:#}"))?;

    // 三方仓库一致性校验（拒绝跑错仓）。
    let validated = validate::validate_three_way(&claude.cwd, &codex.cwd, &input.target_path)
        .map_err(|e| format!("{e:#}"))?;

    let repo_root = validate::display_path(&validated.repo_root);
    let target_abs = validate::display_path(&validated.target_abs);
    let repo_rel = validated
        .target_abs
        .strip_prefix(&validated.repo_root)
        .unwrap_or(&validated.target_abs)
        .to_string_lossy()
        .replace('\\', "/");

    // Codex `exec resume` 的 --cd 用工作树根，消除子目录相对路径歧义；Claude resume 保持原 cwd。
    codex.cwd = repo_root.clone();

    let label = input
        .target_label
        .unwrap_or_else(|| prompt::default_label(&repo_rel));
    let target = TargetSpec {
        label,
        repo_root: repo_root.to_string_lossy().to_string(),
        repo_rel,
        abs: target_abs.to_string_lossy().to_string(),
    };

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
            use_worktree: input.use_worktree,
            parent_loop_id: input.parent_loop_id,
        })
        .map_err(|e| format!("写入复核记录失败：{e:#}"))?;

    let progress = Arc::new(Mutex::new(json!({ "phase": "starting" })));
    let pending = Arc::new(Mutex::new(None));
    let pending_confirm = Arc::new(Mutex::new(None));
    let step_confirm = Arc::new(AtomicBool::new(input.step_confirm));
    let seq = Arc::new(AtomicI64::new(0));

    let ctx = LoopCtx {
        app: app.clone(),
        store,
        db: db.clone(),
        loop_id,
        claude,
        codex,
        target,
        mode: input.mode,
        max_rounds: input.max_rounds.max(1),
        wait_for_claude_idle: input.wait_for_claude_idle,
        step_confirm: step_confirm.clone(),
        use_worktree: input.use_worktree,
        established_codex: input.established.codex,
        established_claude: input.established.claude,
        progress: progress.clone(),
        pending: pending.clone(),
        pending_confirm: pending_confirm.clone(),
        seq,
    };

    let handle = tokio::spawn(run_loop(ctx));
    *cs.inner.lock().await = Some(RunningLoop {
        handle,
        loop_id,
        progress,
        pending,
        pending_confirm,
        step_confirm,
    });
    Ok(())
}

/// 当前循环状态快照：`{ running, progress }`。
#[tauri::command]
pub async fn codeloop_status(state: State<'_, AppState>) -> Result<Value, String> {
    let guard = state.codeloop.inner.lock().await;
    match guard.as_ref() {
        Some(rl) => {
            let running = !rl.handle.is_finished();
            let progress = rl.progress.lock().await.clone();
            let step_confirm = rl.step_confirm.load(Ordering::SeqCst);
            Ok(json!({ "running": running, "progress": progress, "step_confirm": step_confirm }))
        }
        None => Ok(json!({ "running": false, "progress": Value::Null, "step_confirm": true })),
    }
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
        Ok(r) => rows.push(check("claude_locate", "Claude 会话可定位", "passive", "pass", format!("cwd={}", r.cwd.display()))),
        Err(e) => rows.push(check("claude_locate", "Claude 会话可定位", "passive", "fail", format!("{e:#}"))),
    }
    let codex = resolve_ref(&store, Provider::Codex, &input.codex);
    match &codex {
        Ok(r) => rows.push(check("codex_locate", "Codex 会话可定位", "passive", "pass", format!("cwd={}", r.cwd.display()))),
        Err(e) => rows.push(check("codex_locate", "Codex 会话可定位", "passive", "fail", format!("{e:#}"))),
    }

    // 被动：三方仓库一致性（两端都定位到才校验）。
    match (&claude, &codex) {
        (Ok(c), Ok(x)) => match validate::validate_three_way(&c.cwd, &x.cwd, &input.target_path) {
            Ok(_) => rows.push(check("three_way", "三方仓库一致性", "passive", "pass", "claude / codex / target 同仓".into())),
            Err(e) => rows.push(check("three_way", "三方仓库一致性", "passive", "fail", format!("{e:#}"))),
        },
        _ => rows.push(check("three_way", "三方仓库一致性", "passive", "skipped", "会话未定位，跳过".into())),
    }

    // 版本探针：验证 PATH / npm shim 解析 / 可 spawn（C4/C5）。
    match driver::probe_version("codex").await {
        Ok(v) => rows.push(check("codex_version", "codex --version", "version", "pass", v)),
        Err(e) => rows.push(check("codex_version", "codex --version", "version", "fail", format!("{e:#}"))),
    }
    match driver::probe_version("claude").await {
        Ok(v) => rows.push(check("claude_version", "claude --version", "version", "pass", v)),
        Err(e) => rows.push(check("claude_version", "claude --version", "version", "fail", format!("{e:#}"))),
    }

    // 实发往返（合成探针）：仅在用户勾选时跑。一次性只读会话，不污染真实会话。
    const PROBE: &str = "只回复 PROBE_OK 这一个词，不要执行任何其它操作。";
    if live {
        match &codex {
            Ok(x) => {
                let row = match driver::probe_codex_synthetic(&x.cwd, PROBE).await {
                    Ok(reply) if reply.contains("PROBE_OK") => check("codex_live", "Codex 实发往返（合成探针）", "live", "pass", "回复含 PROBE_OK，新建路径全链路通".into()),
                    Ok(reply) => {
                        let mut r = check("codex_live", "Codex 实发往返（合成探针）", "live", "warn", "已收到回复但未含 PROBE_OK（输出格式可能变更）".into());
                        r.raw_excerpt = Some(reply.chars().take(500).collect());
                        r
                    }
                    Err(e) => check("codex_live", "Codex 实发往返（合成探针）", "live", "fail", format!("{e:#}")),
                };
                rows.push(row);
            }
            Err(_) => rows.push(check("codex_live", "Codex 实发往返（合成探针）", "live", "skipped", "会话未定位，跳过".into())),
        }
        match &claude {
            Ok(c) => {
                let row = match driver::probe_claude_synthetic(&c.cwd, PROBE).await {
                    Ok(reply) if reply.contains("PROBE_OK") => check("claude_live", "Claude 实发往返（合成探针）", "live", "pass", "回复含 PROBE_OK，新建路径全链路通".into()),
                    Ok(reply) => {
                        let mut r = check("claude_live", "Claude 实发往返（合成探针）", "live", "warn", "已收到回复但未含 PROBE_OK（输出格式可能变更）".into());
                        r.raw_excerpt = Some(reply.chars().take(500).collect());
                        r
                    }
                    Err(e) => check("claude_live", "Claude 实发往返（合成探针）", "live", "fail", format!("{e:#}")),
                };
                rows.push(row);
            }
            Err(_) => rows.push(check("claude_live", "Claude 实发往返（合成探针）", "live", "skipped", "会话未定位，跳过".into())),
        }
    } else {
        rows.push(check("codex_live", "Codex 实发往返（合成探针）", "live", "skipped", "未勾选实发探针".into()));
        rows.push(check("claude_live", "Claude 实发往返（合成探针）", "live", "skipped", "未勾选实发探针".into()));
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
    enabled: bool,
) -> Result<(), String> {
    let guard = state.codeloop.inner.lock().await;
    let Some(rl) = guard.as_ref() else {
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
    seq: i64,
    text: String,
) -> Result<(), String> {
    let guard = state.codeloop.inner.lock().await;
    let Some(rl) = guard.as_ref() else {
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
    seq: i64,
    approve: bool,
) -> Result<(), String> {
    let guard = state.codeloop.inner.lock().await;
    let Some(rl) = guard.as_ref() else {
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

/// 停止当前循环（abort 后台任务，清状态，记录终态置 aborted_by_user）。
#[tauri::command]
pub async fn codeloop_stop(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.codeloop.inner.lock().await;
    if let Some(rl) = guard.take() {
        rl.handle.abort();
        // abort 是硬终止，任务内 finish 不保证执行 → 在此显式 finalize（幂等）。
        let _ = state
            .codeloop
            .db
            .finalize(rl.loop_id, "aborted", Some("aborted_by_user"), 0, None);
    }
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
