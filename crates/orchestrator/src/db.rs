//! Embedded persistence for the management console: sessions (recording-time
//! stats), segments (history), speakers (voiceprint library), config.
//!
//! rusqlite is sync; calls are short and traffic is low (single-user console
//! + modest segment rate), so a `Mutex<Connection>` is fine.

use std::sync::Mutex;

use rusqlite::Connection;
use serde::Serialize;

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Serialize)]
pub struct Speaker {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct SegmentRow {
    pub id: i64,
    pub session_id: String,
    pub ts: String,
    pub text: String,
    pub optimized: Option<String>,
    pub english: Option<String>,
    /// Secondary recognizer text (comparison; only present when the client
    /// opted in via `hello.want_secondary` and a secondary model was loaded).
    pub secondary: Option<String>,
    pub speaker: Option<String>,
    /// Whether per-segment audio is still retained (purged after 1 day).
    pub has_audio: bool,
}

#[derive(Serialize, Default)]
pub struct Stats {
    pub sessions: i64,
    pub segments: i64,
    pub total_recording_sec: f64,
    pub today_recording_sec: f64,
}

impl Db {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS speakers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                embedding TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS segments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                ts TEXT NOT NULL,
                text TEXT NOT NULL,
                optimized TEXT,
                english TEXT,
                t_start REAL,
                t_end REAL,
                speaker TEXT
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                dur_sec REAL NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS segment_audio (
                segment_id INTEGER PRIMARY KEY,
                wav BLOB NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )?;
        // Lightweight migration: `secondary` column added in the dual-model
        // comparison feature. ALTER ... ADD COLUMN errors if the column
        // already exists, so we swallow that one specific error.
        if let Err(e) = conn.execute("ALTER TABLE segments ADD COLUMN secondary TEXT", []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e.into());
            }
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── sessions ────────────────────────────────────────────────────────
    pub fn session_start(&self, id: &str) {
        let _ = self.lock().execute(
            "INSERT OR IGNORE INTO sessions(id, started_at) \
             VALUES(?1, datetime('now','localtime'))",
            [id],
        );
    }
    pub fn session_end(&self, id: &str, dur_sec: f64) {
        let _ = self.lock().execute(
            "UPDATE sessions SET ended_at=datetime('now','localtime'), dur_sec=?2 \
             WHERE id=?1",
            (id, dur_sec),
        );
    }

