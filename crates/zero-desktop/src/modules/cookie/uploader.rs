//! 后台轮询 CDP 控制的 Chrome 子进程 cookie store；登录态完整时打包 POST 到 toolkit-server。
//!
//! 流程：
//! 1. 每 `POLL_SECS` 秒在 `spawn_blocking` 里调 `tab.get_cookies()`，直接读 cookie store
//! 2. 登录态字段（ttwid / sessionid_ss）齐全才视为登录态；msToken 非必需
//! 3. SHA256 dedup（只认身份 cookie）
//! 4. POST `<g10_base>/api/browser/cookie`，body `{session_id, raw_header}`
//! 5. 落 state.db + 事件通知前端

use crate::shared::settings::NetResolver;
use anyhow::{anyhow, Result};
use custom_utils::trace::{self, SpanScope, SpanStatus, TraceContext};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

use super::CookieState;

const POLL_SECS: u64 = 5;
// msToken 非必需（实测 2026-06-10：空 msToken 下 profile/list/detail/self_info 返回数据
// 一致、不触发风控；抖音新登录 profile 本就常不写 msToken cookie）。只认登录态字段。
const REQUIRED: &[&str] = &["ttwid", "sessionid_ss"];
// 登录身份 cookie：唯一决定「是不是同一个登录」的稳定字段，用于去重判定是否需要重传。
// 不含 msToken/__ac_nonce 等随请求抖动的字段（详见 tick 里 hash 计算处注释）。
const IDENTITY: &[&str] = &[
    "sessionid",
    "sessionid_ss",
    "sid_guard",
    "sid_tt",
    "uid_tt",
    "uid_tt_ss",
    "sid_ucp_v1",
    "ssid_ucp_v1",
    "ttwid",
    "odin_tt",
    "UIFID",
    "passport_csrf_token",
];

#[derive(Default)]
pub struct UploaderState {
    pub(crate) last_hash: Mutex<Option<String>>,
    session_id: Mutex<String>,
}

impl UploaderState {
    pub async fn session_id(&self) -> String {
        let mut s = self.session_id.lock().await;
        if s.is_empty() {
            *s = format!("desktop-{}", chrono::Utc::now().timestamp_millis());
        }
        s.clone()
    }
}

