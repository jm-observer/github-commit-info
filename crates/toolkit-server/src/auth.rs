//! 全局 API 鉴权：`Authorization: Bearer <token>`（WebSocket 走 `?token=`）。
//!
//! **动机**：toolkit-server 的外网入口（38788 → G10:8788）是明文端口映射，`/api/web/*`
//! 此前完全无鉴权即可读写 LLM 配置、上传 cookie、提交任务。桌面端各 handler 早就在发
//! `g10_token`（`reqwest::bearer_auth`），只是服务端没人接——本模块把这条链接上。
//!
//! **开关**：设了环境变量 [`TOKEN_ENV`] 才校验；未设则整层放行并在启动时告警（与
//! `/api/internal` 的 `EGRESS_WORKER_TOKEN` 同风格，本地开发零配置）。
//!
//! **豁免**（见 [`is_exempt`]）：
//! - `OPTIONS`：CORS 预检不带 Authorization。
//! - `/api/web/health`：桌面端 `auto` 模式靠它探测局域网可达性，必须先于鉴权可达。
//! - `/api/internal/*`：有自己的 `x-egress-token` 中间件，不叠加。
//! - 非 `/api` 路径：内嵌控制台的静态资源（HTML/JS/CSS 本身不含数据）。
//!
//! **已知影响**：一旦设了 token，浏览器打开的 web 控制台调 `/api/web/*` 会 401——
//! 浏览器 fetch 不会自动带 Bearer。控制台目前没有填 token 的入口，故公网暴露场景下
//! 建议「设 token + 控制台只在局域网用」。给控制台加 token 输入框是后续的事。

use axum::extract::Request;
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// 配置 token 的环境变量名。未设 = 不鉴权。
pub const TOKEN_ENV: &str = "TOOLKIT_API_TOKEN";

/// 该路径是否豁免鉴权。理由见模块文档。
fn is_exempt(method: &Method, path: &str) -> bool {
    method == Method::OPTIONS
        || path == "/api/web/health"
        || path.starts_with("/api/internal")
        || !path.starts_with("/api")
}

/// 从请求里取出调用方提供的 token：优先 `Authorization: Bearer`，回退 `?token=`。
///
/// 回退分支是为 WebSocket 准备的：浏览器的 `WebSocket` 构造器无法设置请求头，而
/// `/api/asr/stream`、`/api/web/shadow/stream` 都由 webview 直连。
fn extract_token(req: &Request) -> Option<String> {
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if bearer.is_some() {
        return bearer;
    }
    req.uri().query().and_then(|q| {
        q.split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == "token")
            .map(|(_, v)| v.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// 全局鉴权中间件。未配置 [`TOKEN_ENV`] 时整层放行。
pub async fn require_token(req: Request, next: Next) -> Response {
    let Ok(expected) = std::env::var(TOKEN_ENV) else {
        return next.run(req).await;
    };
    let expected = expected.trim();
    if expected.is_empty() {
        return next.run(req).await;
    }
    if is_exempt(req.method(), req.uri().path()) {
        return next.run(req).await;
    }
    match extract_token(&req) {
        // 注意：token 是共享密钥、长度固定，直接比较即可；此处不涉及逐字符时序放大。
        Some(got) if got == expected => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            "missing or bad token (Authorization: Bearer <token> or ?token=)",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_and_static_are_exempt() {
        assert!(is_exempt(&Method::GET, "/api/web/health"));
        assert!(is_exempt(&Method::GET, "/"));
        assert!(is_exempt(&Method::GET, "/hub.js"));
        // internal 有自己的 x-egress-token 中间件。
        assert!(is_exempt(&Method::POST, "/api/internal/egress/next"));
        // CORS 预检不带 Authorization。
        assert!(is_exempt(&Method::OPTIONS, "/api/web/llm/config"));
    }

    #[test]
    fn api_paths_are_protected() {
        assert!(!is_exempt(&Method::GET, "/api/web/llm/config"));
        assert!(!is_exempt(&Method::POST, "/api/browser/cookie"));
        assert!(!is_exempt(&Method::GET, "/api/asr/stream"));
        // 前缀相近但不是 health，仍需鉴权。
        assert!(!is_exempt(&Method::GET, "/api/web/health/detail"));
    }

    fn req_with(auth: Option<&str>, uri: &str) -> Request {
        let mut b = Request::builder().uri(uri);
        if let Some(a) = auth {
            b = b.header(header::AUTHORIZATION, a);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    #[test]
    fn extracts_bearer_then_query() {
        assert_eq!(
            extract_token(&req_with(Some("Bearer abc"), "/api/web/x")).as_deref(),
            Some("abc")
        );
        // WS 无法带头 → query 兜底。
        assert_eq!(
            extract_token(&req_with(None, "/api/asr/stream?token=xyz")).as_deref(),
            Some("xyz")
        );
        // 头优先于 query。
        assert_eq!(
            extract_token(&req_with(Some("Bearer hdr"), "/api/asr/stream?token=qry")).as_deref(),
            Some("hdr")
        );
        assert_eq!(extract_token(&req_with(None, "/api/web/x")), None);
        // 空值不算提供。
        assert_eq!(
            extract_token(&req_with(Some("Bearer   "), "/api/web/x")),
            None
        );
        assert_eq!(extract_token(&req_with(None, "/api/web/x?token=")), None);
    }
}
