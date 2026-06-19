//! Headless codeloop smoke runner —— 跑 design → implementation → review 三段，
//! 不依赖 Tauri 窗口；DB 行与 UI 路径同构。设计见 `docs/codeloop-headless-smoke-runner-plan.md`。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_session::store::Store;
use agent_session::{driver, Provider, SessionRef, SessionStatus, SessionSummary};
use anyhow::{anyhow, bail, Context, Result};
use codeloop_core::prompt::{self, EntryKind, ReviewMode, TargetSpec};
use codeloop_core::validate;
use serde_json::{json, Value};

use super::db;
use super::runtime::{AutoConfirmPolicy, ConfirmPolicy, ConsoleLoopEvents, LoopEvents};
use super::{run_codeloop, Established, RunLoopDeps, RunLoopInput, RunLoopResult};
use crate::shared::workspace::codeloop_db_path;

// ---------- 退出码 ----------

/// 0 = pass; 1 = LLM 判失败（可能可重试）；2 = runner 检测到的不变式违反（产品代码 bug）；
/// 3 = preflight/config 错；4 = `--verify` cargo 命令失败。
pub const EXIT_OK: i32 = 0;
pub const EXIT_NON_PASS: i32 = 1;
pub const EXIT_INVARIANT: i32 = 2;
pub const EXIT_PREFLIGHT: i32 = 3;
pub const EXIT_VERIFY: i32 = 4;

// ---------- CLI 入参 ----------

#[derive(Debug, Clone)]
pub struct SmokeArgs {
    pub repo: PathBuf,
    pub target: String,
    pub workspace: PathBuf,
    pub claude_session: Option<String>,
    pub codex_session: Option<String>,
    pub max_rounds: u32,
    pub auto_confirm: bool,
    pub new_codex_agent: bool,
    pub allow_hijack_current_session: bool,
    pub ask_user_answers: Option<PathBuf>,
    pub recover_after_stop: bool,
    pub verify: bool,
    pub json: bool,
    pub timeout: Duration,
    // ----- 多入口（codeloop-multi-entry-design.md §6.4 第 3 点）-----
    /// 入口类型；默认 `doc_review` 兼容旧脚本。
    pub entry_kind: EntryKind,
    /// 仅 `ReviewSeed(mode=implementation)`：规格依据文档路径（绝对或相对仓根）。
    pub design_doc: Option<String>,
    /// `ReviewSeed`：seed 文件路径。
    pub seed_review: Option<String>,
    /// `ReviewSeed`：从文件读 inline seed 文本（避免 shell 引号陷阱）。
    pub seed_review_inline_file: Option<PathBuf>,
}

// ---------- 入口 ----------

