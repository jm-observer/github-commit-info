//! `egress` 模块：把桌面端对「出口代理 worker 列表」的读取代理到 G10 toolkit-server 的
//! `/api/web/egress/workers`（观测面，只读）。
//!
//! 形态与 `llm` 模块一致：UI `invoke` → 本模块 Rust 命令 → `reqwest` 调
//! `{g10_base}/api/web/egress/workers`（带可选 Bearer token）→ 回传 JSON。集中处理鉴权与
//! 错误映射，UI 不直接发 HTTP。

use std::time::Duration;
use tauri::State;

use crate::app_state::AppState;

/// 读取列表很快。
const QUICK_TIMEOUT: Duration = Duration::from_secs(20);

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

/// 出口代理 worker 列表（只读观测面）：`{g10_base}/api/web/egress/workers`。
#[tauri::command]
pub async fn egress_list_workers(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let resolved = state.net.resolve(&state.workspace).await;
    let endpoint = resolved
        .egress_workers_endpoint()
        .ok_or_else(|| "G10 base 未配置，请到设置页填写局域网/外网地址".to_string())?;

    let client = reqwest::Client::builder()
        .timeout(QUICK_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&endpoint);
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let prefix = "读出口 worker 列表失败";
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
