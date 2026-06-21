//! `CrossReviewTask`（`KIND = "cross_review"`）：编排 Codex↔Claude 复核↔修订循环。
//!
//! 终态语义（RFC §4 映射）：Pass / MaxRounds / AbortedTimeout / AbortedParse 都让 `run`
//! 返回 `Ok(output)` → 任务 succeeded；只有基础设施错（CLI 缺失 / spawn 失败 / DB 错）才
//! `Err` → failed。

use super::io;
use super::parse::{self, AskUser, Verdict};
use super::prompt::{self, ReviewMode, TargetRole, TargetSpec};
use super::validate;
use agent_session::driver;
use agent_session::store::Store;
use agent_session::{Provider, SessionRef};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use toolkit_tasks::{TaskCtx, TaskKind};

/// 等待 Claude 当前轮空闲的超时。
const CLAUDE_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// ASK_USER 挂起等待用户回答的轮询间隔与上限。
const ANSWER_POLL_INTERVAL: Duration = Duration::from_secs(2);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(1800);
/// 连续解析失败到此轮数 → AbortedParse。
const MAX_PARSE_FAILS: u32 = 2;

// ------------------------- 契约 -------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRefDto {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CrossReviewInput {
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
    #[serde(default)]
    pub notify_callback: Option<String>,
}

