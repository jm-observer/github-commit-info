//! Cookie 模块：抖音 / 同花顺登录态采集、Cookie 同步到 G10、本机 bridge。
//!
//! 所有 Tauri command 名称以 `cookie_` 开头。

pub(crate) mod bridge;
mod browser;
mod db;
mod ths;
mod ths_watcher;
mod uploader;

use crate::shared::{settings, trace as tr, workspace as ws};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

/// Cookie 模块状态。
pub struct CookieState {
    pub workspace: PathBuf,
    pub db: Arc<db::Db>,
    pub uploader: Arc<uploader::UploaderState>,
    pub ths: Arc<ths_watcher::ThsState>,
    pub douyin_browser: Arc<browser::Session>,
    pub ths_browser: Arc<browser::Session>,
}

impl CookieState {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let db_path = ws::cookie_db_path(&workspace);
        let db = Arc::new(db::Db::open(&db_path).context("open cookie state.db")?);
        let douyin_profile = ws::douyin_profile_dir(&workspace);
        let ths_profile = ws::ths_profile_dir(&workspace);
        Ok(Self {
            workspace,
            db,
            uploader: Arc::new(uploader::UploaderState::default()),
            ths: Arc::new(ths_watcher::ThsState::default()),
            douyin_browser: Arc::new(browser::Session::new(douyin_profile)),
            ths_browser: Arc::new(browser::Session::new(ths_profile)),
        })
    }
}

/// 初始化 Cookie 模块：启动 uploader、bridge、ths_watcher 后台任务。
pub fn setup(app: &tauri::AppHandle, state: Arc<CookieState>) -> Result<()> {
    use tauri::Manager;
    let net = app.state::<crate::app_state::AppState>().net.clone();
    uploader::spawn(app.clone(), state.clone(), net);
    bridge::spawn(
        state.workspace.clone(),
        state.douyin_browser.clone(),
        state.ths_browser.clone(),
    );
    ths_watcher::spawn(app.clone(), state.clone());
    Ok(())
}

// ============ 基础 command ============

#[tauri::command]
pub fn cookie_workspace_path(state: tauri::State<'_, crate::app_state::AppState>) -> String {
    state.cookie.workspace.to_string_lossy().to_string()
}

// ============ 全局 G10 配置（存 app.json） ============

#[tauri::command]
pub fn cookie_get_app_settings(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> settings::AppSettings {
    settings::load_app_settings(&state.workspace)
}

#[tauri::command]
pub async fn cookie_save_app_settings(
    state: tauri::State<'_, crate::app_state::AppState>,
    settings_data: settings::AppSettings,
) -> Result<(), String> {
    settings::save_app_settings(&state.workspace, &settings_data).map_err(|e| format!("{e:#}"))?;
    // 配置变更后立即失效探测缓存，下次请求按新地址重探/选路。
    state.net.invalidate().await;
    Ok(())
}

// ============ 局域网/外网活动端点状态 ============

/// 返回当前生效的网络路径（供设置页状态条展示）：用户选的 `mode` + 实际选中的 `picked`
/// （auto 模式下经健康探测得出）+ 生效的 g10/asr 地址 + 该地址是否可达。
#[tauri::command]
pub async fn net_resolve_status(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<serde_json::Value, String> {
    let app_settings = settings::load_app_settings(&state.workspace);
    let resolved = state.net.resolve(&state.workspace).await;
    let reachable = if resolved.is_configured() {
        Some(settings::health_ok(&resolved.g10_base).await)
    } else {
        None
    };
    Ok(serde_json::json!({
        "mode": app_settings.mode,
        "picked": resolved.picked,
        "g10_base": resolved.g10_base,
        "asr_url": resolved.asr_url,
        "configured": resolved.is_configured(),
        "reachable": reachable,
    }))
}

/// 强制重新探测（清缓存后再解析），返回与 [`net_resolve_status`] 相同的状态。
#[tauri::command]
pub async fn net_reprobe(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<serde_json::Value, String> {
    state.net.invalidate().await;
    net_resolve_status(state).await
}

// ============ 抖音登录窗（Chrome 子进程） ============

#[tauri::command]
pub async fn cookie_open_douyin_login(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(), String> {
    let mut span = tr::CommandSpan::start(
        "cookie_open_douyin_login",
        serde_json::json!({"action": "open_login_window", "platform": "douyin"}),
    );
    let session = state.cookie.douyin_browser.clone();
    tokio::task::spawn_blocking(move || session.open("https://www.douyin.com"))
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))?
        .map_err(|e| span.fail(format!("{e:#}")))?;
    Ok(())
}

#[tauri::command]
pub async fn cookie_close_douyin_login(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(), String> {
    let session = state.cookie.douyin_browser.clone();
    tokio::task::spawn_blocking(move || session.close())
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))?;
    Ok(())
}

// ============ 同花顺登录窗（Chrome 子进程） ============

#[tauri::command]
pub async fn cookie_open_ths_login(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(), String> {
    let mut span = tr::CommandSpan::start(
        "cookie_open_ths_login",
        serde_json::json!({"action": "open_login_window", "platform": "ths"}),
    );
    let session = state.cookie.ths_browser.clone();
    tokio::task::spawn_blocking(move || session.open(ths::LOGIN_URL))
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))?
        .map_err(|e| span.fail(format!("{e:#}")))?;
    Ok(())
}

