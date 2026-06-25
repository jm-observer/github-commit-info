//! toolkit-core：领域类型 + SQLite schema + URL 模式识别。
//!
//! 见 `toolkit/docs/toolkit-design.md` 与 `docs/toolkit-rfc/2026-06-04-initial-skeleton/`。

pub mod db;
pub mod ids;
pub mod llm_sessions;
pub mod llm_store;
pub mod migrations;
pub mod models;
pub mod schema;
pub mod url_match;

pub use db::{open_pool, SqlitePool};
pub use ids::new_task_id;
pub use migrations::migrate;
pub use url_match::{classify_url, UrlMatch};

/// 当前 UTC ISO8601 字符串（秒级精度）。
pub fn now_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
