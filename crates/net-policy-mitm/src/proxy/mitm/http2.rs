use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use h2::server::SendResponse;
use http::{Request, Response, StatusCode, Uri, Version};
use log::{info, warn};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::sink::{FlowRequest, FlowResponse};

use super::session::MitmSession;

const MAX_H2_BODY_SIZE: usize = 50 * 1024 * 1024;

pub(super) async fn relay_h2<ClientTls, UpstreamTls>(
    client_tls: ClientTls,
    upstream_tls: UpstreamTls,
    session: &MitmSession,
) -> Result<(u64, u64)>
where
    ClientTls: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    UpstreamTls: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    info!("[{}] Starting HTTP/2 interception", session.domain);

    let mut server = h2::server::handshake(client_tls)
        .await
        .context("HTTP/2 handshake with client")?;
    let (send_request, connection) = h2::client::handshake(upstream_tls)
        .await
        .context("HTTP/2 handshake with upstream")?;

    let domain_owned = session.domain.clone();
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            warn!(
                "[{}] Upstream HTTP/2 connection ended: {err:#}",
                domain_owned
            );
        }
    });

    let up_bytes = Arc::new(AtomicU64::new(0));
    let down_bytes = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();

    while let Some(result) = server.accept().await {
        let (request, respond) = match result {
            Ok(stream) => stream,
            Err(err) => {
                warn!("[{}] Failed to accept H2 stream: {err:#}", session.domain);
                continue;
            }
        };

        let sender = send_request.clone();
        let session = session.clone();
        let up_bytes = Arc::clone(&up_bytes);
        let down_bytes = Arc::clone(&down_bytes);
        let domain = session.domain.clone();

        handles.push(tokio::spawn(async move {
            match handle_h2_stream(request, respond, sender, &session).await {
                Ok((stream_up, stream_down)) => {
                    up_bytes.fetch_add(stream_up, Ordering::Relaxed);
                    down_bytes.fetch_add(stream_down, Ordering::Relaxed);
                }
                Err(err) => {
                    warn!("[{domain}] H2 stream error: {err:#}");
                }
            }
        }));
    }

    for handle in handles {
        if let Err(err) = handle.await {
            warn!("[{}] H2 stream task join error: {err}", session.domain);
        }
    }

    Ok((
        up_bytes.load(Ordering::Relaxed),
        down_bytes.load(Ordering::Relaxed),
    ))
}

async fn handle_h2_stream(
    request: Request<h2::RecvStream>,
    mut respond: SendResponse<Bytes>,
    send_request: h2::client::SendRequest<Bytes>,
    session: &MitmSession,
) -> Result<(u64, u64)> {
    let domain = &session.domain;
    let method = request.method().to_string();
    let path = request
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let headers = h2_headers_to_vec(request.headers());
    let uri = build_h2_upstream_uri(domain, &path)?;
    let (parts, mut body_stream) = request.into_parts();
    let request_body = read_h2_body(&mut body_stream)
        .await
        .context("read HTTP/2 request body")?;

    info!("[{domain}] H2 Request: {} {}", method, path);

    session.record_request(&FlowRequest {
        domain,
        method: &method,
        path: &path,
        version: "HTTP/2.0",
        headers: &headers,
        body: &request_body,
    });

    let upstream_request = rebuild_h2_request(parts, uri, &headers)?;
    let (response_future, mut send_stream) = {
        let mut ready = send_request
            .clone()
            .ready()
            .await
            .context("HTTP/2 upstream not ready")?;
        ready
            .send_request(upstream_request, request_body.is_empty())
            .context("send HTTP/2 request upstream")?
    };

    let mut up_bytes = 0u64;
    let mut down_bytes = 0u64;
    if !request_body.is_empty() {
        up_bytes += request_body.len() as u64;
        send_stream
            .send_data(Bytes::from(request_body), true)
            .context("send HTTP/2 request body upstream")?;
    }

    let response = response_future.await.context("await HTTP/2 response")?;
    let response_status = response.status();
    let response_version = response.version();
    let (response_parts, mut response_body_stream) = response.into_parts();
    let response_body = read_h2_body(&mut response_body_stream)
        .await
        .context("read HTTP/2 response body")?;
    down_bytes += response_body.len() as u64;

    info!(
        "[{domain}] H2 Request: {}  Response: {} {} ({} bytes body)",
        path,
        response_status.as_u16(),
        response_status.canonical_reason().unwrap_or(""),
        response_body.len()
    );

    let response_headers = h2_headers_to_vec(&response_parts.headers);
    session.record_response(&FlowResponse {
        domain,
        status: response_status.as_u16(),
        version: &format!("{:?}", response_version),
        headers: &response_headers,
        body: &response_body,
    });

    let client_response = rebuild_h2_response(response_parts, response_status, response_version)?;
    let mut send_body = respond
        .send_response(client_response, response_body.is_empty())
        .context("send HTTP/2 response headers to client")?;
    if !response_body.is_empty() {
        send_body
            .send_data(Bytes::from(response_body), true)
            .context("send HTTP/2 response body to client")?;
    }

    Ok((up_bytes, down_bytes))
}

async fn read_h2_body(stream: &mut h2::RecvStream) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk.context("read HTTP/2 data frame")?;
        if body.len().saturating_add(chunk.len()) > MAX_H2_BODY_SIZE {
            anyhow::bail!("HTTP/2 body exceeds {MAX_H2_BODY_SIZE} bytes");
        }
        body.extend_from_slice(&chunk);
        stream
            .flow_control()
            .release_capacity(chunk.len())
            .context("release HTTP/2 flow-control capacity")?;
    }
    Ok(body)
}

fn build_h2_upstream_uri(domain: &str, path: &str) -> Result<Uri> {
    format!("https://{domain}{path}")
        .parse()
        .context("build HTTP/2 upstream URI")
}

fn rebuild_h2_request(
    parts: http::request::Parts,
    uri: Uri,
    headers: &[(String, String)],
) -> Result<Request<()>> {
    let mut builder = Request::builder()
        .method(parts.method)
        .uri(uri)
        .version(Version::HTTP_2);
    for (name, value) in headers {
        if is_hop_by_hop_header(name) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder.body(()).context("build HTTP/2 request")
}

fn rebuild_h2_response(
    parts: http::response::Parts,
    status: StatusCode,
    version: Version,
) -> Result<Response<()>> {
    let mut builder = Response::builder().status(status).version(version);
    for (name, value) in &parts.headers {
        builder = builder.header(name, value);
    }
    builder.body(()).context("build HTTP/2 response")
}

fn h2_headers_to_vec(headers: &http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection" | "proxy-connection" | "keep-alive" | "transfer-encoding" | "upgrade"
    )
}
