//! `/api/web/llm/*`：公共大模型的连接配置 + 可配提示词 + 连通性自测 + 对话总结。
//!
//! - `GET /config`            读当前生效连接配置（含来源 db/env/none，api_key 仅回 has_api_key）。
//! - `PUT /config`            写连接配置（持久化到 toolkit.db，立即对后续请求生效）。
//! - `GET /prompts`           列全部提示词（内置默认 + DB 覆盖，标注是否已修改）。
//! - `GET /prompts/{name}`    读单条生效提示词 + 内置默认（供控制台对比）。
//! - `PUT /prompts/{name}`    覆盖提示词（写 DB）。
//! - `DELETE /prompts/{name}` 重置为内置默认（删 DB 行）。
//! - `POST /ping`             用当前配置发一次最小请求，自测连通性。
//! - `POST /summarize`        对话总结：用 `chat_summary` 提示词总结粘贴的会话文本。

use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};
use toolkit_core::llm_sessions;
use toolkit_core::llm_store::{self, StoredLlmConfig};
use toolkit_llm::prompt_hash;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/config", get(get_config).put(put_config))
        .route("/prompts", get(list_prompts))
        .route(
            "/prompts/{name}",
            get(get_prompt).put(put_prompt).delete(reset_prompt),
        )
        .route("/ping", post(ping))
        .route("/summarize", post(summarize))
        .route("/sessions", get(list_sessions_h).post(create_chat_h))
        .route("/sessions/{id}", get(get_session_h))
        .route("/sessions/{id}/messages", post(chat_send_h))
}

fn err(code: StatusCode, msg: String) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}

fn internal(e: anyhow::Error) -> (StatusCode, Json<Value>) {
    err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

// ---------------- 连接配置 ----------------

async fn get_config(State(s): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let source = super::config_source(&s.pool).map_err(internal)?;
    let stored = llm_store::get_config(&s.pool).map_err(internal)?;
    // 生效值：能解析出就回显 base_url/model（env 来源也回显），否则空。
    let effective = super::resolve_config(&s.pool).ok();
    let has_api_key = match source {
        super::ConfigSource::Db => stored.as_ref().and_then(|c| c.api_key.as_ref()).is_some(),
        super::ConfigSource::Env => std::env::var("LLM_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some(),
        super::ConfigSource::None => false,
    };
    Ok(Json(json!({
        "source": source.as_str(),
        "db_configured": stored.is_some(),
        "base_url": effective.as_ref().map(|c| c.base_url.clone()).unwrap_or_default(),
        "model": effective.as_ref().map(|c| c.model.clone()).unwrap_or_default(),
        "has_api_key": has_api_key,
    })))
}

#[derive(Debug, Deserialize)]
struct PutConfigBody {
    base_url: String,
    model: String,
    /// 省略 / null = 不改动已存的 key；空串 = 清空 key。
    #[serde(default)]
    api_key: Option<String>,
}

async fn put_config(
    State(s): State<AppState>,
    Json(body): Json<PutConfigBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.base_url.trim().is_empty() || body.model.trim().is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "base_url 与 model 不能为空".to_string(),
        ));
    }
    // api_key 语义：None = 保留原值；Some("") = 清空；Some(k) = 设置。
    let api_key = match body.api_key {
        None => llm_store::get_config(&s.pool)
            .map_err(internal)?
            .and_then(|c| c.api_key),
        Some(k) if k.trim().is_empty() => None,
        Some(k) => Some(k),
    };
    llm_store::set_config(
        &s.pool,
        &StoredLlmConfig {
            base_url: body.base_url.trim_end_matches('/').to_string(),
            model: body.model,
            api_key,
        },
    )
    .map_err(internal)?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------- 提示词 ----------------

async fn list_prompts(State(s): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let overrides = llm_store::list_prompts(&s.pool).map_err(internal)?;
    let find = |name: &str| overrides.iter().find(|p| p.name == name);

    let mut out: Vec<Value> = Vec::new();
    // 内置目录 + DB 覆盖合并。
    for b in super::builtins() {
        let ov = find(b.name);
        let (text, source) = match ov {
            Some(p) => (p.text.clone(), "db"),
            None => (b.default_text.to_string(), "builtin"),
        };
        let modified = ov.map(|p| p.text != b.default_text).unwrap_or(false);
        out.push(json!({
            "name": b.name,
            "description": b.description,
            "version": ov.map(|p| p.version.clone()).unwrap_or_else(|| b.version.to_string()),
            "placeholders": b.placeholders,
            "source": source,
            "modified": modified,
            "has_builtin": true,
            "text": text,
        }));
    }
    // DB 里存在但不在内置目录的自定义提示词。
    for p in &overrides {
        if super::builtin(&p.name).is_none() {
            out.push(json!({
                "name": p.name,
                "description": "(自定义提示词)",
                "version": p.version,
                "placeholders": Vec::<String>::new(),
                "source": "db",
                "modified": true,
                "has_builtin": false,
                "text": p.text,
            }));
        }
    }
    Ok(Json(json!({ "prompts": out })))
}

