use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

/// 从入站请求头解析 W3C `traceparent`，得到上游当前 span 的上下文（供任务接入同一条
/// trace）。无头 / 格式非法返回 None。
fn trace_ctx_from_headers(headers: &HeaderMap) -> Option<toolkit_tasks::TraceContext> {
    custom_utils::trace::extract_traceparent(|name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    })
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/tasks", get(list_tasks).post(submit_task))
        .route("/tasks/{task_id}", get(get_task))
}

async fn health(State(s): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("GIT_COMMIT").unwrap_or("unknown"),
        "db_path": s.db_path.display().to_string(),
    }))
}

#[derive(Debug, Deserialize)]
struct SubmitBody {
    kind: String,
    input: Value,
    #[serde(default)]
    callback_url: Option<String>,
}

async fn submit_task(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SubmitBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let trace_parent = trace_ctx_from_headers(&headers);
    match toolkit_tasks::submit(
        &s.registry,
        &s.pool,
        &s.workspace,
        &body.kind,
        body.input,
        body.callback_url,
        trace_parent,
    ) {
        Ok(id) => Ok(Json(json!({ "task_id": id }))),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("{e:#}") })),
        )),
    }
}

async fn get_task(State(s): State<AppState>, Path(task_id): Path<String>) -> impl IntoResponse {
    match toolkit_tasks::status(&s.pool, &task_id) {
        Ok(Some(dto)) => (StatusCode::OK, Json(serde_json::to_value(dto).unwrap())),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "task not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e:#}")})),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    kind: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
}

async fn list_tasks(State(s): State<AppState>, Query(q): Query<ListQuery>) -> impl IntoResponse {
    let filter = toolkit_tasks::TaskListFilter {
        kind: q.kind,
        state: q.state,
        limit: q.limit,
    };
    match toolkit_tasks::list_tasks(&s.pool, &filter) {
        Ok(v) => (StatusCode::OK, Json(serde_json::to_value(v).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("{e:#}")})),
        ),
    }
}
