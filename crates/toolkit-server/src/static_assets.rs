//! 把 `crates/toolkit-server/web/` 的静态资源 `include_str!` 进二进制。
//!
//! `<workspace>/web/` 存在时由 `ServeDir` 优先托管（用户可覆盖）；
//! 不存在则注册以下 GET 路由走嵌入版本，保证 G10 部署裸跑也能见到完整 UI。

use axum::http::header;
use axum::response::{Html, IntoResponse, Response};

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");
const HUB_HTML: &str = include_str!("../web/hub.html");
const HUB_JS: &str = include_str!("../web/hub.js");
const HUB_CSS: &str = include_str!("../web/hub.css");

pub async fn dashboard() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub async fn app_js() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
        .into_response()
}

pub async fn style_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
        .into_response()
}

pub async fn hub() -> Html<&'static str> {
    Html(HUB_HTML)
}

pub async fn hub_js() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        HUB_JS,
    )
        .into_response()
}

pub async fn hub_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], HUB_CSS).into_response()
}
