//! 管道帧的异步收发（`4 字节小端长度前缀 + JSON`，编解码复用 `net_policy_core::protocol`）。
//!
//! 泛型于任意 `AsyncRead/AsyncWrite`，故 client（NamedPipeClient）与 agent（NamedPipeServer）都能用
//! 同一套读写；这里放在 client crate，agent 侧有一份等价实现（避免 agent → client 的反向依赖）。

use anyhow::{bail, Result};
use net_policy_core::protocol::{self, Frame, MAX_FRAME_LEN};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 写一帧（编码 + write_all + flush）。
pub async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, frame: &Frame) -> Result<()> {
    let bytes = protocol::encode(frame)?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

/// 读一帧：先读 4 字节长度，校验上限，再读 len 字节解码。EOF 时 `read_exact` 返回错误。
pub async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<Frame> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        bail!("帧过大：{len} > {MAX_FRAME_LEN}");
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    protocol::decode(&buf)
}
