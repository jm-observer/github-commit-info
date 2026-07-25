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
    // 0008：场景记录表（每次交付都记，与手动采集的 speech_samples 分开）。
    // 建表 + 索引都是 IF NOT EXISTS，幂等，无条件执行。
    let sql_0008 = include_str!("../../../../migrations/0008_speech_scenes.sql");
    conn.execute_batch(sql_0008)
        .context("failed to run sqlite migration 0008_speech_scenes.sql")?;
    // 守卫：若某库已在加 correction_sample_id 之前建过 speech_scenes（IF NOT EXISTS 会跳过
    // 建表、不补新列），单独 ALTER 补上。新库走建表语句已含此列，这里跳过。
    if !column_exists(conn, "speech_scenes", "correction_sample_id")? {
        conn.execute_batch("ALTER TABLE speech_scenes ADD COLUMN correction_sample_id INTEGER;")
            .context("failed to add speech_scenes.correction_sample_id")?;
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

    /// 0008：场景记录表建起来、能往返，且统计口径正确（总数/今日/按应用聚合）。
    #[test]
    fn migration_0008_scenes_roundtrip_and_stats() {
        use crate::modules::speech::db::repository::NewScene;

        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        // 重跑一次：建表/建索引都是 IF NOT EXISTS，必须幂等。
        run_migrations(&conn).unwrap();

        let mk = |seg: i64, mode: &str, exe: Option<&str>, text: &str, at: &str| NewScene {
            session_id: None,
            segment_id: seg,
            delivery_mode: mode.into(),
            content_kind: "optimized_zh".into(),
            text: text.into(),
            app: SampleAppContext {
                app_exe: exe.map(str::to_string),
                app_path: None,
                app_title: None,
                app_class: None,
                delivery_mode: Some(mode.into()),
            },
            delivered_at: at.into(),
        };
        repository::insert_scene(
            &conn,
            &mk(
                1,
                "auto_paste",
                Some("Code.exe"),
                "四个字啊",
                "2026-07-25 09:00:00",
            ),
        )
        .unwrap();
        repository::insert_scene(
            &conn,
            &mk(
                2,
                "auto_copy",
                Some("Code.exe"),
                "两字",
                "2026-07-25 10:00:00",
            ),
        )
        .unwrap();
        // 抓拍失败的一条：app_exe 为空，仍要入库（记原始事实），但不计入 with_app。
        repository::insert_scene(
            &conn,
            &mk(3, "auto_paste", None, "三个字", "2026-07-24 23:00:00"),
        )
        .unwrap();

        let s = repository::scene_stats(&conn, "2026-07-25").unwrap();
        assert_eq!(s.total, 3);
        assert_eq!(s.today, 2, "只统计 delivered_at 前缀匹配当天的");
        assert_eq!(s.with_app, 2, "抓拍失败的那条不计入");
        assert_eq!(s.distinct_apps, 1, "COUNT(DISTINCT app_exe) 不计 NULL");
        assert_eq!(s.total_chars, 4 + 2 + 3);
        assert_eq!(s.last_at.as_deref(), Some("2026-07-25 10:00:00"));

        let top = &s.top_apps;
        assert_eq!(top[0].app_exe.as_deref(), Some("Code.exe"));
        assert_eq!(top[0].count, 2);
        assert_eq!(top[0].chars, 6);
        // 未知应用单独成组，不被丢弃——覆盖率缺口要看得见。
        assert!(top.iter().any(|a| a.app_exe.is_none() && a.count == 1));
    }

    /// 待办 1：纠错样本落库后按段号回标场景记录，统计能区分「被改过」的记录。
    #[test]
    fn scene_correction_linkage() {
        use crate::modules::speech::db::repository::NewScene;

        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let scene = |seg: i64, at: &str| NewScene {
            session_id: None,
            segment_id: seg,
            delivery_mode: "auto_paste".into(),
            content_kind: "optimized_zh".into(),
            text: "文本".into(),
            app: SampleAppContext {
                app_exe: Some("Code.exe".into()),
                app_path: None,
                app_title: None,
                app_class: None,
                delivery_mode: Some("auto_paste".into()),
            },
            delivered_at: at.into(),
        };
        // 一个 burst 分两段交付各记一行（seg 1、2），另有无关的一段（seg 9）。
        repository::insert_scene(&conn, &scene(1, "2026-07-25 09:00:00")).unwrap();
        repository::insert_scene(&conn, &scene(2, "2026-07-25 09:00:01")).unwrap();
        repository::insert_scene(&conn, &scene(9, "2026-07-25 09:30:00")).unwrap();

        let sample_id = repository::insert_sample(
            &conn,
            &NewSample {
                segment_id: 1,
                session_id: None,
                label: "other".into(),
                text_raw: "原文".into(),
                text_optimized: Some("优化稿".into()),
                text_english: None,
                text_secondary: None,
                correction: Some("改好的".into()),
                note: None,
                audio_status: "skipped".into(),
                marked_at: "2026-07-25 09:01:00".into(),
                source: "copy".into(),
                segment_ids: Some("[1,2]".into()),
                app: SampleAppContext::default(),
            },
        )
        .unwrap();

        // 时间下界先兜住：晚于这几行的 since，一条都不该标（换服务端后段号重头再来时，
        // 靠它挡住「小段号误标几个月前的历史行」）。
        let none_in_window = repository::mark_scenes_corrected(
            &conn,
            &[1, 2],
            sample_id,
            "2026-07-25 09:10:00",
            None,
        )
        .unwrap();
        assert_eq!(none_in_window, 0, "超出时间下界的历史行不该被标");

        // 回标 burst 的两段：命中 2 条，seg 9 不动。
        let n = repository::mark_scenes_corrected(
            &conn,
            &[1, 2],
            sample_id,
            "2026-07-25 08:58:00",
            None,
        )
        .unwrap();
        assert_eq!(n, 2);

        let s = repository::scene_stats(&conn, "2026-07-25").unwrap();
        assert_eq!(s.total, 3);
        assert_eq!(s.corrected, 2, "被改过的两段计入 corrected");

        // 幂等/稳定归属：再标一次，已有 correction_sample_id 的行不被抢标。
        let n2 =
            repository::mark_scenes_corrected(&conn, &[1, 2], 99999, "2026-07-25 08:58:00", None)
                .unwrap();
        assert_eq!(n2, 0);
        assert_eq!(
            repository::scene_stats(&conn, "2026-07-25")
                .unwrap()
                .corrected,
            2
        );

        // 空段号不发空 IN()，安全返回 0。
        assert_eq!(
            repository::mark_scenes_corrected(&conn, &[], sample_id, "2026-07-25 08:58:00", None)
                .unwrap(),
            0
        );
    }

    /// 会话过滤：段号会随服务端重建而重头再来，同号不同会话不得互相误标；
    /// 但会话未知的老行（`session_id IS NULL`）要放行，否则升级过渡期回标全落空。
    #[test]
    fn scene_correction_respects_session() {
        use crate::modules::speech::db::repository::NewScene;

        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let scene = |seg: i64, session: Option<&str>| NewScene {
            session_id: session.map(str::to_string),
            segment_id: seg,
            delivery_mode: "auto_paste".into(),
            content_kind: "optimized_zh".into(),
            text: "文本".into(),
            app: SampleAppContext::default(),
            delivered_at: "2026-07-25 09:00:00".into(),
        };
        repository::insert_scene(&conn, &scene(1, Some("sess-A"))).unwrap();
        repository::insert_scene(&conn, &scene(1, Some("sess-B"))).unwrap();
        repository::insert_scene(&conn, &scene(1, None)).unwrap();

        // 会话 A 的纠错只标 A 的行 + 会话未知的老行，不碰 B。
        let n = repository::mark_scenes_corrected(
            &conn,
            &[1],
            42,
            "2026-07-25 08:00:00",
            Some("sess-A"),
        )
        .unwrap();
        assert_eq!(n, 2, "命中 sess-A 与 session_id IS NULL 两行");

        let still_free: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM speech_scenes
                 WHERE session_id = 'sess-B' AND correction_sample_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_free, 1, "另一会话的同号段不该被误标");
    }

    /// 合并链：同一段的后续累计全文覆盖同一行，而不是各记一行半截话。
    #[test]
    fn scene_update_overwrites_same_row() {
        use crate::modules::speech::db::repository::NewScene;

        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let id = repository::insert_scene(
            &conn,
            &NewScene {
                session_id: None,
                segment_id: 7,
                delivery_mode: "auto_paste".into(),
                content_kind: "optimized_zh".into(),
                text: "两字".into(),
                app: SampleAppContext {
                    app_exe: Some("Code.exe".into()),
                    app_path: None,
                    app_title: Some("文档".into()),
                    app_class: None,
                    delivery_mode: Some("auto_paste".into()),
                },
                delivered_at: "2026-07-25 09:00:00".into(),
            },
        )
        .unwrap();

        // 后续增量打字：累计全文覆盖同一行；抓拍这次为空 → COALESCE 保留原有应用上下文。
        let hit = repository::update_scene_text(
            &conn,
            id,
            "两字变成了六个字",
            &SampleAppContext::default(),
            "2026-07-25 09:00:03",
        )
        .unwrap();
        assert!(hit);

        let s = repository::scene_stats(&conn, "2026-07-25").unwrap();
        assert_eq!(s.total, 1, "还是一行，没被切成半截话");
        assert_eq!(s.total_chars, 8, "字数按累计全文重算，不累加旧片段");
        assert_eq!(s.top_apps[0].app_exe.as_deref(), Some("Code.exe"));
        assert_eq!(
            s.top_titles[0].app_title.as_deref(),
            Some("文档"),
            "抓拍为空时保留原有窗口标题，不被抹成 NULL"
        );
        assert_eq!(s.last_at.as_deref(), Some("2026-07-25 09:00:03"));

        // 行已不在（用户清过库）→ 返回 false，调用方据此改走新增。
        assert!(!repository::update_scene_text(
            &conn,
            9999,
            "无主文本",
            &SampleAppContext::default(),
            "2026-07-25 09:00:04"
        )
        .unwrap());
    }

    /// 待办 3：按「应用 + 窗口标题」聚合。浏览器 exe 相同、title 才区分具体场景。
    #[test]
    fn scene_title_ranking() {
        use crate::modules::speech::db::repository::NewScene;

        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let scene = |seg: i64, exe: &str, title: Option<&str>, at: &str| NewScene {
            session_id: None,
            segment_id: seg,
            delivery_mode: "auto_paste".into(),
            content_kind: "optimized_zh".into(),
            text: "四个字啊".into(),
            app: SampleAppContext {
                app_exe: Some(exe.into()),
                app_path: None,
                app_title: title.map(str::to_string),
                app_class: None,
                delivery_mode: Some("auto_paste".into()),
            },
            delivered_at: at.into(),
        };
        // 同 exe 不同 title：title 才是区分场景的信号。
        repository::insert_scene(
            &conn,
            &scene(1, "chrome.exe", Some("A 站"), "2026-07-25 09:00:00"),
        )
        .unwrap();
        repository::insert_scene(
            &conn,
            &scene(2, "chrome.exe", Some("A 站"), "2026-07-25 09:01:00"),
        )
        .unwrap();
        repository::insert_scene(
            &conn,
            &scene(3, "chrome.exe", Some("B 站"), "2026-07-25 09:02:00"),
        )
        .unwrap();
        repository::insert_scene(&conn, &scene(4, "Code.exe", None, "2026-07-25 09:03:00"))
            .unwrap();

        let s = repository::scene_stats(&conn, "2026-07-25").unwrap();
        // 按 exe 聚合：chrome.exe 3 条居首。
        assert_eq!(s.top_apps[0].app_exe.as_deref(), Some("chrome.exe"));
        assert_eq!(s.top_apps[0].count, 3);

        // 按 exe+title 聚合：(chrome.exe, A 站) 2 条居首，(chrome.exe, B 站) 与 A 站分开。
        let t = &s.top_titles;
        assert_eq!(t[0].app_exe.as_deref(), Some("chrome.exe"));
        assert_eq!(t[0].app_title.as_deref(), Some("A 站"));
        assert_eq!(t[0].count, 2);
        assert!(t
            .iter()
            .any(|x| x.app_title.as_deref() == Some("B 站") && x.count == 1));
        // 无标题记录聚成一行（app_title = NULL），不丢弃。
        assert!(t
            .iter()
            .any(|x| x.app_exe.as_deref() == Some("Code.exe") && x.app_title.is_none()));
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
    }
}
