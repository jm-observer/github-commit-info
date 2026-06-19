use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::modules::speech::db;
use crate::modules::speech::llm_settings::{AutoCopyMode, LlmSettings, MAX_MERGE_WINDOW_MS};
use crate::modules::speech::lock_utils::{mutex_lock, read_lock, write_lock};
use crate::modules::speech::SpeechState;

/// Only client-side language pick (sent in the WS `hello.language` field).
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub(crate) struct VadSettings {
    pub(crate) asr_language: String,
}

impl Default for VadSettings {
    fn default() -> Self {
        Self {
            asr_language: "zh".to_string(),
        }
    }
}

/// Built-in default orchestrator URL.
pub(crate) const DEFAULT_REMOTE_URL: &str = "ws://192.168.0.68:8090/stream";

/// Combined settings DTO exchanged with the frontend.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct CombinedSettings {
    pub(crate) asr_language: String,
    pub(crate) auto_copy_mode: AutoCopyMode,
    pub(crate) merge_window_ms: u64,
    pub(crate) remote_url: String,
    pub(crate) remote_url_presets: Vec<String>,
    #[serde(default)]
    pub(crate) want_secondary: bool,
    #[serde(default = "default_notify_sound_dto")]
    pub(crate) notify_sound: bool,
    #[serde(default)]
    pub(crate) auto_paste: bool,
    #[serde(default)]
    pub(crate) auto_paste_rewrite_retype: bool,
    #[serde(default)]
    pub(crate) auto_start: bool,
}

fn default_notify_sound_dto() -> bool {
    true
}

pub(crate) fn get_settings_from_state(state: &SpeechState) -> Result<CombinedSettings, String> {
    info!(target: "speech", "[get_settings]");
    let vad = read_lock(&state.settings).clone();
    let llm = read_lock(&state.llm_settings).clone();
    let url = read_lock(&state.remote_url).clone();
    let presets = read_lock(&state.remote_url_presets).clone();
    Ok(CombinedSettings {
        asr_language: vad.asr_language,
        auto_copy_mode: llm.auto_copy_mode,
        merge_window_ms: llm.merge_window_ms,
        remote_url: url,
        remote_url_presets: presets,
        want_secondary: llm.want_secondary,
        notify_sound: llm.notify_sound,
        auto_paste: llm.auto_paste,
        auto_paste_rewrite_retype: llm.auto_paste_rewrite_retype,
        auto_start: llm.auto_start,
    })
}

fn validate_language(s: &str) -> Result<(), String> {
    if !matches!(s, "" | "zh" | "en" | "ja" | "ko" | "yue") {
        return Err("asr_language must be one of '', zh, en, ja, ko, yue".to_string());
    }
    Ok(())
}

fn validate_url(s: &str) -> Result<(), String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("remote_url 不能为空".to_string());
    }
    if !(trimmed.starts_with("ws://") || trimmed.starts_with("wss://")) {
        return Err("remote_url 必须以 ws:// 或 wss:// 开头".to_string());
    }
    Ok(())
}

