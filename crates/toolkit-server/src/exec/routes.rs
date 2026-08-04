//! remote-exec 第一期 HTTP 面:
//! - 内部面 `/api/internal/exec/*`(worker 专用,per-worker secret 鉴权,见 [`internal_router`])。
//! - 消费面 `/api/web/exec/*`(operator 专用,独立 `TOOLKIT_EXEC_TOKEN`,见 [`web_router`])。
//!
//! 两面**都不复用**出口代理(`/api/internal` 现有的 `x-egress-token` 中间件、`/api/web` 的
//! `TOOLKIT_API_TOKEN`),各自独立装配,详见模块内注释与 `docs/remote-exec-design.md` 第一期 §4/§5。

use crate::exec::audit::ExecAuditRecord;
use crate::exec::coordinator::{
    HeartbeatOutcome, NextOutcome, ResultOutcome, RunOutcome, NEXT_LONG_POLL_WAIT, PICKUP_WAIT,
    RESULT_GRACE,
};
use crate::state::AppState;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use worker_core::proto::{
    ExecRegisterReq, ExecRequest, ExecResponse, DEFAULT_OUTPUT_LIMIT_BYTES, DEFAULT_TIMEOUT_SECS,
    HDR_EXEC_SECRET, HDR_INSTANCE_ID, HDR_WORKER_ID, MAX_BODY_BYTES, SHELL_POWERSHELL,
};

// ============================================================
// 内部面:/api/internal/exec/*
// ============================================================

/// 请求头里携带、经中间件校验后透传给 handler 的 worker 身份。
#[derive(Clone)]
struct WorkerIdHdr(String);
/// 非 register 端点必须携带、经中间件校验后透传的实例 id。
#[derive(Clone)]
struct InstanceIdHdr(String);

/// 需要传入 `state`(而不是像其余 `router()` 那样无状态构造、最后统一 `.with_state()`)：
/// 内部鉴权中间件要在请求分发前查 `state.pool`(exec_worker_creds 表),axum 的
/// `middleware::from_fn` 若让 handler 以 `State<AppState>` 提取器为参而不显式绑定 state,
/// 编译期推导不出 `FromFn` 的状态类型参数(`S` 会退化成 `()`,`.layer()` 报
/// `trait bound not satisfied`)。改用 `from_fn_with_state` 在装配处直接把 `state` 绑进去。
pub fn internal_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/heartbeat", post(heartbeat))
        .route("/next", get(next))
        .route("/result", post(result))
        .layer(axum::middleware::from_fn_with_state(state, internal_auth))
}

fn header_str(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn unauthorized(msg: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": msg }))).into_response()
}

/// per-worker secret 鉴权 + (除 register 外)实例头存在性校验。真正的 `instance_id` 是否
/// 等于「当前注册实例」由各 handler 调 [`Coordinator`] 时校验(需要 worker_id 才能查,
/// 中间件阶段还不知道注册状态是否存在,故只做「必须携带」的前置校验)。
async fn internal_auth(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let Some(worker_id) = header_str(&req, HDR_WORKER_ID) else {
        return unauthorized("missing x-worker-id");
    };
    let Some(secret) = header_str(&req, HDR_EXEC_SECRET) else {
        return unauthorized("missing x-exec-secret");
    };

    let pool = state.pool.clone();
    let wid = worker_id.clone();
    let sec = secret.clone();
    let verified =
        tokio::task::spawn_blocking(move || toolkit_core::exec_creds::verify(&pool, &wid, &sec))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false);
    if !verified {
        state
            .exec_audit
            .record_event(
                "auth_failed",
                Some(worker_id.clone()),
                None,
                "bad or revoked exec secret",
            )
            .await;
        return unauthorized("bad or revoked worker credential");
    }

    let is_register = req.uri().path().ends_with("/register");
    if !is_register {
        let Some(instance_id) = header_str(&req, HDR_INSTANCE_ID) else {
            return unauthorized("missing x-instance-id");
        };
        req.extensions_mut().insert(InstanceIdHdr(instance_id));
    }
    req.extensions_mut().insert(WorkerIdHdr(worker_id));
    next.run(req).await
}

