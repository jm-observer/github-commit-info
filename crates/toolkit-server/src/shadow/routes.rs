//! 跟读判分 HTTP 路由（挂在 `/api/web/shadow`）：
//!   - `POST /api/web/shadow/score`：query 带单元元信息 + 原始音频 body → 转写打分落库
//!   - `GET  /api/web/shadow/stats`：批量回读某用户在给定句子上的累计统计
//!
//! 端点只服务桌面端（zero-desktop）自身,音频 clip 短（数秒），故不走 multipart：
//! 元信息进 query、音频字节直接做请求 body（仿 `/api/web/audio/clean` 的 raw body 风格）。

use crate::shadow::{self, store, ShadowKind};
use crate::state::AppState;
use asr_client::{AsrClient, TranscribeOpts};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::json;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/score", post(score))
        .route("/stats", get(stats))
}

#[derive(Debug, Deserialize)]
struct ScoreParams {
    customer_id: i64,
    kind: String,
    sentence_id: i64,
    #[serde(default)]
    word_index: Option<i64>,
    ref_text: String,
    #[serde(default)]
    threshold: Option<f64>,
    /// 录音 MIME（如 `audio/webm` / `audio/wav`）；仅作 multipart 元数据，FunASR 靠
    /// ffmpeg 自识别容器。缺省 `audio/webm`。
    #[serde(default)]
    mime: Option<String>,
}

fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.into() })),
    )
        .into_response()
}

fn bad_gateway(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": msg.into() })),
    )
        .into_response()
}

/// `POST /api/web/shadow/score`。
async fn score(
    State(state): State<AppState>,
    Query(p): Query<ScoreParams>,
    body: Bytes,
) -> Response {
    let Some(kind) = ShadowKind::parse(&p.kind) else {
        return bad_request(format!("未知 kind: {}（应为 sentence|word）", p.kind));
    };
    if p.ref_text.trim().is_empty() {
        return bad_request("ref_text 不能为空");
    }
    if body.is_empty() {
        return bad_request("音频 body 为空");
    }
    let threshold = p.threshold.unwrap_or(shadow::DEFAULT_THRESHOLD);
    let mime = p.mime.unwrap_or_else(|| "audio/webm".to_string());
    let file_name = mime_filename(&mime);

    // 转写：vad=false → 整段一锤识别（跟读 clip 短，不需要切段）。
    let client = AsrClient::new(shadow::asr_base());
    let transcription = match client
        .transcribe_bytes(
            body.to_vec(),
            file_name,
            mime,
            TranscribeOpts { vad: false },
        )
        .await
    {
        Ok(t) => t,
        Err(e) => return bad_gateway(format!("ASR 转写失败: {e}")),
    };

    let result = shadow::score(kind, &p.ref_text, &transcription.text, threshold);

    // 落库 + 累加统计。DB 失败不该吞掉判分结果——记日志但仍把分数返回前端。
    let stat = match store::record_attempt(
        &state.pool,
        p.customer_id,
        kind,
        p.sentence_id,
        p.word_index,
        &result,
    ) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("shadow record_attempt 失败（判分仍返回）: {e:#}");
            None
        }
    };

    Json(json!({
        "transcript": result.transcript,
        "ref_text": result.ref_text,
        "score": result.score,
        "passed": result.passed,
        "asr_model": transcription.model,
        "words": result.words,
        "stat": stat,
    }))
    .into_response()
}

#[derive(Debug, Deserialize)]
struct StatsParams {
    customer_id: i64,
    /// 逗号分隔的句子 id 列表。
    sentence_ids: String,
}

/// `GET /api/web/shadow/stats?customer_id=&sentence_ids=1,2,3`。
async fn stats(State(state): State<AppState>, Query(p): Query<StatsParams>) -> Response {
    let ids: Vec<i64> = p
        .sentence_ids
        .split(',')
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .collect();
    match store::query_stats(&state.pool, p.customer_id, &ids) {
        Ok(rows) => Json(json!({ "stats": rows })).into_response(),
        Err(e) => bad_gateway(format!("查询统计失败: {e:#}")),
    }
}

/// MIME → 上传文件名（仅供 multipart 元数据与上游日志）。
fn mime_filename(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or(mime).trim() {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "shadow.wav",
        "audio/mpeg" => "shadow.mp3",
        "audio/ogg" => "shadow.ogg",
        "audio/mp4" | "audio/x-m4a" => "shadow.m4a",
        _ => "shadow.webm",
    }
}
