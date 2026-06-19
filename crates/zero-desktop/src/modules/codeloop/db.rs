//! `{workspace}/codeloop/state.db` — 复核循环记录（loops）+ 逐轮消息（loop_messages）。
//!
//! 单连接 + Mutex 模型（桌面端低并发）。表迁移幂等。`open` 时把上次残留的 `running`
//! 记录标为 `aborted`（进程崩溃 / abort 不会执行任务内的 finalize）。

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

pub struct Db {
    conn: Mutex<Connection>,
}

/// 一条复核循环记录（前端列表/详情消费，字段 snake_case 对齐 TS 类型）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LoopRow {
    pub id: i64,
    pub created_at: String,
    pub updated_at: String,
    pub claude_session: String,
    pub codex_session: String,
    pub repo_root: String,
    pub target_repo_rel: String,
    pub target_abs: String,
    pub target_label: String,
    pub mode: String,
    pub max_rounds: i64,
    pub step_confirm: bool,
    pub use_worktree: bool,
    pub status: String,
    pub final_verdict: Option<String>,
    pub total_rounds: i64,
    pub worktree_path: Option<String>,
    pub error: Option<String>,
    /// 血缘：本记录承接自哪条记录（design→implementation 两阶段），无则 None。
    pub parent_loop_id: Option<i64>,
    /// 多入口标记（`doc_review` / `implement` / `review_seed`）。旧库历史行为 NULL；读侧
    /// 按 `mode` 推断（见 docs/codeloop-multi-entry-design.md §5 兼容表）。
    pub entry_kind: Option<String>,
    /// 仅 `ReviewSeed(mode=implementation)` 写入：规格依据文档的仓库根相对路径（正斜杠）。
    pub design_doc_path: Option<String>,
    /// `ReviewSeed` 入口：seed 文件路径（与 `seed_review_inline_hash` 互斥）。
    pub seed_review_path: Option<String>,
    /// `ReviewSeed` 入口：inline seed 的 sm3 / sha256 短哈希前缀（便于排错与去重）。
    pub seed_review_inline_hash: Option<String>,
}

/// 一条逐轮消息。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LoopMessageRow {
    pub id: i64,
    pub loop_id: i64,
    pub ts: String,
    pub round: i64,
    pub kind: String,
    pub verdict: Option<String>,
    pub content: String,
}

/// insert_loop 的入参（避免超长参数列表）。
pub struct NewLoop {
    pub claude_session: String,
    pub codex_session: String,
    pub claude_cwd: String,
    pub codex_cwd: String,
    pub repo_root: String,
    pub target_repo_rel: String,
    pub target_abs: String,
    pub target_label: String,
    pub mode: String,
    pub max_rounds: i64,
    pub wait_for_idle: bool,
    pub step_confirm: bool,
    pub use_worktree: bool,
    /// 承接来源记录 id（两阶段血缘），新建独立循环时为 None。
    pub parent_loop_id: Option<i64>,
    /// 多入口标记（必传字符串 `doc_review` / `implement` / `review_seed`）。新代码 INSERT
    /// 时由调用方按 §6.1 校验后传入；空 / None 仅供旧代码路径兜底。
    pub entry_kind: Option<String>,
    /// 仅 `ReviewSeed(mode=implementation)` 写入；其它入口写 None。
    pub design_doc_path: Option<String>,
    /// `ReviewSeed` 入口：seed 文件路径（与 `seed_review_inline_hash` 互斥）。
    pub seed_review_path: Option<String>,
    /// `ReviewSeed` 入口：inline seed 的短哈希前缀。
    pub seed_review_inline_hash: Option<String>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create db dir {}", parent.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("open sqlite {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS loops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                claude_session TEXT NOT NULL,
                codex_session TEXT NOT NULL,
                claude_cwd TEXT NOT NULL,
                codex_cwd TEXT NOT NULL,
                repo_root TEXT NOT NULL,
                target_repo_rel TEXT NOT NULL,
                target_abs TEXT NOT NULL,
                target_label TEXT NOT NULL,
                mode TEXT NOT NULL,
                max_rounds INTEGER NOT NULL,
                wait_for_idle INTEGER NOT NULL,
                step_confirm INTEGER NOT NULL,
                use_worktree INTEGER NOT NULL,
                status TEXT NOT NULL,
                final_verdict TEXT,
                total_rounds INTEGER NOT NULL DEFAULT 0,
                worktree_path TEXT,
                error TEXT,
                parent_loop_id INTEGER,
                entry_kind TEXT,
                design_doc_path TEXT,
                seed_review_path TEXT,
                seed_review_inline_hash TEXT
            );
            CREATE INDEX IF NOT EXISTS loops_created ON loops(created_at DESC);

