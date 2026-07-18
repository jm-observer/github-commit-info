use anyhow::Result;
use log::{debug, info, warn};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::http::websocket::{forward_ws_frame, read_ws_frame, WsFrame};
use crate::sink::{FlowWsFrame, WsDirection};

use super::session::MitmSession;

#[derive(Clone)]
pub(super) struct WsRelayContext {
    pub session: MitmSession,
    pub ws_compressed: bool,
}

/// Relay WebSocket frames bidirectionally, forwarding every complete
/// (defragmented, decompressed) text/binary message to the session's sink.
pub(super) async fn relay_websocket<CR, CW, UR, UW>(
    mut client_reader: BufReader<CR>,
    mut client_writer: CW,
    mut upstream_reader: BufReader<UR>,
    mut upstream_writer: UW,
    ws_ctx: WsRelayContext,
) -> Result<(u64, u64)>
where
    CR: AsyncRead + Unpin + Send + 'static,
    CW: AsyncWrite + Unpin + Send + 'static,
    UR: AsyncRead + Unpin + Send + 'static,
    UW: AsyncWrite + Unpin + Send + 'static,
{
    let domain_owned = ws_ctx.session.domain.clone();

    // Client → Upstream
    let c2u = tokio::spawn({
        let ws_ctx = ws_ctx.clone();
        async move {
            pump_ws_direction(
                &mut client_reader,
                &mut upstream_writer,
                &ws_ctx,
                WsDirection::ClientToServer,
            )
            .await
        }
    });

    // Upstream → Client
    let u2c = tokio::spawn({
        let ws_ctx = ws_ctx.clone();
        async move {
            pump_ws_direction(
                &mut upstream_reader,
                &mut client_writer,
                &ws_ctx,
                WsDirection::ServerToClient,
            )
            .await
        }
    });

    let (up_result, down_result) = tokio::join!(c2u, u2c);
    let up_bytes = up_result.unwrap_or(0);
    let down_bytes = down_result.unwrap_or(0);

    info!(
        "[{}] WebSocket relay finished: {up_bytes} up, {down_bytes} down",
        domain_owned
    );
    Ok((up_bytes, down_bytes))
}

async fn pump_ws_direction<R, W>(
    reader: &mut BufReader<R>,
    writer: &mut W,
    ws_ctx: &WsRelayContext,
    direction: WsDirection,
) -> u64
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let domain = &ws_ctx.session.domain;
    let direction_label = match direction {
        WsDirection::ClientToServer => "client→upstream",
        WsDirection::ServerToClient => "upstream→client",
    };
    let mut bytes = 0u64;
    let mut frag_buf: Vec<u8> = Vec::new();
    let mut frag_opcode: u8 = 0;

    loop {
        let frame = match read_ws_frame(reader).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                info!("[{domain}] WS {direction_label}: EOF");
                break;
            }
            Err(e) => {
                warn!("[{domain}] WS {direction_label} read error: {e:#}");
                break;
            }
        };

        info!(
            "[{domain}] WS {direction_label}: {} fin={} payload={} bytes",
            opcode_name(frame.opcode),
            frame.fin,
            frame.payload.len()
        );

        if let Some((is_text, payload)) = process_ws_frame(
            &frame,
            ws_ctx.ws_compressed,
            domain,
            &mut frag_buf,
            &mut frag_opcode,
        ) {
            ws_ctx.session.record_ws_frame(&FlowWsFrame {
                domain,
                direction,
                is_text,
                payload: &payload,
            });
        }

        match forward_ws_frame(writer, &frame).await {
            Ok(n) => bytes += n,
            Err(e) => {
                warn!("[{domain}] WS {direction_label} write error: {e:#}");
                break;
            }
        }

        if frame.is_close() {
            break;
        }
    }
    let _ = writer.shutdown().await;
    bytes
}

fn opcode_name(opcode: u8) -> &'static str {
    match opcode {
        0x0 => "continuation",
        0x1 => "text",
        0x2 => "binary",
        0x8 => "close",
        0x9 => "ping",
        0xA => "pong",
        _ => "unknown",
    }
}

/// Inspect an incoming frame, handling defragmentation and (if negotiated)
/// permessage-deflate decompression.
///
/// Returns `Some((is_text, payload))` once a complete message (single frame or
/// reassembled fragments) is available; `None` while a fragmented message is
/// still being accumulated or the frame is a control frame.
fn process_ws_frame(
    frame: &WsFrame,
    ws_compressed: bool,
    domain: &str,
    frag_buf: &mut Vec<u8>,
    frag_opcode: &mut u8,
) -> Option<(bool, Vec<u8>)> {
    if frame.is_text() || frame.is_binary() {
        if frame.fin {
            debug!(
                "[{domain}] WS inspect: complete {} frame, {} bytes (rsv1={})",
                opcode_name(frame.opcode),
                frame.payload.len(),
                frame.rsv1
            );
            let payload = maybe_decompress_ws(&frame.payload, ws_compressed && frame.rsv1, domain);
            Some((frame.is_text(), payload))
        } else {
            *frag_opcode = frame.opcode;
            frag_buf.clear();
            frag_buf.extend_from_slice(&frame.payload);
            debug!(
                "[{domain}] WS inspect: start fragmented {} message, first chunk {} bytes",
                opcode_name(frame.opcode),
                frame.payload.len()
            );
            None
        }
    } else if frame.is_continuation() {
        frag_buf.extend_from_slice(&frame.payload);
        if frame.fin {
            debug!(
                "[{domain}] WS inspect: fragmented message complete, total {} bytes",
                frag_buf.len()
            );
            let payload = maybe_decompress_ws(frag_buf, ws_compressed, domain);
            let is_text = *frag_opcode == 0x1;
            frag_buf.clear();
            *frag_opcode = 0;
            Some((is_text, payload))
        } else {
            debug!(
                "[{domain}] WS inspect: continuation chunk {} bytes, accumulated {} bytes",
                frame.payload.len(),
                frag_buf.len()
            );
            None
        }
    } else {
        None
    }
}

