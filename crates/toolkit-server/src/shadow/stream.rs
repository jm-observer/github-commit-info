//! 跟读**流式**发音评测 WS 中继(`GET /api/web/shadow/stream`)。
//!
//! 桌面端 ↔ toolkit-server(axum WS 服务端)↔ GB10 `:8098 /assess/stream`(WS 客户端)。
//! 纯双向转发 hello/二进制 PCM/end 与 ready/partial/final/error;**拦截 `final` 事件落库**
//! (权威分,复用 `store::record_attempt`)。设计见 docs/english-shadow-realtime-design.md §7。
//!
//! **未配 `GOP_BASE_URL` → 503**(流式无 v1 回退;v1 没有流式语义,与批量 `/score` 的回退正交)。
//! 单元元信息(customer_id/kind/sentence_id/word_index/threshold)走 WS 的 query 串。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as TMessage;

use crate::shadow::{self, store, ShadowKind};
use crate::state::AppState;

#[derive(Debug, Clone, Deserialize)]
pub struct StreamParams {
    customer_id: i64,
    kind: String,
    sentence_id: i64,
    #[serde(default)]
    word_index: Option<i64>,
    #[serde(default)]
    threshold: Option<f64>,
}

/// `GET /api/web/shadow/stream`：升级为 WS,中继到 `:8098 /assess/stream`。
pub async fn stream(
    State(state): State<AppState>,
    Query(p): Query<StreamParams>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(base) = shadow::gop_base() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "流式发音评测未启用（GOP_BASE_URL 未配置）",
                "hint": "set GOP_BASE_URL (e.g. http://127.0.0.1:8098)"
            })),
        )
            .into_response();
    };
    ws.on_upgrade(move |sock| relay(sock, base, state, p))
}

async fn relay(client: WebSocket, base: String, state: AppState, p: StreamParams) {
    // http(s)://host:port → ws(s)://host:port/assess/stream
    let ws_url = format!(
        "{}/assess/stream",
        base.replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    );
    let req = match ws_url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => return close_with_error(client, &format!("bad upstream url: {e}")).await,
    };
    let upstream = match tokio_tungstenite::connect_async(req).await {
        Ok((s, _)) => s,
        Err(e) => return close_with_error(client, &format!("发音评测流式上游不可达: {e}")).await,
    };

    let (mut cli_tx, mut cli_rx) = client.split();
    let (mut up_tx, mut up_rx) = upstream.split();
    let kind = ShadowKind::parse(&p.kind);
    let threshold = p.threshold.unwrap_or(shadow::DEFAULT_THRESHOLD);

    // 桌面端 → 上游:转发 hello(Text)/ PCM(Binary)/ end(Text)。
    let client_to_upstream = tokio::spawn(async move {
        while let Some(Ok(msg)) = cli_rx.next().await {
            let fwd = match msg {
                Message::Text(t) => TMessage::text(t.to_string()),
                Message::Binary(b) => TMessage::Binary(b.to_vec().into()),
                Message::Close(_) => {
                    let _ = up_tx.send(TMessage::Close(None)).await;
                    break;
                }
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            if up_tx.send(fwd).await.is_err() {
                break;
            }
        }
    });

    // 上游 → 桌面端:转发事件;拦截 final 落库。
    while let Some(Ok(msg)) = up_rx.next().await {
        match msg {
            TMessage::Text(t) => {
                let s = t.to_string();
                maybe_record_final(&s, &state, &p, kind, threshold);
                if cli_tx.send(Message::Text(s.into())).await.is_err() {
                    break;
                }
            }
            TMessage::Binary(b) => {
                if cli_tx
                    .send(Message::Binary(b.to_vec().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            TMessage::Close(_) => {
                let _ = cli_tx.send(Message::Close(None)).await;
                break;
            }
            _ => {}
        }
    }
    client_to_upstream.abort();
}

/// 若是 `final` 事件 → 解析为 `ScoreResult` 落 `shadow_attempt`(权威分)。DB 失败仅记日志。
fn maybe_record_final(
    text: &str,
    state: &AppState,
    p: &StreamParams,
    kind: Option<ShadowKind>,
    threshold: f64,
) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if v.get("type").and_then(|x| x.as_str()) != Some("final") {
        return;
    }
    let (Some(kind), Some(result)) = (kind, shadow::gop::score_result_from_final(&v, threshold))
    else {
        return;
    };
    if let Err(e) = store::record_attempt(
        &state.pool,
        p.customer_id,
        kind,
        p.sentence_id,
        p.word_index,
        &result,
    ) {
        log::warn!("shadow stream final 落库失败: {e:#}");
    }
}

async fn close_with_error(client: WebSocket, message: &str) {
    let (mut tx, _rx) = client.split();
    let _ = tx
        .send(Message::Text(
            json!({ "type": "error", "message": message })
                .to_string()
                .into(),
        ))
        .await;
    let _ = tx.send(Message::Close(None)).await;
}
