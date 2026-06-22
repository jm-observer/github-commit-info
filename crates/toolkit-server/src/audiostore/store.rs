//! audio-store 存储核心：内容寻址 blob 写入 + DB 元信息维护。
//!
//! - id 算法 [`blob_id`]：`aud_` + sm3(bytes) 前 8 字节的 16 hex（与 `toolkit_llm::prompt_hash`
//!   同形，保证既有短哈希约定一致）。同字节 → 同 id，天然去重。
//! - [`put`]：算 id → 查库幂等（已有则原样返回，不重写文件 / 不改 source）→ 否则写
//!   `<workspace>/audio-store/<id>.wav` + `INSERT OR IGNORE` 一行。
//!
//! 表 DDL（`audio_blob`）在 `toolkit_core::schema`（DDL_V1，幂等加表）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::params;
use serde::Serialize;
use toolkit_core::{now_iso8601, SqlitePool};

use crate::audioforge::wav;

/// audio-store 在 workspace 下的路径布局：blob 落 `<workspace>/audio-store/<id>.wav`。
pub struct StorePaths {
    /// 仓库根：`<workspace>/audio-store`。
    pub root: PathBuf,
}

impl StorePaths {
    pub fn new(workspace: &Path) -> Self {
        StorePaths {
            root: workspace.join("audio-store"),
        }
    }

    /// 某 blob 的落盘路径 `<root>/<id>.wav`。
    pub fn blob_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.wav"))
    }
}

/// `put` 的返回：blob id + 字节数 + 时长（解析失败为 None）。
#[derive(Debug, Clone, Serialize)]
pub struct PutResult {
    pub id: String,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

/// 库内一行 blob 元信息（按 id 查回）。
#[derive(Debug, Clone, Serialize)]
pub struct BlobRow {
    pub id: String,
    pub bytes: i64,
    pub duration: Option<f64>,
    pub content_type: String,
    pub source: String,
    pub created_at: String,
}

/// 内容寻址 id：`aud_` + sm3(bytes) 前 8 字节 → 16 hex（与 `toolkit_llm::prompt_hash` 同算法）。
pub fn blob_id(bytes: &[u8]) -> String {
    use sm3::{Digest, Sm3};
    let mut h = Sm3::new();
    h.update(bytes);
    let out = h.finalize();
    let hex: String = out.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("aud_{hex}")
}

/// 内容寻址写入：同字节幂等（已存在直接返回库里的 bytes/duration，不重写文件、不改 source）。
///
/// `source` 仅在**首次**写入时落库（白名单校验在 HTTP 层做）。
pub fn put(pool: &SqlitePool, workspace: &Path, bytes: &[u8], source: &str) -> Result<PutResult> {
    let id = blob_id(bytes);

    // 1) 内容寻址幂等：已有该 id 直接返回库里元信息，不动文件 / source。
    if let Some(existing) = lookup(pool, &id)? {
        return Ok(PutResult {
            id: existing.id,
            bytes: existing.bytes as usize,
            duration: existing.duration,
        });
    }

    // 2) 首次写入：建目录 → 写 <id>.wav → 解析时长 → INSERT OR IGNORE 元信息。
    let paths = StorePaths::new(workspace);
    std::fs::create_dir_all(&paths.root)
        .with_context(|| format!("create audio-store dir {}", paths.root.display()))?;
    let path = paths.blob_path(&id);
    std::fs::write(&path, bytes).with_context(|| format!("write blob {}", path.display()))?;

    let duration = wav::wav_duration_secs(bytes);
    let now = now_iso8601();

    let conn = pool.get().context("get conn for audio_blob put")?;
    // INSERT OR IGNORE：并发下若另一路已插同 id，忽略冲突（内容相同，元信息等价）。
    conn.execute(
        "INSERT OR IGNORE INTO audio_blob
            (id, bytes, duration, content_type, source, created_at)
         VALUES (?1, ?2, ?3, 'audio/wav', ?4, ?5)",
        params![id, bytes.len() as i64, duration, source, now],
    )
    .context("insert audio_blob")?;

    Ok(PutResult {
        id,
        bytes: bytes.len(),
        duration,
    })
}

/// 按 id 查库一行；无则 None。
pub fn lookup(pool: &SqlitePool, id: &str) -> Result<Option<BlobRow>> {
    let conn = pool.get().context("get conn for audio_blob lookup")?;
    let row = conn
        .query_row(
            "SELECT id, bytes, duration, content_type, source, created_at
             FROM audio_blob WHERE id = ?1",
            params![id],
            |r| {
                Ok(BlobRow {
                    id: r.get(0)?,
                    bytes: r.get(1)?,
                    duration: r.get(2)?,
                    content_type: r.get(3)?,
                    source: r.get(4)?,
                    created_at: r.get(5)?,
                })
            },
        )
        .ok();
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_core::open_pool;

    fn make_wav(sample_rate: u32, samples: u32) -> Vec<u8> {
        let channels: u16 = 1;
        let bits: u16 = 16;
        let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
        let data_len = samples * channels as u32 * (bits / 8) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.extend(std::iter::repeat_n(0u8, data_len as usize));
        v
    }

    #[test]
    fn blob_id_is_deterministic_and_prefixed() {
        let a = blob_id(b"hello world");
        let b = blob_id(b"hello world");
        assert_eq!(a, b, "同字节 → 同 id");
        assert!(a.starts_with("aud_"), "id 应带 aud_ 前缀: {a}");
        // aud_ (4) + 16 hex = 20。
        assert_eq!(a.len(), 20, "id 长度应为 20: {a}");
        assert_ne!(
            blob_id(b"hello world"),
            blob_id(b"hello worlD"),
            "不同字节 → 不同 id"
        );
    }

    fn test_pool(dir: &std::path::Path) -> SqlitePool {
        let pool = open_pool(&dir.join("toolkit.db")).unwrap();
        toolkit_core::migrate(&pool).unwrap();
        pool
    }

    #[test]
    fn put_is_content_addressed_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path());
        let ws = dir.path();
        let wav = make_wav(16000, 16000); // 1.0s

        let r1 = put(&pool, ws, &wav, "manual").unwrap();
        assert!(r1.id.starts_with("aud_"));
        assert_eq!(r1.bytes, wav.len());
        assert_eq!(r1.duration, Some(1.0));

        // 第二次同字节：同 id、同元信息。
        let r2 = put(&pool, ws, &wav, "forge").unwrap();
        assert_eq!(r1.id, r2.id, "同字节两次得同 id");
        assert_eq!(r2.bytes, wav.len());
        assert_eq!(r2.duration, Some(1.0));

        // 文件只一份 + DB 只一行；source 保留首次的 manual（第二次的 forge 不覆盖）。
        let path = StorePaths::new(ws).blob_path(&r1.id);
        assert!(path.exists());
        let row = lookup(&pool, &r1.id).unwrap().unwrap();
        assert_eq!(row.source, "manual", "幂等不改 source");
        assert_eq!(row.content_type, "audio/wav");
        let conn = pool.get().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM audio_blob", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "DB 只一行");
    }

    #[test]
    fn lookup_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let pool = test_pool(dir.path());
        assert!(lookup(&pool, "aud_deadbeefdeadbeef").unwrap().is_none());
    }
}