            CREATE TABLE IF NOT EXISTS loop_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                loop_id INTEGER NOT NULL,
                ts TEXT NOT NULL,
                round INTEGER NOT NULL,
                kind TEXT NOT NULL,
                verdict TEXT,
                content TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS loop_messages_loop ON loop_messages(loop_id, id);
            "#,
        )
        .context("migrate codeloop state.db")?;

        // 旧库补列（无增量迁移框架，靠幂等 ALTER；列已存在则报错被忽略）。
        conn.execute("ALTER TABLE loops ADD COLUMN parent_loop_id INTEGER", [])
            .ok();
        // 多入口（codeloop-multi-entry-design.md §5）：四列均 NULLable，无默认值，
        // 兼容历史行（按 mode 推断 entry_kind）。
        conn.execute("ALTER TABLE loops ADD COLUMN entry_kind TEXT", [])
            .ok();
        conn.execute("ALTER TABLE loops ADD COLUMN design_doc_path TEXT", [])
            .ok();
        conn.execute("ALTER TABLE loops ADD COLUMN seed_review_path TEXT", [])
            .ok();
        conn.execute(
            "ALTER TABLE loops ADD COLUMN seed_review_inline_hash TEXT",
            [],
        )
        .ok();

        // 残留清理：上次进程崩溃 / 应用重启留下的 running 记录。
        // 先救「其实已通过」的：transcript 里已有 codex_review=pass 即代表循环已通过
        //（Codex 给 PASS 后循环立即收尾，不可能 pass 后再超时），按 done/pass 修正。
        // 兼顾 running（本次崩溃残留）与 aborted（历史误标成「超时中止」的旧记录），
        // total_rounds 取已记录的最大轮次。
        let ts = now();
        conn.execute(
            "UPDATE loops SET status='done', final_verdict='pass',
                 total_rounds=(SELECT COALESCE(MAX(round),0) FROM loop_messages
                               WHERE loop_id=loops.id),
                 updated_at=?1
             WHERE status IN ('running','aborted') AND EXISTS(
                 SELECT 1 FROM loop_messages m
                 WHERE m.loop_id=loops.id AND m.kind='codex_review' AND m.verdict='pass')",
            params![ts],
        )
        .ok();
        // 其余真正中断的 → aborted/interrupted（明确区别于运行期真超时 aborted_timeout）。
        conn.execute(
            "UPDATE loops SET status='aborted',
                 final_verdict=COALESCE(final_verdict,'interrupted'),
                 updated_at=?1
             WHERE status='running'",
            params![ts],
        )
        .ok();

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 插入一条 running 记录，返回 loop_id。
    pub fn insert_loop(&self, m: &NewLoop) -> Result<i64> {
        let ts = now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO loops(
                created_at, updated_at, claude_session, codex_session, claude_cwd, codex_cwd,
                repo_root, target_repo_rel, target_abs, target_label, mode, max_rounds,
                wait_for_idle, step_confirm, use_worktree, status, total_rounds, parent_loop_id,
                entry_kind, design_doc_path, seed_review_path, seed_review_inline_hash
             ) VALUES (?1,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'running',0,?15,?16,?17,?18,?19)",
            params![
                ts,
                m.claude_session,
                m.codex_session,
                m.claude_cwd,
                m.codex_cwd,
                m.repo_root,
                m.target_repo_rel,
                m.target_abs,
                m.target_label,
                m.mode,
                m.max_rounds,
                m.wait_for_idle as i64,
                m.step_confirm as i64,
                m.use_worktree as i64,
                m.parent_loop_id,
                m.entry_kind,
                m.design_doc_path,
                m.seed_review_path,
                m.seed_review_inline_hash,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 追加一条逐轮消息，并刷新该 loop 的 updated_at。
    pub fn append_message(
        &self,
        loop_id: i64,
        round: i64,
        kind: &str,
        verdict: Option<&str>,
        content: &str,
    ) -> Result<()> {
        let ts = now();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO loop_messages(loop_id, ts, round, kind, verdict, content)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![loop_id, ts, round, kind, verdict, content],
        )?;
        conn.execute(
            "UPDATE loops SET updated_at=?1 WHERE id=?2",
            params![ts, loop_id],
        )?;
        Ok(())
    }

    /// 记录解析到的 worktree 路径。
    pub fn set_worktree(&self, loop_id: i64, path: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE loops SET worktree_path=?1, updated_at=?2 WHERE id=?3",
            params![path, now(), loop_id],
        )?;
        Ok(())
    }

    /// 终态收尾（幂等：仅当仍为 running 时写，防 finish 与 stop 竞争双写）。
    pub fn finalize(
        &self,
        loop_id: i64,
        status: &str,
        final_verdict: Option<&str>,
        total_rounds: i64,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE loops SET status=?1, final_verdict=?2, total_rounds=?3, error=?4, updated_at=?5
             WHERE id=?6 AND status='running'",
            params![status, final_verdict, total_rounds, error, now(), loop_id],
        )?;
        Ok(())
    }

    pub fn list_loops(&self, limit: i64) -> Result<Vec<LoopRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, updated_at, claude_session, codex_session, repo_root,
                    target_repo_rel, target_abs, target_label, mode, max_rounds, step_confirm,
                    use_worktree, status, final_verdict, total_rounds, worktree_path, error,
                    parent_loop_id, entry_kind, design_doc_path, seed_review_path,
                    seed_review_inline_hash
             FROM loops ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |row| {
                let entry_kind_db: Option<String> = row.get(19)?;
                let mode: String = row.get(9)?;
                // 读侧推断（§5 兼容表）：把 NULL 行的 entry_kind 用 mode 推断后写回到 DTO 上
                // 供前端展示，不回填 DB。
                let entry_kind = Some(
                    infer_entry_kind(entry_kind_db.as_deref(), &mode).to_string(),
                );
                Ok(LoopRow {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    updated_at: row.get(2)?,
                    claude_session: row.get(3)?,
                    codex_session: row.get(4)?,
                    repo_root: row.get(5)?,
                    target_repo_rel: row.get(6)?,
                    target_abs: row.get(7)?,
                    target_label: row.get(8)?,
                    mode,
                    max_rounds: row.get(10)?,
                    step_confirm: row.get::<_, i64>(11)? != 0,
                    use_worktree: row.get::<_, i64>(12)? != 0,
                    status: row.get(13)?,
                    final_verdict: row.get(14)?,
                    total_rounds: row.get(15)?,
                    worktree_path: row.get(16)?,
                    error: row.get(17)?,
                    parent_loop_id: row.get(18)?,
                    entry_kind,
                    design_doc_path: row.get(20)?,
                    seed_review_path: row.get(21)?,
                    seed_review_inline_hash: row.get(22)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn loop_messages(&self, loop_id: i64) -> Result<Vec<LoopMessageRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, loop_id, ts, round, kind, verdict, content
             FROM loop_messages WHERE loop_id=?1 ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([loop_id], |row| {
                Ok(LoopMessageRow {
                    id: row.get(0)?,
                    loop_id: row.get(1)?,
                    ts: row.get(2)?,
                    round: row.get(3)?,
                    kind: row.get(4)?,
                    verdict: row.get(5)?,
                    content: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_loop(&self, loop_id: i64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM loop_messages WHERE loop_id=?1",
            params![loop_id],
        )?;
        tx.execute("DELETE FROM loops WHERE id=?1", params![loop_id])?;
        tx.commit()?;
        Ok(())
    }

    /// 读单条 loop 行（含多入口 4 列），供入口分发段与历史补录扫描使用。
    pub fn get_loop(&self, loop_id: i64) -> Result<Option<LoopRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, updated_at, claude_session, codex_session, repo_root,
                    target_repo_rel, target_abs, target_label, mode, max_rounds, step_confirm,
                    use_worktree, status, final_verdict, total_rounds, worktree_path, error,
                    parent_loop_id, entry_kind, design_doc_path, seed_review_path,
                    seed_review_inline_hash
             FROM loops WHERE id=?1",
        )?;
        let mut rows = stmt.query_map([loop_id], |row| {
            Ok(LoopRow {
                id: row.get(0)?,
                created_at: row.get(1)?,
                updated_at: row.get(2)?,
                claude_session: row.get(3)?,
                codex_session: row.get(4)?,
                repo_root: row.get(5)?,
                target_repo_rel: row.get(6)?,
                target_abs: row.get(7)?,
                target_label: row.get(8)?,
                mode: row.get(9)?,
                max_rounds: row.get(10)?,
                step_confirm: row.get::<_, i64>(11)? != 0,
                use_worktree: row.get::<_, i64>(12)? != 0,
                status: row.get(13)?,
                final_verdict: row.get(14)?,
                total_rounds: row.get(15)?,
                worktree_path: row.get(16)?,
                error: row.get(17)?,
                parent_loop_id: row.get(18)?,
                entry_kind: row.get(19)?,
                design_doc_path: row.get(20)?,
                seed_review_path: row.get(21)?,
                seed_review_inline_hash: row.get(22)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// 该 loop 的 `loop_messages` 表里是否存在指定 kind 的消息（任意 round）。
    pub fn has_message_kind(&self, loop_id: i64, kind: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM loop_messages WHERE loop_id=?1 AND kind=?2",
            params![loop_id, kind],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// 已记录的最大轮次（用于 finalize 时回填 `total_rounds`）。无消息记 0。
    pub fn recorded_rounds(&self, loop_id: i64) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row(
            "SELECT COALESCE(MAX(round),0) FROM loop_messages WHERE loop_id=?1",
            params![loop_id],
            |row| row.get(0),
        )?;
        Ok(n)
    }
}

/// 读侧推断入口（§5 兼容表）：`entry_kind` 非 NULL 直接取；NULL 时按 `mode` 推断。
/// 仅供把 LoopRow 序列化给前端的位置使用，**不回填数据库**。
pub fn infer_entry_kind(entry_kind: Option<&str>, mode: &str) -> &'static str {
    match entry_kind {
        Some("doc_review") => "doc_review",
        Some("implement") => "implement",
        Some("review_seed") => "review_seed",
        _ => match mode {
            "design" => "doc_review",
            _ => "implement",
        },
    }
}
