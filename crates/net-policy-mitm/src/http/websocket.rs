use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Opcode values for stored frame encoding.
/// Used when serializing frames to database `stream_data` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredFrameType {
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl StoredFrameType {
    pub fn from_opcode(opcode: u8) -> Option<Self> {
        match opcode {
            0x1 => Some(StoredFrameType::Text),
            0x2 => Some(StoredFrameType::Binary),
            0x8 => Some(StoredFrameType::Close),
            0x9 => Some(StoredFrameType::Ping),
            0xA => Some(StoredFrameType::Pong),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            StoredFrameType::Text => 0x1,
            StoredFrameType::Binary => 0x2,
            StoredFrameType::Close => 0x8,
            StoredFrameType::Ping => 0x9,
            StoredFrameType::Pong => 0xA,
        }
    }
}

/// A parsed WebSocket frame (RFC 6455 §5.2).
#[derive(Debug, Clone)]
pub struct WsFrame {
    pub fin: bool,
    /// RSV1 bit — indicates compressed payload when permessage-deflate is negotiated.
    pub rsv1: bool,
    pub opcode: u8,
    /// Unmasked payload data.
    pub payload: Vec<u8>,
    /// Original raw bytes as received — used for transparent forwarding.
    pub raw: Vec<u8>,
}

/// Encode a frame into the stored format:
/// `[frame_type: u8][payload_len: u64 big-endian][payload bytes...]`
pub fn encode_frame_for_storage(frame: &WsFrame) -> Vec<u8> {
    let frame_type = StoredFrameType::from_opcode(frame.opcode).unwrap_or(StoredFrameType::Binary);
    let mut buf = Vec::with_capacity(1 + 8 + frame.payload.len());
    buf.push(frame_type.to_u8());
    buf.extend_from_slice(&(frame.payload.len() as u64).to_be_bytes());
    buf.extend_from_slice(&frame.payload);
    buf
}

/// Decode a stored frame from the `[type][len][payload]` format.
/// Returns `Some((frame_type, payload))`.
pub fn decode_stored_frame(data: &[u8]) -> Option<(StoredFrameType, Vec<u8>)> {
    if data.len() < 9 {
        return None;
    }
    let frame_type = StoredFrameType::from_opcode(data[0])?;
    let payload_len = u64::from_be_bytes([
        data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
    ]) as usize;
    if data.len() < 9 + payload_len {
        return None;
    }
    let payload = data[9..9 + payload_len].to_vec();
    Some((frame_type, payload))
}

impl WsFrame {
    pub fn is_text(&self) -> bool {
        self.opcode == 0x1
    }

    pub fn is_binary(&self) -> bool {
        self.opcode == 0x2
    }

    pub fn is_continuation(&self) -> bool {
        self.opcode == 0x0
    }

    pub fn is_close(&self) -> bool {
        self.opcode == 0x8
    }
}

/// Maximum payload size we'll accept (16 MiB). Anything larger is likely
/// not a system-prompt JSON and we should bail rather than OOM.
const MAX_PAYLOAD_SIZE: u64 = 16 * 1024 * 1024;

/// Read a single WebSocket frame from `reader`.
///
/// Returns `Ok(None)` on clean EOF (stream closed).
/// The returned `WsFrame.raw` contains the exact bytes read, suitable for
/// forwarding verbatim to the other side.
pub async fn read_ws_frame<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<WsFrame>> {
    // -- First 2 bytes: FIN/opcode + MASK/payload-length-7 --
    let mut header = [0u8; 2];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e).context("read WS frame header"),
    }

    let mut raw = Vec::with_capacity(128);
    raw.extend_from_slice(&header);

    let fin = header[0] & 0x80 != 0;
    let rsv1 = header[0] & 0x40 != 0;
    let opcode = header[0] & 0x0F;
    let masked = header[1] & 0x80 != 0;
    let len7 = (header[1] & 0x7F) as u64;

    // -- Extended payload length --
    let payload_len: u64 = if len7 <= 125 {
        len7
    } else if len7 == 126 {
        let mut buf = [0u8; 2];
        reader
            .read_exact(&mut buf)
            .await
            .context("read WS 16-bit length")?;
        raw.extend_from_slice(&buf);
        u16::from_be_bytes(buf) as u64
    } else {
        // len7 == 127
        let mut buf = [0u8; 8];
        reader
            .read_exact(&mut buf)
            .await
            .context("read WS 64-bit length")?;
        raw.extend_from_slice(&buf);
        u64::from_be_bytes(buf)
    };

    if payload_len > MAX_PAYLOAD_SIZE {
        bail!("WebSocket frame payload too large: {payload_len} bytes (max {MAX_PAYLOAD_SIZE})");
    }

    // -- Masking key (4 bytes, only if MASK bit is set) --
    let mask_key = if masked {
        let mut key = [0u8; 4];
        reader
            .read_exact(&mut key)
            .await
            .context("read WS mask key")?;
        raw.extend_from_slice(&key);
        Some(key)
    } else {
        None
    };

    // -- Payload --
    let mut payload = vec![0u8; payload_len as usize];
    if payload_len > 0 {
        reader
            .read_exact(&mut payload)
            .await
            .context("read WS payload")?;
        raw.extend_from_slice(&payload);
    }

    // Unmask in-place for inspection
    if let Some(key) = mask_key {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= key[i % 4];
        }
    }

    Ok(Some(WsFrame {
        fin,
        rsv1,
        opcode,
        payload,
        raw,
    }))
}

