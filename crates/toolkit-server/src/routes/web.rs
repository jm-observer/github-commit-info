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
        .route("/probe", get(probe))
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

// ============ loopback 探针代理 ============

/// 探针请求超时（G10 本机 loopback，正常毫秒级返回）。
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
/// 回传响应体的截断上限（健康端点是小 JSON，超出必然不是健康端点）。
const PROBE_BODY_LIMIT: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct ProbeQuery {
    /// 目标 URL，**必须是 loopback**（见 [`check_loopback`]）。
    target: String,
}

/// 校验探针目标：只放行 `http(s)://127.x.x.x|localhost|[::1]` 开头的 URL。
///
/// 这个端点让桌面端借 toolkit-server 的本机视角去探 G10 上其它服务的健康端点
/// （trace-hub / zero / alarm-server…），从而**外网只需 toolkit-server 一个入口**，
/// 不必给每个服务做端口映射。代价是它天然是一个受控 SSRF —— 因此：
/// - 目标 host 限死 loopback（探不到外网，也探不到内网其它机器）；
/// - 路由挂在 `/api/web` 下，受 `TOOLKIT_API_TOKEN` 鉴权（**不在 auth 豁免名单里**）。
///
/// 残留风险是「持 token 者可扫 G10 本机端口」，这与持 token 者本就能做的事同量级。
fn check_loopback(target: &str) -> Result<(), String> {
    let rest = match target.split_once("://") {
        Some(("http", r)) | Some(("https", r)) => r,
        _ => return Err("target 必须以 http:// 或 https:// 开头".into()),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.contains('@') {
        return Err("target 不允许携带 userinfo".into());
    }
    // 去端口：IPv6 字面量形如 `[::1]:9120`，按 `]` 切；否则按最后一个 `:` 切。
    let host = match authority.rsplit_once(']') {
        Some((h, _)) => h.trim_start_matches('['),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    let is_loopback = host == "localhost"
        || host == "::1"
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    if is_loopback {
        Ok(())
    } else {
        Err(format!("target host `{host}` 不是 loopback，拒绝代发"))
    }
}

/// `GET /api/web/probe?target=http://127.0.0.1:9120/health`
///
/// 以 toolkit-server 自身（跑在 G10 上）的视角对 loopback 目标发一次 GET，回传状态码 +
/// 响应体 + 耗时。**上游不可达不算本端点失败**——照样 200，把失败信息放进 `error`，
/// 供面板统一渲染红灯（与桌面端直连探测的语义一致）。
async fn probe(Query(q): Query<ProbeQuery>) -> impl IntoResponse {
    if let Err(e) = check_loopback(&q.target) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": e })));
    }
    // 放宽证书校验：G10 上部分服务（english prod feature）跑自签 HTTPS。
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("构建 client 失败：{e}") })),
            )
        }
    };

    let started = std::time::Instant::now();
    let resp = client.get(&q.target).send().await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let body = match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let text = match r.text().await {
                Ok(mut t) => {
                    t.truncate(PROBE_BODY_LIMIT);
                    t
                }
                Err(e) => {
                    return (
                        StatusCode::OK,
                        Json(json!({
                            "ok": false,
                            "status_code": status,
                            "latency_ms": latency_ms,
                            "error": format!("读取响应体失败：{e}"),
                        })),
                    )
                }
            };
            json!({
                "ok": (200..300).contains(&status),
                "status_code": status,
                "latency_ms": latency_ms,
                "body": text,
            })
        }
        Err(e) => json!({
            "ok": false,
            "status_code": Value::Null,
            "latency_ms": latency_ms,
            "error": format!("{e}"),
        }),
    };
    (StatusCode::OK, Json(body))
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

#[cfg(test)]
mod tests {
    use super::check_loopback;

    #[test]
    fn loopback_targets_pass() {
        assert!(check_loopback("http://127.0.0.1:9120/health").is_ok());
        assert!(check_loopback("http://localhost:9001/health").is_ok());
        assert!(check_loopback("https://127.0.0.1:28080/health").is_ok());
        assert!(check_loopback("http://[::1]:8080/api/health").is_ok());
        assert!(check_loopback("http://127.0.0.1/health").is_ok());
    }

    #[test]
    fn non_loopback_targets_rejected() {
        // 内网其它机器 / 公网 / 私网段一律拒绝——探针只服务「G10 本机」这一个用途。
        assert!(check_loopback("http://192.168.0.68:8788/api/web/health").is_err());
        assert!(check_loopback("http://10.0.0.2:9120/health").is_err());
        assert!(check_loopback("https://example.com/health").is_err());
        // userinfo 伪装 host：`evil.com` 才是真目标。
        assert!(check_loopback("http://127.0.0.1@evil.com/health").is_err());
        // 非 http scheme。
        assert!(check_loopback("file:///etc/passwd").is_err());
        assert!(check_loopback("127.0.0.1:9120/health").is_err());
    }
}