async fn get_prompt(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let ov = llm_store::get_prompt(&s.pool, &name).map_err(internal)?;
    let builtin = super::builtin(&name);
    if ov.is_none() && builtin.is_none() {
        return Err(err(StatusCode::NOT_FOUND, format!("未知提示词 {name}")));
    }
    let builtin_text = builtin.as_ref().map(|b| b.default_text.to_string());
    let text = ov
        .as_ref()
        .map(|p| p.text.clone())
        .or_else(|| builtin_text.clone())
        .unwrap_or_default();
    Ok(Json(json!({
        "name": name,
        "description": builtin.as_ref().map(|b| b.description.to_string()),
        "version": ov.as_ref().map(|p| p.version.clone())
            .or_else(|| builtin.as_ref().map(|b| b.version.to_string())),
        "placeholders": builtin.as_ref().map(|b| b.placeholders.to_vec()).unwrap_or_default(),
        "source": if ov.is_some() { "db" } else { "builtin" },
        "modified": match (&ov, &builtin_text) { (Some(p), Some(d)) => &p.text != d, _ => false },
        "has_builtin": builtin.is_some(),
        "text": text,
        "builtin_text": builtin_text,
    })))
}

#[derive(Debug, Deserialize)]
struct PutPromptBody {
    text: String,
    #[serde(default)]
    version: Option<String>,
}

async fn put_prompt(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PutPromptBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.text.trim().is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "提示词文本不能为空".to_string(),
        ));
    }
    let builtin = super::builtin(&name);
    let builtin_hash = builtin.as_ref().map(|b| prompt_hash(b.default_text));
    let version = body
        .version
        .filter(|v| !v.trim().is_empty())
        .or_else(|| builtin.as_ref().map(|b| b.version.to_string()))
        .unwrap_or_else(|| "custom".to_string());
    let hash = prompt_hash(&body.text);
    llm_store::set_prompt(
        &s.pool,
        &name,
        &body.text,
        &version,
        &hash,
        builtin_hash.as_deref(),
    )
    .map_err(internal)?;
    Ok(Json(
        json!({ "ok": true, "hash": hash, "version": version }),
    ))
}

/// 重置为内置默认：删 DB 覆盖行。无内置默认的自定义提示词同样按删除处理。
async fn reset_prompt(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let n = llm_store::delete_prompt(&s.pool, &name).map_err(internal)?;
    Ok(Json(json!({ "ok": true, "deleted": n })))
}

// ---------------- 连通性自测 ----------------

async fn ping(State(s): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = super::resolve_client(&s.pool)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    match client.complete("请只回复两个字：可用").await {
        Ok(reply) => Ok(Json(json!({
            "ok": true,
            "model": client.model(),
            "reply": reply.chars().take(100).collect::<String>(),
        }))),
        Err(e) => Err(err(StatusCode::BAD_GATEWAY, format!("调用失败：{e:#}"))),
    }
}

// ---------------- 对话总结 ----------------

#[derive(Debug, Deserialize)]
struct SummarizeBody {
    /// 待总结的会话/文本。
    text: String,
}

async fn summarize(
    State(s): State<AppState>,
    Json(body): Json<SummarizeBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.text.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "text 不能为空".to_string()));
    }
    let client = super::resolve_client(&s.pool)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let template = super::resolve_prompt(&s.pool, super::NAME_CHAT_SUMMARY).map_err(internal)?;
    let prompt = template.replace("{CONVERSATION}", body.text.trim());
    match client.complete(&prompt).await {
        Ok(summary) => {
            let title: String = body.text.trim().chars().take(40).collect();
            super::record::record_exchange(
                &s.pool,
                super::NAME_CHAT_SUMMARY,
                &title,
                client.model(),
                super::NAME_CHAT_SUMMARY,
                &prompt,
                &summary,
                json!({}),
            );
            Ok(Json(json!({ "summary": summary, "model": client.model() })))
        }
        Err(e) => Err(err(StatusCode::BAD_GATEWAY, format!("调用失败：{e:#}"))),
    }
}

// ---------------- 会话记录 / 对话测试 ----------------

#[derive(Debug, Deserialize)]
struct ListQuery {
    /// 按 kind 过滤（chat_test | douyin_refine | chat_summary）。
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

/// 把 metadata/meta 的 JSON 字符串解析为值；无法解析则回 `{}`。
fn parse_json(s: &str) -> Value {
    serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({}))
}

