//! `/api/internal/*`:出口代理 worker 专用通道(pull 模型,共享 token 鉴权)。
//!
//! worker 主动连中心(NAT 友好):register → 心跳 → 长轮询 `egress/next` 取待发请求
//! → 本机代发 → `egress/result` 回传。请求不落库(dumb pipe),经内存 [`egress_pool::Registry`]。
//!
//! 鉴权:若设了环境变量 `EGRESS_WORKER_TOKEN`,则所有端点要求请求头 `x-egress-token` 匹配;
//! 未设(本地开发)则放行并告警。正式的 bootstrap-token 体系见 P2。

use crate::state::AppState;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use egress_pool::{EgressResponse, NextResult};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// 长轮询挂起上限(< worker 心跳/续期节奏,避免连接被中间代理掐断)。
const LONG_POLL_WAIT: Duration = Duration::from_secs(25);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/workers/register", post(register))
        .route("/workers/{id}/heartbeat", post(heartbeat))
        .route("/egress/next", get(egress_next))
        .route("/egress/result", post(egress_result))
        .layer(from_fn(token_auth))
}

/// 共享 token 中间件。设了 `EGRESS_WORKER_TOKEN` 就强制校验,否则放行(开发)。
async fn token_auth(req: Request, next: Next) -> Response {
    match std::env::var("EGRESS_WORKER_TOKEN") {
        Ok(expected) if !expected.is_empty() => {
            let got = req
                .headers()
                .get("x-egress-token")
                .and_then(|v| v.to_str().ok());
            if got != Some(expected.as_str()) {
                return (StatusCode::UNAUTHORIZED, "bad or missing x-egress-token").into_response();
            }
        }
        _ => {
            log::warn!("EGRESS_WORKER_TOKEN 未设置:/api/internal 无鉴权放行(仅限开发)");
        }
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct RegisterReq {
    worker_id: String,
    egress_ip: String,
    /// worker 代发绑定的网卡名(`--interface`,仅 Linux)。老版本 worker 不传,`#[serde(default)]`
    /// 保证反序列化仍成功。
    #[serde(default)]
    interface: Option<String>,
    /// worker 代发绑定的本地源 IP(`--local-address`)。老版本 worker 不传。
    #[serde(default)]
    local_address: Option<String>,
}

async fn register(State(s): State<AppState>, Json(r): Json<RegisterReq>) -> Json<Value> {
    s.egress.register(
        &r.worker_id,
        &r.egress_ip,
        r.interface.as_deref(),
        r.local_address.as_deref(),
    );
    Json(json!({ "ok": true }))
}

async fn heartbeat(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    if s.egress.heartbeat(&id) {
        (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
    } else {
        // 未知 worker(多半 controller 重启内存丢了)→ 让 worker 重新 register。
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "reason": "unknown worker, re-register" })),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct NextQuery {
    worker_id: String,
}

async fn egress_next(State(s): State<AppState>, Query(q): Query<NextQuery>) -> Response {
    match s.egress.next_request(&q.worker_id, LONG_POLL_WAIT).await {
        NextResult::Job(req) => (StatusCode::OK, Json(req)).into_response(),
        NextResult::Idle => StatusCode::NO_CONTENT.into_response(),
        NextResult::Unknown => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn egress_result(State(s): State<AppState>, Json(resp): Json<EgressResponse>) -> Json<Value> {
    s.egress.complete(resp);
    Json(json!({ "ok": true }))
}
