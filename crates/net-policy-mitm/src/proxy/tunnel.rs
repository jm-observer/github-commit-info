use anyhow::Result;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;

/// Bidirectionally relay data between client and upstream streams.
pub async fn relay(client: &mut TcpStream, upstream: &mut TcpStream) -> Result<(u64, u64)> {
    let (client_to_upstream, upstream_to_client) = copy_bidirectional(client, upstream).await?;
    Ok((client_to_upstream, upstream_to_client))
}