/// CLI 入口：建 tokio 运行时，跑三段编排，返回退出码。**绝不 panic**——所有错误归类到退出码。
pub fn run(args: SmokeArgs) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[fatal] 建 tokio runtime 失败：{e:#}");
            return EXIT_PREFLIGHT;
        }
    };
    rt.block_on(async move {
        let out = Output::new(args.json);

        // 不支持的参数 fail-fast,放在 preflight / Codex 新建之前,避免 preflight 副作用。
        if args.recover_after_stop {
            out.error(
                "recover_after_stop",
                "--recover-after-stop 在 v1 中未实现（仅占位，见 plan §5）",
            );
            return EXIT_PREFLIGHT;
        }
        if !args.auto_confirm {
            out.error(
                "preflight",
                "headless smoke v1 必须显式 --auto-confirm（无人值守语义）",
            );
            return EXIT_PREFLIGHT;
        }

        // DB 在 timeout 之外打开,以便超时分支能 finalize 残留 running 记录。
        if let Err(e) = crate::shared::workspace::ensure_workspace(&args.workspace) {
            out.error("preflight", &format!("ensure_workspace 失败：{e:#}"));
            return EXIT_PREFLIGHT;
        }
        let db = match db::Db::open(&codeloop_db_path(&args.workspace)) {
            Ok(d) => Arc::new(d),
            Err(e) => {
                out.error("preflight", &format!("打开 codeloop DB 失败：{e:#}"));
                return EXIT_PREFLIGHT;
            }
        };
        // 追踪「最近一次 progress 携带的 loop_id」——run_codeloop 内部创建 loop 后会立即
        // 报告进度,据此可在 wall-clock 超时时把残留 running 行收尾为 aborted_timeout。
        let current_loop = Arc::new(AtomicI64::new(0));

        let timeout = args.timeout;
        let json = args.json;
        match tokio::time::timeout(timeout, run_inner(args, db.clone(), current_loop.clone()))
            .await
        {
            Ok(code) => code,
            Err(_) => {
                let lid = current_loop.load(Ordering::Relaxed);
                if lid > 0 {
                    // finalize 内部 WHERE status='running' 是幂等的——若 run_codeloop 已经
                    // 自己收尾过(理论上 timeout 一触发 future 被 drop 不可能),也不会覆盖。
                    if let Err(e) = db.finalize(
                        lid,
                        "aborted",
                        Some("aborted_timeout"),
                        0,
                        Some("global_timeout"),
                    ) {
                        eprintln!("[fatal] timeout 兜底 finalize 失败：{e:#}");
                    }
                }
                if json {
                    println!(
                        "{}",
                        json!({
                            "event": "smoke_done",
                            "status": "fail",
                            "exit_code": EXIT_NON_PASS,
                            "reason": "global_timeout",
                            "aborted_loop_id": if lid > 0 { Value::from(lid) } else { Value::Null },
                        })
                    );
                }
                eprintln!("SMOKE FAIL reason=global_timeout aborted_loop_id={lid}");
                EXIT_NON_PASS
            }
        }
    })
}

