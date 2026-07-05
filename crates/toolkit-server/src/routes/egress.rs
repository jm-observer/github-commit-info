//! `/api/web/egress/*`:出口代理的消费/观测面。
//!
//! - `GET  /workers` —— 当前注册的 worker 快照(在线状态 / 出口 IP / 占用的 type)。
//! - `POST /probe`   —— 端到端自测:匿名 fetch 取出口 IP + 具名 session 钉死复用断言。
//! - `POST /fetch`   —— **F4**:匿名短租,单次代发,不占用 worker。
//! - `POST /session` / `POST /session/{handle}/fetch` / `POST /session/{handle}/release`
//!   —— **F4**:externally-consumable 的 session 生命周期(见 [`crate::egress_sessions`])。
//!   进程内代码仍应直接用 [`egress_pool::Pool`](见 P1 抖音接入);这组 HTTP 端点是给
//!   **拿不到进程内 `Pool` 的外部进程**用的消费面(配套公共 client crate `egress-client`)。

use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/workers", get(workers))
        .route("/probe", post(probe))
        .route("/fetch", post(fetch))
        .route("/session", post(create_session))
        .route("/session/{handle}/fetch", post(session_fetch))
        .route("/session/{handle}/release", post(session_release))
}

async fn workers(State(s): State<AppState>) -> Json<Value> {
    Json(json!({ "workers": s.egress.workers_snapshot() }))
}

#[derive(Deserialize)]
struct ProbeQuery {
    /// 代发目标;默认 ipify(回显出口公网 IP)。离线验证可传本机 `/api/web/health`。
    url: Option<String>,
}

/// 端到端自测:证明「请求确实从 worker 出去」+「session 钉死同一出口」+「具名身份复用同一 worker」。
async fn probe(State(s): State<AppState>, Query(q): Query<ProbeQuery>) -> Json<Value> {
    let pool = egress_pool::Pool::new(s.egress.clone());
    // 默认 ipify 回显调用方公网 IP —— 返回值即 worker 的出口 IP。
    let url = q.url.as_deref().unwrap_or("https://api.ipify.org");

    // 1. 匿名短租
    let anonymous = match pool.fetch("GET", url, vec![], None).await {
        Ok(r) => json!({ "ok": r.ok, "status": r.status, "ip": r.body }),
        Err(e) => json!({ "error": e.to_string() }),
    };

    // 2. 具名 session:同一 session 内两次请求应同 IP(钉死)
    let mut session_ips: Vec<String> = vec![];
    let mut worker_first: Option<String> = None;
    match pool.session("probe", Some("acc1")) {
        Ok(sess) => {
            worker_first = Some(sess.worker_id().to_string());
            for _ in 0..2 {
                match sess.fetch("GET", url, vec![], None).await {
                    Ok(r) => session_ips.push(r.body.unwrap_or_default()),
                    Err(e) => session_ips.push(format!("ERR:{e}")),
                }
            }
        }
        Err(e) => session_ips.push(format!("ERR:{e}")),
    }

    // 3. 再次以同 account 进入 → 应命中同一 worker(绑定复用)
    let worker_reacquired = pool
        .session("probe", Some("acc1"))
        .ok()
        .map(|s2| s2.worker_id().to_string());

    let pinned_same_ip = session_ips.len() == 2
        && !session_ips[0].starts_with("ERR")
        && session_ips[0] == session_ips[1];
    let reuse_same_worker = worker_first.is_some() && worker_first == worker_reacquired;

    Json(json!({
        "anonymous_fetch": anonymous,
        "session_ips": session_ips,
        "session_pinned_same_ip": pinned_same_ip,
        "session_worker_first": worker_first,
        "session_worker_reacquired": worker_reacquired,
        "reuse_same_worker": reuse_same_worker,
        "workers": s.egress.workers_snapshot(),
    }))
}

/// `/fetch`、`/session/{handle}/fetch` 共用的请求体。`headers`/`body` 可选。
#[derive(Deserialize)]
struct FetchReq {
    method: String,
    url: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    body: Option<String>,
}

/// `PoolError` → HTTP 503 + `{error}`(`NoWorker` 也走 503:优雅降级,不是调用方的错)。
fn pool_error_response(e: egress_pool::PoolError) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": e.to_string() })),
    )
        .into_response()
}

/// `POST /fetch` —— 匿名短租:随手挑一个在线 worker 代发,不占用。
async fn fetch(State(s): State<AppState>, Json(r): Json<FetchReq>) -> Response {
    let pool = egress_pool::Pool::new(s.egress.clone());
    match pool.fetch(&r.method, &r.url, r.headers, r.body).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => pool_error_response(e),
    }
}

#[derive(Deserialize)]
struct CreateSessionReq {
    typ: String,
    #[serde(default)]
    account: Option<String>,
}

#[derive(Serialize)]
struct CreateSessionResp {
    session_handle: String,
    worker_id: String,
}

/// `POST /session` —— 钉死长租:拿一个 session,存入 `SessionStore`,把不透明的
/// `session_handle` 交还给外部进程(它没法像进程内代码那样直接持有 `egress_pool::Session`)。
async fn create_session(State(s): State<AppState>, Json(r): Json<CreateSessionReq>) -> Response {
    let pool = egress_pool::Pool::new(s.egress.clone());
    match pool.session(&r.typ, r.account.as_deref()) {
        Ok(session) => {
            let worker_id = session.worker_id().to_string();
            let session_handle = s.egress_sessions.insert(session);
            Json(CreateSessionResp {
                session_handle,
                worker_id,
            })
            .into_response()
        }
        Err(e) => pool_error_response(e),
    }
}

/// `POST /session/{handle}/fetch` —— 经已持有的 session 代发(同一 worker + 连续 cookie)。
///
/// 并发要点:先锁 `SessionStore` 内部 map 取出 `Arc<StoredSession>` 并立即释放锁,
/// 真正的 `.await` 发生在锁外(见 [`crate::egress_sessions`] 模块文档)。
async fn session_fetch(
    State(s): State<AppState>,
    Path(handle): Path<String>,
    Json(r): Json<FetchReq>,
) -> Response {
    // 取出 Arc 后锁已释放,以下 `.await` 不持有 SessionStore 的锁。
    let stored = match s.egress_sessions.get(&handle) {
        Some(stored) => stored,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "unknown or expired session handle" })),
            )
                .into_response();
        }
    };
    let result = stored
        .session
        .fetch(&r.method, &r.url, r.headers, r.body)
        .await;
    // 无论成败都刷新最近使用时间,避免活跃 session 被 TTL reaper 误杀。
    s.egress_sessions.touch(&handle);
    match result {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => pool_error_response(e),
    }
}

/// `POST /session/{handle}/release` —— 显式释放,从 `SessionStore` 移除即触发
/// `egress_pool::Session::drop`,释放 worker 占用。幂等:handle 不存在也回 `{ok:true}`。
async fn session_release(State(s): State<AppState>, Path(handle): Path<String>) -> Json<Value> {
    s.egress_sessions.remove(&handle);
    Json(json!({ "ok": true }))
}
