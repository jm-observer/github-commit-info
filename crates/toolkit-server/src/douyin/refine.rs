//! Phase 2：`douyin_text_refine` TaskKind —— LLM 整理 ASR 原文。
//!
//! 形态（与抖音其它 kind 略有不同：本任务直接在 toolkit 进程内逐条调 LLM，不另起下游
//! worker）：
//! 1. 解析输入：显式 `aweme_ids` 或「全部已转写未整理」（`list_pending_refine`）。
//! 2. 逐条读 ASR 原文（`douyin/transcripts/<id>.json`）→ 调 GB10 vLLM → 落
//!    `douyin/refined/<id>.json`（带模型 / 时间戳 / prompt 版本+hash 元信息）。
//! 3. 每条进度写 progress（done/total/failed）；单条失败不拖垮整批，失败列表进 output。
//! 4. trace：建一个 `text_refine_batch` 顶层 span，每条 LLM 调用是其子 span。
//!
//! 整理稿随后由 `kb_publish` / pipeline 读取回填进 knowledge md（被 rag 优先索引）。

use crate::douyin_mod::paths::DouyinPaths;
use anyhow::Result;
use async_trait::async_trait;
use custom_utils::trace::{self, SpanScope, SpanStatus, TraceContext};
use douyin::process::read_transcript;
use douyin::refine::{list_pending_refine, refine_one_traced};
use serde::{Deserialize, Serialize};
use serde_json::json;
use toolkit_tasks::{TaskCtx, TaskKind};