fn default_max_rounds() -> u32 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundRecord {
    pub round: u32,
    pub codex_verdict: Verdict,
    pub codex_review: String,
    pub claude_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalVerdict {
    Pass,
    MaxRounds,
    AbortedTimeout,
    AbortedParse,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CrossReviewOutput {
    pub rounds: Vec<RoundRecord>,
    pub final_verdict: FinalVerdict,
    pub total_rounds: u32,
}

// ------------------------- TaskKind -------------------------

pub struct CrossReviewTask;

#[async_trait]
impl TaskKind for CrossReviewTask {
    type Input = CrossReviewInput;
    type Output = CrossReviewOutput;
    const KIND: &'static str = "cross_review";

    async fn run(input: CrossReviewInput, ctx: TaskCtx) -> Result<CrossReviewOutput> {
        let store = Store::from_env().context("定位会话存储 (~/.codex / ~/.claude)")?;
        let loop_ctx = LoopCtx::new(input, ctx, store)?;
        loop_ctx.run().await
    }
}

/// 运行期上下文：解析好的两端 SessionRef + target 定位 + 配置。
struct LoopCtx {
    ctx: TaskCtx,
    store: Store,
    claude: SessionRef,
    codex: SessionRef,
    target: TargetSpec,
    mode: ReviewMode,
    max_rounds: u32,
    wait_for_claude_idle: bool,
    notify_callback: Option<String>,
    /// 复核/修订指令模板（DB 可配，缺省用 codeloop-core 内置）。new() 解析一次。
    codex_template: String,
    claude_template: String,
}

impl LoopCtx {
    fn new(input: CrossReviewInput, ctx: TaskCtx, store: Store) -> Result<Self> {
        let claude = resolve_ref(&store, Provider::Claude, &input.claude)?;
        let mut codex = resolve_ref(&store, Provider::Codex, &input.codex)?;

        // §4.1 三方一致性校验在任务内**权威执行**：HTTP /codeloop/submit 只是提前返回 400
        // 的友好层；经通用 /tasks 入口直接提交 cross_review 也必须过此校验，避免跑错仓 / 越界。
        let validated = validate::validate_three_way(&claude.cwd, &codex.cwd, &input.target_path)
            .context("三方仓库一致性校验失败")?;

        // 去掉 Windows `\\?\` 扩展前缀，用于子进程 `--cd` 与 prompt 展示。
        let repo_root = validate::display_path(&validated.repo_root);
        let target_abs = validate::display_path(&validated.target_abs);
        let repo_rel = validated
            .target_abs
            .strip_prefix(&validated.repo_root)
            .unwrap_or(&validated.target_abs)
            .to_string_lossy()
            .replace('\\', "/");

        // Codex `exec resume` 的 `--cd` 用工作树根（比子目录 cwd 更稳），消除相对路径歧义。
        // Claude `--resume` 必须在会话原始 cwd 下执行，保持不动。
        codex.cwd = repo_root.clone();

        let repo_root_s = repo_root.to_string_lossy().to_string();
        let target_abs_s = target_abs.to_string_lossy().to_string();
        let label = input
            .target_label
            .unwrap_or_else(|| prompt::default_label(&repo_rel));
        let target = TargetSpec {
            label,
            repo_root: repo_root_s,
            repo_rel,
            abs: target_abs_s,
        };

        // 解析可配指令模板：DB 覆盖优先，否则 codeloop-core 内置默认。未知 name 不可能（内置已登记）
        // ——保险起见仍回退内置常量。
        let codex_template =
            crate::llm::resolve_prompt(&ctx.pool, crate::llm::NAME_CODELOOP_CODEX_REVIEW)
                .unwrap_or_else(|_| prompt::DEFAULT_CODEX_TEMPLATE.to_string());
        let claude_template =
            crate::llm::resolve_prompt(&ctx.pool, crate::llm::NAME_CODELOOP_CLAUDE_REVISION)
                .unwrap_or_else(|_| prompt::DEFAULT_CLAUDE_TEMPLATE.to_string());

        Ok(Self {
            ctx,
            store,
            claude,
            codex,
            target,
            mode: input.mode,
            max_rounds: input.max_rounds.max(1),
            wait_for_claude_idle: input.wait_for_claude_idle,
            notify_callback: input.notify_callback,
            codex_template,
            claude_template,
        })
    }

    async fn run(&self) -> Result<CrossReviewOutput> {
        if self.wait_for_claude_idle {
            // 等待失败（超时）当业务终态处理，不致整任务 failed。
            if let Err(e) =
                agent_session::watch::wait_for_idle(&self.store, &self.claude, CLAUDE_IDLE_TIMEOUT)
                    .await
            {
                log::warn!("wait_for_claude_idle 超时/失败，按 AbortedTimeout 处理: {e:#}");
                return Ok(self
                    .finish(Vec::new(), FinalVerdict::AbortedTimeout, 0)
                    .await);
            }
        }

        let mut rounds: Vec<RoundRecord> = Vec::new();
        let mut consecutive_parse_fail = 0u32;

        for n in 1..=self.max_rounds {
            // 1. Codex 复核（含 ASK_USER 挂起处理）。
            // first_turn = n==1：常驻说明块（定位 + ASK_USER 协议）只在持续会话首轮发一次，
            // 后续轮依赖会话历史，不再重发（避免每条消息末尾重复刷屏/占 token）。
            // toolkit-server 路径：保持今天行为不变。
            // mode=Design → 文档复核 (target_path 既是规格也是修订对象);
            // mode=Implementation → 复核 target_path 所指代码 (与今天 Implement 入口一致)。
            let target_role = match self.mode {
                ReviewMode::Design => TargetRole::RevisionDoc,
                ReviewMode::Implementation => TargetRole::SpecDoc,
            };
            let codex_prompt = prompt::render_codex_prompt(
                &self.codex_template,
                &self.target,
                self.mode,
                n,
                n == 1,
                target_role,
                None,
                // toolkit-server 旁路（非桌面 codeloop 主路径）暂不开放 evaluate_alternatives——
                // 默认 SCOPE 已含现状分析维度，按需再加入参。
                false,
            );
            let review = match self.send_and_resolve(&self.codex, &codex_prompt).await? {
                Resolved::Reply(r) => r,
                Resolved::Timeout => {
                    return Ok(self
                        .finish(rounds, FinalVerdict::AbortedTimeout, n - 1)
                        .await);
                }
            };

            // 2. 解析 VERDICT。
            let verdict = match parse::parse_verdict(&review) {
                Some(v) => {
                    consecutive_parse_fail = 0;
                    v
                }
                None => {
                    consecutive_parse_fail += 1;
                    if consecutive_parse_fail >= MAX_PARSE_FAILS {
                        self.report(json!({
                            "round": n, "phase": "reviewed",
                            "verdict": "parse_failed", "consecutive_parse_fail": consecutive_parse_fail,
                        }))?;
                        return Ok(self.finish(rounds, FinalVerdict::AbortedParse, n - 1).await);
                    }
                    Verdict::NeedsWork // 单次解析失败保守续跑
                }
            };
            self.report(json!({
                "round": n, "phase": "reviewed", "verdict": verdict,
            }))?;

            // 3. PASS → 终止。
            if verdict == Verdict::Pass {
                rounds.push(RoundRecord {
                    round: n,
                    codex_verdict: verdict,
                    codex_review: review,
                    claude_revision: String::new(),
                });
                return Ok(self.finish(rounds, FinalVerdict::Pass, n).await);
            }

            // 4. Claude 据意见修订（含 ASK_USER 挂起处理）。
            // Claude 仅在 NEEDS_WORK 时被发起，其首次发送恒为第 1 轮 → n==1 即首轮。
            let target_role = match self.mode {
                ReviewMode::Design => TargetRole::RevisionDoc,
                ReviewMode::Implementation => TargetRole::SpecDoc,
            };
            let claude_prompt = prompt::render_claude_prompt(
                &self.claude_template,
                &self.target,
                &review,
                n == 1,
                target_role,
                None,
            );
            let revision = match self.send_and_resolve(&self.claude, &claude_prompt).await? {
                Resolved::Reply(r) => r,
                Resolved::Timeout => {
                    rounds.push(RoundRecord {
                        round: n,
                        codex_verdict: verdict,
                        codex_review: review,
                        claude_revision: String::new(),
                    });
                    return Ok(self.finish(rounds, FinalVerdict::AbortedTimeout, n).await);
                }
            };
            self.report(json!({ "round": n, "phase": "revised" }))?;

            rounds.push(RoundRecord {
                round: n,
                codex_verdict: verdict,
                codex_review: review,
                claude_revision: revision,
            });
        }

        // 跑满未 PASS。
        let total = self.max_rounds;
        Ok(self.finish(rounds, FinalVerdict::MaxRounds, total).await)
    }

    /// 发一轮 → 若含 ASK_USER 则挂起等用户答 → 把答案发回同一会话 → 直到不再 ASK_USER。
    async fn send_and_resolve(&self, session: &SessionRef, prompt: &str) -> Result<Resolved> {
        let mut current_prompt = prompt.to_string();
        loop {
            let turn = driver::send(session, &current_prompt).await?; // 基础设施错 → Err → failed
            let Some(q) = parse::parse_ask_user(&turn.reply_text) else {
                return Ok(Resolved::Reply(turn.reply_text));
            };
            // 挂起：落 DB + 上报 + notify。
            let asked_by = session.provider.as_str();
            let q_json = serde_json::to_string(&q).unwrap_or_else(|_| "{}".to_string());
            let seq = io::insert_question(&self.ctx.pool, &self.ctx.task_id, asked_by, &q_json)?;
            self.report(json!({
                "phase": "awaiting_input", "seq": seq, "asked_by": asked_by, "question": q,
            }))?;
            self.notify_awaiting_input(seq, &q).await;

            // 轮询答案。
            match self.poll_answer(seq).await? {
                Some(answer) => current_prompt = format!("用户答复：{answer}"),
                None => return Ok(Resolved::Timeout),
            }
        }
    }

    /// 每 2s 轮询 codeloop_io.answer_text 直到非 NULL 或超时。
    async fn poll_answer(&self, seq: i64) -> Result<Option<String>> {
        let deadline = std::time::Instant::now() + ANSWER_TIMEOUT;
        loop {
            if let Some(ans) = io::read_answer(&self.ctx.pool, &self.ctx.task_id, seq)? {
                return Ok(Some(ans));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(ANSWER_POLL_INTERVAL).await;
        }
    }

    /// 终态收尾：上报 + done 提醒，构造 Output。
    async fn finish(
        &self,
        rounds: Vec<RoundRecord>,
        final_verdict: FinalVerdict,
        total_rounds: u32,
    ) -> CrossReviewOutput {
        let _ = self.report(json!({
            "phase": "done", "final_verdict": final_verdict, "total_rounds": total_rounds,
        }));
        self.notify_done(final_verdict).await;
        CrossReviewOutput {
            rounds,
            final_verdict,
            total_rounds,
        }
    }

    fn report(&self, value: Value) -> Result<()> {
        self.ctx.report_progress(value)
    }

    async fn notify_awaiting_input(&self, seq: i64, q: &AskUser) {
        self.notify(
            "awaiting_input",
            json!({ "seq": seq, "question": q.question, "options": q.options }),
        )
        .await;
    }

    async fn notify_done(&self, final_verdict: FinalVerdict) {
        self.notify("done", json!({ "verdict": final_verdict }))
            .await;
    }

    /// best-effort POST 到 notify_callback；失败只记日志，不影响任务。
    async fn notify(&self, kind: &str, extra: Value) {
        let Some(url) = self.notify_callback.as_deref() else {
            return;
        };
        let mut payload = json!({
            "task_id": self.ctx.task_id,
            "kind": kind,
            "title": self.target.label,
        });
        if let (Value::Object(map), Value::Object(ex)) = (&mut payload, extra) {
            map.extend(ex);
        }
        let client = reqwest::Client::new();
        match client.post(url).json(&payload).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    log::warn!("codeloop notify({kind}) 回调返回 {}", resp.status());
                }
            }
            Err(e) => log::warn!("codeloop notify({kind}) 投递失败: {e:#}"),
        }
    }
}

/// send_and_resolve 的结果：拿到回复，或等用户答超时。
enum Resolved {
    Reply(String),
    Timeout,
}

/// 把 DTO 解析成 SessionRef：cwd 缺省时从会话存储 snapshot 补全。
fn resolve_ref(store: &Store, provider: Provider, dto: &SessionRefDto) -> Result<SessionRef> {
    let cwd = match &dto.cwd {
        Some(c) if !c.is_empty() => PathBuf::from(c),
        _ => {
            store
                .snapshot(provider, &dto.session_id)
                .with_context(|| format!("解析 {} 会话 cwd", provider.as_str()))?
                .cwd
        }
    };
    Ok(SessionRef {
        provider,
        session_id: dto.session_id.clone(),
        cwd,
    })
}
