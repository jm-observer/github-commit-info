use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

use crate::app_state::AppState;

/// TTS 生成可能 10s+（首次懒加载更久），与 toolkit-server 代理超时（180s）对齐。
const TTS_TIMEOUT: Duration = Duration::from_secs(180);
/// 音色库查询很快。
const VOICES_TIMEOUT: Duration = Duration::from_secs(30);
/// 替换上传（小 WAV）。
const REPLACE_TIMEOUT: Duration = Duration::from_secs(60);
/// 预览 WAV 落盘文件名（每次生成覆盖；前端用查询串做缓存击穿）。
const PREVIEW_FILE: &str = "_tts_preview.wav";

/// English 模块状态（前端通过 plugin-store / plugin-fs 管理自身 KV 和音频缓存）。
#[derive(Default)]
pub struct EnglishState {}

/// 初始化 English 模块：确保 audio-cache 目录已存在（workspace 初始化已创建，此处为文档化）。
pub fn setup(_app: &tauri::AppHandle, _state: Arc<EnglishState>) -> Result<()> {
    Ok(())
}

/// 健康探针。
#[tauri::command]
pub fn english_ping() -> &'static str {
    "ok"
}

/// 返回当前生效路径的 g10_base（用于 ApiService 的 apiBase）；按局域网/外网模式解析。
/// 若未配置返回空字符串，前端应抛错引导用户到设置页配置。
#[tauri::command]
pub async fn english_get_g10_base(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.net.resolve(&state.workspace).await.g10_base)
}

/// 返回 english 音频缓存目录的绝对路径（用于 FileCacheManager 的根路径）。
#[tauri::command]
pub fn english_get_audio_cache_dir(state: State<'_, AppState>) -> String {
    crate::shared::workspace::english_audio_cache_dir(&state.workspace)
        .to_string_lossy()
        .into_owned()
}

/// 把上游/代理返回的非 2xx 状态码映射成可读中文错误（与 speech::clean 风格一致）。
fn map_status_err(prefix: &str, status: reqwest::StatusCode, body: &str) -> String {
    match status.as_u16() {
        503 => format!("{prefix}：服务未配置（toolkit-server 缺 TTS_BASE_URL）"),
        502 => format!("{prefix}：上游不可达"),
        401 | 403 => format!("{prefix}：鉴权失败，请检查 G10 token"),
        400 => format!("{prefix}：请求参数错误"),
        c => format!(
            "{prefix}：HTTP {c} {}",
            body.chars().take(200).collect::<String>()
        ),
    }
}

/// 查询音色库：GET `{g10_base}/api/web/audio/voices`，回传上游 JSON（前端自行解析形态）。
#[tauri::command]
pub async fn english_tts_voices(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let resolved = state.net.resolve(&state.workspace).await;
    let endpoint = resolved
        .voices_endpoint()
        .ok_or_else(|| "G10 base 未配置，请到设置页填写局域网/外网地址".to_string())?;

    let client = reqwest::Client::builder()
        .timeout(VOICES_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(&endpoint);
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("查询音色失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读音色响应失败: {e}"))?;
    if !status.is_success() {
        return Err(map_status_err("查询音色失败", status, &text));
    }
    serde_json::from_str(&text).map_err(|e| format!("解析音色 JSON 失败: {e}"))
}

/// 生成 TTS 预览：POST `{g10_base}/api/web/audio/tts`，把返回的 WAV 落盘到 english 音频缓存目录
/// （固定文件名，每次覆盖），返回绝对路径。前端用 `convertFileSrc` + 查询串缓存击穿后试听。
/// **不写任何业务数据**，仅本地预览。
#[tauri::command]
pub async fn english_tts_preview(
    state: State<'_, AppState>,
    text: String,
    voice_id: String,
    speed: Option<f32>,
) -> Result<String, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("文本不能为空".to_string());
    }
    let resolved = state.net.resolve(&state.workspace).await;
    let endpoint = resolved
        .tts_endpoint()
        .ok_or_else(|| "G10 base 未配置，请到设置页填写局域网/外网地址".to_string())?;

    let mut body = serde_json::json!({ "text": text, "voice_id": voice_id });
    if let Some(sp) = speed {
        body["speed"] = serde_json::json!(sp);
    }

    let client = reqwest::Client::builder()
        .timeout(TTS_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(&endpoint).json(&body);
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("TTS 请求失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(map_status_err("TTS 生成失败", status, &body));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读 TTS 结果失败: {e}"))?;

    let cache_dir = crate::shared::workspace::english_audio_cache_dir(&state.workspace);
    tokio::fs::create_dir_all(&cache_dir)
        .await
        .map_err(|e| format!("创建缓存目录失败: {e}"))?;
    let out_path = cache_dir.join(PREVIEW_FILE);
    tokio::fs::write(&out_path, &bytes)
        .await
        .map_err(|e| format!("落盘预览失败: {e}"))?;

    Ok(out_path.to_string_lossy().into_owned())
}

/// 确认替换：读取预览 WAV 文件（`preview_path`，须为上一步 english_tts_preview 落盘的预览文件），
/// multipart 上传到 english 后端 `{g10_base}/sentence/replace-audio`，原子地改句子文本 + 换音频。
/// 带 g10_token 作 Bearer（后端开 `DESKTOP_REPLACE_TOKEN` 时必需）。
#[tauri::command]
pub async fn english_replace_sentence_audio(
    state: State<'_, AppState>,
    sentence_id: i64,
    audio_id: i64,
    text: String,
    preview_path: String,
) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("文本不能为空".to_string());
    }
    // 只允许读 english 音频缓存目录下的预览文件，避免被诱导上传任意本地文件。
    let cache_dir = crate::shared::workspace::english_audio_cache_dir(&state.workspace);
    let expected = cache_dir.join(PREVIEW_FILE);
    if std::path::Path::new(&preview_path) != expected.as_path() {
        return Err("非法的预览文件路径".to_string());
    }
    let bytes = tokio::fs::read(&expected)
        .await
        .map_err(|e| format!("读预览文件失败（请先生成预览）: {e}"))?;

    let resolved = state.net.resolve(&state.workspace).await;
    let endpoint = resolved
        .replace_sentence_audio_endpoint()
        .ok_or_else(|| "G10 base 未配置，请到设置页填写局域网/外网地址".to_string())?;

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(format!("{audio_id}.wav"))
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .text("sentence_id", sentence_id.to_string())
        .text("audio_id", audio_id.to_string())
        .text("text", text)
        .part("file", part);

    let client = reqwest::Client::builder()
        .timeout(REPLACE_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(&endpoint).multipart(form);
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("替换请求失败: {e}"))?;
    let status = resp.status();
    let text_body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(map_status_err("替换失败", status, &text_body));
    }
    // 后端形如 {code, msg, data:{success,message,...}}；code!=0 视为失败。
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text_body) {
        if v.get("code").and_then(|c| c.as_i64()).unwrap_or(0) != 0 {
            let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("替换失败");
            return Err(msg.to_string());
        }
        if let Some(data) = v.get("data") {
            if data.get("success").and_then(|s| s.as_bool()) == Some(false) {
                let msg = data
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("替换失败");
                return Err(msg.to_string());
            }
        }
    }
    Ok(())
}
