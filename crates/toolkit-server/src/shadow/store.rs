//! `shadow_attempt` / `shadow_stat` 读写：跟读明细落档 + 累计统计维护/查询。
//!
//! 表 DDL 在 `toolkit_core::schema`（DDL_V1，幂等加表）。`word_index` 在 `shadow_stat`
//! 用 `-1` 占位代表整句单元（NULL 进不了复合主键）。

use anyhow::{Context, Result};
use rusqlite::params;
use serde::Serialize;
use toolkit_core::{new_task_id, now_iso8601, SqlitePool};

use super::{ScoreResult, ShadowKind};

/// 整句单元在 `shadow_stat.word_index` 的占位值。
const SENTENCE_WORD_INDEX: i64 = -1;

/// 一个跟读单元的累计统计（对外 JSON）。
#[derive(Debug, Clone, Serialize)]
pub struct StatRow {
    pub kind: String,
    pub sentence_id: i64,
    /// 整句单元为 `null`；词单元为句内词序号。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_index: Option<i64>,
    pub success_count: i64,
    pub fail_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_passed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_at: Option<String>,
}

/// 记一次尝试：插 `shadow_attempt` 明细 + upsert 累加 `shadow_stat`，返回累加后的统计。
pub fn record_attempt(
    pool: &SqlitePool,
    customer_id: i64,
    kind: ShadowKind,
    sentence_id: i64,
    word_index: Option<i64>,
    result: &ScoreResult,
) -> Result<StatRow> {
    let conn = pool.get()?;
    let now = now_iso8601();
    let passed_i = if result.passed { 1 } else { 0 };
    // 仅 GOP 后端有音素/词级发音明细时落 detail_json；v1-ASR 内核为 NULL。
    let detail_json = build_detail_json(result);

    conn.execute(
        "INSERT INTO shadow_attempt
            (id, customer_id, kind, sentence_id, word_index, ref_text, transcript, score, passed, detail_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            new_task_id(),
            customer_id,
            kind.as_str(),
            sentence_id,
            word_index,
            result.ref_text,
            result.transcript,
            result.score,
            passed_i,
            detail_json,
            now,
        ],
    )
    .context("insert shadow_attempt")?;

    let wi = word_index.unwrap_or(SENTENCE_WORD_INDEX);
    let succ_inc = if result.passed { 1 } else { 0 };
    let fail_inc = if result.passed { 0 } else { 1 };
    // upsert：首次插入计数=本次结果；冲突则累加。
    conn.execute(
        "INSERT INTO shadow_stat
            (customer_id, kind, sentence_id, word_index, success_count, fail_count, last_score, last_passed, last_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(customer_id, sentence_id, word_index, kind) DO UPDATE SET
            success_count = success_count + ?5,
            fail_count    = fail_count + ?6,
            last_score    = ?7,
            last_passed   = ?8,
            last_at       = ?9",
        params![
            customer_id,
            kind.as_str(),
            sentence_id,
            wi,
            succ_inc,
            fail_inc,
            result.score,
            passed_i,
            now,
        ],
    )
    .context("upsert shadow_stat")?;

    let row = conn
        .query_row(
            "SELECT kind, sentence_id, word_index, success_count, fail_count, last_score, last_passed, last_at
             FROM shadow_stat
             WHERE customer_id = ?1 AND sentence_id = ?2 AND word_index = ?3 AND kind = ?4",
            params![customer_id, sentence_id, wi, kind.as_str()],
            map_stat_row,
        )
        .context("read back shadow_stat")?;
    Ok(row)
}

/// 构造发音级明细 JSON（落 `shadow_attempt.detail_json`）：仅当结果带 GOP 明细
/// （`bad_phone_count` 有值或任一词带 `pron_status`）时返回 `Some`，否则 `None`（v1 不落）。
/// 存 `{words, bad_phone_count, model}`，便于回看/按 attempt 重算，不重复 ref/transcript。
fn build_detail_json(result: &ScoreResult) -> Option<String> {
    let has_detail =
        result.bad_phone_count.is_some() || result.words.iter().any(|w| w.pron_status.is_some());
    if !has_detail {
        return None;
    }
    serde_json::to_string(&serde_json::json!({
        "words": result.words,
        "bad_phone_count": result.bad_phone_count,
        "model": result.model,
    }))
    .ok()
}

/// 批量查询某用户在给定句子（及其词单元）上的统计，供进入播放时一次性回填。
pub fn query_stats(
    pool: &SqlitePool,
    customer_id: i64,
    sentence_ids: &[i64],
) -> Result<Vec<StatRow>> {
    if sentence_ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = pool.get()?;
    let placeholders = vec!["?"; sentence_ids.len()].join(",");
    let sql = format!(
        "SELECT kind, sentence_id, word_index, success_count, fail_count, last_score, last_passed, last_at
         FROM shadow_stat
         WHERE customer_id = ? AND sentence_id IN ({placeholders})
         ORDER BY sentence_id, word_index"
    );
    let mut stmt = conn.prepare(&sql).context("prepare query_stats")?;
    // 参数：customer_id 在前，随后是各 sentence_id。
    let mut binds: Vec<i64> = Vec::with_capacity(sentence_ids.len() + 1);
    binds.push(customer_id);
    binds.extend_from_slice(sentence_ids);
    let rows = stmt
        .query_map(rusqlite::params_from_iter(binds), map_stat_row)
        .context("query_map shadow_stat")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect shadow_stat rows")?;
    Ok(rows)
}

fn map_stat_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StatRow> {
    let wi: i64 = r.get(2)?;
    let last_passed: Option<i64> = r.get(6)?;
    Ok(StatRow {
        kind: r.get(0)?,
        sentence_id: r.get(1)?,
        word_index: if wi == SENTENCE_WORD_INDEX {
            None
        } else {
            Some(wi)
        },
        success_count: r.get(3)?,
        fail_count: r.get(4)?,
        last_score: r.get(5)?,
        last_passed: last_passed.map(|v| v != 0),
        last_at: r.get(7)?,
    })
}
