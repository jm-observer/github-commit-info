//! 语音识别页 · segment「标注样本采集」。
//!
//! 把一条 segment 卡片标注成训练 / 纠错样本：落库元信息 + 从编排器拉取该段音频存档
//! （音频在编排器只留 1 天，`GET {http_base}/api/segments/:id/audio`，过期 404）。
//! 热词标签可选把「正确词」同步进编排器的 `asr.hotwords` 配置。
//!
//! 标签语义见 docs（asr_wrong / hotword / bad_optimize / ok / other）。

use std::collections::HashSet;
use std::time::Duration;

use chrono::Local;
use serde::Serialize;
use tauri::State;
use tracing::{info, warn};

use crate::app_state::AppState;
use crate::modules::speech::commands::remote::remote_http_base_from_state;
use crate::modules::speech::db::repository::{NewSample, SampleRow};

/// 编排器音频拉取超时。
const AUDIO_TIMEOUT: Duration = Duration::from_secs(30);
/// 编排器配置读写超时。
const CONFIG_TIMEOUT: Duration = Duration::from_secs(15);
/// 热词配置键。
const HOTWORDS_KEY: &str = "asr.hotwords";

/// 返回前端的一条样本（与 `SampleRow` 同形，单独类型便于演进）。
#[derive(Debug, Clone, Serialize)]
pub struct Sample {
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
    pub source: String,
    pub segment_ids: Option<String>,
    pub app_exe: Option<String>,
    pub app_path: Option<String>,
    pub app_title: Option<String>,
    pub app_class: Option<String>,
    pub delivery_mode: Option<String>,
}

impl From<SampleRow> for Sample {
    fn from(r: SampleRow) -> Self {
        Self {
            id: r.id,
            segment_id: r.segment_id,
            session_id: r.session_id,
            label: r.label,
            text_raw: r.text_raw,
            text_optimized: r.text_optimized,
            text_english: r.text_english,
            text_secondary: r.text_secondary,
            correction: r.correction,
            note: r.note,
            audio_path: r.audio_path,
            audio_status: r.audio_status,
            hotword_sync: r.hotword_sync,
            marked_at: r.marked_at,
            source: r.source,
            segment_ids: r.segment_ids,
            app_exe: r.app_exe,
            app_path: r.app_path,
            app_title: r.app_title,
            app_class: r.app_class,
            delivery_mode: r.delivery_mode,
        }
    }
}

fn now_str() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 从 correction 文本里提取要进热词表的「正确词」：含 `→` 或 `->` 取右侧，否则整串，trim。
fn extract_hotword_term(correction: &str) -> String {
    let right = if let Some(idx) = correction.find('→') {
        &correction[idx + '→'.len_utf8()..]
    } else if let Some(idx) = correction.find("->") {
        &correction[idx + 2..]
    } else {
        correction
    };
    right.trim().to_string()
}

/// 解析现有 `asr.hotwords` 文本，得到已存在的词面集合。
/// 规则：按行 trim；跳过空行与 `#` 注释；每行取首个空白前的词面（行可为「词」或「词 权重」）。
fn parse_existing_hotwords(text: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let term = line.split_whitespace().next().unwrap_or("");
        if !term.is_empty() {
            set.insert(term.to_string());
        }
    }
    set
}

/// 把新词 append 到原热词文本末尾：保留原内容，末尾若无换行先补换行。
fn append_hotword(existing: &str, term: &str) -> String {
    if existing.is_empty() {
        return format!("{term}\n");
    }
    if existing.ends_with('\n') {
        format!("{existing}{term}\n")
    } else {
        format!("{existing}\n{term}\n")
    }
}

