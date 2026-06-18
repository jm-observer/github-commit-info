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
