//! `licenses` 表读写（设计 §6.2）：在线续期签发时的权威台账，也是管理端点
//! （`/api/web/license/*`）的存储。**纯存取**，不做任何签名/验签——那是 `signer.rs` /
//! custom-utils 的事；这里只管「一条 lic_id 记录了什么」。SQL 全参数化。

use anyhow::{Context, Result};
use custom_utils::license::MachineFingerprint;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use toolkit_core::SqlitePool;

/// 台账一行，字段对应 `toolkit-core/src/schema.rs` 的 `licenses` 表（设计 §6.2）。
/// `machine_ids`/`features` 在结构体里是已解析的类型，落库/读库时才转 JSON 文本
/// （见 [`row_from_sql`]/插入时的序列化）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LicenseRow {
    pub lic_id: String,
    pub product: String,
    pub subject: String,
    #[serde(default)]
    pub contact_email: Option<String>,
    #[serde(default)]
    pub machine_ids: Vec<MachineFingerprint>,
    pub not_before: String,
    pub business_deadline: String,
    pub grant_window_days: i64,
    #[serde(default)]
    pub lease_days: Option<i64>,
    pub grace_days: i64,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub max_version: Option<String>,
    #[serde(default)]
    pub revoked_at: Option<String>,
    #[serde(default)]
    pub note: String,
    pub created_at: String,
}

const SELECT_COLS: &str = "lic_id, product, subject, contact_email, machine_ids, not_before, \
     business_deadline, grant_window_days, lease_days, grace_days, features, max_version, \
     revoked_at, note, created_at";

