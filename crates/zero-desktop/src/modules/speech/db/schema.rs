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
    // 0009：老库里这张表叫 speech_deliveries（改名前的名字）。必须赶在 0008 建表**之前**处理，
    // 否则 IF NOT EXISTS 会先建出一张空的 speech_scenes，历史行永远留在旧表里成为孤儿。
    // 见 migrations/0009_rename_speech_deliveries.sql。
    if table_exists(conn, "speech_deliveries")? {
        if table_exists(conn, "speech_scenes")? {
            // 两表并存（先跑过新版、又退回老版写了旧表）：按列子集搬行，不带 id，交给自增避免
            // 主键冲突；搬完删旧表，收敛到「旧表不存在」。
            conn.execute_batch(
                "INSERT INTO speech_scenes
                   (session_id, segment_id, delivery_mode, content_kind, text, char_count,
                    app_exe, app_path, app_title, app_class, delivered_at)
                 SELECT session_id, segment_id, delivery_mode, content_kind, text, char_count,
                        app_exe, app_path, app_title, app_class, delivered_at
                 FROM speech_deliveries;
                 DROP TABLE speech_deliveries;",
            )
            .context("failed to merge legacy speech_deliveries into speech_scenes")?;
        } else {
            // 旧索引名基于旧表名，留着会与 0008 以新名重建的同定义索引重复 —— 先删再改名。
            conn.execute_batch(
                "DROP INDEX IF EXISTS idx_speech_deliveries_time;
                 DROP INDEX IF EXISTS idx_speech_deliveries_app_time;
                 ALTER TABLE speech_deliveries RENAME TO speech_scenes;",
            )
            .context("failed to rename speech_deliveries to speech_scenes")?;
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
    // 历史回标：回标逻辑（capture 侧 mark_scenes_corrected）上线前采集的纠错样本，其对应
    // 场景行的 correction_sample_id 全空，统计「真实表达风格」时排除不掉。这里按样本的
    // segment_ids（JSON 数组，缺失回退单列 segment_id）把空行补上。守卫与运行时同一精神：
    // - 只补 NULL 行，不抢标（运行时已标的行优先级更高）;
    // - 只认真有纠正的样本（correction 非空且 label ≠ ok）;
    // - 时间窗：交付须落在样本标注前 1 天内——段号是服务端自增计数器，换服务端后会重头
    //   再来，无界会把同号历史行误标（运行时用的是分钟级采集窗，这里放宽到天级是因为
    //   历史数据只有「标注晚于交付」这一个可靠锚点）;
    // - 会话过滤：仅两侧都已知且不同时排除（老行 session_id 恒 NULL，须放行）。
    // 每次启动都跑，天然幂等（补完即无 NULL 可补）；也顺带兜住运行时回标失败的漏行。
    conn.execute_batch(
        "UPDATE speech_scenes SET correction_sample_id = (
             SELECT sp.id FROM speech_samples sp
             JOIN json_each(COALESCE(sp.segment_ids, json_array(sp.segment_id))) je
               ON je.value = speech_scenes.segment_id
             WHERE sp.correction IS NOT NULL AND sp.label <> 'ok'
               AND speech_scenes.delivered_at <= sp.marked_at
               AND speech_scenes.delivered_at >= datetime(sp.marked_at, '-1 day')
               AND (speech_scenes.session_id IS NULL OR sp.session_id IS NULL
                    OR speech_scenes.session_id = sp.session_id)
             ORDER BY sp.marked_at, sp.id LIMIT 1
         )
         WHERE correction_sample_id IS NULL AND EXISTS (
             SELECT 1 FROM speech_samples sp
             JOIN json_each(COALESCE(sp.segment_ids, json_array(sp.segment_id))) je
               ON je.value = speech_scenes.segment_id
             WHERE sp.correction IS NOT NULL AND sp.label <> 'ok'
               AND speech_scenes.delivered_at <= sp.marked_at
               AND speech_scenes.delivered_at >= datetime(sp.marked_at, '-1 day')
               AND (speech_scenes.session_id IS NULL OR sp.session_id IS NULL
                    OR speech_scenes.session_id = sp.session_id)
         );",
    )
    .context("failed to backfill speech_scenes.correction_sample_id")?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .with_context(|| format!("failed to check table existence for {table}"))?;
    Ok(n > 0)
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

    /// 历史回标：回标逻辑上线前采集的样本，重跑迁移时把对应场景行的
    /// correction_sample_id 补上；无纠正的样本（label=ok）不参与；已标行不被抢标。
    #[test]
    fn migration_backfills_correction_links() {
        use crate::modules::speech::db::repository::NewScene;

        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let scene = |seg: i64, at: &str| NewScene {
            session_id: None,
            segment_id: seg,
            delivery_mode: "auto_paste".into(),
            content_kind: "optimized_zh".into(),
            text: "文本".into(),
            app: SampleAppContext::default(),
            delivered_at: at.into(),
        };
        // seg 1/2 = 被纠错样本覆盖的历史交付；seg 3 = 无关交付；
        // seg 4 = 同号但在时间窗外（样本标注前 1 天以上），不得误标。
        repository::insert_scene(&conn, &scene(1, "2026-07-25 09:00:00")).unwrap();
        repository::insert_scene(&conn, &scene(2, "2026-07-25 09:00:05")).unwrap();
        repository::insert_scene(&conn, &scene(3, "2026-07-25 09:30:00")).unwrap();
        repository::insert_scene(&conn, &scene(1, "2026-07-20 09:00:00")).unwrap();

        let sample = |seg: i64, ids: &str, label: &str, correction: Option<&str>| NewSample {
            segment_id: seg,
            session_id: None,
            label: label.into(),
            text_raw: "原文".into(),
            text_optimized: None,
            text_english: None,
            text_secondary: None,
            correction: correction.map(str::to_string),
            note: None,
            audio_status: "skipped".into(),
            marked_at: "2026-07-25 09:05:00".into(),
            source: "copy".into(),
            segment_ids: Some(ids.into()),
            app: SampleAppContext::default(),
        };
        let sid =
            repository::insert_sample(&conn, &sample(1, "[1,2]", "other", Some("改好的"))).unwrap();
        // ok 样本（无纠正）覆盖 seg 3——不得把 seg 3 标成「被改过」。
        repository::insert_sample(&conn, &sample(3, "[3]", "ok", None)).unwrap();

        // 模拟「回标逻辑上线前」：样本在库、场景行全空 → 重跑迁移即回补。
        run_migrations(&conn).unwrap();

        let corrected: Vec<(i64, Option<i64>)> = {
            let mut stmt = conn
                .prepare("SELECT segment_id, correction_sample_id FROM speech_scenes ORDER BY id")
                .unwrap();
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        assert_eq!(corrected[0], (1, Some(sid)), "窗内 seg 1 应被回补");
        assert_eq!(corrected[1], (2, Some(sid)), "窗内 seg 2 应被回补");
        assert_eq!(corrected[2], (3, None), "ok 样本不算纠错");
        assert_eq!(corrected[3], (1, None), "时间窗外的同号历史行不得误标");

        // 已标行不被抢标：再插一条覆盖 seg 1 的新样本后重跑，归属不变。
        repository::insert_sample(&conn, &sample(1, "[1]", "other", Some("又改"))).unwrap();
        run_migrations(&conn).unwrap();
        let still: Option<i64> = conn
            .query_row(
                "SELECT correction_sample_id FROM speech_scenes WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still, Some(sid));
    }

    /// 老库形态：改名前的 speech_deliveries（无 correction_sample_id、索引名带旧表名）。
    fn make_legacy_deliveries(conn: &Connection) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS speech_scenes;
             CREATE TABLE speech_deliveries (
               id            INTEGER PRIMARY KEY AUTOINCREMENT,
               session_id    TEXT,
               segment_id    INTEGER NOT NULL,
               delivery_mode TEXT NOT NULL,
               content_kind  TEXT NOT NULL,
               text          TEXT NOT NULL,
               char_count    INTEGER NOT NULL,
               app_exe       TEXT,
               app_path      TEXT,
               app_title     TEXT,
               app_class     TEXT,
               delivered_at  TEXT NOT NULL
             );
             CREATE INDEX idx_speech_deliveries_time ON speech_deliveries(delivered_at);
             CREATE INDEX idx_speech_deliveries_app_time
               ON speech_deliveries(app_exe, delivered_at);
             INSERT INTO speech_deliveries
               (session_id, segment_id, delivery_mode, content_kind, text, char_count,
                app_exe, app_title, delivered_at)
             VALUES (NULL, 5, 'auto_paste', 'optimized_zh', '四个字啊', 4,
                     'claude.exe', 'Claude', '2026-07-25 09:00:00');",
        )
        .unwrap();
    }

    fn index_exists(conn: &Connection, name: &str) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [name],
                |r| r.get(0),
            )
            .unwrap();
        n > 0
    }

    /// 0009：老库的 speech_deliveries 改名成 speech_scenes，历史行不丢、id 不变。
    #[test]
    fn migration_0009_renames_legacy_deliveries_table() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        make_legacy_deliveries(&conn);

        run_migrations(&conn).unwrap();

        assert!(
            !table_exists(&conn, "speech_deliveries").unwrap(),
            "旧表应已消失"
        );
        assert!(column_exists(&conn, "speech_scenes", "correction_sample_id").unwrap());
        // 旧索引名删干净，只留 0008 以新名重建的那套，避免同定义索引重复。
        assert!(!index_exists(&conn, "idx_speech_deliveries_time"));
        assert!(!index_exists(&conn, "idx_speech_deliveries_app_time"));
        assert!(index_exists(&conn, "idx_speech_scenes_time"));

        let s = repository::scene_stats(&conn, "2026-07-25").unwrap();
        assert_eq!(s.total, 1, "改名前的历史行必须还在");
        assert_eq!(s.total_chars, 4);
        assert_eq!(s.top_apps[0].app_exe.as_deref(), Some("claude.exe"));
        // id 原样保留（correction_sample_id 之类的外部引用不能错位）。
        let id: i64 = conn
            .query_row("SELECT id FROM speech_scenes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, 1);

        // 再跑一次：旧表已不存在，守卫整段跳过。
        run_migrations(&conn).unwrap();
        assert_eq!(
            repository::scene_stats(&conn, "2026-07-25").unwrap().total,
            1
        );
    }

    /// 0009 的另一条路径：两表并存（跑过新版又退回老版）时按列子集合并，不留孤儿行。
    #[test]
    fn migration_0009_merges_when_both_tables_exist() {
        use crate::modules::speech::db::repository::NewScene;

        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        repository::insert_scene(
            &conn,
            &NewScene {
                session_id: None,
                segment_id: 1,
                delivery_mode: "auto_paste".into(),
                content_kind: "optimized_zh".into(),
                text: "两字".into(),
                app: SampleAppContext::default(),
                delivered_at: "2026-07-25 08:00:00".into(),
            },
        )
        .unwrap();
        // 新表保留，旁边再造一张老版写出来的旧表。
        make_legacy_deliveries_keeping_scenes(&conn);

        run_migrations(&conn).unwrap();

        assert!(!table_exists(&conn, "speech_deliveries").unwrap());
        let s = repository::scene_stats(&conn, "2026-07-25").unwrap();
        assert_eq!(s.total, 2, "两边的行都在");
        assert_eq!(s.total_chars, 2 + 4);
        assert!(s
            .top_apps
            .iter()
            .any(|a| a.app_exe.as_deref() == Some("claude.exe")));
    }

    /// 同 `make_legacy_deliveries`，但不删已有的 speech_scenes。
    fn make_legacy_deliveries_keeping_scenes(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE speech_deliveries (
               id            INTEGER PRIMARY KEY AUTOINCREMENT,
               session_id    TEXT,
               segment_id    INTEGER NOT NULL,
               delivery_mode TEXT NOT NULL,
               content_kind  TEXT NOT NULL,
               text          TEXT NOT NULL,
               char_count    INTEGER NOT NULL,
               app_exe       TEXT,
               app_path      TEXT,
               app_title     TEXT,
               app_class     TEXT,
               delivered_at  TEXT NOT NULL
             );
             INSERT INTO speech_deliveries
               (session_id, segment_id, delivery_mode, content_kind, text, char_count,
                app_exe, app_title, delivered_at)
             VALUES (NULL, 5, 'auto_paste', 'optimized_zh', '四个字啊', 4,
                     'claude.exe', 'Claude', '2026-07-25 09:00:00');",
        )
        .unwrap();
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
