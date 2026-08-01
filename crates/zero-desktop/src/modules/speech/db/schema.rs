use anyhow::{Context, Result};
use rusqlite::Connection;

pub(crate) fn run_migrations(conn: &Connection) -> Result<()> {
    let sql_0001 = include_str!("../../../../migrations/0001_init.sql");
    conn.execute_batch(sql_0001)
        .context("failed to run sqlite migration 0001_init.sql")?;
    if !column_exists(conn, "asr_raw_records", "optimize_status")? {
        let sql_0002 = include_str!("../../../../migrations/0002_split_optimize_translate.sql");
        conn.execute_batch(sql_0002)
            .context("failed to run sqlite migration 0002_split_optimize_translate.sql")?;
    }
    if !column_exists(conn, "asr_raw_records", "segment_id")? {
        let sql_0003 = include_str!("../../../../migrations/0003_add_segment_id.sql");
        conn.execute_batch(sql_0003)
            .context("failed to run sqlite migration 0003_add_segment_id.sql")?;
    }
    if !column_exists(conn, "asr_raw_records", "is_discarded")? {
        let sql_0004 = include_str!("../../../../migrations/0004_add_discard_fields.sql");
        conn.execute_batch(sql_0004)
            .context("failed to run sqlite migration 0004_add_discard_fields.sql")?;
    }
    // Backfill legacy rows introduced with default segment_id=0.
    conn.execute_batch("UPDATE asr_raw_records SET segment_id = id WHERE segment_id = 0;")
        .context("failed to backfill legacy segment_id")?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_asr_raw_records_session_seg
         ON asr_raw_records(session_id, segment_id);",
    )
    .context("failed to ensure unique index idx_asr_raw_records_session_seg")?;
    // 0005：标注样本表（CREATE TABLE IF NOT EXISTS，幂等，无条件执行）。
    let sql_0005 = include_str!("../../../../migrations/0005_speech_samples.sql");
    conn.execute_batch(sql_0005)
        .context("failed to run sqlite migration 0005_speech_samples.sql")?;
    // 0006：区分样本来源（ui/copy）+ 记录完整 segment_ids（每列独立守卫）。
    if !column_exists(conn, "speech_samples", "source")? {
        conn.execute_batch(
            "ALTER TABLE speech_samples ADD COLUMN source TEXT NOT NULL DEFAULT 'ui';",
        )
        .context("failed to run sqlite migration 0006 (source column)")?;
    }
    if !column_exists(conn, "speech_samples", "segment_ids")? {
        conn.execute_batch("ALTER TABLE speech_samples ADD COLUMN segment_ids TEXT;")
            .context("failed to run sqlite migration 0006 (segment_ids column)")?;
    }
    // 0007：交付时的应用上下文（同音字纠错数据收集期，宽记原始事实、不做归类）。
    // 每列独立守卫，便于将来单独追加（如数据表明需要补 app_profile 分组列时）。
    for column in [
        "app_exe",
        "app_path",
        "app_title",
        "app_class",
        "delivery_mode",
    ] {
        if !column_exists(conn, "speech_samples", column)? {
            conn.execute_batch(&format!(
                "ALTER TABLE speech_samples ADD COLUMN {column} TEXT;"
            ))
            .with_context(|| format!("failed to run sqlite migration 0007 ({column} column)"))?;
        }
    }
    // 0008：标注时该段被识别成的说话人（声纹识别错误标注 speaker_wrong 的「错成谁」）。
    if !column_exists(conn, "speech_samples", "speaker")? {
        conn.execute_batch("ALTER TABLE speech_samples ADD COLUMN speaker TEXT;")
            .context("failed to run sqlite migration 0008 (speaker column)")?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("failed to prepare table info query for {table}"))?;
    let mut rows = stmt
        .query([])
        .with_context(|| format!("failed to query table info for {table}"))?;
    while let Some(row) = rows.next().context("failed to read table info row")? {
        let name: String = row.get(1).context("failed to get table column name")?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::speech::db::repository::{self, NewSample, SampleAppContext};

    #[test]
    fn migrations_create_speech_samples_and_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        // 表存在且 0005/0006/0007 列齐全。
        assert!(column_exists(&conn, "speech_samples", "audio_status").unwrap());
        assert!(column_exists(&conn, "speech_samples", "hotword_sync").unwrap());
        assert!(column_exists(&conn, "speech_samples", "source").unwrap());
        assert!(column_exists(&conn, "speech_samples", "segment_ids").unwrap());
        for column in [
            "app_exe",
            "app_path",
            "app_title",
            "app_class",
            "delivery_mode",
        ] {
            assert!(
                column_exists(&conn, "speech_samples", column).unwrap(),
                "0007 column missing: {column}"
            );
        }
        assert!(column_exists(&conn, "speech_samples", "speaker").unwrap());

        // 插入 → 列出 → 读回。
        let id = repository::insert_sample(
            &conn,
            &NewSample {
                segment_id: 42,
                session_id: Some("sess-1".into()),
                label: "hotword".into(),
                text_raw: "旧菜盒子".into(),
                text_optimized: None,
                text_english: None,
                text_secondary: None,
                correction: Some("韭菜盒子".into()),
                note: None,
                audio_status: "skipped".into(),
                marked_at: "2026-06-15 10:00:00".into(),
                source: "ui".into(),
                segment_ids: None,
                app: SampleAppContext {
                    app_exe: Some("Code.exe".into()),
                    app_path: Some(r"C:\Program Files\Code\Code.exe".into()),
                    app_title: Some("main.rs - toolkit".into()),
                    app_class: Some("Chrome_WidgetWin_1".into()),
                    delivery_mode: Some("auto_paste".into()),
                },
                speaker: None,
            },
        )
        .unwrap();
        repository::update_sample_audio(&conn, id, Some("/x/1.wav"), "saved").unwrap();
        repository::update_sample_hotword_sync(&conn, id, "added").unwrap();

        let rows = repository::list_samples(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.segment_id, 42);
        assert_eq!(r.audio_status, "saved");
        assert_eq!(r.audio_path.as_deref(), Some("/x/1.wav"));
        assert_eq!(r.hotword_sync.as_deref(), Some("added"));

        let one = repository::get_sample(&conn, id).unwrap().unwrap();
        assert_eq!(one.label, "hotword");
        assert_eq!(one.correction.as_deref(), Some("韭菜盒子"));
        // 0007：应用上下文原样往返。
        assert_eq!(one.app_exe.as_deref(), Some("Code.exe"));
        assert_eq!(one.app_title.as_deref(), Some("main.rs - toolkit"));
        assert_eq!(one.delivery_mode.as_deref(), Some("auto_paste"));
    }

    /// 老库（已有 0005/0006 但无 0007 列）重跑迁移应幂等补列，且不丢已有行。
    #[test]
    fn migration_0007_is_additive_on_existing_db() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        repository::insert_sample(
            &conn,
            &NewSample {
                segment_id: 1,
                session_id: None,
                label: "other".into(),
                text_raw: "旧行".into(),
                text_optimized: None,
                text_english: None,
                text_secondary: None,
                correction: None,
                note: None,
                audio_status: "skipped".into(),
                marked_at: "2026-07-24 10:00:00".into(),
                source: "copy".into(),
                segment_ids: None,
                app: SampleAppContext::default(),
                speaker: None,
            },
        )
        .unwrap();

        // 重跑：列已存在，守卫应跳过 ALTER 而非报错。
        run_migrations(&conn).unwrap();

        let rows = repository::list_samples(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text_raw, "旧行");
        // 无交付上下文时五列为空，不编造。
        assert!(rows[0].app_exe.is_none());
        assert!(rows[0].delivery_mode.is_none());
        // 0008：未提供说话人时留空。
        assert!(rows[0].speaker.is_none());
    }

    /// 0008：声纹识别错误样本的「错成谁 → 应该是谁」成对往返。
    #[test]
    fn speaker_wrong_sample_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let id = repository::insert_sample(
            &conn,
            &NewSample {
                segment_id: 7,
                session_id: None,
                label: "speaker_wrong".into(),
                text_raw: "Thank you.".into(),
                text_optimized: None,
                text_english: None,
                text_secondary: None,
                correction: Some("fengqi".into()),
                note: None,
                audio_status: "skipped".into(),
                marked_at: "2026-07-30 19:41:46".into(),
                source: "ui".into(),
                segment_ids: None,
                app: SampleAppContext::default(),
                speaker: Some("guest-2".into()),
            },
        )
        .unwrap();

        let one = repository::get_sample(&conn, id).unwrap().unwrap();
        assert_eq!(one.label, "speaker_wrong");
        assert_eq!(one.speaker.as_deref(), Some("guest-2"));
        assert_eq!(one.correction.as_deref(), Some("fengqi"));
    }
}
