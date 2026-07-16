use std::time::Duration;

use anyhow::{Context, Result};
use log::{info, warn};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;

use crate::http::body::is_sse;
use crate::http::request::{read_request, ParsedRequest};
use crate::http::response::{read_response, ParsedResponse};
use crate::sink::{FlowRequest, FlowResponse};

use super::protocol::looks_like_http1;
use super::relay::relay_plaintext_with_bufreaders;
use super::session::MitmSession;
use super::util::peek_buffer_preview;
use super::websocket::{relay_websocket, WsRelayContext};

pub(super) async fn relay_http1<ClientTls, UpstreamTls>(
    client_tls: ClientTls,
    upstream_tls: UpstreamTls,
    session: &MitmSession,
) -> Result<(u64, u64)>
where
    ClientTls: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    UpstreamTls: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (client_read, client_write) = tokio::io::split(client_tls);
    let (upstream_read, upstream_write) = tokio::io::split(upstream_tls);

    let mut client_reader = BufReader::new(client_read);
    let upstream_reader = BufReader::new(upstream_read);
    let client_writer = client_write;
    let upstream_writer = upstream_write;

    info!("[{}] Starting HTTP/1.1 interception", session.domain);

    match looks_like_http1(&mut client_reader).await {
        Ok(Some(true)) => {
            info!("[{}] Confirmed HTTP/1.1 traffic", session.domain);
        }
        Ok(Some(false)) => {
            info!(
                "[{}] Stream looks like HTTP/2 or other protocol, falling back to relay",
                session.domain
            );
            return relay_plaintext_with_bufreaders(
                client_reader,
                client_writer,
                upstream_reader,
                upstream_writer,
            )
            .await;
        }
        Ok(None) => {
            info!(
                "[{}] Stream reached EOF during protocol detection",
                session.domain
            );
            return Ok((0, 0));
        }
        Err(e) => {
            warn!(
                "[{}] Failed to peek stream: {e:#}, falling back to relay",
                session.domain
            );
            return relay_plaintext_with_bufreaders(
                client_reader,
                client_writer,
                upstream_reader,
                upstream_writer,
            )
            .await;
        }
    }

    let mut loop_ctrl = Http1Loop {
        session,
        client_reader,
        client_writer,
        upstream_reader,
        upstream_writer,
        up_bytes: 0,
        down_bytes: 0,
        msg_count: 0,
    };

    loop {
        let current_up = loop_ctrl.up_bytes;
        let current_down = loop_ctrl.down_bytes;
        match loop_ctrl.pump_one_request().await? {
            StepOutcome::Continue(new_loop) => {
                loop_ctrl = new_loop;
                continue;
            }
            StepOutcome::SwitchedWebSocket(handoff) => {
                let (ws_up, ws_down) = handoff.run().await?;
                return Ok((current_up + ws_up, current_down + ws_down));
            }
            StepOutcome::FallbackToPlaintext(l) => {
                let (fallback_up, fallback_down) = relay_plaintext_with_bufreaders(
                    l.client_reader,
                    l.client_writer,
                    l.upstream_reader,
                    l.upstream_writer,
                )
                .await?;
                return Ok((l.up_bytes + fallback_up, l.down_bytes + fallback_down));
            }
            StepOutcome::Done(up, down) => return Ok((up, down)),
        }
    }
}

struct Http1Loop<'a, CR, CW, UR, UW> {
    session: &'a MitmSession,
    client_reader: BufReader<CR>,
    client_writer: CW,
    upstream_reader: BufReader<UR>,
    upstream_writer: UW,
    up_bytes: u64,
    down_bytes: u64,
    msg_count: u32,
}

