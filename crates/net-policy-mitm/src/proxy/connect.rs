use anyhow::{bail, Context, Result};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

const MAX_CONNECT_LINE: usize = 8 * 1024;
const MAX_CONNECT_HEADERS: usize = 32 * 1024;

/// A parsed CONNECT request.
#[derive(Debug)]
pub struct ConnectRequest {
    pub host: String,
    pub port: u16,
    pub proxy_authorization: Option<String>,
}

/// Read and parse a CONNECT request from the client stream.
///
/// Expected format:
/// ```text
/// CONNECT host:port HTTP/1.1\r\n
/// Host: host:port\r\n
/// \r\n
/// ```
///
/// Returns an error for non-CONNECT methods or malformed requests.
pub async fn parse_connect_request(stream: &mut TcpStream) -> Result<ConnectRequest> {
    // 逐字节读，既能硬限制内存，也避免 BufReader 预读并在 drop 时吞掉紧随 headers 的 TLS 字节。
    let request_line = read_limited_line(stream, MAX_CONNECT_LINE)
        .await
        .context("Reading CONNECT request line")?;

    let parts: Vec<&str> = request_line.trim_end().splitn(3, ' ').collect();
    if parts.len() < 3 {
        bail!("Malformed request line: {request_line}");
    }

    let method = parts[0];
    if method != "CONNECT" {
        bail!("Unsupported method: {method} (only CONNECT is supported)");
    }

    let authority = parts[1];
    let (host, port) = parse_authority(authority)?;

    // Consume remaining headers until \r\n\r\n
    let mut header_bytes = 0usize;
    let mut proxy_authorization = None;
    loop {
        let line = read_limited_line(stream, MAX_CONNECT_LINE).await?;
        header_bytes = header_bytes.saturating_add(line.len());
        if header_bytes > MAX_CONNECT_HEADERS {
            bail!("CONNECT headers exceed {MAX_CONNECT_HEADERS} bytes");
        }
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            if name.eq_ignore_ascii_case("proxy-authorization") {
                proxy_authorization = Some(value.trim().to_string());
            }
        }
    }

    Ok(ConnectRequest {
        host,
        port,
        proxy_authorization,
    })
}

async fn read_limited_line(stream: &mut TcpStream, limit: usize) -> Result<String> {
    let mut bytes = Vec::with_capacity(128);
    loop {
        let byte = stream.read_u8().await.context("read CONNECT line")?;
        bytes.push(byte);
        if bytes.len() > limit {
            bail!("CONNECT line exceeds {limit} bytes");
        }
        if byte == b'\n' {
            break;
        }
    }
    String::from_utf8(bytes).context("CONNECT line is not UTF-8")
}

/// Parse "host:port" authority string.
fn parse_authority(authority: &str) -> Result<(String, u16)> {
    // Handle IPv6 addresses like [::1]:443
    if let Some(bracket_end) = authority.find("]:") {
        let host = authority[..=bracket_end].to_string();
        let port: u16 = authority[bracket_end + 2..]
            .parse()
            .with_context(|| format!("Invalid port in authority: {authority}"))?;
        return Ok((host, port));
    }

    let (host, port_str) = authority
        .rsplit_once(':')
        .with_context(|| format!("Missing port in authority: {authority}"))?;
    let port: u16 = port_str
        .parse()
        .with_context(|| format!("Invalid port in authority: {authority}"))?;
    Ok((host.to_string(), port))
}

/// The HTTP response to send when the method is not CONNECT.
pub const METHOD_NOT_ALLOWED: &[u8] = b"HTTP/1.1 405 Method Not Allowed\r\n\r\n";
pub const PROXY_AUTH_REQUIRED: &[u8] =
    b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"net-policy\"\r\n\r\n";
/// The HTTP response to send when the upstream connection fails.
pub const BAD_GATEWAY: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\n\r\n";
/// The HTTP response to send on successful tunnel establishment.
pub const CONNECTION_ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_authority_standard() {
        let (host, port) = parse_authority("api.openai.com:443").unwrap();
        assert_eq!(host, "api.openai.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_parse_authority_non_standard_port() {
        let (host, port) = parse_authority("example.com:8443").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8443);
    }

    #[test]
    fn test_parse_authority_missing_port() {
        assert!(parse_authority("example.com").is_err());
    }

    #[test]
    fn test_parse_authority_invalid_port() {
        assert!(parse_authority("example.com:abc").is_err());
    }

    #[tokio::test]
    async fn test_parse_connect_via_tcp() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client_handle = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"CONNECT httpbin.org:443 HTTP/1.1\r\nHost: httpbin.org:443\r\n\r\n")
                .await
                .unwrap();
        });

        let (mut server_stream, _) = listener.accept().await.unwrap();
        let req = parse_connect_request(&mut server_stream).await.unwrap();
        assert_eq!(req.host, "httpbin.org");
        assert_eq!(req.port, 443);
        assert!(req.proxy_authorization.is_none());

        client_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_parse_proxy_authorization() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(
                    b"CONNECT example.com:443 HTTP/1.1\r\nProxy-Authorization: Basic dTpw\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = parse_connect_request(&mut stream).await.unwrap();
        assert_eq!(request.proxy_authorization.as_deref(), Some("Basic dTpw"));
    }

    #[tokio::test]
    async fn test_reject_non_connect() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n")
                .await
                .unwrap();
        });

        let (mut server_stream, _) = listener.accept().await.unwrap();
        let result = parse_connect_request(&mut server_stream).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unsupported method"));
    }
}