async fn run_inner(
    args: SmokeArgs,
    db: Arc<db::Db>,
    current_loop: Arc<AtomicI64>,
) -> i32 {
    let out = Output::new(args.json);

    // ---- preflight ----
    let pf = match preflight(&args, db.clone()).await {
        Ok(pf) => {
            out.preflight_done(&args, &pf);
            pf
        }
        Err(e) => {
            out.error("preflight", &format!("{e:#}"));
            return EXIT_PREFLIGHT;
        }
    };

    // ConsoleLoopEvents → stdout(`--json` 模式 logger 已抑制,无串扰):JSON 事件契约要求
    // progress / stage_* / smoke_done 全部在 stdout 上。CurrentLoopTracker 截取 loop_id
    // 供超时 finalize 兜底用。
    let console: Arc<dyn LoopEvents> = Arc::new(ConsoleLoopEvents::new(
        args.json,
        Box::new(std::io::stdout()),
    ));
    let events: Arc<dyn LoopEvents> = Arc::new(CurrentLoopTracker {
        inner: console,
        current: current_loop.clone(),
    });
    let confirm: Arc<dyn ConfirmPolicy> = Arc::new(AutoConfirmPolicy::new(pf.ask_answers.clone()));

    // ---- stage 1: design ----
    out.stage_starting("design", None);
    let design = match run_stage(
        &pf,
        events.clone(),
        confirm.clone(),
        StageInput {
            mode: ReviewMode::Design,
            max_rounds: args.max_rounds,
            use_worktree: false,
            parent_loop_id: None,
            resume_worktree_path: None,
            entry_kind: EntryKind::DocReview,
            design_doc_path: None,
            seed_review_path: None,
            seed_review_inline: None,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            out.error("design", &format!("{e:#}"));
            return EXIT_INVARIANT;
        }
    };
    if !is_pass(&design) {
        out.stage_failed("design", &design);
        out.smoke_done(false, EXIT_NON_PASS, &design, None, None);
        return EXIT_NON_PASS;
    }
    out.stage_done("design", &design);

    // ---- stage 2: implementation ----
    // v1 强制 worktree 模式 —— review 阶段依赖 worktree_path 续跑;若放宽,实现/评审两段
    // 共用同一仓库工作树会出现「review 抓到实现阶段刚刚写的脏文件」的语义混淆。
    out.stage_starting("implementation", None);
    let impl_ = match run_stage(
        &pf,
        events.clone(),
        confirm.clone(),
        StageInput {
            mode: ReviewMode::Implementation,
            max_rounds: args.max_rounds,
            use_worktree: true,
            parent_loop_id: Some(design.loop_id),
            resume_worktree_path: None,
            entry_kind: EntryKind::Implement,
            design_doc_path: None,
            seed_review_path: None,
            seed_review_inline: None,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            out.error("implementation", &format!("{e:#}"));
            return EXIT_INVARIANT;
        }
    };
    if !is_pass(&impl_) {
        out.stage_failed("implementation", &impl_);
        out.smoke_done(false, EXIT_NON_PASS, &design, Some(&impl_), None);
        return EXIT_NON_PASS;
    }
    if impl_.worktree_path.is_none() {
        out.error(
            "implementation",
            "实现记录已 PASS 但 worktree_path 为空（worktree 模式开启时不应如此）",
        );
        return EXIT_INVARIANT;
    }
    out.stage_done("implementation", &impl_);

    // ---- stage 3: review ----
    let resume = match impl_.worktree_path.clone() {
        Some(wt) => wt,
        None => {
            out.error(
                "review",
                "implementation 阶段未产出 worktree_path，无法继续 review",
            );
            return EXIT_INVARIANT;
        }
    };
    out.stage_starting("review", Some(&resume));
    let review = match run_stage(
        &pf,
        events.clone(),
        confirm.clone(),
        StageInput {
            mode: ReviewMode::Implementation,
            max_rounds: args.max_rounds,
            use_worktree: true,
            parent_loop_id: Some(impl_.loop_id),
            resume_worktree_path: Some(resume.clone()),
            entry_kind: EntryKind::Implement,
            design_doc_path: None,
            seed_review_path: None,
            seed_review_inline: None,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            out.error("review", &format!("{e:#}"));
            return EXIT_INVARIANT;
        }
    };
    if !is_pass(&review) {
        out.stage_failed("review", &review);
        out.smoke_done(false, EXIT_NON_PASS, &design, Some(&impl_), Some(&review));
        return EXIT_NON_PASS;
    }
    // 不变式：review 记录的 round-0 不应出现 claude_implement（应跳过实现首步）。
    match pf.db.has_message_kind(review.loop_id, "claude_implement") {
        Ok(true) => {
            out.error(
                "review",
                "review 记录出现 claude_implement —— resume_from_worktree 分支异常",
            );
            return EXIT_INVARIANT;
        }
        Ok(false) => {}
        Err(e) => {
            out.error("review", &format!("DB invariant 查询失败：{e:#}"));
            return EXIT_INVARIANT;
        }
    }
    out.stage_done("review", &review);

    // ---- verify ----
    if args.verify {
        let log_dir = std::env::temp_dir().join(format!("codeloop-smoke-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&log_dir);
        let cmds: &[&[&str]] = &[
            &["cargo", "test", "-p", "codeloop-core"],
            &["cargo", "check", "-p", "zero-desktop"],
        ];
        for cmd in cmds {
            let label = cmd.join(" ");
            let log = log_dir.join(format!(
                "verify-{}.log",
                cmd.iter()
                    .skip(1)
                    .take(2)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("-")
            ));
            let ok = run_verify_cmd(&resume, cmd, &log);
            out.verify_done(&label, ok, &log);
            if !ok {
                out.smoke_done(false, EXIT_VERIFY, &design, Some(&impl_), Some(&review));
                return EXIT_VERIFY;
            }
        }
    }

    out.smoke_done(true, EXIT_OK, &design, Some(&impl_), Some(&review));
    EXIT_OK
}

fn is_pass(r: &RunLoopResult) -> bool {
    r.status == "done" && r.final_verdict.as_deref() == Some("pass")
}

fn entry_kind_label(k: EntryKind) -> &'static str {
    match k {
        EntryKind::DocReview => "doc_review",
        EntryKind::Implement => "implement",
        EntryKind::ReviewSeed => "review_seed",
    }
}

// ---------- preflight ----------

struct Preflight {
    store: Store,
    db: Arc<db::Db>,
    repo_root: PathBuf,
    target_path: String,
    claude: SessionRef,
    codex: SessionRef,
    ask_answers: Option<HashMap<String, String>>,
    // ----- 多入口（codeloop-multi-entry-design.md §6.4 第 3 点）-----
    /// CLI 传入的入口类型（影响 preflight_done JSON 输出）。
    entry_kind: EntryKind,
    /// `ReviewSeed(mode=implementation)`：规格依据文档绝对路径（已 canonicalize）。
    design_doc_path: Option<PathBuf>,
    /// `ReviewSeed`：seed 文件路径（args 透传）。
    seed_review_path: Option<String>,
    /// `ReviewSeed`：inline seed 文本（运行前由 inline_file 读入）。
    ///
    /// 当前 smoke runner 跑固定三段（design → impl → review），尚未支持以 ReviewSeed 起点的
    /// 自定义编排，本字段挂在 Preflight 上仅用于 `preflight_done` 事件日志（hash 已抽出）；
    /// 待新增 `--entry-kind=review_seed` 编排实现时直接喂给 stage。
    #[allow(dead_code)]
    seed_review_inline: Option<String>,
    /// inline seed 文本短哈希前缀（便于无头日志判读）。
    seed_review_inline_hash: Option<String>,
}

async fn preflight(args: &SmokeArgs, db: Arc<db::Db>) -> Result<Preflight> {
    // 仓库根：必须是 git toplevel，并与 --repo 一致。
    let repo_root = canonicalize_repo(&args.repo)?;

    // target 必须存在。
    let target_abs = repo_root.join(&args.target);
    if !target_abs.exists() {
        bail!(
            "--target 不存在：{} (从 {} 解析)",
            target_abs.display(),
            args.target
        );
    }

    // ASK_USER 答案映射（若提供）。
    let ask_answers = match &args.ask_user_answers {
        Some(p) => Some(load_ask_answers(p)?),
        None => None,
    };

    // workspace 已由 run() 创建;DB 由 run() 打开后传入(便于超时分支访问)。

    // 会话存储。
    let store = Store::from_env().context("定位会话存储（~/.codex / ~/.claude）失败")?;

    // 会话选择强制规则:
    // - --claude-session 必填(显式 id 或 "auto"),不给就拒绝,避免误选当前活会话;
    // - --codex-session 同理,除非用 --new-codex-agent 走新建路径。
    let claude_arg = args.claude_session.as_deref().ok_or_else(|| {
        anyhow!(
            "缺少 --claude-session。请显式传一个 id,或传 `auto` 让 runner 按 \
             cwd + 最近活跃自动选(仍受劫持保护)。\n\
             默认不允许自动选,以免劫持你当前正在用的 Claude Code 会话。"
        )
    })?;
    if !args.new_codex_agent && args.codex_session.is_none() {
        bail!(
            "缺少 --codex-session 且未启用 --new-codex-agent。\
             请显式传 id / 传 `auto`,或加 --new-codex-agent。"
        );
    }

    // 选会话:repo cwd 过滤 + 健康探针。
    let sessions = store.list(200).context("list sessions")?;
    let claude = pick_session(&sessions, Provider::Claude, claude_arg, &repo_root)?;

    // 劫持保护:目标会话 transcript 近 5 分钟内仍有写入 → 拒绝(除非显式放行)。
    enforce_hijack_guard(
        &sessions,
        &claude,
        args.allow_hijack_current_session,
        "Claude",
    )?;

    let codex = if args.new_codex_agent {
        let id = driver::create_codex_session(&claude.cwd, NEW_CODEX_SEED)
            .await
            .context("create_codex_session")?;
        SessionRef {
            provider: Provider::Codex,
            session_id: id,
            cwd: claude.cwd.clone(),
        }
    } else {
        // codex_session 此时必非 None(上面已校验)。
        let codex_arg = args.codex_session.as_deref().unwrap();
        let codex_ref = pick_session(&sessions, Provider::Codex, codex_arg, &repo_root)?;
        enforce_hijack_guard(
            &sessions,
            &codex_ref,
            args.allow_hijack_current_session,
            "Codex",
        )?;
        codex_ref
    };

    // 三方仓库一致性（与 codeloop_start 相同的护栏）。
    validate::validate_three_way(&claude.cwd, &codex.cwd, &args.target)
        .map_err(|e| anyhow!("三方仓库一致性校验失败：{e:#}"))?;

    // 多入口（§6.4 第 3 点）：CLI seed_review_inline_file → 文本 + 短哈希；design_doc 路径
    // 解析为仓内绝对路径（与 codeloop_start 同形校验）。
    let seed_review_inline_text = match args.seed_review_inline_file.as_deref() {
        Some(p) => Some(
            std::fs::read_to_string(p)
                .with_context(|| format!("读 --seed-review-inline-file 失败：{}", p.display()))?,
        ),
        None => None,
    };
    let seed_review_inline_hash = seed_review_inline_text
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(super::seed_inline_hash);
    let design_doc_path = match args.design_doc.as_deref().filter(|s| !s.is_empty()) {
        Some(p) => Some(
            super::resolve_design_doc(&repo_root, p)
                .map_err(|e| anyhow!("--design-doc 校验失败：{e}"))?,
        ),
        None => None,
    };

    Ok(Preflight {
        store,
        db,
        repo_root,
        target_path: args.target.clone(),
        claude,
        codex,
        ask_answers,
        entry_kind: args.entry_kind,
        design_doc_path,
        seed_review_path: args.seed_review.clone(),
        seed_review_inline: seed_review_inline_text,
        seed_review_inline_hash,
    })
}

fn canonicalize_repo(p: &Path) -> Result<PathBuf> {
    let canon =
        std::fs::canonicalize(p).with_context(|| format!("--repo 不存在：{}", p.display()))?;
    let top = validate::find_repo_root(&canon)
        .ok_or_else(|| anyhow!("--repo 不是 git 工作树（未找到 .git）：{}", canon.display()))?;
    let top = std::fs::canonicalize(&top).unwrap_or(top);
    if top != canon {
        bail!(
            "--repo 必须是 git 工作树根，但 {} 的根在 {}",
            canon.display(),
            top.display()
        );
    }
    // 去掉 Windows extended-length 前缀，避免后续比较/输出错位。
    Ok(strip_win_prefix(top))
}

fn strip_win_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

fn load_ask_answers(path: &Path) -> Result<HashMap<String, String>> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("读取 --ask-user-answers 失败：{}", path.display()))?;
    let map: HashMap<String, String> = serde_json::from_str(&s)
        .with_context(|| format!("解析 --ask-user-answers JSON 失败：{}", path.display()))?;
    Ok(map)
}

