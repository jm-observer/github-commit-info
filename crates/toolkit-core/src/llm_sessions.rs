//! `llm_sessions` / `llm_messages` 表读写：大模型会话记录的持久层。
//!
//! 把「对话测试」的交互式聊天与各业务的大模型调用（抖音整理 / 对话总结）统一以 session
//! 形式落库。本模块只做**通用存取**，不持有任何业务语义——kind / metadata / 每条 meta 的含义
//! 由调用方（toolkit-server 的 llm 层）决定。风格对齐同目录 `llm_store.rs`。

use crate::{new_task_id, now_iso8601, SqlitePool};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

/// 一条会话记录。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmSession {
    pub id: String,
    /// 来源：chat_test | douyin_refine | chat_summary（将来可加 agent）。
    pub kind: String,
    pub title: String,
    pub model: Option<String>,
    /// 生效提示词名（chat_test 为 None）。
    pub prompt_name: Option<String>,
    /// ok | error。
    pub status: String,
    /// 原始 JSON 字符串（aweme_id / unique_id / prompt_version / prompt_hash 等）。
    pub metadata: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 一条会话消息。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmMessage {
    pub id: String,
    pub session_id: String,
    pub seq: i64,
    /// system | user | assistant。
    pub role: String,
    pub content: String,
    /// 原始 JSON 字符串（latency_ms 等逐条元信息）。
    pub meta: String,
    pub created_at: String,
}

/// 新建会话的入参（metadata 省略时落 `{}`）。
#[derive(Clone, Debug, Default)]
pub struct NewSession<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub model: Option<&'a str>,
    pub prompt_name: Option<&'a str>,
    pub metadata: Option<&'a str>,
}

/// 建会话，返回新 id（`tk_*`）。
pub fn create_session(pool: &SqlitePool, s: NewSession<'_>) -> Result<String> {
    let conn = pool.get()?;
    let id = new_task_id();
    let now = now_iso8601();
    conn.execute(
        "INSERT INTO llm_sessions(id, kind, title, model, prompt_name, status, metadata, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'ok', ?6, ?7, ?7)",
        params![
            id,
            s.kind,
            s.title,
            s.model,
            s.prompt_name,
            s.metadata.unwrap_or("{}"),
            now,
        ],
    )
    .context("insert llm_session")?;
    Ok(id)
}

/// 追加一条消息（seq 自增为 `MAX(seq)+1`，并 touch 会话 updated_at）。返回落库后的消息。
pub fn append_message(
    pool: &SqlitePool,
    session_id: &str,
    role: &str,
    content: &str,
    meta: Option<&str>,
) -> Result<LlmMessage> {
    let conn = pool.get()?;
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq) + 1, 0) FROM llm_messages WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .context("next llm_message seq")?;
    let id = new_task_id();
    let now = now_iso8601();
    let meta = meta.unwrap_or("{}");
    conn.execute(
        "INSERT INTO llm_messages(id, session_id, seq, role, content, meta, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, session_id, seq, role, content, meta, now],
    )
    .context("insert llm_message")?;
    conn.execute(
        "UPDATE llm_sessions SET updated_at = ?2 WHERE id = ?1",
        params![session_id, now],
    )
    .context("touch llm_session")?;
    Ok(LlmMessage {
        id,
        session_id: session_id.to_string(),
        seq,
        role: role.to_string(),
        content: content.to_string(),
        meta: meta.to_string(),
        created_at: now,
    })
}

/// 置会话状态（ok | error），同步 updated_at。
pub fn set_session_status(pool: &SqlitePool, session_id: &str, status: &str) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "UPDATE llm_sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![session_id, status, now_iso8601()],
    )
    .context("update llm_session status")?;
    Ok(())
}

/// 仅刷新 updated_at。
pub fn touch_session(pool: &SqlitePool, session_id: &str) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "UPDATE llm_sessions SET updated_at = ?2 WHERE id = ?1",
        params![session_id, now_iso8601()],
    )
    .context("touch llm_session")?;
    Ok(())
}

