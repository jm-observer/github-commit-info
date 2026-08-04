//! `exec` 模块：远程节点（remote-exec）的**审批与观测面**。
//!
//! 对方机器上的 `toolkit-worker run` 首次启动会自行提交一条权限申请，本模块把
//! G10 toolkit-server 的 `/api/web/exec/*` 代理给 UI：列申请 → 批准 N 小时 / 拒绝 →
//! 看在线节点与凭据到期时间。
//!
//! **鉴权用独立的 `exec_token`（对应 G10 的 `TOOLKIT_EXEC_TOKEN`），不是 `g10_token`**：
//! 批准一台机器等于授予在它上面执行任意命令的权限，安全边界比普通 API 高一档，不该因为
//! 配了全局 token 就顺带获得。没配 exec_token 时这里直接返回可操作的提示。
//!
//! 形态与 `egress` / `llm` 模块一致：UI `invoke` → 本模块 Rust 命令 → `reqwest` → JSON。

use std::time::Duration;
use tauri::State;

use crate::app_state::AppState;

const QUICK_TIMEOUT: Duration = Duration::from_secs(20);

/// 未配置 exec token 时的统一提示（UI 直接展示给用户）。
const NO_TOKEN_HINT: &str =
    "未配置远程执行 token：请到设置页填写「远程执行 token」（对应 G10 的 TOOLKIT_EXEC_TOKEN）";

/// 解析出 `{g10_base}/api/web/exec{path}` 与 exec token；缺一不可。
async fn endpoint(state: &State<'_, AppState>, path: &str) -> Result<(String, String), String> {
    let resolved = state.net.resolve(&state.workspace).await;
    let url = resolved
        .exec_endpoint(path)
        .ok_or_else(|| "G10 base 未配置，请到设置页填写局域网/外网地址".to_string())?;
    let token = resolved
        .exec_token
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| NO_TOKEN_HINT.to_string())?;
    Ok((url, token))
}

/// 统一的错误映射：优先取 server 的 `{error}`，401 特别提示 token。
fn map_err(prefix: &str, status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect::<String>());
    match status.as_u16() {
        401 | 403 => format!("{prefix}：鉴权失败，请检查「远程执行 token」是否与 G10 一致"),
        404 if detail.trim().is_empty() => format!(
            "{prefix}：HTTP 404（G10 未开启远程执行——需给 toolkit-server 配 TOOLKIT_EXEC_TOKEN 并重新部署）"
        ),
        c => format!("{prefix}：HTTP {c} {detail}"),
    }
}

/// 发一个带 exec token 的请求并解析 JSON。
async fn send(
    prefix: &str,
    build: impl FnOnce(&reqwest::Client) -> reqwest::RequestBuilder,
    token: &str,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(QUICK_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = build(&client)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("{prefix}：{e}"))?;
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

/// 待审批 / 历史申请列表。
#[tauri::command]
pub async fn exec_list_requests(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let (url, token) = endpoint(&state, "/requests").await?;
    send("读远程节点申请失败", |c| c.get(&url), &token).await
}

/// 在线的远程节点（含 busy / PowerShell 版本 / 主机名）。
#[tauri::command]
pub async fn exec_list_workers(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let (url, token) = endpoint(&state, "/workers").await?;
    send("读远程节点列表失败", |c| c.get(&url), &token).await
}

/// 已签发的凭据（含到期时间），用于「谁还有权限、还剩多久」。
#[tauri::command]
pub async fn exec_list_creds(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let (url, token) = endpoint(&state, "/creds").await?;
    send("读远程节点凭据失败", |c| c.get(&url), &token).await
}

/// 批准一条申请，授权 `hours` 小时。
#[tauri::command]
pub async fn exec_approve_request(
    state: State<'_, AppState>,
    worker_id: String,
    hours: f64,
) -> Result<serde_json::Value, String> {
    let (url, token) = endpoint(&state, &format!("/requests/{worker_id}/approve")).await?;
    let body = serde_json::json!({ "hours": hours });
    send("批准失败", |c| c.post(&url).json(&body), &token).await
}

/// 拒绝一条申请。
#[tauri::command]
pub async fn exec_reject_request(
    state: State<'_, AppState>,
    worker_id: String,
) -> Result<serde_json::Value, String> {
    let (url, token) = endpoint(&state, &format!("/requests/{worker_id}/reject")).await?;
    send("拒绝失败", |c| c.post(&url), &token).await
}
