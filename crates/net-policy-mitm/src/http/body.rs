use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};
use log::warn;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};

const MAX_BODY_SIZE: usize = 50 * 1024 * 1024;

pub async fn read_body<R>(
    reader: &mut BufReader<R>,
    headers: &[(String, String)],
) -> Result<(Vec<u8>, Vec<u8>)>
where
    R: AsyncRead + Unpin,
{
    let (raw_body, decoded_body) = if is_chunked(headers) {
        read_chunked_body(reader).await?
    } else if let Some(length) = get_content_length(headers)? {
        let raw = read_fixed_body(reader, length).await?;
        (raw.clone(), raw)
    } else {
        (Vec::new(), Vec::new())
    };

    let body = match get_content_encoding(headers).as_deref() {
        Some("gzip") => match decompress_gzip(&decoded_body) {
            Ok(body) => body,
            Err(err) => {
                warn!("Failed to decompress gzip body: {err:#}");
                decoded_body
            }
        },
        Some("br") => match decompress_brotli(&decoded_body) {
            Ok(body) => body,
            Err(err) => {
                warn!("Failed to decompress brotli body: {err:#}");
                decoded_body
            }
        },
        _ => decoded_body,
    };

    Ok((raw_body, body))
}

pub async fn read_fixed_body<R>(reader: &mut BufReader<R>, length: usize) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    if length > MAX_BODY_SIZE {
        bail!("HTTP body too large: {length} bytes");
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(body)
}

pub async fn read_chunked_body<R>(reader: &mut BufReader<R>) -> Result<(Vec<u8>, Vec<u8>)>
where
    R: AsyncRead + Unpin,
{
    let mut raw_body = Vec::new();
    let mut decoded_body = Vec::new();

    loop {
        let size_line = read_line(reader).await?;
        raw_body.extend_from_slice(size_line.as_bytes());

        let size_token = size_line
            .trim_end_matches(['\r', '\n'])
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_token, 16)
            .with_context(|| format!("invalid chunk size line: {}", size_line.trim_end()))?;

        if size == 0 {
            loop {
                let trailer_line = read_line(reader).await?;
                raw_body.extend_from_slice(trailer_line.as_bytes());
                if trailer_line == "\r\n" || trailer_line == "\n" {
                    break;
                }
            }
            break;
        }

        let next_len = decoded_body.len() + size;
        if next_len > MAX_BODY_SIZE {
            bail!("HTTP chunked body too large: {next_len} bytes");
        }

        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk).await?;
        raw_body.extend_from_slice(&chunk);
        decoded_body.extend_from_slice(&chunk);

        let mut chunk_crlf = [0u8; 2];
        reader.read_exact(&mut chunk_crlf).await?;
        raw_body.extend_from_slice(&chunk_crlf);
        if &chunk_crlf != b"\r\n" {
            bail!("chunk payload missing trailing CRLF");
        }
    }

    Ok((raw_body, decoded_body))
}

pub fn get_content_length(headers: &[(String, String)]) -> Result<Option<usize>> {
    match header_value(headers, "content-length") {
        Some(value) => Ok(Some(value.parse()?)),
        None => Ok(None),
    }
}

pub fn is_chunked(headers: &[(String, String)]) -> bool {
    header_value(headers, "transfer-encoding")
        .map(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
        })
        .unwrap_or(false)
}

pub fn get_content_encoding(headers: &[(String, String)]) -> Option<String> {
    header_value(headers, "content-encoding").map(|value| {
        value
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    })
}

pub fn is_sse(headers: &[(String, String)]) -> bool {
    header_value(headers, "content-type")
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn read_line<R>(reader: &mut BufReader<R>) -> Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        bail!("unexpected EOF while reading line");
    }
    Ok(line)
}

fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = flate2::read::GzDecoder::new(Cursor::new(data));
    let mut output = Vec::new();
    decoder
        .by_ref()
        .take((MAX_BODY_SIZE + 1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > MAX_BODY_SIZE {
        bail!("decompressed gzip body exceeds {MAX_BODY_SIZE} bytes");
    }
    Ok(output)
}

fn decompress_brotli(data: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut decoder = brotli::Decompressor::new(Cursor::new(data), 4096);
    decoder
        .by_ref()
        .take((MAX_BODY_SIZE + 1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > MAX_BODY_SIZE {
        bail!("decompressed brotli body exceeds {MAX_BODY_SIZE} bytes");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use flate2::{write::GzEncoder, Compression};
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn reads_chunked_body() {
        let input = b"5\r\nHello\r\n6\r\n World\r\n0\r\n\r\n";
        let mut reader = BufReader::new(&input[..]);
        let (raw, decoded) = read_chunked_body(&mut reader).await.unwrap();
        assert_eq!(raw, input);
        assert_eq!(decoded, b"Hello World");
    }

    #[tokio::test]
    async fn reads_fixed_body() {
        let input = b"hello";
        let mut reader = BufReader::new(&input[..]);
        let body = read_fixed_body(&mut reader, input.len()).await.unwrap();
        assert_eq!(body, input);
    }

    #[tokio::test]
    async fn decompresses_gzip_body() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, b"hello gzip").unwrap();
        let compressed = encoder.finish().unwrap();
        let mut reader = BufReader::new(compressed.as_slice());
        let headers = vec![
            ("Content-Length".to_string(), compressed.len().to_string()),
            ("Content-Encoding".to_string(), "gzip".to_string()),
        ];

        let (_, body) = read_body(&mut reader, &headers).await.unwrap();
        assert_eq!(body, b"hello gzip");
    }
}