#[tauri::command]
pub async fn cookie_close_ths_login(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(), String> {
    let session = state.cookie.ths_browser.clone();
    tokio::task::spawn_blocking(move || session.close())
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn cookie_ths_status(state: tauri::State<'_, crate::app_state::AppState>) -> ths::StatusReport {
    ths::status_report(&state.cookie.workspace)
}

// ============ 解析当前博主 ============

#[tauri::command]
pub async fn cookie_track_current_creator(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<serde_json::Value, String> {
    let mut span = tr::CommandSpan::start(
        "cookie_track_current_creator",
        serde_json::json!({"action": "track_creator"}),
    );
    let session = state.cookie.douyin_browser.clone();
    let url = tokio::task::spawn_blocking(move || session.current_url())
        .await
        .map_err(|e| span.fail(e.to_string()))?
        .ok_or_else(|| span.fail("没有打开抖音登录窗口或读 URL 失败".to_string()))?;

    let resolved = state.net.resolve(&state.workspace).await;
    if !resolved.is_configured() {
        return Err(span.fail("G10 base 未配置".to_string()));
    }
    let endpoint = resolved
        .endpoint("/api/web/douyin/creators")
        .ok_or_else(|| span.fail("G10 base 未配置".to_string()))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| span.fail(e.to_string()))?;
    let mut req = client
        .post(&endpoint)
        .json(&serde_json::json!({ "handle": url }));
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| span.fail(e.to_string()))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| span.fail(e.to_string()))?;
    if !status.is_success() {
        return Err(span.fail(format!("server {status}: {body}")));
    }
    Ok(body)
}

// ============ 登录 cookie 失效时间（CDP 拿） ============

#[tauri::command]
pub async fn cookie_login_expiry(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<serde_json::Value, String> {
    use chrono::TimeZone;
    let Some(tab) = state.cookie.douyin_browser.tab() else {
        return Ok(serde_json::json!({ "state": "no_window" }));
    };
    let cookies = tokio::task::spawn_blocking(move || tab.get_cookies())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    const CRITICAL: &[&str] = &["sessionid_ss", "ttwid", "sid_guard", "sid_tt"];
    let now = chrono::Utc::now().timestamp();
    let mut entries: Vec<serde_json::Value> = Vec::new();
    let mut earliest: Option<i64> = None;
    for c in &cookies {
        if !CRITICAL.contains(&c.name.as_str()) {
            continue;
        }
        let ts = if c.expires > 0.0 {
            Some(c.expires as i64)
        } else {
            None
        };
        let iso = ts.and_then(|t| {
            chrono::Utc
                .timestamp_opt(t, 0)
                .single()
                .map(|d| d.with_timezone(&chrono::Local).to_rfc3339())
        });
        let remaining = ts.map(|t| t - now);
        if let Some(t) = ts {
            earliest = Some(earliest.map_or(t, |e| e.min(t)));
        }
        entries.push(serde_json::json!({
            "name": c.name,
            "expires_at": iso,
            "remaining_secs": remaining,
            "is_session": ts.is_none(),
        }));
    }
    let earliest_iso = earliest.and_then(|t| {
        chrono::Utc
            .timestamp_opt(t, 0)
            .single()
            .map(|d| d.with_timezone(&chrono::Local).to_rfc3339())
    });
    let earliest_remaining = earliest.map(|t| t - now);
    Ok(serde_json::json!({
        "state": "ok",
        "critical": entries,
        "earliest_expires_at": earliest_iso,
        "earliest_remaining_secs": earliest_remaining,
        "cookies_total": cookies.len(),
    }))
}

// ============ G10 server 探活 ============

#[tauri::command]
pub async fn cookie_ping_server(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<serde_json::Value, String> {
    let resolved = state.net.resolve(&state.workspace).await;
    if !resolved.is_configured() {
        return Ok(serde_json::json!({ "state": "unconfigured" }));
    }
    let base = resolved.g10_base.trim_end_matches('/');
    let url = format!("{base}/api/web/health");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    match client.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let latency_ms = started.elapsed().as_millis() as u64;
            let body = resp.text().await.unwrap_or_default();
            let version = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(String::from));
            Ok(serde_json::json!({
                "state": if (200..300).contains(&status) { "ok" } else { "http_err" },
                "status": status,
                "latency_ms": latency_ms,
                "server_base": base,
                "server_version": version,
            }))
        }
        Err(e) => Ok(serde_json::json!({
            "state": "unreachable",
            "error": e.to_string(),
            "server_base": base,
        })),
    }
}