async fn register(
    State(state): State<AppState>,
    Extension(WorkerIdHdr(hdr_worker_id)): Extension<WorkerIdHdr>,
    Json(body): Json<ExecRegisterReq>,
) -> Response {
    if body.worker_id != hdr_worker_id {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "worker_id in body must match x-worker-id header" })),
        )
            .into_response();
    }
    state.exec.register(
        &body.worker_id,
        &body.instance_id,
        body.powershell.as_deref(),
        body.hostname.as_deref(),
    );
    (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
}

async fn heartbeat(
    State(state): State<AppState>,
    Extension(WorkerIdHdr(worker_id)): Extension<WorkerIdHdr>,
    Extension(InstanceIdHdr(instance_id)): Extension<InstanceIdHdr>,
) -> Response {
    match state.exec.heartbeat(&worker_id, &instance_id) {
        HeartbeatOutcome::Ok => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        HeartbeatOutcome::UnknownWorker => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "reason": "unknown worker, re-register" })),
        )
            .into_response(),
        HeartbeatOutcome::StaleInstance => {
            state
                .exec_audit
                .record_event(
                    "stale_instance",
                    Some(worker_id),
                    None,
                    "heartbeat with stale instance_id",
                )
                .await;
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "stale_instance" })),
            )
                .into_response()
        }
    }
}

async fn next(
    State(state): State<AppState>,
    Extension(WorkerIdHdr(worker_id)): Extension<WorkerIdHdr>,
    Extension(InstanceIdHdr(instance_id)): Extension<InstanceIdHdr>,
) -> Response {
    match state
        .exec
        .next(&worker_id, &instance_id, NEXT_LONG_POLL_WAIT)
        .await
    {
        NextOutcome::Job(req) => (StatusCode::OK, Json(req)).into_response(),
        NextOutcome::Idle => StatusCode::NO_CONTENT.into_response(),
        NextOutcome::UnknownWorker => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "reason": "unknown worker, re-register" })),
        )
            .into_response(),
        NextOutcome::StaleInstance => {
            state
                .exec_audit
                .record_event(
                    "stale_instance",
                    Some(worker_id),
                    None,
                    "next with stale instance_id",
                )
                .await;
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "stale_instance" })),
            )
                .into_response()
        }
    }
}

async fn result(
    State(state): State<AppState>,
    Extension(WorkerIdHdr(worker_id)): Extension<WorkerIdHdr>,
    Extension(InstanceIdHdr(instance_id)): Extension<InstanceIdHdr>,
    Json(resp): Json<ExecResponse>,
) -> Response {
    let id = resp.id.clone();
    // 落审计所需的字段先拷出来:resp 马上要整个 move 进 coordinator(转发给 `/run` 等待方),
    // 事后就拿不到了。controller 侧的「一次执行」权威审计记录就写在这里(而不是 `/run`
    // 的成功分支)——因为只有这里保证「不管 `/run` 是否还在等」都会执行一次,`/run` 侧等
    // 结果超时放弃后不会再收到这份 resp。
    let state_str = resp.state.as_str().to_string();
    let exit_code = resp.exit_code;
    let stdout_bytes = resp.stdout.len();
    let stderr_bytes = resp.stderr.len();
    let stdout_truncated = resp.stdout_truncated;
    let stderr_truncated = resp.stderr_truncated;
    let duration_ms = resp.duration_ms;

    match state.exec.result(&worker_id, &instance_id, resp) {
        Ok(done) => {
            state
                .exec_audit
                .record_exec(ExecAuditRecord {
                    operator: done.req.operator.clone(),
                    worker_id: worker_id.clone(),
                    id: id.clone(),
                    shell: done.req.shell.clone(),
                    cwd: done.req.cwd.clone(),
                    script_hash: worker_core::proto::script_hash(&done.req.script),
                    state: state_str,
                    exit_code,
                    stdout_bytes,
                    stderr_bytes,
                    stdout_truncated,
                    stderr_truncated,
                    duration_ms,
                    started_at: done.queued_at,
                    finished_at: toolkit_core::now_iso8601(),
                })
                .await;
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(ResultOutcome::UnknownWorker) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "reason": "unknown worker, re-register" })),
        )
            .into_response(),
        Err(ResultOutcome::StaleInstance) => {
            state
                .exec_audit
                .record_event(
                    "stale_instance",
                    Some(worker_id),
                    Some(id),
                    "result with stale instance_id",
                )
                .await;
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "stale_instance" })),
            )
                .into_response()
        }
        Err(ResultOutcome::IdMismatch) => {
            state
                .exec_audit
                .record_event(
                    "result_ownership_rejected",
                    Some(worker_id),
                    Some(id),
                    "result id does not match current slot (forged/late/duplicate)",
                )
                .await;
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "id_mismatch" })),
            )
                .into_response()
        }
    }
}