fn norm_path(s: &str) -> String {
    let s = s.replace('\\', "/").to_lowercase();
    // 去掉 Windows extended-length 前缀 "//?/"（canonicalize 后常见）。
    s.strip_prefix("//?/").map(str::to_string).unwrap_or(s)
}

/// 选会话:`id_or_auto` 必须是具体 id 或哨兵 `"auto"`(由调用方保证非空)。
/// `"auto"` → cwd 过滤 + `updated_at` 倒序挑首个非 Unknown 状态的;
/// 具体 id → 直接 lookup(不做健康过滤,假定调用方知道自己在做什么)。
fn pick_session(
    all: &[SessionSummary],
    provider: Provider,
    id_or_auto: &str,
    repo_root: &Path,
) -> Result<SessionRef> {
    if id_or_auto != "auto" {
        let hit = all
            .iter()
            .find(|s| s.provider == provider && s.id == id_or_auto)
            .ok_or_else(|| anyhow!("找不到 {provider:?} 会话 id={id_or_auto}"))?;
        return Ok(SessionRef {
            provider,
            session_id: hit.id.clone(),
            cwd: PathBuf::from(&hit.cwd),
        });
    }

    let repo_str = norm_path(&repo_root.to_string_lossy());
    let mut cands: Vec<&SessionSummary> = all
        .iter()
        .filter(|s| s.provider == provider)
        .filter(|s| norm_path(&s.cwd) == repo_str)
        .filter(|s| !matches!(s.status, SessionStatus::Unknown))
        .collect();
    cands.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let top = cands.first().ok_or_else(|| {
        anyhow!(
            "auto 选 {provider:?} 失败:没有会话满足 cwd={} —— 请先在该仓库下开一个会话,\
             或直接传具体 session id",
            repo_root.display()
        )
    })?;
    Ok(SessionRef {
        provider,
        session_id: top.id.clone(),
        cwd: PathBuf::from(&top.cwd),
    })
}

