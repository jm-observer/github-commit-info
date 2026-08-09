//! `llm` 模块：把桌面端对「公共大模型层」的操作代理到 G10 toolkit-server 的
//! `/api/web/llm/*`（连接配置 / 可配提示词 / 连通性自测 / 对话总结）。
//!
//! 形态与 english/cookie 模块一致：UI `invoke` → 本模块 Rust 命令 → `reqwest` 调
//! `{g10_base}/api/web/llm/...`（带可选 Bearer token）→ 回传 JSON / 文本。集中处理鉴权与
//! 错误映射，UI 不直接发 HTTP。

use std::time::Duration;
use tauri::State;

use crate::app_state::AppState;

/// 配置 / 提示词读写很快。
const QUICK_TIMEOUT: Duration = Duration::from_secs(20);
/// 连通性自测要真打一次模型，可能慢。
const PING_TIMEOUT: Duration = Duration::from_secs(60);
/// 对话总结是一次完整生成，给足时间（与 server 端 LLM 超时对齐）。
const SUMMARIZE_TIMEOUT: Duration = Duration::from_secs(180);
/// 对话测试一轮也是完整生成，给足时间。
const CHAT_TIMEOUT: Duration = Duration::from_secs(180);

/// 把上游非 2xx 响应映射成可读中文错误：优先取 server 的 `{error}` 字段，否则截断原文。
fn map_err(prefix: &str, status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| body.chars().take(200).collect::<String>());
    // 空 body（如老 server 没有该路由 → 404 空响应）给出可操作提示，避免「失败：」空尾巴。
    let detail = if detail.trim().is_empty() {
        format!(
            "HTTP {}（响应为空，可能 G10 toolkit-server 版本过旧、缺少该接口，请重新部署）",
            status.as_u16()
        )
    } else {
        detail
    };
    match status.as_u16() {
        401 | 403 => format!("{prefix}：鉴权失败，请检查 G10 token"),
        404 => format!("{prefix}：{detail}"),
        c => format!("{prefix}：HTTP {c} {detail}"),
    }
}

/// 通用 JSON 请求：构造 client（带 token）→ 发 method+path（可带 body）→ 解析 JSON。
async fn request_json(
    state: &State<'_, AppState>,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
    timeout: Duration,
    prefix: &str,
) -> Result<serde_json::Value, String> {
    let resolved = state.net.resolve(&state.workspace).await;
    let endpoint = resolved
        .llm_endpoint(path)
        .ok_or_else(|| "G10 base 未配置，请到设置页填写局域网/外网地址".to_string())?;

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.request(method, &endpoint);
    if let Some(b) = body {
        req = req.json(&b);
    }
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("{prefix}：{e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("{prefix}：读响应失败 {e}"))?;
    if !status.is_success() {
        return Err(map_err(prefix, status, &text));
    }
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&text).map_err(|e| format!("{prefix}：解析响应 JSON 失败 {e}"))
}

// ---------------- 连接配置 ----------------

#[tauri::command]
pub async fn llm_get_config(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    request_json(
        &state,
        reqwest::Method::GET,
        "/config",
        None,
        QUICK_TIMEOUT,
        "读大模型配置失败",
    )
    .await
}

#[tauri::command]
pub async fn llm_put_config(
    state: State<'_, AppState>,
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut body = serde_json::json!({ "base_url": base_url, "model": model });
    // api_key 语义沿用 server：省略=保留原值；空串=清空；有值=设置。
    if let Some(k) = api_key {
        body["api_key"] = serde_json::json!(k);
    }
    request_json(
        &state,
        reqwest::Method::PUT,
        "/config",
        Some(body),
        QUICK_TIMEOUT,
        "保存大模型配置失败",
    )
    .await
}

// ---------------- 提示词 ----------------

#[tauri::command]
pub async fn llm_list_prompts(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    request_json(
        &state,
        reqwest::Method::GET,
        "/prompts",
        None,
        QUICK_TIMEOUT,
        "读提示词列表失败",
    )
    .await
}

#[tauri::command]
pub async fn llm_get_prompt(
    state: State<'_, AppState>,
    name: String,
) -> Result<serde_json::Value, String> {
    let path = format!("/prompts/{name}");
    request_json(
        &state,
        reqwest::Method::GET,
        &path,
        None,
        QUICK_TIMEOUT,
        "读提示词失败",
    )
    .await
}

#[tauri::command]
pub async fn llm_put_prompt(
    state: State<'_, AppState>,
    name: String,
    text: String,
    version: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut body = serde_json::json!({ "text": text });
    if let Some(v) = version.filter(|v| !v.trim().is_empty()) {
        body["version"] = serde_json::json!(v);
    }
    let path = format!("/prompts/{name}");
    request_json(
        &state,
        reqwest::Method::PUT,
        &path,
        Some(body),
        QUICK_TIMEOUT,
        "保存提示词失败",
    )
    .await
}

