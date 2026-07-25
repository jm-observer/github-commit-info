use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, Connection};
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

// ---- 场景记录（speech_scenes）----

/// 一次交付的落库入参。应用上下文可能全空（抓拍失败 / 非 Windows），留空不猜。
pub struct NewScene {
    pub session_id: Option<String>,
    pub segment_id: i64,
    /// `auto_paste` | `auto_copy`
    pub delivery_mode: String,
    /// `optimized_zh` | `english`
    pub content_kind: String,
    pub text: String,
    pub app: SampleAppContext,
    pub delivered_at: String,
}

/// 按应用聚合的一行统计。
#[derive(Debug, Clone, Serialize)]
pub struct SceneAppStat {
    /// 抓拍失败时为 `None`，前端显示为「未知」。
    pub app_exe: Option<String>,
    pub count: i64,
    pub chars: i64,
    pub last_at: Option<String>,
}

/// 按「应用 + 窗口标题」聚合的一行统计。比 [`SceneAppStat`] 细一层：浏览器/编辑器里 exe 相同、
/// title 才是区分具体场景（哪个网站、哪个文档、跟谁聊天）的唯一信号。**收集期只做原始聚合、
/// 不解析归纳**（提取域名/文档名等留给数据摊开后再定），故这里就是 exe+title 原样分组计数。
#[derive(Debug, Clone, Serialize)]
pub struct SceneTitleStat {
    pub app_exe: Option<String>,
    /// 无标题窗口 / 抓拍失败为 `None`，前端显示为「（无标题）」。
    pub app_title: Option<String>,
    pub count: i64,
    pub chars: i64,
    pub last_at: Option<String>,
}

/// 场景记录总览：给桌面端一眼看出「在涨」的几个数。
#[derive(Debug, Clone, Serialize)]
pub struct SceneStats {
    pub total: i64,
    pub today: i64,
    pub total_chars: i64,
    /// 抓到应用上下文的条数——这个数与 total 的差距就是抓拍覆盖率。
    pub with_app: i64,
    pub distinct_apps: i64,
    /// 被纠错样本回标过的记录数（`correction_sample_id IS NOT NULL`）。统计真实表达风格时
    /// 应排除这部分——它们的交付文本含 ASR/LLM 错误，不是用户想说的。
    pub corrected: i64,
    pub last_at: Option<String>,
    /// 按条数倒序的应用排行（最多 12 项，够看趋势又不至于刷屏）。
    pub top_apps: Vec<SceneAppStat>,
    /// 按「应用 + 窗口标题」的具体场景排行（最多 15 项）。title 基数大，只取头部。
    pub top_titles: Vec<SceneTitleStat>,
}

/// 插入一条场景记录，返回自增 id。
pub fn insert_scene(conn: &Connection, d: &NewScene) -> Result<i64> {
    conn.execute(
        "INSERT INTO speech_scenes(
            session_id, segment_id, delivery_mode, content_kind, text, char_count,
            app_exe, app_path, app_title, app_class, delivered_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            d.session_id,
            d.segment_id,
            d.delivery_mode,
            d.content_kind,
            d.text,
            d.text.chars().count() as i64,
            d.app.app_exe,
            d.app.app_path,
            d.app.app_title,
            d.app.app_class,
            d.delivered_at,
        ],
    )
    .context("failed to insert speech delivery")?;
    Ok(conn.last_insert_rowid())
}

/// 把 `segment_ids` 命中的场景记录回标为「被 `sample_id` 这条纠错样本改过」，返回受影响行数。
///
/// - 只标 `correction_sample_id IS NULL` 的行：一条场景记录归属首次纠正它的样本，归属稳定
///   （同段被改两次时不来回抢标；具体哪条样本是次要信息，"被改过"这个事实才是关键）。
/// - 段号可能跨多条场景记录（auto_paste 一段长话分几次打字各记一行），故用 `IN`。
/// - `segment_ids` 为空直接返回 0，不发空 `IN ()`。
pub fn mark_scenes_corrected(
    conn: &Connection,
    segment_ids: &[i64],
    sample_id: i64,
) -> Result<usize> {
    if segment_ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; segment_ids.len()].join(",");
    let sql = format!(
        "UPDATE speech_scenes SET correction_sample_id = ?
         WHERE segment_id IN ({placeholders}) AND correction_sample_id IS NULL"
    );
    let mut params: Vec<i64> = Vec::with_capacity(segment_ids.len() + 1);
    params.push(sample_id);
    params.extend_from_slice(segment_ids);
    let n = conn
        .execute(&sql, params_from_iter(params))
        .context("failed to mark scenes corrected")?;
    Ok(n)
}

/// 汇总场景记录。`today_prefix` 传本地日期前缀（`YYYY-MM-DD`）——`delivered_at` 存的是本地
/// 时间字符串，用前缀比较即可，不引入时区换算。
pub fn scene_stats(conn: &Connection, today_prefix: &str) -> Result<SceneStats> {
    let mut stmt = conn
        .prepare(
            "SELECT COUNT(*),
                    COALESCE(SUM(char_count), 0),
                    COALESCE(SUM(app_exe IS NOT NULL), 0),
                    COUNT(DISTINCT app_exe),
                    COALESCE(SUM(correction_sample_id IS NOT NULL), 0),
                    MAX(delivered_at)
             FROM speech_scenes",
        )
        .context("failed to prepare scene_stats")?;
    let (total, total_chars, with_app, distinct_apps, corrected, last_at) = stmt
        .query_row([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .context("failed to query scene_stats")?;

    let today: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM speech_scenes WHERE delivered_at LIKE ?1 || '%'",
            params![today_prefix],
            |row| row.get(0),
        )
        .context("failed to query today's delivery count")?;

    let mut stmt = conn
        .prepare(
            "SELECT app_exe, COUNT(*) n, COALESCE(SUM(char_count), 0), MAX(delivered_at)
             FROM speech_scenes
             GROUP BY app_exe
             ORDER BY n DESC
             LIMIT 12",
        )
        .context("failed to prepare delivery app ranking")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SceneAppStat {
                app_exe: row.get(0)?,
                count: row.get(1)?,
                chars: row.get(2)?,
                last_at: row.get(3)?,
            })
        })
        .context("failed to query delivery app ranking")?;
    let mut top_apps = Vec::new();
    for r in rows {
        top_apps.push(r.context("failed to read delivery app stat row")?);
    }

    // 按「应用 + 窗口标题」的具体场景排行。SQLite 的 GROUP BY 把 NULL 归为同一组，
    // 故无标题的记录会聚成一行（app_title = NULL），前端另行显示「（无标题）」。
    let mut stmt = conn
        .prepare(
            "SELECT app_exe, app_title, COUNT(*) n, COALESCE(SUM(char_count), 0), MAX(delivered_at)
             FROM speech_scenes
             GROUP BY app_exe, app_title
             ORDER BY n DESC
             LIMIT 15",
        )
        .context("failed to prepare scene title ranking")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SceneTitleStat {
                app_exe: row.get(0)?,
                app_title: row.get(1)?,
                count: row.get(2)?,
                chars: row.get(3)?,
                last_at: row.get(4)?,
            })
        })
        .context("failed to query scene title ranking")?;
    let mut top_titles = Vec::new();
    for r in rows {
        top_titles.push(r.context("failed to read scene title stat row")?);
    }

    Ok(SceneStats {
        total,
        today,
        total_chars,
        with_app,
        distinct_apps,
        corrected,
        last_at,
        top_apps,
        top_titles,
    })
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
