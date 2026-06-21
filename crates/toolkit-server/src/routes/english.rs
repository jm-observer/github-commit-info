//! english 后端反代：`/api/english/*tail` → `{ENGLISH_BASE_URL}/{tail}{?query}`。
//!
//! english 独立监听 `:28080` 自签 HTTPS（与 toolkit-server 不在一个进程），桌面端 LAN 模式
//! 直连会被 Tauri plugin-http 的证书校验拒掉。让 toolkit-server 用 reqwest（`danger_accept_
//! invalid_certs(true)`）代理转发，前端 LAN 走明文 `http://<host>:8788/api/english/...`，
//! 不再碰证书。WAN 模式因为有真证书，桌面端继续直连 english 域名，不走这条代理。
//!
//! 上游地址由环境变量 `ENGLISH_BASE_URL` 配置；缺省 `https://127.0.0.1:28080`（部署事实）。
//! 缺省即正确，所以不像 TTS/CLEAN 那样未配 → 503，而是默认就能用。

use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use std::sync::OnceLock;
use std::time::Duration;

/// 默认上游 = 同机回环（部署事实）。`ENGLISH_BASE_URL` 可覆盖。
const DEFAULT_BASE: &str = "https://127.0.0.1:28080";
/// 与 english 端点最长合理调用对齐（替换音频等小文件上传）。
const PROXY_TIMEOUT: Duration = Duration::from_secs(60);

fn english_base_url() -> String {
    std::env::var("ENGLISH_BASE_URL")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

/// 复用一个接受自签的 reqwest client，避免每请求重建 TLS context。
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(PROXY_TIMEOUT)
            .danger_accept_invalid_certs(true)
            .build()
            .expect("english proxy reqwest client build")
    })
}

pub fn router() -> Router<AppState> {
    Router::new().route("/{*tail}", any(proxy))
}

async fn proxy(req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();

    // 路径：nest 在 `/api/english` 下后，req.uri().path() 已是 `/<tail>`；query 原样带上。
    let tail = uri.path();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("{}{}{}", english_base_url(), tail, query);

    let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => return bad_gateway(format!("read request body: {e}")),
    };

    match forward(&method, &url, &headers, body_bytes).await {
        Ok(resp) => resp,
        Err(e) => bad_gateway(format!("english upstream {method} {url}: {e}")),
    }
}

async fn forward(
    method: &Method,
    url: &str,
    in_headers: &HeaderMap,
    body: Bytes,
) -> Result<Response, reqwest::Error> {
    let rmethod =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut req = client().request(rmethod, url).body(body.to_vec());

    // 透传请求头：剥掉 hop-by-hop 头 + host（reqwest 自己填）。
    for (name, value) in in_headers {
        let n = name.as_str();
        if matches!(
            n.to_ascii_lowercase().as_str(),
            "host"
                | "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailers"
                | "transfer-encoding"
                | "upgrade"
                | "content-length"
        ) {
            continue;
        }
        req = req.header(n, value);
    }

    let resp = req.send().await?;
    let status = resp.status();
    let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    let mut out_headers = HeaderMap::new();
    for (name, value) in resp.headers() {
        let n = name.as_str().to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "content-length"
                | "content-encoding"
        ) {
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            HeaderName::from_bytes(name.as_ref()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out_headers.insert(hn, hv);
        }
    }

    let bytes = resp.bytes().await?;
    Ok((code, out_headers, Body::from(bytes)).into_response())
}

fn bad_gateway(msg: String) -> Response {
    log::warn!("english proxy: {msg}");
    (
        StatusCode::BAD_GATEWAY,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg,
    )
        .into_response()
}
