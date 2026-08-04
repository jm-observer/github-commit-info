//! `exec_cred_requests` 表读写：worker 临时权限**申请**的存取。
//!
//! 流程（见 `docs/remote-exec-design.md`）：worker 端 `run` 首次启动或凭据过期时自行
//! `submit` 一条申请 → 你在 zero-desktop 面板看到待审批 → `approve(小时数)` 签发一份带
//! `expires_at` 的凭据、明文暂存 `issued_secret` → worker 轮询 `poll` 取走后立即清空。
//!
//! **申请端点公网可达且不要求凭据**（这是"申请"本身的前提），所以防刷靠三道：
//!
//! 1. 同 `worker_id` 去重——重复申请只刷新同一行，扫描器灌不出无限行；
//! 2. `MAX_PENDING` 上限——pending 满了直接拒（controller 返回 429）；
//! 3. `PENDING_TTL_SECS` 过期清理——超时的 pending 自动作废，不长期占位。
//!
//! 挡得住无脑扫描，挡不住有针对性的骚扰；真被盯上就得加"一次性申请码"（当前明确不做）。

use crate::exec_creds::{issue_with_expiry, now_epoch};
use crate::SqlitePool;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

/// 同时存在的 pending 申请上限（防刷第二道）。
pub const MAX_PENDING: usize = 32;
/// pending 申请的存活时长：超过即视为过期作废（防刷第三道）。
pub const PENDING_TTL_SECS: i64 = 24 * 3600;

/// 一条申请（不含明文 secret），供面板列表展示。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredRequest {
    pub worker_id: String,
    pub label: String,
    pub hostname: String,
    pub os: String,
    /// `pending` | `approved` | `rejected`
    pub state: String,
    pub requested_at: i64,
    pub decided_at: Option<i64>,
    pub approved_by: Option<String>,
    pub expires_at: Option<i64>,
}

/// `submit` 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// 新建或刷新了一条 pending 申请，等人工批准。
    Pending,
    /// pending 数量已达上限，拒绝受理（controller 映射 429）。
    TooMany,
}

/// worker 提交申请。同 `worker_id` 重复提交 = 刷新该行（更新 label/hostname/os 与时间，
/// 回到 `pending`），**不新增行**。这也是"续期"的入口：凭据过期后再 `run` 会重新申请。
pub fn submit(
    pool: &SqlitePool,
    worker_id: &str,
    label: &str,
    hostname: &str,
    os: &str,
) -> Result<SubmitOutcome> {
    let conn = pool.get()?;
    purge_stale(&conn)?;

    // 已有本 worker 的行 → 直接刷新，不受 MAX_PENDING 约束（否则老 worker 续期会被
    // 扫描器灌满的队列饿死）。
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM exec_cred_requests WHERE worker_id = ?1",
            params![worker_id],
            |_| Ok(true),
        )
        .optional()
        .context("probe exec_cred_requests")?
        .unwrap_or(false);

    if !exists {
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM exec_cred_requests WHERE state = 'pending'",
                [],
                |r| r.get(0),
            )
            .context("count pending requests")?;
        if pending as usize >= MAX_PENDING {
            return Ok(SubmitOutcome::TooMany);
        }
    }

    conn.execute(
        "INSERT INTO exec_cred_requests
             (worker_id, label, hostname, os, state, requested_at,
              decided_at, approved_by, expires_at, issued_secret)
         VALUES (?1, ?2, ?3, ?4, 'pending', ?5, NULL, NULL, NULL, NULL)
         ON CONFLICT(worker_id) DO UPDATE SET
             label         = excluded.label,
             hostname      = excluded.hostname,
             os            = excluded.os,
             state         = 'pending',
             requested_at  = excluded.requested_at,
             decided_at    = NULL,
             approved_by   = NULL,
             expires_at    = NULL,
             issued_secret = NULL",
        params![worker_id, label, hostname, os, now_epoch()],
    )
    .context("insert exec_cred_requests")?;
    Ok(SubmitOutcome::Pending)
}