pub fn spawn(app: AppHandle, state: Arc<CookieState>, net: Arc<NetResolver>) {
    tauri::async_runtime::spawn(async move {
        // 顶层 anchor span：仅 emit_start，长循环不 emit_end。
        let _loop_scope = trace::enabled().then(|| {
            let ctx = TraceContext::root();
            let scope = SpanScope::new(ctx, "cookie_uploader_loop").with_summary(
                serde_json::json!({"service": "cookie_uploader", "poll_secs": POLL_SECS}),
            );
            scope.emit_start();
            scope
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        let mut ticker = tokio::time::interval(Duration::from_secs(POLL_SECS));
        // tick() 偶尔会因 CDP 调用阻塞 >5s；默认 Burst 行为会把积压的 tick 一次性补发
        // （实测 1.3s 内挤 7 拍）。改 Skip：错过就跳过，永远每 ~5s 一拍，不补发。
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = tick(&app, &state, &net, &client).await {
                log::warn!("uploader tick: {e:#}");
                let _ = app.emit("uploader:status", json_err(&e.to_string()));
            }
        }
    });
}

async fn tick(
    app: &AppHandle,
    state: &Arc<CookieState>,
    net: &Arc<NetResolver>,
    client: &reqwest::Client,
) -> Result<()> {
    let resolved = net.resolve(&state.workspace).await;
    let Some(endpoint) = resolved.cookie_endpoint() else {
        let _ = app.emit(
            "uploader:status",
            serde_json::json!({ "state": "unconfigured", "hint": "填 G10 base 并保存" }),
        );
        return Ok(());
    };
    if !state.douyin_browser.is_open() {
        let _ = app.emit(
            "uploader:status",
            serde_json::json!({
                "state": "no_login_window",
                "hint": "点「抖音登录」打开 Chrome",
            }),
        );
        return Ok(());
    }

    let pairs = read_cookies(state).await?;
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|k| !pairs.iter().any(|(n, _)| n == k))
        .collect();
    if !missing.is_empty() {
        let hint = "未检测到登录态：请在登录窗里完成抖音账号登录。";
        let _ = app.emit(
            "uploader:status",
            serde_json::json!({
                "state": "waiting_login",
                "missing": missing,
                "have": pairs.len(),
                "hint": hint,
            }),
        );
        return Ok(());
    }

    let raw_header = build_header(&pairs);
    // 去重 hash **只认登录身份 cookie**：抖音页面活跃时会不停增删/刷新一大票易变 cookie
    // （msToken / __ac_nonce / biz_trace_id / 各种 security 与推荐流 cookie，实测每拍
    // 都变、cookie 数在 59~64 间跳）。若按全量算 hash，会每拍都判「变化」→ 每 5s 重传 +
    // 前端「已同步」刷屏。身份 cookie（登录态）稳定，只有真正登录/换号才变，正是该重传
    // 的时机。上传仍发最新、全量的 raw_header。
    let mut ident: Vec<(String, String)> = pairs
        .iter()
        .filter(|(n, _)| IDENTITY.contains(&n.as_str()))
        .cloned()
        .collect();
    ident.sort();
    let hash = sha256_hex(&build_header(&ident));
    {
        let mut last = state.uploader.last_hash.lock().await;
        if last.as_deref() == Some(&hash) {
            let _ = app.emit(
                "uploader:status",
                serde_json::json!({ "state": "unchanged", "fields": pairs.len() }),
            );
            return Ok(());
        }
        *last = Some(hash.clone());
    }
    let session_id = state.uploader.session_id().await;
    let now = chrono::Utc::now().to_rfc3339();
    let _ = state.db.upsert_session(&session_id, &now);

    // A.4 traceparent 注入 + 上传子 span。
    // ctx 单独保存，便于在 req 上注入 traceparent header。
    let upload_ctx = trace::enabled().then(TraceContext::root);
    let upload_scope = upload_ctx.as_ref().map(|ctx| {
        let scope =
            SpanScope::new(ctx.clone(), "cookie_upload_once").with_summary(serde_json::json!({
                "fields": pairs.len(),
                // session_id 非敏感：是内部生成的 desktop-{timestamp} 字符串
                "session_id": session_id,
            }));
        scope.emit_start();
        scope
    });

    let body = serde_json::json!({ "session_id": session_id, "raw_header": raw_header });
    let mut req = client.post(&endpoint).json(&body);
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    // A.4：注入 W3C traceparent，让 G10 端的 cookie 接收请求接入同一条 trace。
    if let Some(ctx) = &upload_ctx {
        req = req.header("traceparent", ctx.to_traceparent());
    }
    let send_result = req.send().await;
    match send_result {
        Err(e) => {
            let _ = state.db.record_upload(
                &now,
                &hash,
                pairs.len() as i64,
                false,
                None,
                Some(&e.to_string()),
            );
            if let Some(scope) = upload_scope {
                scope.emit_end(None, SpanStatus::Error(e.to_string()), None);
            }
            return Err(anyhow!("POST cookie: {e}"));
        }
        Ok(resp) => {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                let err_msg = format!("server {status}: {text}");
                let _ = state.db.record_upload(
                    &now,
                    &hash,
                    pairs.len() as i64,
                    false,
                    Some(&text),
                    Some(&err_msg),
                );
                if let Some(scope) = upload_scope {
                    scope.emit_end(None, SpanStatus::Error(err_msg.clone()), None);
                }
                return Err(anyhow!(err_msg));
            }
            let _ =
                state
                    .db
                    .record_upload(&now, &hash, pairs.len() as i64, true, Some(&text), None);
            if let Some(scope) = upload_scope {
                scope.emit_end(Some(text.chars().take(512).collect()), SpanStatus::Ok, None);
            }
            let _ = app.emit(
                "uploader:status",
                serde_json::json!({
                    "state": "uploaded",
                    "fields": pairs.len(),
                    "endpoint": endpoint,
                    "at": now,
                    "server_response": text,
                }),
            );
        }
    }
    Ok(())
}

/// 通过 CDP 拿 douyin tab 的全量 cookies。`tab.get_cookies()` 是同步阻塞 IO，扔进
/// spawn_blocking 不阻塞 tokio runtime。
async fn read_cookies(state: &Arc<CookieState>) -> Result<Vec<(String, String)>> {
    let Some(tab) = state.douyin_browser.tab() else {
        return Err(anyhow!("douyin browser not open"));
    };
    let mut pairs = tokio::task::spawn_blocking(move || -> Result<Vec<(String, String)>> {
        let cookies = tab.get_cookies().map_err(|e| anyhow!("get_cookies: {e}"))?;
        Ok(cookies.into_iter().map(|c| (c.name, c.value)).collect())
    })
    .await
    .map_err(|e| anyhow!("spawn_blocking: {e}"))??;
    // cookie store 里没有 msToken 时（新登录 profile 常如此），用从网络请求 harvest 到的
    // 兜底补上。msToken 非必需，纯属锦上添花。
    if !pairs.iter().any(|(n, _)| n == "msToken") {
        if let Some(tok) = state.douyin_browser.harvested_ms_token() {
            pairs.push(("msToken".to_string(), tok));
        }
    }
    Ok(pairs)
}

fn build_header(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

fn json_err(msg: &str) -> serde_json::Value {
    serde_json::json!({ "state": "error", "error": msg })
}