fn maybe_decompress_ws(data: &[u8], compressed: bool, domain: &str) -> Vec<u8> {
    if !compressed || data.is_empty() {
        return data.to_vec();
    }

    let mut input = data.to_vec();
    input.extend_from_slice(&[0x00, 0x00, 0xff, 0xff]);

    use std::io::Read;
    let mut decoder = flate2::read::DeflateDecoder::new(input.as_slice());
    let mut output = Vec::with_capacity(data.len() * 3);
    match decoder.read_to_end(&mut output) {
        Ok(_) => {
            debug!(
                "[{domain}] WS decompress: {} -> {} bytes",
                data.len(),
                output.len()
            );
            output
        }
        Err(e) => {
            warn!("[{domain}] WS decompress failed: {e}, using raw payload");
            data.to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use tokio::io::AsyncWrite;
    use tokio::io::BufReader;

    use crate::http::websocket::WsFrame;
    use crate::sink::test_support::CollectingSink;

    use super::{pump_ws_direction, MitmSession, WsDirection, WsRelayContext};

    fn build_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let byte0 = if fin { 0x80 | opcode } else { opcode };
        buf.push(byte0);
        buf.push(payload.len() as u8);
        buf.extend_from_slice(payload);
        buf
    }

    #[derive(Default)]
    struct VecWriter {
        buf: Vec<u8>,
    }

    impl AsyncWrite for VecWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.buf.extend_from_slice(data);
            Poll::Ready(Ok(data.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn make_session() -> (MitmSession, Arc<CollectingSink>) {
        let sink = Arc::new(CollectingSink::default());
        let session = MitmSession::new_for_test(
            "api.example.com".to_string(),
            "session-1".to_string(),
            sink.clone(),
        );
        (session, sink)
    }

    fn make_ws_context(session: MitmSession) -> WsRelayContext {
        WsRelayContext {
            session,
            ws_compressed: false,
        }
    }

    #[tokio::test]
    async fn pump_ws_direction_forwards_frame_and_calls_sink() {
        let (session, sink) = make_session();
        let payload = br#"{"hello":"world"}"#;
        let raw = build_frame(true, 0x1, payload);
        let mut reader = BufReader::new(raw.as_slice());
        let mut writer = VecWriter::default();

        let bytes = pump_ws_direction(
            &mut reader,
            &mut writer,
            &make_ws_context(session),
            WsDirection::ClientToServer,
        )
        .await;

        assert_eq!(bytes as usize, raw.len());
        assert_eq!(writer.buf, raw);
        let frames = sink.ws_frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert!(frames[0].2); // is_text
        assert_eq!(frames[0].3, payload);
    }

    #[test]
    fn process_ws_frame_returns_payload_for_single_text_frame() {
        let frame = WsFrame {
            fin: true,
            rsv1: false,
            opcode: 0x1,
            payload: br#"{"hello":"world"}"#.to_vec(),
            raw: Vec::new(),
        };
        let mut frag_opcode = 0;
        let mut frag_buf = Vec::new();

        let (is_text, payload) = super::process_ws_frame(
            &frame,
            false,
            "api.example.com",
            &mut frag_buf,
            &mut frag_opcode,
        )
        .unwrap();

        assert!(is_text);
        assert_eq!(payload, br#"{"hello":"world"}"#);
    }

    #[test]
    fn process_ws_frame_reassembles_fragmented_message() {
        let first = WsFrame {
            fin: false,
            rsv1: false,
            opcode: 0x1,
            payload: b"hel".to_vec(),
            raw: Vec::new(),
        };
        let last = WsFrame {
            fin: true,
            rsv1: false,
            opcode: 0x0,
            payload: b"lo".to_vec(),
            raw: Vec::new(),
        };
        let mut frag_opcode = 0;
        let mut frag_buf = Vec::new();

        let first_result = super::process_ws_frame(
            &first,
            false,
            "api.example.com",
            &mut frag_buf,
            &mut frag_opcode,
        );
        assert!(first_result.is_none());

        let (is_text, payload) = super::process_ws_frame(
            &last,
            false,
            "api.example.com",
            &mut frag_buf,
            &mut frag_opcode,
        )
        .unwrap();
        assert!(is_text);
        assert_eq!(payload, b"hello");
    }
}