// ============================================================
// 消费面:/api/web/exec/*
// ============================================================

/// 配置 exec 专用 token 的环境变量名。支持多枚 `token:operator`(逗号分隔);单枚无冒号时
/// operator 取 `"default"`。**未设置或为空时不挂载 `/api/web/exec/*`**(见 [`web_router`])。
pub const EXEC_TOKEN_ENV: &str = "TOOLKIT_EXEC_TOKEN";

#[derive(Clone)]
struct Operator(String);

fn parse_tokens(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        match entry.split_once(':') {
            Some((tok, op)) => {
                let tok = tok.trim();
                let op = op.trim();
                if !tok.is_empty() {
                    let op = if op.is_empty() { "default" } else { op };
                    out.push((tok.to_string(), op.to_string()));
                }
            }
            None => out.push((entry.to_string(), "default".to_string())),
        }
    }
    out
}

fn extract_bearer(req: &Request) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// exec 专用 token 鉴权中间件:每次请求都重新读环境变量(与仓库 `auth::require_token` /
/// `internal.rs::token_auth` 同风格),命中的 token 对应哪个 operator 就把 operator
/// 写进请求扩展,供 handler 读取并**强制覆盖**调用方 body 里可能自称的 operator。
async fn exec_token_auth(mut req: Request, next: Next) -> Response {
    let raw = std::env::var(EXEC_TOKEN_ENV).unwrap_or_default();
    let tokens = parse_tokens(&raw);
    let got = extract_bearer(&req);
    let operator = got.as_deref().and_then(|g| {
        tokens
            .iter()
            .find(|(t, _)| t == g)
            .map(|(_, op)| op.clone())
    });
    match operator {
        Some(op) => {
            req.extensions_mut().insert(Operator(op));
            next.run(req).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing or bad exec token (Authorization: Bearer <token>)" })),
        )
            .into_response(),
    }
}

/// 未设置(或空) [`EXEC_TOKEN_ENV`] 时返回 `None` —— 调用方必须据此**不挂载**
/// `/api/web/exec/*`,而不是挂载后再靠鉴权全部拒绝(设计明确要求「不存在」)。
pub fn web_router() -> Option<Router<AppState>> {
    let raw = std::env::var(EXEC_TOKEN_ENV).unwrap_or_default();
    if raw.trim().is_empty() {
        log::info!("未设置 {EXEC_TOKEN_ENV}:/api/web/exec/* 不挂载(远程执行功能关闭)");
        return None;
    }
    log::info!("{EXEC_TOKEN_ENV} 已配置:挂载 /api/web/exec/*");
    Some(
        Router::new()
            .route("/workers", get(list_workers))
            .route("/run", post(run))
            // 临时权限审批面（zero-desktop 的「远程节点」页消费）。放在 exec token 之下：
            // 批准 = 授予对方机器上的任意命令执行权，安全边界与 /run 同级，不能降级到
            // 全局 TOOLKIT_API_TOKEN。
            .route("/requests", get(list_requests))
            .route("/requests/{worker_id}/approve", post(approve_request))
            .route("/requests/{worker_id}/reject", post(reject_request))
            .route("/creds", get(list_creds))
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .layer(from_fn(exec_token_auth)),
    )
}