/// 批准一条申请：签发带 `expires_at` 的凭据，明文暂存待 worker 领取。
/// 返回 `false` 表示没有这条申请。`hours` 为授权时长（小时）。
pub fn approve(pool: &SqlitePool, worker_id: &str, hours: f64, approved_by: &str) -> Result<bool> {
    let conn = pool.get()?;
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM exec_cred_requests WHERE worker_id = ?1",
            params![worker_id],
            |_| Ok(true),
        )
        .optional()
        .context("probe exec_cred_requests")?
        .unwrap_or(false);
    if !exists {
        return Ok(false);
    }

    let expires_at = now_epoch() + (hours * 3600.0) as i64;
    // 凭据与申请行分属两条语句：签发失败就不该标记为已批准，故先签发再改状态。
    let secret = issue_with_expiry(pool, worker_id, Some(expires_at))?;
    conn.execute(
        "UPDATE exec_cred_requests
            SET state = 'approved', decided_at = ?2, approved_by = ?3,
                expires_at = ?4, issued_secret = ?5
          WHERE worker_id = ?1",
        params![worker_id, now_epoch(), approved_by, expires_at, secret],
    )
    .context("approve exec_cred_requests")?;
    Ok(true)
}

/// 拒绝一条申请。返回 `false` 表示没有这条申请。
pub fn reject(pool: &SqlitePool, worker_id: &str, decided_by: &str) -> Result<bool> {
    let conn = pool.get()?;
    let n = conn
        .execute(
            "UPDATE exec_cred_requests
                SET state = 'rejected', decided_at = ?2, approved_by = ?3, issued_secret = NULL
              WHERE worker_id = ?1",
            params![worker_id, now_epoch(), decided_by],
        )
        .context("reject exec_cred_requests")?;
    Ok(n > 0)
}

/// worker 轮询结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// 还没人处理。
    Pending,
    /// 已批准，**明文 secret 只在这里返回一次**（返回后 DB 立即清空）。
    Approved {
        secret: String,
        expires_at: i64,
    },
    /// 已批准但 secret 已被领走（worker 落盘后又来轮询）。
    AlreadyClaimed,
    Rejected,
    /// 查无此申请（被清理或从未提交）。
    Unknown,
}