/// 重置为内置默认（删 DB 覆盖行）。
#[tauri::command]
pub async fn llm_reset_prompt(
    state: State<'_, AppState>,
    name: String,
) -> Result<serde_json::Value, String> {
    let path = format!("/prompts/{name}");
    request_json(
        &state,
        reqwest::Method::DELETE,
        &path,
        None,
        QUICK_TIMEOUT,
        "重置提示词失败",
    )
    .await
}

// ---------------- 连通性自测 / 对话总结 ----------------

#[tauri::command]
pub async fn llm_ping(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    request_json(
        &state,
        reqwest::Method::POST,
        "/ping",
        Some(serde_json::json!({})),
        PING_TIMEOUT,
        "连通性自测失败",
    )
    .await
}

#[tauri::command]
pub async fn llm_summarize(
    state: State<'_, AppState>,
    text: String,
) -> Result<serde_json::Value, String> {
    if text.trim().is_empty() {
        return Err("会话内容不能为空".to_string());
    }
    let body = serde_json::json!({ "text": text });
    request_json(
        &state,
        reqwest::Method::POST,
        "/summarize",
        Some(body),
        SUMMARIZE_TIMEOUT,
        "对话总结失败",
    )
    .await
}

// ---------------- 会话记录 / 对话测试 ----------------

/// 列会话记录（可选按 origin/kind 过滤，limit 限量）。
#[tauri::command]
pub async fn llm_list_sessions(
    state: State<'_, AppState>,
    origin: Option<String>,
    limit: Option<i64>,
) -> Result<serde_json::Value, String> {
    let mut qs: Vec<String> = Vec::new();
    // origin 取值是受控标识符（chat_test/douyin_refine/chat_summary），无需 URL 编码。
    if let Some(o) = origin.filter(|s| !s.trim().is_empty()) {
        qs.push(format!("origin={o}"));
    }
    if let Some(l) = limit {
        qs.push(format!("limit={l}"));
    }
    let path = if qs.is_empty() {
        "/sessions".to_string()
    } else {
        format!("/sessions?{}", qs.join("&"))
    };
    request_json(
        &state,
        reqwest::Method::GET,
        &path,
        None,
        QUICK_TIMEOUT,
        "读会话列表失败",
    )
    .await
}

/// 读单条会话（含全部消息，只读回看）。
#[tauri::command]
pub async fn llm_get_session(
    state: State<'_, AppState>,
    id: String,
) -> Result<serde_json::Value, String> {
    let path = format!("/sessions/{id}");
    request_json(
        &state,
        reqwest::Method::GET,
        &path,
        None,
        QUICK_TIMEOUT,
        "读会话失败",
    )
    .await
}

/// 新建对话测试会话（可带首条消息直接对话）。
#[tauri::command]
pub async fn llm_create_chat(
    state: State<'_, AppState>,
    message: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut body = serde_json::json!({});
    if let Some(m) = message.filter(|s| !s.trim().is_empty()) {
        body["message"] = serde_json::json!(m);
    }
    request_json(
        &state,
        reqwest::Method::POST,
        "/sessions",
        Some(body),
        CHAT_TIMEOUT,
        "新建对话失败",
    )
    .await
}

/// 在已有对话测试会话上追加一轮。
#[tauri::command]
pub async fn llm_chat_send(
    state: State<'_, AppState>,
    id: String,
    message: String,
) -> Result<serde_json::Value, String> {
    if message.trim().is_empty() {
        return Err("消息不能为空".to_string());
    }
    let path = format!("/sessions/{id}/messages");
    let body = serde_json::json!({ "message": message });
    request_json(
        &state,
        reqwest::Method::POST,
        &path,
        Some(body),
        CHAT_TIMEOUT,
        "发送消息失败",
    )
    .await
}

/// 重命名会话（仅改标题）。
#[tauri::command]
pub async fn llm_rename_session(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> Result<serde_json::Value, String> {
    if title.trim().is_empty() {
        return Err("标题不能为空".to_string());
    }
    let path = format!("/sessions/{id}");
    let body = serde_json::json!({ "title": title });
    request_json(
        &state,
        reqwest::Method::PUT,
        &path,
        Some(body),
        QUICK_TIMEOUT,
        "重命名会话失败",
    )
    .await
}

/// 删除会话及其全部消息。
#[tauri::command]
pub async fn llm_delete_session(
    state: State<'_, AppState>,
    id: String,
) -> Result<serde_json::Value, String> {
    let path = format!("/sessions/{id}");
    request_json(
        &state,
        reqwest::Method::DELETE,
        &path,
        None,
        QUICK_TIMEOUT,
        "删除会话失败",
    )
    .await
}
