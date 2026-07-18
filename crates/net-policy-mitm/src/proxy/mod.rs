pub mod connect;
pub mod mitm;
pub mod tunnel;

use std::sync::Arc;

use anyhow::Result;
use log::{debug, error, info, warn};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio::time::{sleep, Duration};

use crate::cert::site::CertCache;
use crate::shutdown::ShutdownToken;
use crate::sink::FlowSink;
use crate::upstream::Upstream;
use connect::{
    parse_connect_request, BAD_GATEWAY, CONNECTION_ESTABLISHED, METHOD_NOT_ALLOWED,
    PROXY_AUTH_REQUIRED,
};

/// Shared runtime handed to every proxy connection.
///
/// This replaces `system-prompt-show`'s `ProxyRuntime` (which carried
/// `capture_tx`/`registry`/`fixture_dumper`/`storage_tx` for system-prompt
/// extraction and SQLite persistence). Here it is reduced to the generic
/// pieces the MITM engine actually needs: the certificate cache, a
/// [`FlowSink`] callback for decrypted traffic, and a per-domain interception
/// policy (replacing the old hardcoded `should_parse_http`).
#[derive(Clone)]
pub struct ProxyRuntime {
    pub cert_cache: Arc<CertCache>,
    pub sink: Arc<dyn FlowSink>,
    /// Domain -> whether to MITM-intercept (false = plain TCP tunnel passthrough).
    pub should_intercept: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// 完整的 Proxy-Authorization 值；`Some` 时不匹配即 407，防止其它本地进程借代理绕过策略。
    pub expected_proxy_authorization: Option<String>,
}

/// Run the proxy server: accept connections and handle CONNECT + MITM TLS.
pub async fn run_proxy(
    host: &str,
    port: u16,
    upstream: Arc<Upstream>,
    runtime: ProxyRuntime,
    shutdown: ShutdownToken,
) -> Result<()> {
    let listener = TcpListener::bind((host, port)).await?;
    run_proxy_on(listener, upstream, runtime, shutdown).await
}

/// 使用调用方已成功 bind 的 listener 运行代理；用于需要在返回“会话已启动”前确认端口可用的编排。
pub async fn run_proxy_on(
    listener: TcpListener,
    upstream: Arc<Upstream>,
    runtime: ProxyRuntime,
    shutdown: ShutdownToken,
) -> Result<()> {
    info!("Proxy listening on {}", listener.local_addr()?);

    let mut connections = JoinSet::new();

    let mut shutdown_rx = shutdown.cancelled();

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if shutdown.is_cancelled() {
                    info!("proxy shutdown requested, stopping accept loop");
                    break;
                }
            }

            result = listener.accept() => {
                let (client_stream, client_addr) = match result {
                    Ok(conn) => conn,
                    Err(err) => {
                        error!("Failed to accept connection: {err}");
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };

                if shutdown.is_cancelled() {
                    info!("proxy shutting down, rejecting new connection from {client_addr}");
                    break;
                }

                let upstream = upstream.clone();
                let runtime = runtime.clone();
                let child_shutdown = shutdown.child_token();

                connections.spawn(async move {
                    handle_connection(client_stream, client_addr, upstream, runtime, child_shutdown).await;
                });
            }
        }
    }

    info!(
        "proxy accept loop exited, cancelling {} active connections",
        connections.len()
    );
    connections.abort_all();

    let mut had_error = false;
    while let Some(result) = connections.join_next().await {
        match result {
            Ok(()) => {}
            Err(err) => {
                error!("proxy connection task error: {err:#}");
                had_error = true;
            }
        }
    }

    if had_error {
        Err(anyhow::anyhow!("proxy connection tasks had errors"))
    } else {
        Ok(())
    }
}