/// 列会话（按 created_at 倒序），可选按 kind 过滤，limit 限量。
pub fn list_sessions(pool: &SqlitePool, kind: Option<&str>, limit: i64) -> Result<Vec<LlmSession>> {
    let conn = pool.get()?;
    let rows = match kind {
        Some(k) => {
            let mut stmt = conn.prepare(
                "SELECT id, kind, title, model, prompt_name, status, metadata, created_at, updated_at
                 FROM llm_sessions WHERE kind = ?1 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let v = stmt
                .query_map(params![k, limit], row_to_session)?
                .collect::<rusqlite::Result<Vec<_>>>();
            v
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, kind, title, model, prompt_name, status, metadata, created_at, updated_at
                 FROM llm_sessions ORDER BY created_at DESC LIMIT ?1",
            )?;
            let v = stmt
                .query_map(params![limit], row_to_session)?
                .collect::<rusqlite::Result<Vec<_>>>();
            v
        }
    }
    .context("list llm_sessions")?;
    Ok(rows)
}

/// 读单条会话（无行 = None）。
pub fn get_session(pool: &SqlitePool, id: &str) -> Result<Option<LlmSession>> {
    let conn = pool.get()?;
    let row = conn
        .query_row(
            "SELECT id, kind, title, model, prompt_name, status, metadata, created_at, updated_at
             FROM llm_sessions WHERE id = ?1",
            params![id],
            row_to_session,
        )
        .optional()
        .context("read llm_session")?;
    Ok(row)
}

/// 读会话全部消息（按 seq 升序）。
pub fn get_messages(pool: &SqlitePool, session_id: &str) -> Result<Vec<LlmMessage>> {
    let conn = pool.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, seq, role, content, meta, created_at
         FROM llm_messages WHERE session_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt
        .query_map(params![session_id], row_to_message)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("list llm_messages")?;
    Ok(rows)
}

fn row_to_session(r: &rusqlite::Row) -> rusqlite::Result<LlmSession> {
    Ok(LlmSession {
        id: r.get(0)?,
        kind: r.get(1)?,
        title: r.get(2)?,
        model: r.get(3)?,
        prompt_name: r.get(4)?,
        status: r.get(5)?,
        metadata: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn row_to_message(r: &rusqlite::Row) -> rusqlite::Result<LlmMessage> {
    Ok(LlmMessage {
        id: r.get(0)?,
        session_id: r.get(1)?,
        seq: r.get(2)?,
        role: r.get(3)?,
        content: r.get(4)?,
        meta: r.get(5)?,
        created_at: r.get(6)?,
    })
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
    fn session_and_messages_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());

        let id = create_session(
            &p,
            NewSession {
                kind: "chat_test",
                title: "新对话",
                model: Some("qwen"),
                prompt_name: None,
                metadata: None,
            },
        )
        .unwrap();

        // seq 自增 0/1/2。
        let m0 = append_message(&p, &id, "user", "hi", None).unwrap();
        let m1 = append_message(&p, &id, "assistant", "你好", None).unwrap();
        let m2 = append_message(&p, &id, "user", "再来", None).unwrap();
        assert_eq!((m0.seq, m1.seq, m2.seq), (0, 1, 2));

        let msgs = get_messages(&p, &id).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "hi");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[2].seq, 2);

        // list 命中 + kind 过滤。
        let all = list_sessions(&p, None, 100).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].metadata, "{}");
        assert_eq!(list_sessions(&p, Some("chat_test"), 100).unwrap().len(), 1);
        assert!(list_sessions(&p, Some("nope"), 100).unwrap().is_empty());

        // get_session 命中 / 未命中。
        assert_eq!(get_session(&p, &id).unwrap().unwrap().title, "新对话");
        assert!(get_session(&p, "tk_nope").unwrap().is_none());

        // status 翻转。
        set_session_status(&p, &id, "error").unwrap();
        assert_eq!(get_session(&p, &id).unwrap().unwrap().status, "error");
    }

    #[test]
    fn list_orders_newest_first_and_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());
        let a = create_session(
            &p,
            NewSession {
                kind: "chat_test",
                title: "a",
                ..Default::default()
            },
        )
        .unwrap();
        let b = create_session(
            &p,
            NewSession {
                kind: "douyin_refine",
                title: "b",
                ..Default::default()
            },
        )
        .unwrap();
        // 两条都在；limit 生效。
        assert_eq!(list_sessions(&p, None, 100).unwrap().len(), 2);
        assert_eq!(list_sessions(&p, None, 1).unwrap().len(), 1);
        // metadata 显式传入。
        let _ = (a, b);
        let c = create_session(
            &p,
            NewSession {
                kind: "chat_summary",
                title: "c",
                metadata: Some(r#"{"k":1}"#),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(get_session(&p, &c).unwrap().unwrap().metadata, r#"{"k":1}"#);
    }
}
