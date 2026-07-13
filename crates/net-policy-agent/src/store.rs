//! 持久化记录（SQLite，`<workspace>/net-policy/net-policy.db`）：
//! - `requests`：进程请求历史（连接观测留痕；按 mihomo 连接 id 去重，带保留上限）。
//! - `events`：生命周期事件（agent 启停 / 策略 apply·stop / 临时直连开关）。
//!
//! **错误不伪装**（评审点 2）：查询返回 `Result`，写入失败 `log::warn` + dropped 计数；降级到内存库
//! 时置 `degraded`，经 `status.record_store_degraded` 暴露给 UI。

use anyhow::{Context, Result};
use net_policy_core::types::{LifecycleEvent, RequestLogEntry};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const RETAIN_REQUESTS: i64 = 100_000;
const RETAIN_EVENTS: i64 = 20_000;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS requests(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  conn_id TEXT NOT NULL DEFAULT '',
  ts_ms INTEGER NOT NULL,
  process TEXT NOT NULL DEFAULT '',
  process_path TEXT NOT NULL DEFAULT '',
  host TEXT NOT NULL DEFAULT '',
  dest_ip TEXT NOT NULL DEFAULT '',
  dest_port TEXT NOT NULL DEFAULT '',
  network TEXT NOT NULL DEFAULT '',
  outbound TEXT NOT NULL DEFAULT '',
  rule TEXT NOT NULL DEFAULT ''
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_requests_conn ON requests(conn_id) WHERE conn_id != '';
CREATE INDEX IF NOT EXISTS idx_requests_ts ON requests(ts_ms);
CREATE TABLE IF NOT EXISTS events(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts_ms INTEGER NOT NULL,
  kind TEXT NOT NULL,
  detail TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts_ms);
"#;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct Store {
    conn: Mutex<Connection>,
    /// 是否已降级为内存库（磁盘打开失败）。
    degraded: bool,
    /// 写入被丢弃的条数（磁盘满/锁异常等）。
    dropped: AtomicU64,
}

impl Store {
    /// 打开库；失败降级到内存库（记录功能不因磁盘问题拖垮 agent，但 `degraded=true` 并告警）。
    pub fn open_or_memory(workspace: &Path) -> Store {
        match Self::open(workspace) {
            Ok(s) => s,
            Err(e) => {
                log::error!("打开 net-policy.db 失败，降级为内存记录（重启即失）：{e:#}");
                Self::memory()
            }
        }
    }

    fn new(conn: Connection, degraded: bool) -> Store {
        Store {
            conn: Mutex::new(conn),
            degraded,
            dropped: AtomicU64::new(0),
        }
    }

    fn memory() -> Store {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(DDL).expect("migrate in-memory sqlite");
        Self::new(conn, true)
    }

    /// 打开/建库（幂等）。失败上抛。
    pub fn open(workspace: &Path) -> Result<Store> {
        let dir = net_policy_core::config::net_policy_dir(workspace);
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("net-policy.db");
        let conn = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        conn.execute_batch(DDL).context("migrate net-policy.db")?;
        Ok(Self::new(conn, false))
    }

    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 记一条请求（非空 conn_id 幂等去重）。失败 warn + 计数（不阻塞采样）。
    pub fn record_request(&self, e: &RequestLogEntry) {
        let c = self.conn.lock().unwrap();
        if let Err(err) = c.execute(
            "INSERT OR IGNORE INTO requests(conn_id,ts_ms,process,process_path,host,dest_ip,dest_port,network,outbound,rule)\
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![e.conn_id, e.ts_ms as i64, e.process, e.process_path, e.host, e.dest_ip, e.dest_port, e.network, e.outbound, e.rule],
        ) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            log::warn!("写请求记录失败（已丢弃 {} 条）：{err}", self.dropped_count());
        }
    }

    /// 最近 `limit` 条请求（倒序）。失败上抛（server 映射为 Error，不谎报为空）。
    pub fn recent_requests(&self, limit: u32) -> Result<Vec<RequestLogEntry>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT conn_id,ts_ms,process,process_path,host,dest_ip,dest_port,network,outbound,rule\
             FROM requests ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(RequestLogEntry {
                    conn_id: r.get(0)?,
                    ts_ms: r.get::<_, i64>(1)? as u64,
                    process: r.get(2)?,
                    process_path: r.get(3)?,
                    host: r.get(4)?,
                    dest_ip: r.get(5)?,
                    dest_port: r.get(6)?,
                    network: r.get(7)?,
                    outbound: r.get(8)?,
                    rule: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?; // 坏行整体报错，不静默丢
        Ok(rows)
    }

    /// 记一条生命周期事件。失败 warn + 计数。
    pub fn record_event(&self, kind: &str, detail: &str) {
        let c = self.conn.lock().unwrap();
        if let Err(err) = c.execute(
            "INSERT INTO events(ts_ms,kind,detail) VALUES(?1,?2,?3)",
            params![now_ms() as i64, kind, detail],
        ) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            log::warn!("写事件失败（{kind}）：{err}");
        }
    }

    /// 最近 `limit` 条事件（倒序）。失败上抛。
    pub fn recent_events(&self, limit: u32) -> Result<Vec<LifecycleEvent>> {
        let c = self.conn.lock().unwrap();
        let mut stmt =
            c.prepare("SELECT ts_ms,kind,detail FROM events ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(LifecycleEvent {
                    ts_ms: r.get::<_, i64>(0)? as u64,
                    kind: r.get(1)?,
                    detail: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// 清空请求记录（隐私，评审点 6）。返回删除行数。
    pub fn clear_requests(&self) -> Result<usize> {
        let c = self.conn.lock().unwrap();
        Ok(c.execute("DELETE FROM requests", [])?)
    }

    /// 清空生命周期事件。返回删除行数。
    pub fn clear_events(&self) -> Result<usize> {
        let c = self.conn.lock().unwrap();
        Ok(c.execute("DELETE FROM events", [])?)
    }

    /// 修剪超出保留上限的旧行（采样循环偶发调用）。失败仅 warn。
    pub fn prune(&self) {
        let c = self.conn.lock().unwrap();
        for (sql, cap) in [
            ("DELETE FROM requests WHERE id NOT IN (SELECT id FROM requests ORDER BY id DESC LIMIT ?1)", RETAIN_REQUESTS),
            ("DELETE FROM events WHERE id NOT IN (SELECT id FROM events ORDER BY id DESC LIMIT ?1)", RETAIN_EVENTS),
        ] {
            if let Err(err) = c.execute(sql, params![cap]) {
                log::warn!("prune 失败：{err}");
            }
        }
    }
}
