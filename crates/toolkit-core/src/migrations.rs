use crate::db::SqlitePool;
use crate::schema::{DDL_V1, SCHEMA_VERSION};
use anyhow::{Context, Result};
use rusqlite::params;

/// 启动时调用一次，幂等。
pub fn migrate(pool: &SqlitePool) -> Result<()> {
    let conn = pool.get().context("acquire connection")?;
    conn.execute_batch(DDL_V1).context("apply v1 ddl")?;

    // 给「已存在的表」补列：DDL 的 CREATE TABLE IF NOT EXISTS 对存量库不会加列，
    // 故用 PRAGMA 守卫 + ALTER 幂等补列（SQLite 无 ADD COLUMN IF NOT EXISTS）。
    // 本仓无增量迁移框架，纯加列同 DDL 一样不 bump SCHEMA_VERSION。
    add_column_if_missing(&conn, "shadow_attempt", "detail_json", "TEXT")
        .context("add shadow_attempt.detail_json")?;

    let current: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .ok();
    if current.is_none() {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

/// 幂等给 `table` 补一列 `column`（声明 `decl`，如 `"TEXT"`）。列已存在则 no-op。
/// 表名/列名来自代码常量（非外部输入），故直接拼进 SQL 安全。
fn add_column_if_missing(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("pragma table_info({table})"))?;
    let exists = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .context("query columns")?
        .filter_map(|c| c.ok())
        .any(|c| c.eq_ignore_ascii_case(column));
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )
        .with_context(|| format!("alter table {table} add {column}"))?;
    }
    Ok(())
}

/// 读 schema_version，主要给测试用。
pub fn schema_version(pool: &SqlitePool) -> Result<i64> {
    let conn = pool.get()?;
    let v: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |r| r.get(0),
    )?;
    Ok(v.parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_pool;

    fn has_column(pool: &SqlitePool, table: &str, column: &str) -> bool {
        let conn = pool.get().unwrap();
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let found = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .any(|c| c == column);
        found
    }

    /// 新库：migrate 后 shadow_attempt 有 detail_json（来自 DDL）。
    #[test]
    fn fresh_db_has_detail_json() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open_pool(&dir.path().join("t.db")).unwrap();
        migrate(&pool).unwrap();
        assert!(has_column(&pool, "shadow_attempt", "detail_json"));
    }

    /// 存量库（手建无 detail_json 的旧表）：migrate 幂等补列，且重复调用不报错。
    #[test]
    fn legacy_db_gets_column_added_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open_pool(&dir.path().join("t.db")).unwrap();
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                "CREATE TABLE shadow_attempt (
                    id TEXT PRIMARY KEY, customer_id INTEGER NOT NULL, kind TEXT NOT NULL,
                    sentence_id INTEGER NOT NULL, word_index INTEGER, ref_text TEXT NOT NULL,
                    transcript TEXT, score REAL NOT NULL, passed INTEGER NOT NULL,
                    created_at TEXT NOT NULL
                );",
            )
            .unwrap();
        }
        assert!(!has_column(&pool, "shadow_attempt", "detail_json"));
        migrate(&pool).unwrap();
        assert!(has_column(&pool, "shadow_attempt", "detail_json"));
        // 二次 migrate 幂等，不应因列已存在而报错。
        migrate(&pool).unwrap();
        assert!(has_column(&pool, "shadow_attempt", "detail_json"));
    }
}