// ============ cookie 状态诊断（摘要，不暴露原文） ============

#[tauri::command]
pub async fn cookie_inspect_cookies(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<serde_json::Value, String> {
    let Some(tab) = state.cookie.douyin_browser.tab() else {
        return Ok(serde_json::json!({
            "state": "no_login_window",
            "hint": "请先点「抖音登录」",
        }));
    };
    let cookies = tokio::task::spawn_blocking(move || tab.get_cookies())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

    let names: Vec<&str> = cookies.iter().map(|c| c.name.as_str()).collect();
    let has_ms = names.contains(&"msToken");
    // 从网络请求 harvest 到的 msToken（cookie 里没有时的兜底来源）。
    let harvested = state.cookie.douyin_browser.harvested_ms_token();
    let ms_info = cookies.iter().find(|c| c.name == "msToken").map(|c| {
        serde_json::json!({
            "len": c.value.len(),
            "domain": c.domain,
            "path": c.path,
            "http_only": c.http_only,
            "secure": c.secure,
            "expires": c.expires,
        })
    });
    // 只展示摘要（name、长度、domain 等非敏感字段），不暴露 cookie 原文值。
    let all: Vec<serde_json::Value> = cookies
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "len": c.value.len(),
                "domain": c.domain,
                "path": c.path,
                "http_only": c.http_only,
                "secure": c.secure,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "state": "ok",
        "count": cookies.len(),
        "has_ms_token": has_ms,
        "ms_token": ms_info,
        "ms_token_harvested": harvested.as_deref(),
        "has_ms_token_any": has_ms || harvested.is_some(),
        "names": names,
        "all": all,
    }))
}

// ============ G10 server cookie 状态查询 ============

#[tauri::command]
pub async fn cookie_server_cookie_status(
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<serde_json::Value, String> {
    let resolved = state.net.resolve(&state.workspace).await;
    if !resolved.is_configured() {
        return Ok(serde_json::json!({ "state": "unconfigured" }));
    }
    let base = resolved.g10_base.trim_end_matches('/');
    let url = format!("{base}/api/web/douyin/cookie_status");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&url);
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    return Ok(serde_json::json!({
                        "state": "parse_err", "error": e.to_string(),
                    }));
                }
            };
            if !status.is_success() {
                return Ok(serde_json::json!({
                    "state": "http_err", "status": status.as_u16(), "body": body,
                }));
            }
            Ok(serde_json::json!({ "state": "ok", "body": body }))
        }
        Err(e) => Ok(serde_json::json!({
            "state": "unreachable", "error": e.to_string(),
        })),
    }
}

// ============ 强制立即上传 ============

#[tauri::command]
pub async fn cookie_force_upload_now(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(), String> {
    *state.cookie.uploader.last_hash.lock().await = None;
    use tauri::Emitter;
    let _ = app.emit("uploader:status", serde_json::json!({"state": "forced"}));
    Ok(())
}

// ============ 最近上传历史 ============

#[tauri::command]
pub fn cookie_recent_uploads(
    state: tauri::State<'_, crate::app_state::AppState>,
    limit: Option<i64>,
) -> Result<Vec<db::UploadRow>, String> {
    state
        .cookie
        .db
        .recent_uploads(limit.unwrap_or(20))
        .map_err(|e| format!("{e:#}"))
}
