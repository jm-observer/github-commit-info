use anyhow::Result;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

use super::util::is_connection_closed;

pub(super) async fn relay_one_direction<R, W>(reader: &mut R, writer: &mut W) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = [0u8; 16 * 1024];
    let mut total = 0u64;

    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if is_connection_closed(&err) => break,
            Err(err) => return Err(err.into()),
        };

        match writer.write_all(&buf[..n]).await {
            Ok(()) => {}
            Err(err) if is_connection_closed(&err) => break,
            Err(err) => return Err(err.into()),
        }
        total += n as u64;
    }

    // Best-effort shutdown; peer may already be gone
    match writer.shutdown().await {
        Ok(()) => {}
        Err(err) if is_connection_closed(&err) => {}
        Err(err) => return Err(err.into()),
    }
    Ok(total)
}

/// Relay plaintext when streams have already been split and wrapped in BufReaders.
/// Used as a fallback when a stream that was expected to be HTTP/1.1 turns out to
/// be HTTP/2 or otherwise non-textual.
pub(super) async fn relay_plaintext_with_bufreaders<CR, CW, UR, UW>(
    mut client_reader: BufReader<CR>,
    mut client_writer: CW,
    mut upstream_reader: BufReader<UR>,
    mut upstream_writer: UW,
) -> Result<(u64, u64)>
where
    CR: AsyncRead + Unpin,
    CW: AsyncWrite + Unpin,
    UR: AsyncRead + Unpin,
    UW: AsyncWrite + Unpin,
{
    let (up_bytes, down_bytes) = tokio::try_join!(
        async { relay_one_direction(&mut client_reader, &mut upstream_writer).await },
        async { relay_one_direction(&mut upstream_reader, &mut client_writer).await },
    )?;

    Ok((up_bytes, down_bytes))
}
