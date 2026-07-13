//! 管道帧的异步收发（agent 侧；与 client 侧等价，避免 agent → client 反向依赖）。

use anyhow::{bail, Result};
use net_policy_core::protocol::{self, Frame, MAX_FRAME_LEN};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, frame: &Frame) -> Result<()> {
    let bytes = protocol::encode(frame)?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

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