/// worker 轮询自己的申请结果；批准态会**一次性**取走明文 secret 并清空 DB 里的暂存。
pub fn poll(pool: &SqlitePool, worker_id: &str) -> Result<PollOutcome> {
    let conn = pool.get()?;
    let row: Option<(String, Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT state, expires_at, issued_secret FROM exec_cred_requests WHERE worker_id = ?1",
            params![worker_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .context("read exec_cred_requests")?;
    let Some((state, expires_at, secret)) = row else {
        return Ok(PollOutcome::Unknown);
    };
    match state.as_str() {
        "approved" => match (secret, expires_at) {
            (Some(s), Some(exp)) => {
                conn.execute(
                    "UPDATE exec_cred_requests SET issued_secret = NULL WHERE worker_id = ?1",
                    params![worker_id],
                )
                .context("clear issued_secret")?;
                Ok(PollOutcome::Approved {
                    secret: s,
                    expires_at: exp,
                })
            }
            _ => Ok(PollOutcome::AlreadyClaimed),
        },
        "rejected" => Ok(PollOutcome::Rejected),
        _ => Ok(PollOutcome::Pending),
    }
}

/// 列出全部申请（不含明文 secret），最近提交的在前。
pub fn list(pool: &SqlitePool) -> Result<Vec<CredRequest>> {
    let conn = pool.get()?;
    purge_stale(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT worker_id, label, hostname, os, state, requested_at,
                decided_at, approved_by, expires_at
           FROM exec_cred_requests ORDER BY requested_at DESC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CredRequest {
                worker_id: r.get(0)?,
                label: r.get(1)?,
                hostname: r.get(2)?,
                os: r.get(3)?,
                state: r.get(4)?,
                requested_at: r.get(5)?,
                decided_at: r.get(6)?,
                approved_by: r.get(7)?,
                expires_at: r.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("list exec_cred_requests")?;
    Ok(rows)
}

/// 删除超过 [`PENDING_TTL_SECS`] 仍未处理的 pending 申请（防刷第三道）。
fn purge_stale(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM exec_cred_requests WHERE state = 'pending' AND requested_at < ?1",
        params![now_epoch() - PENDING_TTL_SECS],
    )
    .context("purge stale pending requests")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_creds;

    fn pool(dir: &std::path::Path) -> SqlitePool {
        let p = crate::open_pool(&dir.join("t.db")).unwrap();
        crate::migrate(&p).unwrap();
        p
    }

    #[test]
    fn submit_approve_poll_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());

        assert_eq!(poll(&p, "w1").unwrap(), PollOutcome::Unknown);
        assert_eq!(
            submit(&p, "w1", "老王的机器", "PC-A", "windows").unwrap(),
            SubmitOutcome::Pending
        );
        assert_eq!(poll(&p, "w1").unwrap(), PollOutcome::Pending);

        assert!(approve(&p, "w1", 20.0, "fengqi").unwrap());
        let PollOutcome::Approved { secret, expires_at } = poll(&p, "w1").unwrap() else {
            panic!("expected approved");
        };
        // 领到的 secret 立刻可用，且带到期时间。
        assert!(exec_creds::verify(&p, "w1", &secret).unwrap());
        assert!(expires_at > now_epoch());
        // 明文只发一次。
        assert_eq!(poll(&p, "w1").unwrap(), PollOutcome::AlreadyClaimed);
        // 列表不泄露 secret 字段（结构体里压根没有）。
        let all = list(&p).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, "approved");
        assert_eq!(all[0].label, "老王的机器");
    }

    #[test]
    fn resubmit_refreshes_same_row_and_resets_state() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        submit(&p, "w1", "a", "h", "windows").unwrap();
        approve(&p, "w1", 1.0, "me").unwrap();
        // 续期：再申请 → 同一行回到 pending，不新增行。
        submit(&p, "w1", "b", "h2", "linux").unwrap();
        let all = list(&p).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, "pending");
        assert_eq!(all[0].label, "b");
        assert_eq!(poll(&p, "w1").unwrap(), PollOutcome::Pending);
    }

    #[test]
    fn reject_is_visible_to_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        submit(&p, "w1", "a", "h", "windows").unwrap();
        assert!(reject(&p, "w1", "me").unwrap());
        assert_eq!(poll(&p, "w1").unwrap(), PollOutcome::Rejected);
        assert!(!reject(&p, "nope", "me").unwrap());
    }

    #[test]
    fn pending_cap_rejects_new_workers_but_not_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        for i in 0..MAX_PENDING {
            assert_eq!(
                submit(&p, &format!("w{i}"), "", "", "").unwrap(),
                SubmitOutcome::Pending
            );
        }
        // 满了 → 新 worker 被拒。
        assert_eq!(
            submit(&p, "brand-new", "", "", "").unwrap(),
            SubmitOutcome::TooMany
        );
        // 已在册的 worker 仍能刷新（续期不被扫描器饿死）。
        assert_eq!(
            submit(&p, "w0", "x", "", "").unwrap(),
            SubmitOutcome::Pending
        );
    }

    #[test]
    fn stale_pending_is_purged() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        submit(&p, "old", "", "", "").unwrap();
        {
            let conn = p.get().unwrap();
            conn.execute(
                "UPDATE exec_cred_requests SET requested_at = ?1 WHERE worker_id = 'old'",
                params![now_epoch() - PENDING_TTL_SECS - 1],
            )
            .unwrap();
        }
        assert!(list(&p).unwrap().is_empty());
    }
}