async fn handle_connection(
    mut client_stream: tokio::net::TcpStream,
    client_addr: std::net::SocketAddr,
    upstream: Arc<Upstream>,
    runtime: ProxyRuntime,
    _shutdown: ShutdownToken,
) {
    // 1. Parse CONNECT request
    let req = match parse_connect_request(&mut client_stream).await {
        Ok(req) => req,
        Err(e) => {
            error!("[{client_addr}] Failed to parse request: {e}");
            let _ = client_stream.write_all(METHOD_NOT_ALLOWED).await;
            return;
        }
    };

    info!("[{client_addr}] CONNECT {}:{}", req.host, req.port);

    if runtime.expected_proxy_authorization.as_deref() != req.proxy_authorization.as_deref() {
        warn!("[{client_addr}] rejected unauthenticated CONNECT");
        let _ = client_stream.write_all(PROXY_AUTH_REQUIRED).await;
        return;
    }

    // 2. Connect through upstream proxy (passes domain name, no DNS resolution)
    let mut upstream_stream = match upstream.connect(&req.host, req.port).await {
        Ok(s) => s,
        Err(e) => {
            error!(
                "[{client_addr}] Upstream connect to {}:{} failed: {e}",
                req.host, req.port
            );
            let _ = client_stream.write_all(BAD_GATEWAY).await;
            return;
        }
    };

    // 3. Send 200 Connection Established to client
    if let Err(e) = client_stream.write_all(CONNECTION_ESTABLISHED).await {
        error!("[{client_addr}] Failed to send 200 response: {e}");
        return;
    }

    // 4. Non-intercepted domains: plain TCP tunnel (no TLS interception)
    if !(runtime.should_intercept)(&req.host) {
        debug!(
            "[{client_addr}] {} — not intercepted, plain TCP tunnel",
            req.host
        );
        // Honest per-domain audit (§17.9): relayed opaque, no plaintext produced.
        runtime.sink.on_passthrough(&req.host);
        match tunnel::relay(&mut client_stream, &mut upstream_stream).await {
            Ok((up, down)) => {
                debug!(
                    "[{client_addr}] Tunnel closed {}:{} (up: {up}B, down: {down}B)",
                    req.host, req.port
                );
            }
            Err(e) => {
                debug!(
                    "[{client_addr}] Tunnel ended {}:{}: {e:#}",
                    req.host, req.port
                );
            }
        }
        return;
    }

    // 5. Intercepted domains: MITM TLS interception
    // sink 在 handle_mitm 拿走 runtime 所有权前克隆，供拒证审计回调用（§17.7）。
    let sink = runtime.sink.clone();
    match mitm::handle_mitm(
        client_stream,
        upstream_stream,
        &req.host,
        runtime,
        false, // Allow H2 fallback (relay-only) to fix TLS EOF issues
    )
    .await
    {
        Ok((up, down)) => {
            info!(
                "[{client_addr}] MITM closed {}:{} (up: {up}B, down: {down}B)",
                req.host, req.port
            );
        }
        Err(e) => {
            let err_msg = format!("{e:#}");
            // Client refused our forged leaf cert: pinning / bundled CA / mTLS
            // (§17.7). These are TLS alerts the client sends back during our
            // server-side handshake. Report honestly — never claim decryption.
            let is_cert_rejected = err_msg.contains("CertificateUnknown")
                || err_msg.contains("BadCertificate")
                || err_msg.contains("UnknownCA")
                || err_msg.contains("unknown_ca")
                || err_msg.contains("bad_certificate")
                || err_msg.contains("certificate_unknown")
                || err_msg.contains("AccessDenied")
                || err_msg.contains("access_denied")
                || err_msg.contains("HandshakeFailure")
                || err_msg.contains("handshake_failure");
            if is_cert_rejected {
                sink.on_client_cert_rejected(&req.host);
            }
            let is_benign = e.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .map(|io_err| {
                        matches!(
                            io_err.kind(),
                            std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::UnexpectedEof
                        )
                    })
                    .unwrap_or(false)
            }) || err_msg.contains("close_notify")
                || err_msg.contains("handshake eof")
                || err_msg.contains("tls handshake eof")
                || is_cert_rejected;

            if is_benign {
                warn!(
                    "[{client_addr}] MITM peer disconnected {}:{}: {e:#}",
                    req.host, req.port
                );
            } else {
                error!(
                    "[{client_addr}] MITM ended {}:{}: {e:#}",
                    req.host, req.port
                );
            }
        }
    }
}
