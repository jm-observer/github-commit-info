use anyhow::{bail, Context, Result};
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Connect to target through an HTTP CONNECT proxy.
/// Sends the domain name directly — the proxy handles DNS resolution.
pub async fn connect(
    proxy_addr: SocketAddr,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(proxy_addr)
        .await
        .with_context(|| format!("TCP connect to HTTP proxy {proxy_addr}"))?;

    let request = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;

    // Read the response status line
    let mut reader = BufReader::new(&mut stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).await?;

    // Expect "HTTP/1.x 200 ..."
    let parts: Vec<&str> = status_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        bail!("Invalid HTTP CONNECT response: {status_line}");
    }
    let status_code: u16 = parts[1]
        .parse()
        .with_context(|| format!("Invalid status code in: {status_line}"))?;
    if status_code != 200 {
        bail!(
            "HTTP CONNECT proxy returned {status_code} for {target_host}:{target_port}: {status_line}"
        );
    }

    // Consume remaining headers until empty line
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
    }

    // Return the raw stream (BufReader may have buffered data, but for CONNECT
    // the proxy should not send anything beyond the response headers)
    drop(reader);
    Ok(stream)
}