/// 拉取该段音频并存档到 `{workspace}/speech_samples/{sample_id}.wav`。
/// 返回 (audio_path, audio_status)。任何失败都不抛错，只返回对应 status。
async fn fetch_and_store_audio(
    base: &str,
    segment_id: i64,
    workspace: &std::path::Path,
    sample_id: i64,
) -> (Option<String>, String) {
    let url = format!("{base}/api/segments/{segment_id}/audio");
    let client = match reqwest::Client::builder().timeout(AUDIO_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(target: "speech", "[sample] build http client failed: {e}");
            return (None, "fetch_failed".to_string());
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(target: "speech", "[sample] fetch audio {url} failed: {e}");
            return (None, "fetch_failed".to_string());
        }
    };
    match resp.status().as_u16() {
        200 => {}
        404 => return (None, "expired".to_string()),
        other => {
            warn!(target: "speech", "[sample] fetch audio {url} status {other}");
            return (None, "fetch_failed".to_string());
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(target: "speech", "[sample] read audio body failed: {e}");
            return (None, "fetch_failed".to_string());
        }
    };
    let dir = workspace.join("speech_samples");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        warn!(target: "speech", "[sample] create speech_samples dir failed: {e}");
        return (None, "fetch_failed".to_string());
    }
    let out_path = dir.join(format!("{sample_id}.wav"));
    if let Err(e) = tokio::fs::write(&out_path, &bytes).await {
        warn!(target: "speech", "[sample] write audio failed {}: {e}", out_path.display());
        return (None, "fetch_failed".to_string());
    }
    info!(
        target: "speech",
        "[sample] audio archived seg={segment_id} -> {} ({} bytes)", out_path.display(), bytes.len()
    );
    (
        Some(out_path.to_string_lossy().to_string()),
        "saved".to_string(),
    )
}

/// 把「正确词」同步进编排器 `asr.hotwords`。返回 "added" | "exists" | "failed"。
/// 任何失败都返回 "failed"（不抛错）。
async fn sync_hotword_to_orchestrator(base: &str, correction: Option<&str>) -> String {
    let Some(correction) = correction else {
        return "failed".to_string();
    };
    let term = extract_hotword_term(correction);
    if term.is_empty() {
        return "failed".to_string();
    }

    let client = match reqwest::Client::builder().timeout(CONFIG_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(target: "speech", "[sample] build config client failed: {e}");
            return "failed".to_string();
        }
    };
    let cfg_url = format!("{base}/api/config");

    // 读现有配置。
    let existing_text = match client.get(&cfg_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => v
                .get(HOTWORDS_KEY)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            Err(e) => {
                warn!(target: "speech", "[sample] parse config json failed: {e}");
                return "failed".to_string();
            }
        },
        Ok(resp) => {
            warn!(target: "speech", "[sample] get config status {}", resp.status());
            return "failed".to_string();
        }
        Err(e) => {
            warn!(target: "speech", "[sample] get config failed: {e}");
            return "failed".to_string();
        }
    };

    if parse_existing_hotwords(&existing_text).contains(&term) {
        return "exists".to_string();
    }

    let new_text = append_hotword(&existing_text, &term);
    let body = serde_json::json!({ HOTWORDS_KEY: new_text });
    match client.post(&cfg_url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            info!(target: "speech", "[sample] hotword added: {term:?}");
            "added".to_string()
        }
        Ok(resp) => {
            warn!(target: "speech", "[sample] post config status {}", resp.status());
            "failed".to_string()
        }
        Err(e) => {
            warn!(target: "speech", "[sample] post config failed: {e}");
            "failed".to_string()
        }
    }
}

