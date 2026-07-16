pub mod http1;
pub mod http2;
pub mod protocol;
pub mod relay;
pub mod session;
pub mod tls;
pub mod util;
pub mod websocket;

use anyhow::Result;
use log::info;
use tokio::net::TcpStream;

use crate::proxy::ProxyRuntime;

use self::http1::relay_http1;
use self::http2::relay_h2;
use self::session::MitmSession;
use self::tls::{accept_client, connect_upstream};

/// Perform MITM TLS interception between client and upstream.
pub async fn handle_mitm(
    client_tcp: TcpStream,
    upstream_tcp: TcpStream,
    domain: &str,
    runtime: ProxyRuntime,
    force_h1: bool,
) -> Result<(u64, u64)> {
    // 1. Accept TLS from client using a fake cert
    let (cert_der, key_der) = runtime.cert_cache.get_or_create(domain)?;
    let (client_tls, negotiated_alpn) =
        accept_client(client_tcp, cert_der, key_der, force_h1).await?;

    let alpn_str = String::from_utf8_lossy(&negotiated_alpn);
    info!("[{domain}] Client negotiated ALPN: {alpn_str}");

    // 2. Connect TLS to upstream server
    let upstream_tls = connect_upstream(upstream_tcp, domain, &negotiated_alpn).await?;

    info!("[{domain}] MITM TLS established (ALPN: {alpn_str})");

    // 3. Create session (fresh id per MITM connection; forwards decrypted flow
    //    data to `runtime.sink`)
    let session = MitmSession::new(domain.to_string(), &runtime);

    // 4. Bidirectionally relay the plaintext based on ALPN
    let result = if negotiated_alpn.as_slice() == b"h2" {
        relay_h2(client_tls, upstream_tls, &session).await
    } else {
        relay_http1(client_tls, upstream_tls, &session).await
    };

    // 5. Close session
    session.close();

    result
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{duplex, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};

    use super::relay::relay_one_direction;
    use super::util::peek_buffer_preview;

    struct UnexpectedEofReader {
        data: Vec<u8>,
        pos: usize,
        emitted_error: bool,
    }

    impl AsyncRead for UnexpectedEofReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.pos < self.data.len() {
                let remaining = &self.data[self.pos..];
                let n = remaining.len().min(buf.remaining());
                buf.put_slice(&remaining[..n]);
                self.pos += n;
                return Poll::Ready(Ok(()));
            }

            if !self.emitted_error {
                self.emitted_error = true;
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "peer closed without close_notify",
                )));
            }

            Poll::Ready(Ok(()))
        }
    }

    #[derive(Default)]
    struct VecWriter {
        buf: Vec<u8>,
        shutdown: bool,
    }

    impl AsyncWrite for VecWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.buf.extend_from_slice(data);
            Poll::Ready(Ok(data.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.shutdown = true;
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn relay_one_direction_copies_bytes_and_shuts_down_writer() {
        let (mut src_writer, mut src_reader) = duplex(64);
        let (mut dst_writer, mut dst_reader) = duplex(64);

        let write_task = tokio::spawn(async move {
            src_writer.write_all(b"hello world").await.unwrap();
            src_writer.shutdown().await.unwrap();
        });

        let copied = relay_one_direction(&mut src_reader, &mut dst_writer)
            .await
            .unwrap();
        assert_eq!(copied, 11);

        write_task.await.unwrap();

        let mut output: Vec<u8> = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut dst_reader, &mut output)
            .await
            .unwrap();
        assert_eq!(output, b"hello world");
    }

    #[tokio::test]
    async fn relay_one_direction_treats_unexpected_eof_as_clean_shutdown() {
        let mut reader = UnexpectedEofReader {
            data: b"payload".to_vec(),
            pos: 0,
            emitted_error: false,
        };
        let mut writer = VecWriter::default();

        let copied = relay_one_direction(&mut reader, &mut writer).await.unwrap();

        assert_eq!(copied, 7);
        assert_eq!(writer.buf, b"payload");
        assert!(writer.shutdown);
    }

    #[tokio::test]
    async fn peek_buffer_preview_returns_buffered_bytes() {
        let (mut writer, reader) = duplex(64);
        writer.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let mut reader = tokio::io::BufReader::new(reader);
        let _ = reader.fill_buf().await.unwrap();

        let preview = peek_buffer_preview(&reader, 8);

        assert_eq!(preview, b"GET / HT".to_vec());
    }
}