/// 劫持保护:目标会话的 transcript `updated_at` 若在最近 5 分钟内,视为「有人正在用」,
/// 默认拒绝。`--allow-hijack-current-session` 显式放行。
const HIJACK_WINDOW_SECS: i64 = 300;

fn enforce_hijack_guard(
    all: &[SessionSummary],
    chosen: &SessionRef,
    allow: bool,
    provider_label: &str,
) -> Result<()> {
    if allow {
        return Ok(());
    }
    let summary = all
        .iter()
        .find(|s| s.provider == chosen.provider && s.id == chosen.session_id);
    let Some(summary) = summary else {
        return Ok(()); // 找不到摘要就放行(理论上 pick_session 之后不会发生)。
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(&summary.updated_at);
    let Ok(updated) = parsed else {
        // updated_at 解析失败:fail-closed —— guard 的目的就是防误劫持,解析失败时无法判断
        // 安全状态,默认拒绝;要继续就显式 --allow-hijack-current-session。
        bail!(
            "{provider_label} 会话 {} 的 updated_at={:?} 解析失败(预期 RFC3339)。\
             为防误劫持,默认拒绝;若确认安全可加 --allow-hijack-current-session 旁路。",
            chosen.session_id,
            summary.updated_at
        );
    };
    let now = chrono::Utc::now();
    let elapsed = now.signed_duration_since(updated.with_timezone(&chrono::Utc));
    if elapsed.num_seconds() >= 0 && elapsed.num_seconds() < HIJACK_WINDOW_SECS {
        bail!(
            "{provider_label} 会话 {} 的 transcript 在最近 {}s 内仍有写入(updated_at={}),\
             视为有人正在使用 → 拒绝劫持。\n\
             要么换一个静默已久的会话 id,要么用一个独立的 --repo cwd,\
             要么显式加 --allow-hijack-current-session(开发机上不要这么做)。",
            chosen.session_id,
            elapsed.num_seconds(),
            summary.updated_at,
        );
    }
    Ok(())
}

/// LoopEvents wrapper:转发到底层 sink 的同时,把最近一次 progress 携带的 loop_id 暴露给
/// run() 的超时分支用于兜底 finalize。
struct CurrentLoopTracker {
    inner: Arc<dyn LoopEvents>,
    current: Arc<AtomicI64>,
}

impl LoopEvents for CurrentLoopTracker {
    fn progress(&self, loop_id: i64, value: Value) {
        if loop_id > 0 {
            self.current.store(loop_id, Ordering::Relaxed);
        }
        self.inner.progress(loop_id, value);
    }
}

const NEW_CODEX_SEED: &str =
    "你好。这是一个用于跨会话复核的新会话，已就绪。请回复「已就绪」，等待后续复核任务。";

// ---------- 单段执行 ----------

struct StageInput {
    mode: ReviewMode,
    max_rounds: u32,
    use_worktree: bool,
    parent_loop_id: Option<i64>,
    resume_worktree_path: Option<String>,
    /// 多入口：当前阶段对应的 entry_kind。
    entry_kind: EntryKind,
    /// `ReviewSeed(mode=implementation)`：规格依据文档绝对路径（已 canonicalize）。
    design_doc_path: Option<PathBuf>,
    /// `ReviewSeed`：seed 文件路径。
    seed_review_path: Option<String>,
    /// `ReviewSeed`：inline seed 文本（运行前已从 inline_file 读出）。
    seed_review_inline: Option<String>,
}

async fn run_stage(
    pf: &Preflight,
    events: Arc<dyn LoopEvents>,
    confirm: Arc<dyn ConfirmPolicy>,
    s: StageInput,
) -> Result<RunLoopResult> {
    // 与 codeloop_start 等价的 target 计算（repo_root + target_path）。
    let validated = validate::validate_three_way(&pf.claude.cwd, &pf.codex.cwd, &pf.target_path)?;
    let repo_root = validate::display_path(&validated.repo_root);
    let target_abs = validate::display_path(&validated.target_abs);
    let repo_rel = validated
        .target_abs
        .strip_prefix(&validated.repo_root)
        .unwrap_or(&validated.target_abs)
        .to_string_lossy()
        .replace('\\', "/");

    let mut codex = pf.codex.clone();
    codex.cwd = repo_root.clone();

    let target = TargetSpec {
        label: prompt::default_label(&repo_rel),
        repo_root: repo_root.to_string_lossy().to_string(),
        repo_rel,
        abs: target_abs.to_string_lossy().to_string(),
    };

    let deps = RunLoopDeps {
        store: pf.store.clone(),
        db: pf.db.clone(),
        events,
        confirm,
    };
    let input = RunLoopInput {
        claude: pf.claude.clone(),
        codex,
        target,
        mode: s.mode,
        max_rounds: s.max_rounds,
        wait_for_claude_idle: false,
        use_worktree: s.use_worktree,
        parent_loop_id: s.parent_loop_id,
        resume_worktree_path: s.resume_worktree_path,
        established: Established::default(),
        entry_kind: s.entry_kind,
        design_doc_path: s.design_doc_path.clone(),
        seed_review_path: s.seed_review_path.clone(),
        seed_review_inline: s.seed_review_inline.clone(),
    };
    run_codeloop(deps, input).await
}

// ---------- verify ----------

fn run_verify_cmd(worktree: &str, argv: &[&str], log: &Path) -> bool {
    use std::process::Command;
    let mut f = match std::fs::File::create(log) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[verify] 建日志文件失败 {}: {e:#}", log.display());
            return false;
        }
    };
    let _ = writeln!(f, "$ {}", argv.join(" "));
    let _ = writeln!(f, "(cwd: {worktree})");
    let out = Command::new(argv[0])
        .args(&argv[1..])
        .current_dir(worktree)
        .output();
    match out {
        Ok(o) => {
            let _ = f.write_all(&o.stdout);
            let _ = f.write_all(&o.stderr);
            o.status.success()
        }
        Err(e) => {
            let _ = writeln!(f, "spawn failed: {e:#}");
            false
        }
    }
}