#[derive(Debug, Serialize, Deserialize)]
pub struct RefineInput {
    /// 抖音号（仅用于回显/日志，整理本身按 aweme_id 工作）。
    #[serde(default)]
    pub unique_id: Option<String>,
    /// 显式整理这些 aweme_id；为空时整理「全部已转写未整理」。
    #[serde(default)]
    pub aweme_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefineFailure {
    pub aweme_id: String,
    pub error: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefineOutput {
    pub total: usize,
    pub refined: usize,
    pub skipped: usize,
    pub failed: usize,
    pub model: String,
    pub prompt_version: String,
    pub prompt_hash: String,
    pub failures: Vec<RefineFailure>,
    pub refined_ids: Vec<String>,
}

pub struct DouyinTextRefine;

#[async_trait]
impl TaskKind for DouyinTextRefine {
    type Input = RefineInput;
    type Output = RefineOutput;
    const KIND: &'static str = "douyin_text_refine";

    async fn run(input: RefineInput, ctx: TaskCtx) -> Result<RefineOutput> {
        // 提交即校验 LLM 配置：DB / env 都没配大模型时明确报错（不进入逐条循环）。
        // 提示词与版本走可配目录（DB 覆盖优先，否则 douyin 内置默认 douyin_refine）。
        let client = crate::llm::resolve_client(&ctx.pool)?;
        let prompt_template =
            crate::llm::resolve_prompt(&ctx.pool, crate::llm::NAME_DOUYIN_REFINE)?;
        let prompt_version =
            crate::llm::resolve_prompt_version(&ctx.pool, crate::llm::NAME_DOUYIN_REFINE)?;
        let prompt_hash = toolkit_llm::prompt_hash(&prompt_template);
        let model = client.model().to_string();
        let paths = DouyinPaths::new(&ctx.data_dir);
        paths.ensure_dirs()?;

        let ids = if input.aweme_ids.is_empty() {
            list_pending_refine(&paths.transcript_dir, &paths.refined_dir)?
        } else {
            input.aweme_ids.clone()
        };
        if ids.is_empty() {
            return Ok(RefineOutput {
                total: 0,
                refined: 0,
                skipped: 0,
                failed: 0,
                model: model.clone(),
                prompt_version: prompt_version.clone(),
                prompt_hash: prompt_hash.clone(),
                failures: vec![],
                refined_ids: vec![],
            });
        }

        let total = ids.len();
        ctx.report_progress(json!({
            "stage": "refine",
            "total": total,
            "done": 0,
            "failed": 0,
            "model": model,
        }))?;

        // text_refine_batch 顶层 span（两阶段）：每条 LLM 调用作为其子 span（用
        // batch_ctx.child()）。trace 关闭时为 None，子 span 也 no-op。
        let batch_ctx = trace::enabled().then(TraceContext::root);
        let batch_scope = batch_ctx.as_ref().map(|c| {
            let scope = SpanScope::new(c.clone(), "text_refine_batch").with_summary(json!({
                "total": total,
                "model": model,
            }));
            scope.emit_start();
            scope
        });

        let mut refined_ids = Vec::new();
        let mut failures = Vec::new();
        let mut skipped = 0usize;

        for (i, id) in ids.iter().enumerate() {
            let transcript = read_transcript(&paths.transcript_dir, id);
            let asr_text = match transcript {
                Some(t) if !t.text.trim().is_empty() => t.text,
                Some(_) => {
                    skipped += 1;
                    Self::report(&ctx, total, &refined_ids, &failures, skipped, i + 1)?;
                    continue;
                }
                None => {
                    failures.push(RefineFailure {
                        aweme_id: id.clone(),
                        error: "无 ASR 转写缓存".to_string(),
                    });
                    Self::report(&ctx, total, &refined_ids, &failures, skipped, i + 1)?;
                    continue;
                }
            };

            // 复现 douyin crate 实际发送的 user 消息（{TRANSCRIPT} 是稳定契约），供会话记录。
            let rendered = prompt_template.replace("{TRANSCRIPT}", asr_text.trim());
            match refine_one_traced(
                &client,
                &prompt_template,
                &prompt_version,
                &paths.refined_dir,
                id,
                &asr_text,
                batch_ctx.as_ref(),
            )
            .await
            {
                Ok(refined) => {
                    // 每条整理落一个 session（kind=douyin_refine），失败只 warn 不拖垮批处理。
                    crate::llm::record::record_exchange(
                        &ctx.pool,
                        crate::llm::NAME_DOUYIN_REFINE,
                        &format!("整理 {id}"),
                        &model,
                        crate::llm::NAME_DOUYIN_REFINE,
                        &rendered,
                        &refined.refined_text,
                        json!({
                            "aweme_id": id,
                            "unique_id": input.unique_id,
                            "prompt_version": refined.prompt_version,
                            "prompt_hash": refined.prompt_hash,
                        }),
                    );
                    refined_ids.push(id.clone());
                }
                Err(e) => failures.push(RefineFailure {
                    aweme_id: id.clone(),
                    error: format!("{e:#}"),
                }),
            }
            Self::report(&ctx, total, &refined_ids, &failures, skipped, i + 1)?;
        }

        if let Some(scope) = batch_scope {
            let status = if failures.is_empty() {
                SpanStatus::Ok
            } else {
                SpanStatus::Error(format!("{} 条失败", failures.len()))
            };
            scope.emit_end(
                None,
                status,
                Some(json!({
                    "total": total,
                    "refined": refined_ids.len(),
                    "failed": failures.len(),
                })),
            );
        }

        Ok(RefineOutput {
            total,
            refined: refined_ids.len(),
            skipped,
            failed: failures.len(),
            model,
            prompt_version,
            prompt_hash,
            failures,
            refined_ids,
        })
    }
}

impl DouyinTextRefine {
    fn report(
        ctx: &TaskCtx,
        total: usize,
        refined_ids: &[String],
        failures: &[RefineFailure],
        skipped: usize,
        done: usize,
    ) -> Result<()> {
        ctx.report_progress(json!({
            "stage": "refine",
            "total": total,
            "done": done,
            "refined": refined_ids.len(),
            "skipped": skipped,
            "failed": failures.len(),
        }))
    }
}