/// 试听：从编排器拉取该段音频，base64 返回给前端播放。**不落盘、不落库**——
/// 与标注存档（`fetch_and_store_audio`）是两条线，这里只是「听一下再决定怎么标」。
/// 编排器只保留 1 天内的音频 blob（每小时清理），过期 404 → 明确报「已过期」。
#[tauri::command]
pub async fn speech_fetch_segment_audio(
    segment_id: i64,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let Some(base) = remote_http_base_from_state(&state.speech.remote_url) else {
        return Err("远程识别地址未配置".to_string());
    };
    let url = format!("{base}/api/segments/{segment_id}/audio");
    let client = reqwest::Client::builder()
        .timeout(AUDIO_TIMEOUT)
        .build()
        .map_err(|e| format!("构建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("拉取音频失败: {e}"))?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Err("音频已过期（服务端只保留 1 天）".to_string()),
        other => return Err(format!("拉取音频失败: HTTP {other}")),
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取音频内容失败: {e}"))?;
    info!(target: "speech", "[sample] audition seg={segment_id} ({} bytes)", bytes.len());
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn speech_mark_sample(
    segment_id: i64,
    session_id: Option<String>,
    text_raw: String,
    text_optimized: Option<String>,
    text_english: Option<String>,
    text_secondary: Option<String>,
    label: String,
    correction: Option<String>,
    note: Option<String>,
    sync_hotword: Option<bool>,
    state: State<'_, AppState>,
) -> Result<Sample, String> {
    let workspace = state.workspace.clone();
    let db = {
        let guard = state
            .speech
            .db
            .lock()
            .map_err(|_| "speech db mutex poisoned".to_string())?;
        guard
            .clone()
            .ok_or_else(|| "speech db 未初始化".to_string())?
    };

    // a. 先落库（音频暂置 skipped、hotword_sync 暂空），拿自增 id。
    let new = NewSample {
        segment_id,
        session_id,
        label: label.clone(),
        text_raw,
        text_optimized,
        text_english,
        text_secondary,
        correction: correction.clone(),
        note,
        audio_status: "skipped".to_string(),
        marked_at: now_str(),
        source: "ui".to_string(),
        segment_ids: None,
        // 手工标注面板没有「交付动作」这一时刻，应用上下文留空（只有自动交付链路才抓拍）。
        app: Default::default(),
    };
    let sample_id = db.insert_sample(new).await.map_err(|e| e.to_string())?;

    // b. 拉取并存档音频（失败不影响整体）。
    let base = remote_http_base_from_state(&state.speech.remote_url);
    let (audio_path, audio_status) = match &base {
        Some(b) => fetch_and_store_audio(b, segment_id, &workspace, sample_id).await,
        None => {
            warn!(target: "speech", "[sample] remote url 未配置，跳过音频存档");
            (None, "skipped".to_string())
        }
    };
    db.update_sample_audio(sample_id, audio_path.clone(), audio_status.clone())
        .await
        .map_err(|e| e.to_string())?;

    // c. 热词同步（仅 hotword 标签 + 开关开 + base 可用）。
    let mut hotword_sync_result: Option<String> = None;
    if label == "hotword" && sync_hotword == Some(true) {
        let sync = match &base {
            Some(b) => sync_hotword_to_orchestrator(b, correction.as_deref()).await,
            None => "failed".to_string(),
        };
        db.update_sample_hotword_sync(sample_id, sync.clone())
            .await
            .map_err(|e| e.to_string())?;
        hotword_sync_result = Some(sync);
    }

    // d. 返回最终样本（直接读回，确保字段一致）。
    let row = db
        .get_sample(sample_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "样本落库后读取失败".to_string())?;
    let mut sample: Sample = row.into();
    // get_sample 已带 hotword_sync；冗余保险一致。
    if hotword_sync_result.is_some() {
        sample.hotword_sync = hotword_sync_result;
    }
    Ok(sample)
}

#[tauri::command]
pub async fn speech_list_samples(state: State<'_, AppState>) -> Result<Vec<Sample>, String> {
    let db = {
        let guard = state
            .speech
            .db
            .lock()
            .map_err(|_| "speech db mutex poisoned".to_string())?;
        guard
            .clone()
            .ok_or_else(|| "speech db 未初始化".to_string())?
    };
    let rows = db.list_samples().await.map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(Sample::from).collect())
}

/// 场景记录总览：桌面端据此显示「收了多少、今天多少、都来自哪些应用」。
///
/// 与样本采集是两条线：这里统计的是**每次交付都记**的全量日志，不是手动采集的纠错样本。
#[tauri::command]
pub async fn speech_scene_stats(
    state: State<'_, AppState>,
) -> Result<crate::modules::speech::db::repository::SceneStats, String> {
    let db = {
        let guard = state
            .speech
            .db
            .lock()
            .map_err(|_| "speech db mutex poisoned".to_string())?;
        guard
            .clone()
            .ok_or_else(|| "speech db 未初始化".to_string())?
    };
    db.scene_stats().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn speech_export_samples(state: State<'_, AppState>) -> Result<String, String> {
    let workspace = state.workspace.clone();
    let db = {
        let guard = state
            .speech
            .db
            .lock()
            .map_err(|_| "speech db mutex poisoned".to_string())?;
        guard
            .clone()
            .ok_or_else(|| "speech db 未初始化".to_string())?
    };
    let rows = db.list_samples().await.map_err(|e| e.to_string())?;

    let dir = workspace.join("speech_samples");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("创建 speech_samples 目录失败: {e}"))?;

    // 序列化：全部字段 + 音频相对路径（相对 speech_samples 目录）。
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            let audio_rel = r.audio_path.as_ref().and_then(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            });
            serde_json::json!({
                "id": r.id,
                "segment_id": r.segment_id,
                "session_id": r.session_id,
                "label": r.label,
                "text_raw": r.text_raw,
                "text_optimized": r.text_optimized,
                "text_english": r.text_english,
                "text_secondary": r.text_secondary,
                "correction": r.correction,
                "note": r.note,
                "audio_path": r.audio_path,
                "audio_rel_path": audio_rel,
                "audio_status": r.audio_status,
                "hotword_sync": r.hotword_sync,
                "marked_at": r.marked_at,
                "source": r.source,
                "segment_ids": r.segment_ids,
                // 交付时的应用上下文（同音字纠错数据收集期）。app_title 可能含聊天对象名 /
                // 文档名 / 网页标题——分享导出文件前请自行留意。
                "app_exe": r.app_exe,
                "app_path": r.app_path,
                "app_title": r.app_title,
                "app_class": r.app_class,
                "delivery_mode": r.delivery_mode,
            })
        })
        .collect();

    let ts = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let out_path = dir.join(format!("export-{ts}.json"));
    let json = serde_json::to_string_pretty(&items).map_err(|e| e.to_string())?;
    tokio::fs::write(&out_path, json)
        .await
        .map_err(|e| format!("写导出文件失败 {}: {e}", out_path.display()))?;
    info!(target: "speech", "[sample] exported {} samples -> {}", items.len(), out_path.display());
    Ok(out_path.to_string_lossy().to_string())
}