fn row_from_sql(r: &rusqlite::Row) -> rusqlite::Result<LicenseRow> {
    let machine_ids_json: String = r.get(4)?;
    let features_json: String = r.get(10)?;
    let machine_ids: Vec<MachineFingerprint> =
        serde_json::from_str(&machine_ids_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?;
    let features: Vec<String> = serde_json::from_str(&features_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(LicenseRow {
        lic_id: r.get(0)?,
        product: r.get(1)?,
        subject: r.get(2)?,
        contact_email: r.get(3)?,
        machine_ids,
        not_before: r.get(5)?,
        business_deadline: r.get(6)?,
        grant_window_days: r.get(7)?,
        lease_days: r.get(8)?,
        grace_days: r.get(9)?,
        features,
        max_version: r.get(11)?,
        revoked_at: r.get(12)?,
        note: r.get(13)?,
        created_at: r.get(14)?,
    })
}

/// 读一条（无行 = `None`）。
pub fn get(pool: &SqlitePool, lic_id: &str) -> Result<Option<LicenseRow>> {
    let conn = pool.get()?;
    conn.query_row(
        &format!("SELECT {SELECT_COLS} FROM licenses WHERE lic_id = ?1"),
        params![lic_id],
        row_from_sql,
    )
    .optional()
    .context("read licenses")
}

/// 列出全部（按 created_at 倒序）。
pub fn list(pool: &SqlitePool) -> Result<Vec<LicenseRow>> {
    let conn = pool.get()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {SELECT_COLS} FROM licenses ORDER BY created_at DESC"
    ))?;
    let rows = stmt
        .query_map([], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("list licenses")?;
    Ok(rows)
}

/// upsert 一整行（管理端点 `POST /api/web/license` 新建/覆盖台账用；`created_at` 在插入时
/// 首次写入，覆盖时保留原值——用 `COALESCE` 避免管理端点重复调用把创建时间抹掉）。
pub fn upsert(pool: &SqlitePool, row: &LicenseRow) -> Result<()> {
    let conn = pool.get()?;
    let machine_ids_json =
        serde_json::to_string(&row.machine_ids).context("serialize machine_ids")?;
    let features_json = serde_json::to_string(&row.features).context("serialize features")?;
    conn.execute(
        "INSERT INTO licenses(
            lic_id, product, subject, contact_email, machine_ids, not_before, business_deadline,
            grant_window_days, lease_days, grace_days, features, max_version, revoked_at, note, created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
         ON CONFLICT(lic_id) DO UPDATE SET
            product = excluded.product,
            subject = excluded.subject,
            contact_email = excluded.contact_email,
            machine_ids = excluded.machine_ids,
            not_before = excluded.not_before,
            business_deadline = excluded.business_deadline,
            grant_window_days = excluded.grant_window_days,
            lease_days = excluded.lease_days,
            grace_days = excluded.grace_days,
            features = excluded.features,
            max_version = excluded.max_version,
            note = excluded.note",
        params![
            row.lic_id,
            row.product,
            row.subject,
            row.contact_email,
            machine_ids_json,
            row.not_before,
            row.business_deadline,
            row.grant_window_days,
            row.lease_days,
            row.grace_days,
            features_json,
            row.max_version,
            row.revoked_at,
            row.note,
            row.created_at,
        ],
    )
    .context("upsert licenses")?;
    Ok(())
}

/// 改续期窗口天数（"后台改到期日自动恢复" = 调这个，设计 §6.2）。返回是否命中。
pub fn set_grant_window(pool: &SqlitePool, lic_id: &str, grant_window_days: i64) -> Result<bool> {
    let conn = pool.get()?;
    let n = conn
        .execute(
            "UPDATE licenses SET grant_window_days = ?1 WHERE lic_id = ?2",
            params![grant_window_days, lic_id],
        )
        .context("update grant_window_days")?;
    Ok(n > 0)
}

/// 改在线租约天数（`None` = 转纯离线模式，不再签发 `lease_until`）。返回是否命中。
pub fn set_lease(pool: &SqlitePool, lic_id: &str, lease_days: Option<i64>) -> Result<bool> {
    let conn = pool.get()?;
    let n = conn
        .execute(
            "UPDATE licenses SET lease_days = ?1 WHERE lic_id = ?2",
            params![lease_days, lic_id],
        )
        .context("update lease_days")?;
    Ok(n > 0)
}

/// 改联系人邮箱。返回是否命中。
pub fn set_contact_email(
    pool: &SqlitePool,
    lic_id: &str,
    contact_email: Option<&str>,
) -> Result<bool> {
    let conn = pool.get()?;
    let n = conn
        .execute(
            "UPDATE licenses SET contact_email = ?1 WHERE lic_id = ?2",
            params![contact_email, lic_id],
        )
        .context("update contact_email")?;
    Ok(n > 0)
}

/// 吊销（`revoked_at` 非空即拒绝续期）。返回是否命中。幂等：已吊销的再调不报错，只是
/// 时间戳会更新为最新一次调用（吊销没有"撤销吊销"，与 custom-utils `RevokedSet` 单调语义呼应）。
pub fn revoke(pool: &SqlitePool, lic_id: &str) -> Result<bool> {
    let conn = pool.get()?;
    let n = conn
        .execute(
            "UPDATE licenses SET revoked_at = ?1 WHERE lic_id = ?2",
            params![toolkit_core::now_iso8601(), lic_id],
        )
        .context("revoke license")?;
    Ok(n > 0)
}

/// 删除一条台账记录（区别于吊销：这是彻底移除记账行，不留痕迹；吊销仍保留记录 + 拒绝续期）。
/// 返回是否命中。
pub fn delete(pool: &SqlitePool, lic_id: &str) -> Result<bool> {
    let conn = pool.get()?;
    let n = conn
        .execute("DELETE FROM licenses WHERE lic_id = ?1", params![lic_id])
        .context("delete license")?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(dir: &std::path::Path) -> SqlitePool {
        let p = toolkit_core::open_pool(&dir.join("t.db")).unwrap();
        toolkit_core::migrate(&p).unwrap();
        p
    }

    fn sample(lic_id: &str) -> LicenseRow {
        LicenseRow {
            lic_id: lic_id.to_string(),
            product: "zero-desktop".to_string(),
            subject: "测试客户".to_string(),
            contact_email: Some("a@b.com".to_string()),
            machine_ids: vec![],
            not_before: "2026-01-01T00:00:00Z".to_string(),
            business_deadline: "2027-01-01T00:00:00Z".to_string(),
            grant_window_days: 30,
            lease_days: Some(14),
            grace_days: 14,
            features: vec!["speech".to_string()],
            max_version: None,
            revoked_at: None,
            note: "".to_string(),
            created_at: toolkit_core::now_iso8601(),
        }
    }

    #[test]
    fn crud_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        assert_eq!(get(&p, "L-1").unwrap(), None);

        upsert(&p, &sample("L-1")).unwrap();
        let got = get(&p, "L-1").unwrap().unwrap();
        assert_eq!(got.product, "zero-desktop");
        assert_eq!(got.features, vec!["speech".to_string()]);
        assert_eq!(got.lease_days, Some(14));
        assert_eq!(got.revoked_at, None);

        // upsert 覆盖但保留 created_at 之外的字段更新。
        let mut updated = sample("L-1");
        updated.subject = "改名客户".to_string();
        upsert(&p, &updated).unwrap();
        assert_eq!(get(&p, "L-1").unwrap().unwrap().subject, "改名客户");

        assert!(set_grant_window(&p, "L-1", 60).unwrap());
        assert_eq!(get(&p, "L-1").unwrap().unwrap().grant_window_days, 60);
        assert!(!set_grant_window(&p, "nope", 60).unwrap());

        assert!(set_lease(&p, "L-1", None).unwrap());
        assert_eq!(get(&p, "L-1").unwrap().unwrap().lease_days, None);

        assert!(set_contact_email(&p, "L-1", Some("c@d.com")).unwrap());
        assert_eq!(
            get(&p, "L-1").unwrap().unwrap().contact_email.as_deref(),
            Some("c@d.com")
        );

        assert!(revoke(&p, "L-1").unwrap());
        assert!(get(&p, "L-1").unwrap().unwrap().revoked_at.is_some());

        upsert(&p, &sample("L-2")).unwrap();
        assert_eq!(list(&p).unwrap().len(), 2);

        assert!(delete(&p, "L-1").unwrap());
        assert_eq!(get(&p, "L-1").unwrap(), None);
        assert_eq!(list(&p).unwrap().len(), 1);
    }
}