pub(crate) async fn apply_settings_to_state(
    new_settings: CombinedSettings,
    state: &SpeechState,
) -> Result<(), String> {
    info!(target: "speech", "[apply_settings]");
    validate_language(&new_settings.asr_language)?;
    validate_url(&new_settings.remote_url)?;

    let mut cleaned_presets: Vec<String> = Vec::new();
    for p in &new_settings.remote_url_presets {
        let t = p.trim();
        if t.is_empty() || t == DEFAULT_REMOTE_URL {
            continue;
        }
        if !(t.starts_with("ws://") || t.starts_with("wss://")) {
            continue;
        }
        let s = t.to_string();
        if !cleaned_presets.contains(&s) {
            cleaned_presets.push(s);
        }
    }

    let new_vad = VadSettings {
        asr_language: new_settings.asr_language,
    };
    let new_llm = LlmSettings {
        auto_copy_mode: new_settings.auto_copy_mode,
        merge_window_ms: new_settings.merge_window_ms.min(MAX_MERGE_WINDOW_MS),
        want_secondary: new_settings.want_secondary,
        notify_sound: new_settings.notify_sound,
        auto_paste: new_settings.auto_paste,
        auto_paste_rewrite_retype: new_settings.auto_paste_rewrite_retype,
        auto_start: new_settings.auto_start,
    };
    let new_url = new_settings.remote_url.trim().to_string();

    let settings_arc = Arc::clone(&state.settings);
    let llm_arc = Arc::clone(&state.llm_settings);
    let url_arc = Arc::clone(&state.remote_url);
    let presets_arc = Arc::clone(&state.remote_url_presets);
    let db_arc = Arc::clone(&state.db);

    *write_lock(&settings_arc) = new_vad.clone();
    *write_lock(&llm_arc) = new_llm.clone();
    *write_lock(&url_arc) = new_url.clone();
    *write_lock(&presets_arc) = cleaned_presets.clone();

    let db = {
        let guard = mutex_lock(&db_arc);
        guard.as_ref().cloned().ok_or("Database not initialized")?
    };
    db.upsert_setting("asr.language".to_string(), new_vad.asr_language)
        .await
        .map_err(|e| e.to_string())?;
    db.upsert_setting(
        "llm.auto_copy_mode".to_string(),
        match new_llm.auto_copy_mode {
            AutoCopyMode::Off => "off",
            AutoCopyMode::English => "english",
            AutoCopyMode::OptimizedZh => "optimized_zh",
        }
        .to_string(),
    )
    .await
    .map_err(|e| e.to_string())?;
    db.upsert_setting(
        "llm.merge_window_ms".to_string(),
        new_llm.merge_window_ms.to_string(),
    )
    .await
    .map_err(|e| e.to_string())?;
    db.upsert_setting(
        "ui.want_secondary".to_string(),
        if new_llm.want_secondary {
            "1".into()
        } else {
            "0".into()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    db.upsert_setting(
        "ui.notify_sound".to_string(),
        if new_llm.notify_sound {
            "1".into()
        } else {
            "0".into()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    db.upsert_setting(
        "llm.auto_paste".to_string(),
        if new_llm.auto_paste {
            "1".into()
        } else {
            "0".into()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    db.upsert_setting(
        "llm.auto_paste_rewrite_retype".to_string(),
        if new_llm.auto_paste_rewrite_retype {
            "1".into()
        } else {
            "0".into()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    db.upsert_setting(
        "ui.auto_start".to_string(),
        if new_llm.auto_start {
            "1".into()
        } else {
            "0".into()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    db.upsert_setting("remote.url".to_string(), new_url)
        .await
        .map_err(|e| e.to_string())?;
    let presets_json = serde_json::to_string(&cleaned_presets).map_err(|e| e.to_string())?;
    db.upsert_setting("remote.url_presets".to_string(), presets_json)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) async fn load_vad_settings_from_db(db: &db::SpeechDatabase) -> VadSettings {
    let mut s = VadSettings::default();
    if let Ok(Some(v)) = db.get_setting("asr.language".to_string()).await {
        if matches!(v.as_str(), "" | "zh" | "en" | "ja" | "ko" | "yue") {
            s.asr_language = v;
        }
    }
    s
}

pub(crate) async fn load_llm_settings_from_db(db: &db::SpeechDatabase) -> LlmSettings {
    let mut s = LlmSettings::default();
    if let Ok(Some(v)) = db.get_setting("llm.auto_copy_mode".to_string()).await {
        s.auto_copy_mode = match v.as_str() {
            "off" => AutoCopyMode::Off,
            "optimized_zh" => AutoCopyMode::OptimizedZh,
            _ => AutoCopyMode::English,
        };
    }
    if let Ok(Some(v)) = db.get_setting("llm.merge_window_ms".to_string()).await {
        if let Ok(n) = v.parse::<u64>() {
            s.merge_window_ms = n.min(MAX_MERGE_WINDOW_MS);
        }
    }
    if let Ok(Some(v)) = db.get_setting("ui.want_secondary".to_string()).await {
        s.want_secondary = !matches!(v.as_str(), "0" | "off" | "false" | "");
    }
    if let Ok(Some(v)) = db.get_setting("ui.notify_sound".to_string()).await {
        s.notify_sound = !matches!(v.as_str(), "0" | "off" | "false");
    }
    if let Ok(Some(v)) = db.get_setting("llm.auto_paste".to_string()).await {
        s.auto_paste = matches!(v.as_str(), "1" | "on" | "true");
    }
    if let Ok(Some(v)) = db
        .get_setting("llm.auto_paste_rewrite_retype".to_string())
        .await
    {
        s.auto_paste_rewrite_retype = matches!(v.as_str(), "1" | "on" | "true");
    }
    if let Ok(Some(v)) = db.get_setting("ui.auto_start".to_string()).await {
        s.auto_start = matches!(v.as_str(), "1" | "on" | "true");
    }
    s
}

pub(crate) async fn load_remote_settings_from_db(db: &db::SpeechDatabase) -> (String, Vec<String>) {
    let url = match db.get_setting("remote.url".to_string()).await {
        Ok(Some(v))
            if !v.trim().is_empty() && (v.starts_with("ws://") || v.starts_with("wss://")) =>
        {
            v.trim().to_string()
        }
        _ => DEFAULT_REMOTE_URL.to_string(),
    };
    let presets = match db.get_setting("remote.url_presets".to_string()).await {
        Ok(Some(v)) => serde_json::from_str::<Vec<String>>(&v)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| {
                let t = s.trim();
                !t.is_empty()
                    && (t.starts_with("ws://") || t.starts_with("wss://"))
                    && t != DEFAULT_REMOTE_URL
            })
            .collect(),
        _ => Vec::new(),
    };
    (url, presets)
}