async fn list_workers(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "workers": state.exec.list_workers() }))
}

// ---------- 临时权限：worker 申请（免凭据）+ 面板审批（exec token）----------

/// worker 自助申请通道。**刻意不挂 [`internal_auth`]**——申请的前提就是还没有凭据。
///
/// 挡刷靠 `toolkit_core::exec_requests` 的三道（同 id 去重 / pending 上限 / 24h TTL），
/// **不做按来源 IP 限频**：外网入口在 caddy 反代之后，这里看到的 peer 永远是反代自己，
/// 真实来源只能靠 `X-Forwarded-For`，而那是可伪造的头，据此限频既没用又会误伤。
pub fn access_router() -> Router<AppState> {
    Router::new()
        .route("/request", post(access_request))
        .route("/poll", get(access_poll))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

async fn access_request(
    State(state): State<AppState>,
    Json(body): Json<worker_core::proto::ExecAccessReq>,
) -> Response {
    if body.worker_id.trim().is_empty() || body.worker_id.len() > 128 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "invalid worker_id" })),
        )
            .into_response();
    }
    let pool = state.pool.clone();
    let (wid, label, host, os) = (
        body.worker_id.clone(),
        truncate(&body.label, 64),
        truncate(&body.hostname, 64),
        truncate(&body.os, 32),
    );
    let outcome = tokio::task::spawn_blocking(move || {
        toolkit_core::exec_requests::submit(&pool, &wid, &label, &host, &os)
    })
    .await;
    match outcome {
        Ok(Ok(toolkit_core::exec_requests::SubmitOutcome::Pending)) => {
            state
                .exec_audit
                .record_event(
                    "access_requested",
                    Some(body.worker_id),
                    None,
                    "worker requested temporary access",
                )
                .await;
            (StatusCode::OK, Json(json!({ "state": "pending" }))).into_response()
        }
        Ok(Ok(toolkit_core::exec_requests::SubmitOutcome::TooMany)) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "too_many_pending_requests" })),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "request_failed" })),
        )
            .into_response(),
    }
}

/// 截断到 `max` 字节（按字符边界，避免切坏 UTF-8）。
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    s.chars().take(max / 3).collect()
}

#[derive(Deserialize)]
struct AccessPollQuery {
    worker_id: String,
}

async fn access_poll(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<AccessPollQuery>,
) -> Response {
    use toolkit_core::exec_requests::PollOutcome;
    let pool = state.pool.clone();
    let wid = q.worker_id.clone();
    let outcome =
        tokio::task::spawn_blocking(move || toolkit_core::exec_requests::poll(&pool, &wid)).await;
    let resp = match outcome {
        Ok(Ok(PollOutcome::Approved { secret, expires_at })) => {
            worker_core::proto::ExecAccessPollResp {
                state: "approved".into(),
                secret: Some(secret),
                expires_at: Some(expires_at),
            }
        }
        Ok(Ok(PollOutcome::AlreadyClaimed)) => worker_core::proto::ExecAccessPollResp {
            state: "already_claimed".into(),
            secret: None,
            expires_at: None,
        },
        Ok(Ok(PollOutcome::Rejected)) => worker_core::proto::ExecAccessPollResp {
            state: "rejected".into(),
            secret: None,
            expires_at: None,
        },
        Ok(Ok(PollOutcome::Pending)) => worker_core::proto::ExecAccessPollResp {
            state: "pending".into(),
            secret: None,
            expires_at: None,
        },
        _ => worker_core::proto::ExecAccessPollResp {
            state: "unknown".into(),
            secret: None,
            expires_at: None,
        },
    };
    if resp.state == "approved" {
        state
            .exec_audit
            .record_event(
                "access_granted",
                Some(q.worker_id),
                None,
                "worker claimed approved credential",
            )
            .await;
    }
    (StatusCode::OK, Json(resp)).into_response()
}