fn session_json(s: &llm_sessions::LlmSession) -> Value {
    json!({
        "id": s.id,
        "kind": s.kind,
        "title": s.title,
        "model": s.model,
        "prompt_name": s.prompt_name,
        "status": s.status,
        "metadata": parse_json(&s.metadata),
        "created_at": s.created_at,
        "updated_at": s.updated_at,
    })
}

fn message_json(m: &llm_sessions::LlmMessage) -> Value {
    json!({
        "id": m.id,
        "seq": m.seq,
        "role": m.role,
        "content": m.content,
        "meta": parse_json(&m.meta),
        "created_at": m.created_at,
    })
}

/// GET /sessions?origin=&limit= —— 列会话（倒序，默认 100，clamp ≤500）。
async fn list_sessions_h(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let origin = q.origin.as_deref().filter(|s| !s.trim().is_empty());
    let rows = llm_sessions::list_sessions(&s.pool, origin, limit).map_err(internal)?;
    let sessions: Vec<Value> = rows.iter().map(session_json).collect();
    Ok(Json(json!({ "sessions": sessions })))
}

/// GET /sessions/{id} —— 单条会话 + 全部消息（只读回看）。
async fn get_session_h(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let session = llm_sessions::get_session(&s.pool, &id)
        .map_err(internal)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("会话不存在 {id}")))?;
    let msgs = llm_sessions::get_messages(&s.pool, &id).map_err(internal)?;
    let mut out = session_json(&session);
    out["messages"] = Value::Array(msgs.iter().map(message_json).collect());
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
struct CreateChatBody {
    /// 可选首条用户消息；带则立刻跑一轮对话。
    #[serde(default)]
    message: Option<String>,
}

/// POST /sessions —— 新建 chat_test 会话，可带首条消息直接对话。
async fn create_chat_h(
    State(s): State<AppState>,
    Json(body): Json<CreateChatBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = super::resolve_client(&s.pool)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let first = body
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let title: String = first
        .map(|m| m.chars().take(40).collect())
        .unwrap_or_else(|| "新对话".to_string());
    let id = llm_sessions::create_session(
        &s.pool,
        llm_sessions::NewSession {
            kind: "chat_test",
            title: &title,
            model: Some(client.model()),
            prompt_name: None,
            metadata: None,
        },
    )
    .map_err(internal)?;

    if let Some(msg) = first {
        run_chat_turn(&s, &client, &id, msg).await?;
    }

    let msgs = llm_sessions::get_messages(&s.pool, &id).map_err(internal)?;
    Ok(Json(json!({
        "id": id,
        "model": client.model(),
        "messages": msgs.iter().map(message_json).collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
struct ChatSendBody {
    message: String,
}

/// POST /sessions/{id}/messages —— 在已有 chat_test 会话上追加一轮对话。
async fn chat_send_h(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ChatSendBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.message.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "message 不能为空".to_string()));
    }
    let session = llm_sessions::get_session(&s.pool, &id)
        .map_err(internal)?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("会话不存在 {id}")))?;
    if session.kind != "chat_test" {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "该会话不可续聊（仅对话测试支持）".to_string(),
        ));
    }
    let client = super::resolve_client(&s.pool)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let assistant = run_chat_turn(&s, &client, &id, body.message.trim()).await?;
    Ok(Json(json!({
        "message": message_json(&assistant),
        "model": client.model(),
    })))
}

/// 跑一轮对话：先持久化 user（即便模型出错也保住该轮）→ 用全量历史调 chat →
/// 成功落 assistant 返回；失败置会话 error 并 502。
async fn run_chat_turn(
    s: &AppState,
    client: &toolkit_llm::LlmClient,
    id: &str,
    user_msg: &str,
) -> Result<llm_sessions::LlmMessage, (StatusCode, Json<Value>)> {
    llm_sessions::append_message(&s.pool, id, "user", user_msg, None).map_err(internal)?;
    let history = llm_sessions::get_messages(&s.pool, id).map_err(internal)?;
    let msgs: Vec<toolkit_llm::Message> = history
        .iter()
        .map(|m| toolkit_llm::Message {
            role: m.role.clone(),
            content: m.content.clone(),
        })
        .collect();
    match client.chat(&msgs).await {
        Ok(reply) => {
            let assistant = llm_sessions::append_message(&s.pool, id, "assistant", &reply, None)
                .map_err(internal)?;
            Ok(assistant)
        }
        Err(e) => {
            let _ = llm_sessions::set_session_status(&s.pool, id, "error");
            Err(err(StatusCode::BAD_GATEWAY, format!("调用失败：{e:#}")))
        }
    }
}
