use anyhow::{Context, Result};
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;

/// Connect to target through a SOCKS5 proxy.
/// Uses SOCKS5 domain-name mode (ATYP=0x03) so the proxy handles DNS resolution.
pub async fn connect(
    proxy_addr: SocketAddr,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let stream = Socks5Stream::connect(proxy_addr, (target_host, target_port))
        .await
        .with_context(|| {
            format!("SOCKS5 connect to {target_host}:{target_port} via {proxy_addr}")
        })?;
    Ok(stream.into_inner())
}
