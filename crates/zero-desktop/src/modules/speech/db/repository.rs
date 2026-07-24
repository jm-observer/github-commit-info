use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

/// 一条标注样本的完整落库形态（与 `speech_samples` 表逐列对应）。
#[derive(Debug, Clone, Serialize)]
pub struct SampleRow {
    pub id: i64,
    pub segment_id: i64,
    pub session_id: Option<String>,
    pub label: String,
    pub text_raw: String,
    pub text_optimized: Option<String>,
    pub text_english: Option<String>,
    pub text_secondary: Option<String>,
    pub correction: Option<String>,
    pub note: Option<String>,
    pub audio_path: Option<String>,
    pub audio_status: String,
    pub hotword_sync: Option<String>,
    pub marked_at: String,
    /// 样本来源：`"ui"`（手工标注面板）| `"copy"`（专用快捷键一键采集）。
    pub source: String,
    /// 采集涉及的完整 segment id 列表（JSON 数组文本），一条样本可能由多段 burst 拼成。
    /// 手工标注（单段）通常为 `None`，`segment_id` 单列已够用。
    pub segment_ids: Option<String>,
    /// 交付时前台应用的可执行文件名，如 `Code.exe`。
    pub app_exe: Option<String>,
    /// 交付时前台应用的可执行文件全路径。
    pub app_path: Option<String>,
    /// 交付时的窗口标题。浏览器场景下是区分具体站点的唯一线索。
    pub app_title: Option<String>,
    /// 交付时的窗口类名。
    pub app_class: Option<String>,
    /// 交付模式：`"auto_paste"` | `"auto_copy"`；手工标注或未开启自动交付时为 `None`。
    pub delivery_mode: Option<String>,
}

/// 一次交付时的应用上下文（`speech_samples` 的 `app_*` / `delivery_mode` 五列）。
///
/// 手工标注路径没有交付动作，取 `Default`（全空）即可。
#[derive(Debug, Clone, Default)]
pub struct SampleAppContext {
    pub app_exe: Option<String>,
    pub app_path: Option<String>,
    pub app_title: Option<String>,
    pub app_class: Option<String>,
    pub delivery_mode: Option<String>,
}

/// 新插入标注样本前的入参（不含自增 id / 落盘音频字段）。
pub struct NewSample {
    pub segment_id: i64,
    pub session_id: Option<String>,
    pub label: String,
    pub text_raw: String,
    pub text_optimized: Option<String>,
    pub text_english: Option<String>,
    pub text_secondary: Option<String>,
    pub correction: Option<String>,
    pub note: Option<String>,
    pub audio_status: String,
    pub marked_at: String,
    pub source: String,
    pub segment_ids: Option<String>,
    /// 交付时的应用上下文；手工标注路径传 `Default::default()`。
    pub app: SampleAppContext,
}

/// 插入一条样本，返回自增 id。`audio_path`/`hotword_sync` 暂为空，后续 update。
pub fn insert_sample(conn: &Connection, s: &NewSample) -> Result<i64> {
    conn.execute(
        "INSERT INTO speech_samples(
            segment_id, session_id, label, text_raw, text_optimized,
            text_english, text_secondary, correction, note,
            audio_path, audio_status, hotword_sync, marked_at,
            source, segment_ids,
            app_exe, app_path, app_title, app_class, delivery_mode
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, NULL, ?11, ?12, ?13,
                  ?14, ?15, ?16, ?17, ?18)",
        params![
            s.segment_id,
            s.session_id,
            s.label,
            s.text_raw,
            s.text_optimized,
            s.text_english,
            s.text_secondary,
            s.correction,
            s.note,
            s.audio_status,
            s.marked_at,
            s.source,
            s.segment_ids,
            s.app.app_exe,
            s.app.app_path,
            s.app.app_title,
            s.app.app_class,
            s.app.delivery_mode,
        ],
    )
    .context("failed to insert speech sample")?;
    Ok(conn.last_insert_rowid())
}

/// 更新样本的音频落盘结果。
pub fn update_sample_audio(
    conn: &Connection,
    id: i64,
    audio_path: Option<&str>,
    audio_status: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE speech_samples SET audio_path = ?1, audio_status = ?2 WHERE id = ?3",
        params![audio_path, audio_status, id],
    )
    .context("failed to update sample audio")?;
    Ok(())
}

/// 更新热词同步结果（added | exists | failed）。
pub fn update_sample_hotword_sync(conn: &Connection, id: i64, sync: &str) -> Result<()> {
    conn.execute(
        "UPDATE speech_samples SET hotword_sync = ?1 WHERE id = ?2",
        params![sync, id],
    )
    .context("failed to update sample hotword_sync")?;
    Ok(())
}

fn row_to_sample(row: &rusqlite::Row<'_>) -> rusqlite::Result<SampleRow> {
    Ok(SampleRow {
        id: row.get(0)?,
        segment_id: row.get(1)?,
        session_id: row.get(2)?,
        label: row.get(3)?,
        text_raw: row.get(4)?,
        text_optimized: row.get(5)?,
        text_english: row.get(6)?,
        text_secondary: row.get(7)?,
        correction: row.get(8)?,
        note: row.get(9)?,
        audio_path: row.get(10)?,
        audio_status: row.get(11)?,
        hotword_sync: row.get(12)?,
        marked_at: row.get(13)?,
        source: row.get(14)?,
        segment_ids: row.get(15)?,
        app_exe: row.get(16)?,
        app_path: row.get(17)?,
        app_title: row.get(18)?,
        app_class: row.get(19)?,
        delivery_mode: row.get(20)?,
    })
}

const SAMPLE_COLS: &str = "id, segment_id, session_id, label, text_raw, text_optimized,
        text_english, text_secondary, correction, note,
        audio_path, audio_status, hotword_sync, marked_at,
        source, segment_ids,
        app_exe, app_path, app_title, app_class, delivery_mode";

/// 读取单条样本。
pub fn get_sample(conn: &Connection, id: i64) -> Result<Option<SampleRow>> {
    let sql = format!("SELECT {SAMPLE_COLS} FROM speech_samples WHERE id = ?1");
    let mut stmt = conn.prepare(&sql).context("failed to prepare get_sample")?;
    let mut rows = stmt
        .query(params![id])
        .context("failed to query get_sample")?;
    if let Some(row) = rows.next().context("failed to read sample row")? {
        Ok(Some(row_to_sample(row)?))
    } else {
        Ok(None)
    }
}

/// 列出全部样本，按 marked_at 倒序。
pub fn list_samples(conn: &Connection) -> Result<Vec<SampleRow>> {
    let sql = format!("SELECT {SAMPLE_COLS} FROM speech_samples ORDER BY marked_at DESC, id DESC");
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare list_samples")?;
    let rows = stmt
        .query_map([], row_to_sample)
        .context("failed to query list_samples")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("failed to read sample row")?);
    }
    Ok(out)
}

pub fn upsert_setting(conn: &Connection, key: &str, value: &str, now: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_settings(key, value, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, now],
    )
    .with_context(|| format!("failed to upsert setting: {key}"))?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT value FROM app_settings WHERE key = ?1")
        .with_context(|| format!("failed to prepare get_setting for {key}"))?;
    let mut rows = stmt
        .query(params![key])
        .context("failed to query setting")?;
    if let Some(row) = rows.next().context("failed to read setting row")? {
        Ok(Some(row.get(0)?))
    } else {
        Ok(None)
    }
}