    // ── segments ────────────────────────────────────────────────────────
    #[allow(clippy::too_many_arguments)]
    pub fn segment_upsert(
        &self,
        sid: i64,
        session_id: &str,
        text: &str,
        optimized: Option<&str>,
        english: Option<&str>,
        t0: f64,
        t1: f64,
        speaker: Option<&str>,
    ) {
        let c = self.lock();
        // sid is the orchestrator's global segment id (stable per segment).
        let _ = c.execute(
            "INSERT INTO segments(id,session_id,ts,text,optimized,english,t_start,t_end,speaker)
             VALUES(?1,?2,datetime('now','localtime'),?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET text=?3, optimized=COALESCE(?4,optimized),
               english=COALESCE(?5,english), speaker=COALESCE(?8,speaker)",
            rusqlite::params![sid, session_id, text, optimized, english, t0, t1, speaker],
        );
    }

    /// User correction of the recognized text (builds a corrected sample).
    pub fn segment_set_text(&self, sid: i64, text: &str) -> bool {
        self.lock()
            .execute("UPDATE segments SET text=?2 WHERE id=?1", (sid, text))
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// Look up a single segment (for rerun / get-by-id flows).
    pub fn segment_get(&self, sid: i64) -> Option<SegmentRow> {
        self.lock()
            .query_row(
                "SELECT s.id,s.session_id,s.ts,s.text,s.optimized,s.english,s.speaker,s.secondary,
                        EXISTS(SELECT 1 FROM segment_audio a WHERE a.segment_id=s.id)
                 FROM segments s WHERE s.id=?1",
                [sid],
                |r| {
                    Ok(SegmentRow {
                        id: r.get(0)?,
                        session_id: r.get(1)?,
                        ts: r.get(2)?,
                        text: r.get(3)?,
                        optimized: r.get(4)?,
                        english: r.get(5)?,
                        speaker: r.get(6)?,
                        secondary: r.get(7)?,
                        has_audio: r.get::<_, i64>(8)? != 0,
                    })
                },
            )
            .ok()
    }

    /// Delete a segment row and its retained audio. Returns true if a row was
    /// actually removed (false if the id didn't exist).
    pub fn segment_delete(&self, sid: i64) -> bool {
        let c = self.lock();
        // best-effort: drop audio first so we don't leave orphan blobs if the
        // segments delete races (single-conn anyway, so really fine).
        let _ = c.execute("DELETE FROM segment_audio WHERE segment_id=?1", [sid]);
        c.execute("DELETE FROM segments WHERE id=?1", [sid])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// Wipe ALL transcript history (segments + retained audio). Sessions are
    /// kept (used by the recording-time stats on the overview tab). Returns
    /// the number of segment rows removed.
    pub fn segments_clear_all(&self) -> usize {
        let c = self.lock();
        let _ = c.execute("DELETE FROM segment_audio", []);
        c.execute("DELETE FROM segments", []).unwrap_or(0)
    }

    // ── per-segment audio (retained 1 day, for re-listen / download /
    //    voiceprint enrollment input / corrected-sample building) ─────────
    pub fn audio_put(&self, sid: i64, wav: &[u8]) {
        let _ = self.lock().execute(
            "INSERT INTO segment_audio(segment_id,wav,created_at)
             VALUES(?1,?2,datetime('now','localtime'))
             ON CONFLICT(segment_id) DO UPDATE SET wav=?2,
               created_at=datetime('now','localtime')",
            rusqlite::params![sid, wav],
        );
    }
    pub fn audio_get(&self, sid: i64) -> Option<Vec<u8>> {
        self.lock()
            .query_row("SELECT wav FROM segment_audio WHERE segment_id=?1", [sid], |r| r.get(0))
            .ok()
    }
    /// Purge audio blobs older than one day. Returns number removed.
    pub fn audio_purge_expired(&self) -> usize {
        self.lock()
            .execute(
                "DELETE FROM segment_audio \
                 WHERE created_at < datetime('now','localtime','-1 day')",
                [],
            )
            .unwrap_or(0)
    }

    pub fn segment_set_optimized(&self, sid: i64, opt: &str) {
        let _ = self
            .lock()
            .execute("UPDATE segments SET optimized=?2 WHERE id=?1", (sid, opt));
    }
    pub fn segment_set_english(&self, sid: i64, eng: &str) {
        let _ = self
            .lock()
            .execute("UPDATE segments SET english=?2 WHERE id=?1", (sid, eng));
    }
    /// Persist the secondary recognizer's transcription for a segment (used
    /// in dual-model comparison mode; not produced for default sessions).
    pub fn segment_set_secondary(&self, sid: i64, text: &str) {
        let _ = self
            .lock()
            .execute("UPDATE segments SET secondary=?2 WHERE id=?1", (sid, text));
    }

    /// Optimized texts for segments in the same session whose t_end falls in
    /// [before_t - window_sec, before_t). Returned oldest-first so they can
    /// be presented as chronological context to the LLM.
    pub fn segments_context_before(&self, session_id: &str, before_t: f64, window_sec: f64) -> Vec<String> {
        let c = self.lock();
        let mut stmt = match c.prepare(
            "SELECT optimized FROM segments \
             WHERE session_id=?1 AND t_end >= ?2 AND t_end < ?3 \
             AND optimized IS NOT NULL \
             ORDER BY t_end ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let from = before_t - window_sec;
        stmt.query_map(rusqlite::params![session_id, from, before_t], |r| r.get(0))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// Largest segment id on disk (0 if none). Used to seed the in-memory
    /// SEG_ID counter on startup so a restart never reuses an id and
    /// overwrites an existing segment row / its retained audio.
    pub fn max_segment_id(&self) -> i64 {
        self.lock()
            .query_row("SELECT COALESCE(MAX(id),0) FROM segments", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn segments_recent(&self, limit: i64) -> Vec<SegmentRow> {
        let c = self.lock();
        let mut stmt = match c.prepare(
            "SELECT s.id,s.session_id,s.ts,s.text,s.optimized,s.english,s.speaker,s.secondary,
                    EXISTS(SELECT 1 FROM segment_audio a WHERE a.segment_id=s.id)
             FROM segments s ORDER BY s.id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([limit], |r| {
            Ok(SegmentRow {
                id: r.get(0)?,
                session_id: r.get(1)?,
                ts: r.get(2)?,
                text: r.get(3)?,
                optimized: r.get(4)?,
                english: r.get(5)?,
                speaker: r.get(6)?,
                secondary: r.get(7)?,
                has_audio: r.get::<_, i64>(8)? != 0,
            })
        });
        rows.map(|it| it.filter_map(Result::ok).collect()).unwrap_or_default()
    }

    pub fn stats(&self) -> Stats {
        let c = self.lock();
        let g = |sql: &str| -> f64 { c.query_row(sql, [], |r| r.get::<_, f64>(0)).unwrap_or(0.0) };
        Stats {
            sessions: g("SELECT COUNT(*) FROM sessions") as i64,
            segments: g("SELECT COUNT(*) FROM segments") as i64,
            total_recording_sec: g("SELECT COALESCE(SUM(dur_sec),0) FROM sessions"),
            today_recording_sec: g("SELECT COALESCE(SUM(dur_sec),0) FROM sessions \
                 WHERE substr(started_at,1,10)=date('now','localtime')"),
        }
    }

    // ── speakers ────────────────────────────────────────────────────────
    pub fn speaker_add(&self, name: &str, embedding_csv: &str) -> anyhow::Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO speakers(name,embedding,enabled,created_at) \
             VALUES(?1,?2,1,datetime('now','localtime'))",
            (name, embedding_csv),
        )?;
        Ok(c.last_insert_rowid())
    }
    pub fn speaker_delete(&self, id: i64) {
        let _ = self.lock().execute("DELETE FROM speakers WHERE id=?1", [id]);
    }
    pub fn speaker_rename(&self, id: i64, name: &str) {
        let _ = self
            .lock()
            .execute("UPDATE speakers SET name=?2 WHERE id=?1", (id, name));
    }
    pub fn speaker_set_enabled(&self, id: i64, enabled: bool) {
        let _ = self
            .lock()
            .execute("UPDATE speakers SET enabled=?2 WHERE id=?1", (id, enabled as i64));
    }
    pub fn speakers_list(&self) -> Vec<Speaker> {
        let c = self.lock();
        let mut stmt = match c.prepare("SELECT id,name,enabled,created_at FROM speakers ORDER BY id") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok(Speaker {
                id: r.get(0)?,
                name: r.get(1)?,
                enabled: r.get::<_, i64>(2)? != 0,
                created_at: r.get(3)?,
            })
        });
        rows.map(|it| it.filter_map(Result::ok).collect()).unwrap_or_default()
    }
    /// (name, embedding) for all enabled speakers — pushed to asr for gating.
    pub fn enabled_voiceprints(&self) -> Vec<(String, Vec<f32>)> {
        let c = self.lock();
        let mut stmt = match c.prepare("SELECT name,embedding FROM speakers WHERE enabled=1") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            let csv: String = r.get(1)?;
            Ok((name, csv))
        });
        rows.map(|it| {
            it.filter_map(Result::ok)
                .map(|(n, csv)| {
                    let v = csv.split(',').filter_map(|x| x.trim().parse::<f32>().ok()).collect();
                    (n, v)
                })
                .collect()
        })
        .unwrap_or_default()
    }

    // ── config ──────────────────────────────────────────────────────────
    pub fn config_get(&self, key: &str) -> Option<String> {
        self.lock()
            .query_row("SELECT value FROM config WHERE key=?1", [key], |r| r.get(0))
            .ok()
    }
    pub fn config_set(&self, key: &str, value: &str) {
        let _ = self.lock().execute(
            "INSERT INTO config(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=?2",
            (key, value),
        );
    }
    pub fn config_all(&self) -> Vec<(String, String)> {
        let c = self.lock();
        let mut stmt = match c.prepare("SELECT key,value FROM config ORDER BY key") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)));
        rows.map(|it| it.filter_map(Result::ok).collect()).unwrap_or_default()
    }
}
