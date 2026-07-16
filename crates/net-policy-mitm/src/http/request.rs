use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use super::body::read_body;

#[derive(Debug, Clone)]
pub struct ParsedRequest {
    pub method: String,
    pub path: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub raw_body: Vec<u8>,
    pub body: Vec<u8>,
}

pub async fn read_request<R>(reader: &mut BufReader<R>) -> Result<Option<ParsedRequest>>
where
    R: AsyncRead + Unpin,
{
    let request_line = match read_line_allow_eof(reader).await? {
        Some(line) => line,
        None => return Ok(None),
    };
    if request_line == "\r\n" || request_line == "\n" {
        return Ok(None);
    }

    let (method, path, version) = parse_request_line(&request_line)?;
    let headers = read_headers(reader).await?;
    let (raw_body, body) = read_body(reader, &headers).await?;

    Ok(Some(ParsedRequest {
        method,
        path,
        version,
        headers,
        raw_body,
        body,
    }))
}

fn parse_request_line(line: &str) -> Result<(String, String, String)> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let mut parts = trimmed.splitn(3, ' ');
    let method = parts.next().context("missing request method")?;
    let path = parts.next().context("missing request path")?;
    let version = parts.next().context("missing request version")?;

    if !version.starts_with("HTTP/1.") {
        bail!("unsupported HTTP version: {version}");
    }

    Ok((method.to_string(), path.to_string(), version.to_string()))
}

async fn read_headers<R>(reader: &mut BufReader<R>) -> Result<Vec<(String, String)>>
where
    R: AsyncRead + Unpin,
{
    let mut headers = Vec::new();

    loop {
        let line = read_line_required(reader).await?;
        if line == "\r\n" || line == "\n" {
            break;
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        let (name, value) = trimmed
            .split_once(':')
            .with_context(|| format!("malformed header line: {trimmed}"))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }

    Ok(headers)
}

async fn read_line_allow_eof<R>(reader: &mut BufReader<R>) -> Result<Option<String>>
where
    R: AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(line))
}

async fn read_line_required<R>(reader: &mut BufReader<R>) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    read_line_allow_eof(reader)
        .await?
        .context("unexpected EOF while reading headers")
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn parses_request_with_content_length_body() {
        let raw = b"POST /v1/chat/completions HTTP/1.1\r\nHost: api.openai.com\r\nContent-Length: 5\r\n\r\nhello";
        let mut reader = BufReader::new(&raw[..]);

        let request = read_request(&mut reader).await.unwrap().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.body, b"hello");
        assert_eq!(request.raw_body, b"hello");
    }

    #[tokio::test]
    async fn parses_request_without_body() {
        let raw = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let mut reader = BufReader::new(&raw[..]);

        let request = read_request(&mut reader).await.unwrap().unwrap();
        assert_eq!(request.method, "GET");
        assert!(request.body.is_empty());
    }
}
