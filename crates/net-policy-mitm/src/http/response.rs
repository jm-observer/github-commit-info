use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use super::body::read_body;

#[derive(Debug, Clone)]
pub struct ParsedResponse {
    pub version: String,
    pub status_code: u16,
    pub reason_phrase: String,
    pub headers: Vec<(String, String)>,
    pub raw_body: Vec<u8>,
    pub body: Vec<u8>,
}

pub async fn read_response<R>(reader: &mut BufReader<R>) -> Result<ParsedResponse>
where
    R: AsyncRead + Unpin,
{
    let status_line = read_line_required(reader).await?;
    let (version, status_code, reason_phrase) = parse_status_line(&status_line)?;
    let headers = read_headers(reader).await?;
    let (raw_body, body) = read_body(reader, &headers).await?;

    Ok(ParsedResponse {
        version,
        status_code,
        reason_phrase,
        headers,
        raw_body,
        body,
    })
}

fn parse_status_line(line: &str) -> Result<(String, u16, String)> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let mut parts = trimmed.splitn(3, ' ');
    let version = parts.next().context("missing response version")?;
    let status_code = parts
        .next()
        .context("missing response status code")?
        .parse()?;
    let reason_phrase = parts.next().unwrap_or_default().to_string();

    if !version.starts_with("HTTP/1.") {
        bail!("unsupported HTTP version: {version}");
    }

    Ok((version.to_string(), status_code, reason_phrase))
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

async fn read_line_required<R>(reader: &mut BufReader<R>) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        bail!("unexpected EOF while reading response");
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn parses_response_with_chunked_body() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let mut reader = BufReader::new(&raw[..]);

        let response = read_response(&mut reader).await.unwrap();
        assert_eq!(response.status_code, 200);
        assert_eq!(response.reason_phrase, "OK");
        assert_eq!(response.body, b"hello");
        assert_eq!(response.raw_body, b"5\r\nhello\r\n0\r\n\r\n");
    }
}