async fn list_requests(State(state): State<AppState>) -> Response {
    let pool = state.pool.clone();
    match tokio::task::spawn_blocking(move || toolkit_core::exec_requests::list(&pool)).await {
        Ok(Ok(rows)) => (StatusCode::OK, Json(json!({ "requests": rows }))).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "list_failed" })),
        )
            .into_response(),
    }
}

async fn list_creds(State(state): State<AppState>) -> Response {
    let pool = state.pool.clone();
    match tokio::task::spawn_blocking(move || toolkit_core::exec_creds::list(&pool)).await {
        Ok(Ok(rows)) => (StatusCode::OK, Json(json!({ "creds": rows }))).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "list_failed" })),
        )
            .into_response(),
    }
}

/// 批准时长上限：一次最多授权 7 天，避免手滑点出一个"永久"权限。
const MAX_APPROVE_HOURS: f64 = 24.0 * 7.0;

#[derive(Deserialize)]
struct ApproveReq {
    /// 授权时长（小时），默认 20。
    #[serde(default)]
    hours: Option<f64>,
}

async fn approve_request(
    State(state): State<AppState>,
    axum::extract::Path(worker_id): axum::extract::Path<String>,
    Extension(Operator(operator)): Extension<Operator>,
    body: Option<Json<ApproveReq>>,
) -> Response {
    let hours = body.and_then(|Json(b)| b.hours).unwrap_or(20.0);
    if !(hours > 0.0 && hours <= MAX_APPROVE_HOURS) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": format!("hours must be in (0, {MAX_APPROVE_HOURS}]") })),
        )
            .into_response();
    }
    let pool = state.pool.clone();
    let (wid, by) = (worker_id.clone(), operator.clone());
    match tokio::task::spawn_blocking(move || {
        toolkit_core::exec_requests::approve(&pool, &wid, hours, &by)
    })
    .await
    {
        Ok(Ok(true)) => {
            state
                .exec_audit
                .record_event(
                    "access_approved",
                    Some(worker_id),
                    None,
                    &format!("approved for {hours}h by {operator}"),
                )
                .await;
            (StatusCode::OK, Json(json!({ "ok": true, "hours": hours }))).into_response()
        }
        Ok(Ok(false)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no_such_request" })),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "approve_failed" })),
        )
            .into_response(),
    }
}

async fn reject_request(
    State(state): State<AppState>,
    axum::extract::Path(worker_id): axum::extract::Path<String>,
    Extension(Operator(operator)): Extension<Operator>,
) -> Response {
    let pool = state.pool.clone();
    let (wid, by) = (worker_id.clone(), operator.clone());
    match tokio::task::spawn_blocking(move || toolkit_core::exec_requests::reject(&pool, &wid, &by))
        .await
    {
        Ok(Ok(true)) => {
            state
                .exec_audit
                .record_event(
                    "access_rejected",
                    Some(worker_id),
                    None,
                    &format!("rejected by {operator}"),
                )
                .await;
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Ok(Ok(false)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no_such_request" })),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "reject_failed" })),
        )
            .into_response(),
    }
}

/// `POST /run` 请求体。`env` 用对象(贴合设计文档 §5.2 的 JSON 示例),内部转换为
/// [`ExecRequest`] 要求的 `Vec<(String,String)>`。**不接受调用方指定 `operator`**——
/// 该字段由 [`exec_token_auth`] 命中的 token 注入,body 里即便带了也会被忽略
/// (未在本结构体声明该字段,serde 默认忽略未知字段)。
#[derive(Deserialize)]
struct RunReq {
    worker_id: String,
    script: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    stdout_limit_bytes: Option<usize>,
    #[serde(default)]
    stderr_limit_bytes: Option<usize>,
}

/// 对 operator 的统一投影(设计 §5.2 `ExecOutcome`)。
#[derive(Serialize)]
struct ExecOutcomeJson<'a> {
    state: &'a str,
    source: &'a str,
    id: &'a str,
    exec: Option<&'a ExecResponse>,
    reason: Option<&'a str>,
}

