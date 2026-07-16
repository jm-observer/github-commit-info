use tokio::io::AsyncRead;
use tokio::io::BufReader;

/// Check if an IO error is a benign connection-closed error.
pub(super) fn is_connection_closed(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof
    )
}

pub(super) fn peek_buffer_preview<R>(reader: &BufReader<R>, max_len: usize) -> Vec<u8>
where
    R: AsyncRead + Unpin,
{
    let buf = reader.buffer();
    buf[..buf.len().min(max_len)].to_vec()
}