// ---------- 输出（plain text / JSONL） ----------

struct Output {
    json: bool,
}

impl Output {
    fn new(json: bool) -> Self {
        Self { json }
    }

    fn emit_json(&self, v: Value) {
        println!("{v}");
    }
    fn emit_plain(&self, line: &str) {
        if self.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    fn preflight_done(&self, args: &SmokeArgs, pf: &Preflight) {
        if self.json {
            self.emit_json(json!({
                "event": "preflight_done",
                "repo": pf.repo_root.display().to_string(),
                "target": &args.target,
                "entry_kind": entry_kind_label(pf.entry_kind),
                "design_doc_path": pf.design_doc_path.as_ref().map(|p| p.display().to_string()),
                "seed_review_path": pf.seed_review_path.clone(),
                "seed_review_inline_hash": pf.seed_review_inline_hash.clone(),
            }));
            self.emit_json(json!({
                "event": "sessions_resolved",
                "claude": &pf.claude.session_id,
                "codex": &pf.codex.session_id,
            }));
        }
        self.emit_plain(&format!(
            "[preflight] repo={} target={} entry_kind={}",
            pf.repo_root.display(),
            args.target,
            entry_kind_label(pf.entry_kind),
        ));
        self.emit_plain(&format!(
            "[sessions] claude={} codex={}",
            pf.claude.session_id, pf.codex.session_id
        ));
    }

    /// `stage_starting`:阶段调用即将开始,此时 loop_id 还未生成。
    /// 真正的 loop_id 通过随后的 `progress` 事件携带(`{"event":"progress","loop_id":N,...}`),
    /// 终态 `stage_done` / `stage_failed` 也都带 loop_id。
    fn stage_starting(&self, stage: &str, resume_worktree: Option<&str>) {
        if self.json {
            let mut o = serde_json::Map::new();
            o.insert("event".into(), json!("stage_starting"));
            o.insert("stage".into(), json!(stage));
            if let Some(wt) = resume_worktree {
                o.insert("resume_worktree_path".into(), json!(wt));
            }
            self.emit_json(Value::Object(o));
        }
        self.emit_plain(&format!("[{stage}] starting"));
    }

    fn stage_done(&self, stage: &str, r: &RunLoopResult) {
        if self.json {
            self.emit_json(json!({
                "event": "stage_done",
                "stage": stage,
                "loop_id": r.loop_id,
                "status": r.status,
                "verdict": r.final_verdict,
                "rounds": r.total_rounds,
                "worktree_path": r.worktree_path,
            }));
        }
        let wt = r
            .worktree_path
            .as_deref()
            .map(|w| format!(" worktree={w}"))
            .unwrap_or_default();
        self.emit_plain(&format!(
            "[{stage}] loop_id={} status={} verdict={} rounds={}{}",
            r.loop_id,
            r.status,
            r.final_verdict.as_deref().unwrap_or("-"),
            r.total_rounds,
            wt
        ));
    }

    fn stage_failed(&self, stage: &str, r: &RunLoopResult) {
        if self.json {
            self.emit_json(json!({
                "event": "stage_failed",
                "stage": stage,
                "loop_id": r.loop_id,
                "status": r.status,
                "verdict": r.final_verdict,
                "reason": r.final_verdict.clone().unwrap_or_else(|| r.status.clone()),
            }));
        }
        self.emit_plain(&format!(
            "[{stage}] FAIL loop_id={} status={} verdict={}",
            r.loop_id,
            r.status,
            r.final_verdict.as_deref().unwrap_or("-"),
        ));
    }

    fn verify_done(&self, cmd: &str, ok: bool, log: &Path) {
        if self.json {
            self.emit_json(json!({
                "event": "verify_done",
                "cmd": cmd,
                "status": if ok { "pass" } else { "fail" },
                "log_tail_path": log.display().to_string(),
            }));
        }
        if ok {
            self.emit_plain(&format!("[verify] {cmd}: pass"));
        } else {
            self.emit_plain(&format!("[verify] {cmd}: fail (tail: {})", log.display()));
        }
    }

    fn error(&self, scope: &str, msg: &str) {
        if self.json {
            self.emit_json(json!({
                "event": "error",
                "scope": scope,
                "message": msg,
            }));
        }
        self.emit_plain(&format!("[error/{scope}] {msg}"));
    }

    fn smoke_done(
        &self,
        ok: bool,
        exit_code: i32,
        design: &RunLoopResult,
        impl_: Option<&RunLoopResult>,
        review: Option<&RunLoopResult>,
    ) {
        if self.json {
            self.emit_json(json!({
                "event": "smoke_done",
                "status": if ok { "pass" } else { "fail" },
                "exit_code": exit_code,
                "design_loop_id": design.loop_id,
                "implementation_loop_id": impl_.map(|r| r.loop_id),
                "review_loop_id": review.map(|r| r.loop_id),
            }));
        }
        let tag = if ok { "SMOKE PASS" } else { "SMOKE FAIL" };
        let mut line = format!("{tag} design={}", design.loop_id);
        if let Some(r) = impl_ {
            line.push_str(&format!(" implementation={}", r.loop_id));
            if let Some(wt) = &r.worktree_path {
                line.push_str(&format!(" worktree={wt}"));
            }
        }
        if let Some(r) = review {
            line.push_str(&format!(" review={}", r.loop_id));
        }
        line.push_str(&format!(" exit={exit_code}"));
        self.emit_plain(&line);
    }
}
