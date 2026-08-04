//! `exec_worker_creds` 表读写：远程执行（remote-exec）第一期的 per-worker 凭据存储。
//!
//! 只做**通用存取**：`issue` 签发（明文只回传一次）、`verify` 校验（吊销恒 false）、
//! `revoke` 吊销、`list` 列出（不含 secret/hash）。SQL 全参数化。
//!
//! secret / salt 均为随机高熵值：本 crate 不新增随机数依赖，复用已有的 `uuid`
//! （拼接多个 `Uuid::new_v4()` 的原始字节作为熵源）。哈希复用 workspace 已有 `sm3`
//! （`sm3(salt || secret)` 的 hex），与 `worker-core::proto::script_hash` 同风格。
//!
//! 见 `docs/remote-exec-design.md` 第一期 §4.2。

use crate::SqlitePool;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 不含 secret/hash 的凭据概览，供 `exec-cred list` 与面板展示。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredInfo {
    pub worker_id: String,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
    /// 到期时间（unix 秒）；`None` = 永不过期（手工 `exec-cred add` 签发的形态）。
    pub expires_at: Option<i64>,
}

/// 当前 unix epoch 秒。
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 生成 `n_bytes` 字节的随机十六进制串：拼接若干 `Uuid::new_v4()` 的原始字节做熵源，
/// 不引入新的随机数依赖。
fn random_hex(n_bytes: usize) -> String {
    let mut bytes = Vec::with_capacity(n_bytes + 16);
    while bytes.len() < n_bytes {
        bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    bytes.truncate(n_bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `sm3(salt || "|" || secret)` 的十六进制串（完整 32 字节摘要，不截断——这里是安全比对，
/// 不是 `script_hash` 那种用于审计展示的短哈希）。
fn hash_secret(salt: &str, secret: &str) -> String {
    use sm3::{Digest, Sm3};
    let mut h = Sm3::new();
    h.update(salt.as_bytes());
    h.update(b"|");
    h.update(secret.as_bytes());
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// 签发（或重新签发）一个 worker 的 exec 凭据，返回**明文 secret**（只在此处出现一次，
/// 调用方须立即带外交付；DB 只存哈希）。对已存在的 `worker_id` 是「重新签发」语义：
/// 覆盖旧 hash/salt 并清空 `revoked_at`（等价于撤销旧 secret、发新 secret）。
pub fn issue(pool: &SqlitePool, worker_id: &str) -> Result<String> {
    issue_with_expiry(pool, worker_id, None)
}

/// 同 [`issue`]，但可指定到期时间（unix 秒）。`None` = 永不过期。
/// 「申请 → 批准 N 小时」通道走这个入口。
pub fn issue_with_expiry(
    pool: &SqlitePool,
    worker_id: &str,
    expires_at: Option<i64>,
) -> Result<String> {
    let secret = random_hex(32);
    let salt = random_hex(16);
    let hash = hash_secret(&salt, &secret);
    let conn = pool.get()?;
    conn.execute(
        "INSERT INTO exec_worker_creds(worker_id, secret_hash, salt, created_at, revoked_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5)
         ON CONFLICT(worker_id) DO UPDATE SET
             secret_hash = excluded.secret_hash,
             salt        = excluded.salt,
             created_at  = excluded.created_at,
             revoked_at  = NULL,
             expires_at  = excluded.expires_at",
        params![worker_id, hash, salt, now_epoch(), expires_at],
    )
    .context("insert exec_worker_creds")?;
    Ok(secret)
}

/// 校验 `worker_id` + 明文 `secret`。不存在 / 已吊销 / **已过期** 恒 `false`。
pub fn verify(pool: &SqlitePool, worker_id: &str, secret: &str) -> Result<bool> {
    let conn = pool.get()?;
    let row: Option<(String, String, Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT secret_hash, salt, revoked_at, expires_at FROM exec_worker_creds
             WHERE worker_id = ?1",
            params![worker_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .context("read exec_worker_creds")?;
    let Some((hash, salt, revoked_at, expires_at)) = row else {
        return Ok(false);
    };
    if revoked_at.is_some() {
        return Ok(false);
    }
    // 到期即失效。注意这只挡住「后续领任务 / 回传」，杀不掉已经在跑的命令
    // （远程可靠中止是第二期的 cancel + 本地控制面）。
    if expires_at.is_some_and(|exp| now_epoch() >= exp) {
        return Ok(false);
    }
    Ok(hash_secret(&salt, secret) == hash)
}

/// 吊销一个 worker 的凭据（幂等：已吊销再吊销仍返回 `true`）。返回该 `worker_id` 是否存在。
pub fn revoke(pool: &SqlitePool, worker_id: &str) -> Result<bool> {
    let conn = pool.get()?;
    let n = conn
        .execute(
            "UPDATE exec_worker_creds SET revoked_at = ?2 WHERE worker_id = ?1",
            params![worker_id, now_epoch()],
        )
        .context("revoke exec_worker_creds")?;
    Ok(n > 0)
}

/// 列出全部凭据概览（不含 secret/hash），按 `worker_id` 排序。
pub fn list(pool: &SqlitePool) -> Result<Vec<CredInfo>> {
    let conn = pool.get()?;
    let mut stmt = conn.prepare(
        "SELECT worker_id, created_at, revoked_at, expires_at FROM exec_worker_creds
         ORDER BY worker_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CredInfo {
                worker_id: r.get(0)?,
                created_at: r.get(1)?,
                revoked_at: r.get(2)?,
                expires_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("list exec_worker_creds")?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(dir: &std::path::Path) -> SqlitePool {
        let p = crate::open_pool(&dir.join("t.db")).unwrap();
        crate::migrate(&p).unwrap();
        p
    }

    #[test]
    fn issue_verify_revoke_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());

        // 未签发 → 校验恒 false。
        assert!(!verify(&p, "w1", "whatever").unwrap());

        let secret = issue(&p, "w1").unwrap();
        assert_eq!(secret.len(), 64); // 32 字节 hex 化
        assert!(verify(&p, "w1", &secret).unwrap());
        assert!(!verify(&p, "w1", "wrong-secret").unwrap());
        assert!(!verify(&p, "unknown-worker", &secret).unwrap());

        // 吊销后即便 secret 正确也拒绝。
        assert!(revoke(&p, "w1").unwrap());
        assert!(!verify(&p, "w1", &secret).unwrap());
        // 幂等：再吊销一次仍 true（行存在）。
        assert!(revoke(&p, "w1").unwrap());
        // 未知 worker 吊销 → false。
        assert!(!revoke(&p, "nope").unwrap());

        // 重新签发覆盖旧 secret 并恢复可用。
        let secret2 = issue(&p, "w1").unwrap();
        assert_ne!(secret, secret2);
        assert!(verify(&p, "w1", &secret2).unwrap());
        assert!(!verify(&p, "w1", &secret).unwrap());
    }

    #[test]
    fn list_reports_without_secret() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        issue(&p, "w1").unwrap();
        issue(&p, "w2").unwrap();
        revoke(&p, "w2").unwrap();

        let all = list(&p).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].worker_id, "w1");
        assert!(all[0].revoked_at.is_none());
        assert_eq!(all[1].worker_id, "w2");
        assert!(all[1].revoked_at.is_some());
    }

    #[test]
    fn expired_credential_fails_verify() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());

        // 已过期（到期时间在过去）→ secret 正确也拒绝。
        let past = issue_with_expiry(&p, "w-past", Some(now_epoch() - 1)).unwrap();
        assert!(!verify(&p, "w-past", &past).unwrap());

        // 未过期 → 正常放行。
        let future = issue_with_expiry(&p, "w-future", Some(now_epoch() + 3600)).unwrap();
        assert!(verify(&p, "w-future", &future).unwrap());

        // 无到期时间 = 永不过期（手工签发的老形态）。
        let forever = issue(&p, "w-forever").unwrap();
        assert!(verify(&p, "w-forever", &forever).unwrap());

        // 到期信息在 list 里可见。
        let all = list(&p).unwrap();
        let f = all.iter().find(|c| c.worker_id == "w-forever").unwrap();
        assert!(f.expires_at.is_none());
        let t = all.iter().find(|c| c.worker_id == "w-future").unwrap();
        assert!(t.expires_at.is_some());
    }

    #[test]
    fn secrets_are_high_entropy_and_unique() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        let s1 = issue(&p, "w1").unwrap();
        let s2 = issue(&p, "w2").unwrap();
        assert_ne!(s1, s2);
    }
}