/// 同音候选导出（P2a）：对全部纠错样本跑 `homophone::mine`，按 `(wrong, right, scope)`
/// 聚合后写 JSON，返回文件路径。**只导出给人工审读，不建表、不改任何配置**（§6.3：
/// 读音等价 ≠ 同音纠错，必须人工 approve；`homophone_pairs` 表按 v4 约定收集期不建）。
///
/// scope 取 `app_exe` 原值（分组维度按设计由 P1 数据决定，这里不预设归类）。
/// `eligible_pending` = exact 且 hits ≥ 2（§6.2 入表门槛），仅是给人工看的排序信号。
#[tauri::command]
pub async fn speech_export_homophone_candidates(
    state: State<'_, AppState>,
) -> Result<String, String> {
    use crate::modules::speech::homophone;
    use std::collections::BTreeMap;

    let workspace = state.workspace.clone();
    let db = {
        let guard = state
            .speech
            .db
            .lock()
            .map_err(|_| "speech db mutex poisoned".to_string())?;
        guard
            .clone()
            .ok_or_else(|| "speech db 未初始化".to_string())?
    };
    let rows = db.list_samples().await.map_err(|e| e.to_string())?;

    struct Agg {
        reading: String,
        match_kind: &'static str,
        hits: usize,
        sample_ids: Vec<i64>,
        contexts: Vec<String>,
    }
    let mut agg: BTreeMap<(String, String, String), Agg> = BTreeMap::new();
    let mut mined_samples = 0usize;
    for r in &rows {
        let Some(correction) = r.correction.as_deref().filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        // O = 实际交付的文本（优化文，缺则原文）。ok 标签本就无 correction，天然被上面滤掉。
        let delivered = r
            .text_optimized
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&r.text_raw);
        let found = homophone::mine(delivered, correction);
        if found.is_empty() {
            continue;
        }
        mined_samples += 1;
        let scope = r.app_exe.clone().unwrap_or_else(|| "unknown".to_string());
        for c in found {
            let entry = agg
                .entry((c.wrong.clone(), c.right.clone(), scope.clone()))
                .or_insert_with(|| Agg {
                    reading: c.reading.clone(),
                    match_kind: c.match_kind.as_str(),
                    hits: 0,
                    sample_ids: Vec::new(),
                    contexts: Vec::new(),
                });
            entry.hits += 1;
            entry.sample_ids.push(r.id);
            if entry.contexts.len() < 3 {
                entry.contexts.push(c.context);
            }
        }
    }

    let mut candidates: Vec<serde_json::Value> = agg
        .into_iter()
        .map(|((wrong, right, scope), a)| {
            serde_json::json!({
                "wrong": wrong,
                "right": right,
                "scope": scope,
                "reading": a.reading,
                "match_kind": a.match_kind,
                "hits": a.hits,
                "eligible_pending": a.match_kind == "exact" && a.hits >= 2,
                "sample_ids": a.sample_ids,
                "contexts": a.contexts,
            })
        })
        .collect();
    // exact 优先、hits 降序，人工从最可信的看起。
    candidates.sort_by(|x, y| {
        let rank = |v: &serde_json::Value| match v["match_kind"].as_str() {
            Some("exact") => 0,
            Some("polyphone") => 1,
            _ => 2,
        };
        rank(x)
            .cmp(&rank(y))
            .then(y["hits"].as_u64().cmp(&x["hits"].as_u64()))
    });

    let doc = serde_json::json!({
        "generated_at": now_str(),
        "samples_total": rows.len(),
        "samples_with_candidates": mined_samples,
        "note": "读音等价≠同音纠错(改专名/改习惯也读音等价)。exact 才可考虑入表, polyphone/fuzzy 仅供参考; 入表须人工确认(§6.3)。",
        "candidates": candidates,
    });

    let dir = workspace.join("speech_samples");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("创建 speech_samples 目录失败: {e}"))?;
    let ts = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let out_path = dir.join(format!("homophone-candidates-{ts}.json"));
    let json = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    tokio::fs::write(&out_path, json)
        .await
        .map_err(|e| format!("写导出文件失败 {}: {e}", out_path.display()))?;
    info!(
        target: "speech",
        "[homophone] exported {} candidate group(s) from {} sample(s) -> {}",
        doc["candidates"].as_array().map(Vec::len).unwrap_or(0),
        mined_samples,
        out_path.display()
    );
    Ok(out_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_term_plain() {
        assert_eq!(extract_hotword_term("  韭菜盒子 "), "韭菜盒子");
    }

    #[test]
    fn extract_term_arrow_unicode() {
        assert_eq!(extract_hotword_term("旧菜盒子 → 韭菜盒子"), "韭菜盒子");
    }

    #[test]
    fn extract_term_arrow_ascii() {
        assert_eq!(extract_hotword_term("jiucai -> 韭菜"), "韭菜");
    }

    #[test]
    fn parse_existing_skips_comments_and_blanks_and_weights() {
        let text = "# 注释\n韭菜盒子\n\nGB10 5\n  ths  ";
        let set = parse_existing_hotwords(text);
        assert!(set.contains("韭菜盒子"));
        assert!(set.contains("GB10"));
        assert!(set.contains("ths"));
        assert!(!set.contains("#"));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn append_to_empty_adds_trailing_newline() {
        assert_eq!(append_hotword("", "韭菜"), "韭菜\n");
    }

    #[test]
    fn append_without_trailing_newline_inserts_one() {
        assert_eq!(append_hotword("a\nb", "c"), "a\nb\nc\n");
    }

    #[test]
    fn append_with_trailing_newline_preserved() {
        assert_eq!(append_hotword("a\n", "b"), "a\nb\n");
    }
}