enum StepOutcome<'a, CR, CW, UR, UW>
where
    CR: AsyncRead + Unpin + Send + 'static,
    CW: AsyncWrite + Unpin + Send + 'static,
    UR: AsyncRead + Unpin + Send + 'static,
    UW: AsyncWrite + Unpin + Send + 'static,
{
    Continue(Http1Loop<'a, CR, CW, UR, UW>),
    SwitchedWebSocket(WsRelayHandoff<CR, CW, UR, UW>),
    FallbackToPlaintext(Http1Loop<'a, CR, CW, UR, UW>),
    Done(u64, u64),
}

struct WsRelayHandoff<CR, CW, UR, UW> {
    client_reader: BufReader<CR>,
    client_writer: CW,
    upstream_reader: BufReader<UR>,
    upstream_writer: UW,
    ws_ctx: WsRelayContext,
}

impl<CR, CW, UR, UW> WsRelayHandoff<CR, CW, UR, UW>
where
    CR: AsyncRead + Unpin + Send + 'static,
    CW: AsyncWrite + Unpin + Send + 'static,
    UR: AsyncRead + Unpin + Send + 'static,
    UW: AsyncWrite + Unpin + Send + 'static,
{
    async fn run(self) -> Result<(u64, u64)> {
        relay_websocket(
            self.client_reader,
            self.client_writer,
            self.upstream_reader,
            self.upstream_writer,
            self.ws_ctx,
        )
        .await
    }
}

impl<'a, CR, CW, UR, UW> Http1Loop<'a, CR, CW, UR, UW>
where
    CR: AsyncRead + Unpin + Send + 'static,
    CW: AsyncWrite + Unpin + Send + 'static,
    UR: AsyncRead + Unpin + Send + 'static,
    UW: AsyncWrite + Unpin + Send + 'static,
{
    async fn pump_one_request(mut self) -> Result<StepOutcome<'a, CR, CW, UR, UW>> {
        let domain = &self.session.domain;

        match looks_like_http1(&mut self.client_reader).await {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                let preview = peek_buffer_preview(&self.client_reader, 32);
                warn!(
                    "[{domain}] Stream switched to non-HTTP/1.1 protocol after {} requests, first bytes: {:?}, falling back to relay",
                    self.msg_count / 2,
                    preview
                );
                return Ok(StepOutcome::FallbackToPlaintext(self));
            }
            Ok(None) | Err(_) => {
                let _ = self.upstream_writer.shutdown().await;
                return Ok(StepOutcome::Done(self.up_bytes, self.down_bytes));
            }
        }

        const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
        let request = match timeout(REQUEST_TIMEOUT, read_request(&mut self.client_reader)).await {
            Ok(Ok(Some(request))) => request,
            Ok(Ok(None)) => {
                info!("[{domain}] Client closed connection (EOF)");
                let _ = self.upstream_writer.shutdown().await;
                return Ok(StepOutcome::Done(self.up_bytes, self.down_bytes));
            }
            Ok(Err(err)) => {
                warn!("[{domain}] HTTP parse failed: {err:#}, falling back to relay");
                return Ok(StepOutcome::FallbackToPlaintext(self));
            }
            Err(_elapsed) => {
                warn!("[{domain}] Client request read timed out after {REQUEST_TIMEOUT:?}");
                let _ = self.upstream_writer.shutdown().await;
                return Ok(StepOutcome::Done(self.up_bytes, self.down_bytes));
            }
        };

        info!("[{domain}] Request: {} {}", request.method, request.path);

        self.session.record_request(&FlowRequest {
            domain,
            method: &request.method,
            path: &request.path,
            version: &request.version,
            headers: &request.headers,
            body: &request.body,
        });
        self.msg_count += 1;

        self.up_bytes += forward_request(&mut self.upstream_writer, &request)
            .await
            .context("forward request to upstream")?;

        const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);
        let response =
            match timeout(RESPONSE_TIMEOUT, read_response(&mut self.upstream_reader)).await {
                Ok(result) => result.context("read upstream HTTP response")?,
                Err(_elapsed) => {
                    warn!("[{domain}] Upstream response read timed out after {RESPONSE_TIMEOUT:?}");
                    return Ok(StepOutcome::Done(self.up_bytes, self.down_bytes));
                }
            };

        info!(
            "[{domain}] Request: {}  Response: {} {} ({} bytes body)",
            request.path,
            response.status_code,
            response.reason_phrase,
            response.body.len()
        );

        self.session.record_response(&FlowResponse {
            domain,
            status: response.status_code,
            version: &response.version,
            headers: &response.headers,
            body: &response.body,
        });
        self.msg_count += 1;

        if response.status_code == 101 {
            let ws_compressed = response.headers.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case("Sec-WebSocket-Extensions")
                    && v.to_ascii_lowercase().contains("permessage-deflate")
            });
            info!("[{domain}] 101 Switching Protocols — switching to WebSocket frame parsing (compressed={ws_compressed})");

            self.down_bytes += forward_response(&mut self.client_writer, &response)
                .await
                .context("forward 101 response to client")?;

            return Ok(StepOutcome::SwitchedWebSocket(WsRelayHandoff {
                client_reader: self.client_reader,
                client_writer: self.client_writer,
                upstream_reader: self.upstream_reader,
                upstream_writer: self.upstream_writer,
                ws_ctx: WsRelayContext {
                    session: self.session.clone(),
                    ws_compressed,
                },
            }));
        }

        self.down_bytes += forward_response(&mut self.client_writer, &response)
            .await
            .context("forward response to client")?;

        if is_sse(&response.headers) {
            info!(
                "[{domain}] SSE stream response ({} bytes body), continuing request loop",
                response.body.len()
            );
        }

        if should_close(&request.headers) || should_close(&response.headers) {
            let _ = self.client_writer.shutdown().await;
            return Ok(StepOutcome::Done(self.up_bytes, self.down_bytes));
        }

        Ok(StepOutcome::Continue(self))
    }
}

async fn forward_request<W>(writer: &mut W, request: &ParsedRequest) -> Result<u64>
where
    W: AsyncWrite + Unpin,
{
    let mut written = 0u64;
    let start_line = format!(
        "{} {} {}\r\n",
        request.method, request.path, request.version
    );
    writer.write_all(start_line.as_bytes()).await?;
    written += start_line.len() as u64;

    for (name, value) in &request.headers {
        let line = format!("{name}: {value}\r\n");
        writer.write_all(line.as_bytes()).await?;
        written += line.len() as u64;
    }

    writer.write_all(b"\r\n").await?;
    written += 2;
    writer.write_all(&request.raw_body).await?;
    written += request.raw_body.len() as u64;
    writer.flush().await?;

    Ok(written)
}

async fn forward_response<W>(writer: &mut W, response: &ParsedResponse) -> Result<u64>
where
    W: AsyncWrite + Unpin,
{
    let mut written = 0u64;
    let start_line = format!(
        "{} {} {}\r\n",
        response.version, response.status_code, response.reason_phrase
    );
    writer.write_all(start_line.as_bytes()).await?;
    written += start_line.len() as u64;

    for (name, value) in &response.headers {
        let line = format!("{name}: {value}\r\n");
        writer.write_all(line.as_bytes()).await?;
        written += line.len() as u64;
    }

    writer.write_all(b"\r\n").await?;
    written += 2;
    writer.write_all(&response.raw_body).await?;
    written += response.raw_body.len() as u64;
    writer.flush().await?;

    Ok(written)
}

fn should_close(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("connection") && value.eq_ignore_ascii_case("close")
    })
}