async fn run(
    State(state): State<AppState>,
    Extension(Operator(operator)): Extension<Operator>,
    Json(body): Json<RunReq>,
) -> Response {
    let id = uuid::Uuid::new_v4().to_string();
    let req = ExecRequest {
        id: id.clone(),
        operator: operator.clone(),
        shell: SHELL_POWERSHELL.to_string(),
        script: body.script,
        args: body.args,
        cwd: body.cwd,
        env: body.env.into_iter().collect(),
        timeout_secs: body.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        stdout_limit_bytes: body
            .stdout_limit_bytes
            .unwrap_or(DEFAULT_OUTPUT_LIMIT_BYTES),
        stderr_limit_bytes: body
            .stderr_limit_bytes
            .unwrap_or(DEFAULT_OUTPUT_LIMIT_BYTES),
    };
    if let Err(e) = worker_core::proto::validate(&req) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    let worker_id = body.worker_id;

    let outcome = state
        .exec
        .run(&worker_id, req, PICKUP_WAIT, RESULT_GRACE)
        .await;

    match outcome {
        RunOutcome::NotExecCapable => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "worker_not_exec_capable" })),
        )
            .into_response(),
        RunOutcome::Offline => {
            state
                .exec_audit
                .record_event(
                    "worker_offline",
                    Some(worker_id),
                    Some(id),
                    "worker heartbeat expired",
                )
                .await;
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "worker_offline" })),
            )
                .into_response()
        }
        RunOutcome::Busy => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "worker_busy" })),
        )
            .into_response(),
        RunOutcome::NotPickedUp => {
            state
                .exec_audit
                .record_event(
                    "not_picked_up",
                    Some(worker_id),
                    Some(id.clone()),
                    "worker did not poll next() within pickup window",
                )
                .await;
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ExecOutcomeJson {
                    state: "not_picked_up",
                    source: "controller",
                    id: &id,
                    exec: None,
                    reason: Some("worker did not pick up the task in time"),
                }),
            )
                .into_response()
        }
        RunOutcome::Unknown => {
            state
                .exec_audit
                .record_event(
                    "unknown",
                    Some(worker_id),
                    Some(id.clone()),
                    "picked up but no confirmed result before deadline",
                )
                .await;
            (
                StatusCode::BAD_GATEWAY,
                Json(ExecOutcomeJson {
                    state: "unknown",
                    source: "controller",
                    id: &id,
                    exec: None,
                    reason: Some("命令可能已执行，controller 无法确认结果，禁止自动重试"),
                }),
            )
                .into_response()
        }
        RunOutcome::Completed(resp) => {
            // 权威审计记录已在 worker 回传 `POST /api/internal/exec/result` 时写入
            // （见 `result()` handler 注释）：那里保证「不管 `/run` 是否还在等」都会执行一次；
            // 这里只负责把已经拿到的结果投影给 operator。
            let state_str = resp.state.as_str();
            (
                StatusCode::OK,
                Json(ExecOutcomeJson {
                    state: state_str,
                    source: "worker",
                    id: &id,
                    exec: Some(&resp),
                    reason: None,
                }),
            )
                .into_response()
        }
    }
}
