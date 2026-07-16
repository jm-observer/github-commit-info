use anyhow::Result;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::BufReader;

/// Known HTTP/1.x request methods.
pub(super) const HTTP_METHODS: &[&[u8]] = &[
    b"GET ",
    b"POST ",
    b"PUT ",
    b"DELETE ",
    b"PATCH ",
    b"HEAD ",
    b"OPTIONS ",
    b"TRACE ",
    b"CONNECT ",
];

/// Check if the buffered data looks like the start of a valid HTTP/1.1 request.
/// Returns:
/// - Ok(Some(true)) if it looks like HTTP/1.1
/// - Ok(Some(false)) if it looks like something else (e.g. HTTP/2)
/// - Ok(None) if the stream reached EOF
pub(super) async fn looks_like_http1<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<bool>> {
    let buf = reader.fill_buf().await?;
    if buf.is_empty() {
        return Ok(None);
    }

    // HTTP/2 connection preface starts with "PRI * HTTP/2.0\r\n..." (all ASCII!)
    // Must be checked explicitly because it would pass a naive ASCII test.
    if buf.starts_with(b"PRI ") {
        return Ok(Some(false));
    }

    // Positive check: buffer must start with a known HTTP/1.x method
    let starts_with_method = HTTP_METHODS.iter().any(|method| buf.starts_with(method));
    Ok(Some(starts_with_method))
}
