pub mod repository;
pub mod schema;

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::Local;
use rusqlite::Connection;

/// Cloneable handle to the speech SQLite database.
#[derive(Clone)]
pub struct SpeechDatabase {
    conn: Arc<Mutex<Connection>>,
}

impl SpeechDatabase {
    pub async fn init(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create database directory: {}", parent.display())
            })?;
        }

        let db_path = db_path.to_path_buf();
        let conn = tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path)
                .with_context(|| format!("failed to open speech db at {}", db_path.display()))?;
            schema::run_migrations(&conn)?;
            Ok::<Connection, anyhow::Error>(conn)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn upsert_setting(&self, key: String, value: String) -> Result<()> {
        let now = now_str();
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::upsert_setting(&guard, &key, &value, &now)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(())
    }

    pub async fn get_setting(&self, key: String) -> Result<Option<String>> {
        let conn = Arc::clone(&self.conn);
        let result = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::get_setting(&guard, &key)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(result)
    }

    /// 插入一条标注样本，返回自增 id。
    pub async fn insert_sample(&self, new: repository::NewSample) -> Result<i64> {
        let conn = Arc::clone(&self.conn);
        let id = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::insert_sample(&guard, &new)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(id)
    }

    /// 更新样本音频落盘结果。
    pub async fn update_sample_audio(
        &self,
        id: i64,
        audio_path: Option<String>,
        audio_status: String,
    ) -> Result<()> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::update_sample_audio(&guard, id, audio_path.as_deref(), &audio_status)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(())
    }

    /// 更新样本热词同步结果。
    pub async fn update_sample_hotword_sync(&self, id: i64, sync: String) -> Result<()> {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::update_sample_hotword_sync(&guard, id, &sync)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(())
    }

    /// 读取单条样本。
    pub async fn get_sample(&self, id: i64) -> Result<Option<repository::SampleRow>> {
        let conn = Arc::clone(&self.conn);
        let row = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::get_sample(&guard, id)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(row)
    }

    /// 记一次交付（日常全量收集，与手动采集的样本分开）。
    pub async fn insert_scene(&self, new: repository::NewScene) -> Result<i64> {
        let conn = Arc::clone(&self.conn);
        let id = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::insert_scene(&guard, &new)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(id)
    }

    /// 用累计全文改写已有的那行场景记录（`auto_paste` 合并链），命中返回 `true`。
    pub async fn update_scene_text(
        &self,
        id: i64,
        text: String,
        app: repository::SampleAppContext,
        delivered_at: String,
    ) -> Result<bool> {
        let conn = Arc::clone(&self.conn);
        let hit = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::update_scene_text(&guard, id, &text, &app, &delivered_at)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(hit)
    }

    /// 把纠错样本 `sample_id` 关联到它改动的那几条场景记录（按段号回标），返回受影响行数。
    ///
    /// `since` 是 `delivered_at` 的下界（本地时间字符串），`session_id` 是可选的会话过滤——
    /// 两者共同防止「换服务端后段号重头再来」误标历史记录，见 `repository::mark_scenes_corrected`。
    pub async fn mark_scenes_corrected(
        &self,
        segment_ids: Vec<i64>,
        sample_id: i64,
        since: String,
        session_id: Option<String>,
    ) -> Result<usize> {
        let conn = Arc::clone(&self.conn);
        let n = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::mark_scenes_corrected(
                &guard,
                &segment_ids,
                sample_id,
                &since,
                session_id.as_deref(),
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(n)
    }

    /// 场景记录总览（总数/今日/按应用排行）。
    pub async fn scene_stats(&self) -> Result<repository::SceneStats> {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let conn = Arc::clone(&self.conn);
        let stats = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::scene_stats(&guard, &today)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(stats)
    }

    /// 全部场景记录的交付文本（热词候选挖掘的输入）。
    pub async fn list_scene_texts(&self) -> Result<Vec<String>> {
        let conn = Arc::clone(&self.conn);
        let rows = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::list_scene_texts(&guard)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(rows)
    }

    /// 列出全部样本（marked_at 倒序）。
    pub async fn list_samples(&self) -> Result<Vec<repository::SampleRow>> {
        let conn = Arc::clone(&self.conn);
        let rows = tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("db mutex poisoned"))?;
            repository::list_samples(&guard)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))??;
        Ok(rows)
    }
}

fn now_str() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