/// Forward a WebSocket frame's original raw bytes to the writer.
pub async fn forward_ws_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &WsFrame,
) -> Result<u64> {
    writer
        .write_all(&frame.raw)
        .await
        .context("forward WS frame")?;
    writer.flush().await.context("flush WS frame")?;
    Ok(frame.raw.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    /// Build a raw WebSocket frame from parts.
    fn build_frame(fin: bool, opcode: u8, mask_key: Option<[u8; 4]>, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let byte0 = if fin { 0x80 | opcode } else { opcode };
        buf.push(byte0);

        let masked = mask_key.is_some();
        let mask_bit: u8 = if masked { 0x80 } else { 0x00 };

        if payload.len() <= 125 {
            buf.push(mask_bit | payload.len() as u8);
        } else if payload.len() <= 65535 {
            buf.push(mask_bit | 126);
            buf.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            buf.push(mask_bit | 127);
            buf.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }

        if let Some(key) = mask_key {
            buf.extend_from_slice(&key);
            let masked_payload: Vec<u8> = payload
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ key[i % 4])
                .collect();
            buf.extend_from_slice(&masked_payload);
        } else {
            buf.extend_from_slice(payload);
        }

        buf
    }

    #[tokio::test]
    async fn parse_unmasked_text_frame() {
        let raw = build_frame(true, 0x1, None, b"hello");
        let mut reader = BufReader::new(raw.as_slice());
        let frame = read_ws_frame(&mut reader).await.unwrap().unwrap();

        assert!(frame.fin);
        assert!(frame.is_text());
        assert_eq!(frame.payload, b"hello");
        assert_eq!(frame.raw, raw);
    }

    #[tokio::test]
    async fn parse_masked_text_frame() {
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        let payload = b"Hello";
        let raw = build_frame(true, 0x1, Some(mask), payload);
        let mut reader = BufReader::new(raw.as_slice());
        let frame = read_ws_frame(&mut reader).await.unwrap().unwrap();

        assert!(frame.fin);
        assert!(frame.is_text());
        assert_eq!(frame.payload, b"Hello");
        assert_eq!(frame.raw, raw);
    }

    #[tokio::test]
    async fn parse_binary_frame_16bit_length() {
        let payload = vec![0xAB; 300];
        let raw = build_frame(true, 0x2, None, &payload);
        let mut reader = BufReader::new(raw.as_slice());
        let frame = read_ws_frame(&mut reader).await.unwrap().unwrap();

        assert!(frame.fin);
        assert!(frame.is_binary());
        assert_eq!(frame.payload.len(), 300);
    }

    #[tokio::test]
    async fn parse_continuation_frame() {
        let raw = build_frame(false, 0x0, None, b"cont");
        let mut reader = BufReader::new(raw.as_slice());
        let frame = read_ws_frame(&mut reader).await.unwrap().unwrap();

        assert!(!frame.fin);
        assert!(frame.is_continuation());
        assert_eq!(frame.payload, b"cont");
    }

    #[tokio::test]
    async fn parse_close_frame() {
        let raw = build_frame(true, 0x8, None, &[0x03, 0xE8]); // status 1000
        let mut reader = BufReader::new(raw.as_slice());
        let frame = read_ws_frame(&mut reader).await.unwrap().unwrap();

        assert!(frame.is_close());
    }

    #[tokio::test]
    async fn eof_returns_none() {
        let data: &[u8] = &[];
        let mut reader = BufReader::new(data);
        let result = read_ws_frame(&mut reader).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn multiple_frames_sequential() {
        let mut data = build_frame(true, 0x1, None, b"first");
        data.extend(build_frame(true, 0x1, None, b"second"));

        let mut reader = BufReader::new(data.as_slice());
        let f1 = read_ws_frame(&mut reader).await.unwrap().unwrap();
        let f2 = read_ws_frame(&mut reader).await.unwrap().unwrap();
        let f3 = read_ws_frame(&mut reader).await.unwrap();

        assert_eq!(f1.payload, b"first");
        assert_eq!(f2.payload, b"second");
        assert!(f3.is_none());
    }

    #[test]
    fn encode_decode_stored_frame_roundtrip_for_supported_types() {
        let cases = vec![
            (0x1u8, b"hello".to_vec()),
            (0x2u8, vec![0x00, 0x01, 0xFE, 0xFF]),
            (0x9u8, b"ping".to_vec()),
            (0xAu8, b"pong".to_vec()),
            (0x8u8, vec![0x03, 0xE8]),
        ];

        for (opcode, payload) in cases {
            let frame = WsFrame {
                fin: true,
                rsv1: false,
                opcode,
                payload: payload.clone(),
                raw: Vec::new(),
            };
            let encoded = encode_frame_for_storage(&frame);
            let decoded = decode_stored_frame(&encoded).expect("decode should succeed");
            assert_eq!(decoded.0.to_u8(), opcode);
            assert_eq!(decoded.1, payload);
        }
    }
}
