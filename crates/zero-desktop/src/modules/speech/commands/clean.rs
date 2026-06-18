//! Speech 音频清洗（设计 docs/2026-06-15-zero-desktop-audio-cleanup/design.md）。
//!
//! 用户经文件选择器挑任意本地音/视频文件，经 **toolkit-server 代理**
//! （`POST /api/web/audio/clean`）清洗后，**并列**保存为 cleaned variant
//! （`<stem>.cleaned.<format>`），**绝不覆盖原文件**。桌面端不直连 GB10 :8097，统一走
//! toolkit-server :8788 代理（与 TTS 走 `/api/web/audio/tts` 同一先例）。
//!
//! 配置由后端自取：从全局 `app.json` 读 `g10_base`/`g10_token`（与 cookie / english 模块共享），
//! 前端不传 base/token。请求带 Bearer token（G10 开鉴权时必需）。

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use serde::Serialize;
use tracing::info;

use crate::app_state::AppState;

/// 清洗（开 Demucs 整段清洗）可能数分钟，与上游 `MAX_DURATION_SEC=600` / 代理超时对齐。
const CLEAN_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Serialize)]
pub struct CleanedRecording {
    /// cleaned variant 的落盘路径（与原文件并列，原文件保留不动）。
    pub cleaned_path: String,
    pub stages: Vec<String>,
    pub in_lufs: f32,
    pub out_lufs: f32,
}

/// 按扩展名映射 multipart MIME。服务端用 ffmpeg 按内容解码（视频自动抽音轨），MIME 仅作提示，
/// 但仍如实标注；未知扩展名回退 `application/octet-stream`。
fn mime_for_ext(input: &Path) -> &'static str {
    match input
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("m4a") | Some("aac") => "audio/mp4",
        Some("flac") => "audio/flac",
        Some("ogg") | Some("opus") => "audio/ogg",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// 由原文件路径派生 cleaned variant 的并列路径：`<stem>.cleaned.<format>`（同目录兄弟文件，
/// 且必不等于原路径——保证不覆盖）。
fn cleaned_variant_path(input: &Path, format: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recording");
    let name = format!("{stem}.cleaned.{format}");
    match input.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

fn header_f32(resp: &reqwest::Response, name: &str) -> f32 {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(f32::NAN)
}

/// 选项白名单校验：非法值早返回可读错误，避免无谓上传。`None` 表示用上游默认（不发该字段）。
fn validate_choice(field: &str, value: &Option<String>, allowed: &[&str]) -> Result<(), String> {
    if let Some(v) = value {
        if !allowed.contains(&v.as_str()) {
            return Err(format!(
                "非法 {field} 取值「{v}」，可选：{}",
                allowed.join(" / ")
            ));
        }
    }
    Ok(())
}

/// 弹系统文件选择器挑一段音/视频文件，返回绝对路径（取消则返回 `None`）。用后端
/// `tauri-plugin-dialog` 的 Rust API，避免新增前端 npm 依赖（设计 §5.3 首选方案）。
#[tauri::command]
pub async fn speech_pick_audio_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter(
            "音频/视频",
            &[
                "wav", "mp3", "m4a", "aac", "flac", "ogg", "opus", "mp4", "webm", "mkv", "mov",
            ],
        )
        .pick_file(move |f| {
            let _ = tx.send(f);
        });
    let picked = rx.await.map_err(|e| format!("文件选择失败: {e}"))?;
    match picked {
        Some(fp) => Ok(Some(
            fp.into_path()
                .map_err(|e| format!("解析所选路径失败: {e}"))?
                .to_string_lossy()
                .into_owned(),
        )),
        None => Ok(None),
    }
}

/// 打开给定文件所在的文件夹（用后端 `tauri-plugin-shell` 的 Rust API，不引入新前端依赖，
/// 设计 §5.3 推荐方案）。
#[tauri::command]
pub async fn speech_open_in_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;

    let p = PathBuf::from(&path);
    let target = match p.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => p,
    };
    // 刻意复用已有 tauri-plugin-shell（无 opener 插件，避免新增依赖，设计 §5.3）。
    #[allow(deprecated)]
    app.shell()
        .open(target.to_string_lossy().to_string(), None)
        .map_err(|e| format!("打开文件夹失败: {e}"))
}

/// 清洗一段本地音/视频文件并并列落盘。配置（g10_base/g10_token）由后端自取，前端只传
/// `input_path` + 清洗选项。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn speech_clean_recording(
    state: tauri::State<'_, AppState>,
    input_path: String,
    denoise: Option<bool>,
    pause: Option<String>,
    separate: Option<bool>,
    level: Option<String>,
    loudness: Option<String>,
    sr: Option<u32>,
    format: Option<String>,
) -> Result<CleanedRecording, String> {
    // 选项白名单校验（早返回，不发请求）。
    validate_choice("pause", &pause, &["drop", "duck", "off"])?;
    validate_choice("level", &level, &["gentle", "balanced", "aggressive"])?;
    validate_choice("format", &format, &["wav", "mp3", "flac"])?;
    if let Some(rate) = sr {
        if !matches!(rate, 16000 | 24000 | 48000) {
            return Err(format!(
                "非法 sr 取值「{rate}」，可选：16000 / 24000 / 48000"
            ));
        }
    }

    // 后端自取全局配置（与 cookie/english 同源），按局域网/外网模式解析端点 + Bearer。
    let resolved = state.net.resolve(&state.workspace).await;
    let endpoint = resolved
        .clean_endpoint()
        .ok_or_else(|| "G10 base 未配置，请到设置页填写局域网/外网地址".to_string())?;

    let input = PathBuf::from(&input_path);
    let bytes = tokio::fs::read(&input)
        .await
        .map_err(|e| format!("读文件失败 {}: {e}", input.display()))?;
    let file_name = input
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("input")
        .to_string();

    // 输出格式：用于落盘后缀 + （format 显式给出时）发给上游。
    let out_format = format.clone().unwrap_or_else(|| "wav".to_string());

    let part = Part::bytes(bytes)
        .file_name(file_name)
        .mime_str(mime_for_ext(&input))
        .map_err(|e| e.to_string())?;
    // 仅当选项显式给出时才发对应字段，缺省让上游用其默认（向后兼容）。
    let mut form = Form::new().part("audio", part);
    if let Some(v) = denoise {
        form = form.text("denoise", if v { "1" } else { "0" });
    }
    if let Some(v) = separate {
        form = form.text("separate", if v { "1" } else { "0" });
    }
    if let Some(v) = pause {
        form = form.text("pause", v);
    }
    if let Some(v) = level {
        form = form.text("level", v);
    }
    if let Some(v) = loudness {
        form = form.text("loudness", v);
    }
    if let Some(v) = sr {
        form = form.text("sr", v.to_string());
    }
    if let Some(v) = format {
        form = form.text("format", v);
    }

    let client = reqwest::Client::builder()
        .timeout(CLEAN_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(&endpoint).multipart(form);
    if let Some(tok) = resolved.g10_token.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(tok);
    }
    let resp = req.send().await.map_err(|e| format!("清洗请求失败: {e}"))?;

    let status = resp.status();
    match status.as_u16() {
        503 => {
            // 代理 503=未配置（带 X-Clean-Proxy: unconfigured）vs 上游 503=队列满 busy（无该头）。
            let unconfigured = resp
                .headers()
                .get("X-Clean-Proxy")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.eq_ignore_ascii_case("unconfigured"))
                .unwrap_or(false);
            return Err(if unconfigured {
                "清洗服务未配置（toolkit-server 缺 CLEAN_BASE_URL）".to_string()
            } else {
                "清洗服务繁忙，请稍后重试".to_string()
            });
        }
        502 => return Err("清洗服务不可达（上游 audio-cleanup 无响应）".to_string()),
        401 | 403 => return Err("鉴权失败，请检查 G10 token 配置".to_string()),
        400 => return Err("文件无法解码或字段错误".to_string()),
        413 => return Err("文件过大，请先转码/截取".to_string()),
        422 => return Err("音频时长超限，请切分后再传".to_string()),
        504 => return Err("处理超时（超 600s），请切分输入".to_string()),
        500 => return Err("清洗服务内部错误".to_string()),
        c if !(200..300).contains(&c) => {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "清洗失败 {status}: {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        _ => {}
    }

    let stages = resp
        .headers()
        .get("X-Cleanup-Stages")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let in_lufs = header_f32(&resp, "X-Cleanup-In-LUFS");
    let out_lufs = header_f32(&resp, "X-Cleanup-Out-LUFS");
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("读清洗结果失败: {e}"))?;

    let out_path = cleaned_variant_path(&input, &out_format);
    // 安全闸：绝不覆盖原文件。
    if out_path == input {
        return Err("派生的 cleaned 路径与原文件相同，已中止以防覆盖".to_string());
    }
    tokio::fs::write(&out_path, &body)
        .await
        .map_err(|e| format!("落盘 cleaned 失败 {}: {e}", out_path.display()))?;
    info!(
        target: "speech",
        "[clean] {} -> {} ({} bytes)", input.display(), out_path.display(), body.len()
    );

    Ok(CleanedRecording {
        cleaned_path: out_path.to_string_lossy().to_string(),
        stages,
        in_lufs,
        out_lufs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_is_sibling_and_never_equals_input() {
        let input = Path::new("/data/rec/abc.wav");
        let out = cleaned_variant_path(input, "wav");
        assert_eq!(out.as_path(), Path::new("/data/rec/abc.cleaned.wav"));
        assert_ne!(out.as_path(), input);
    }

    #[test]
    fn variant_handles_no_extension() {
        let out = cleaned_variant_path(Path::new("rec"), "wav");
        assert_eq!(out.as_path(), Path::new("rec.cleaned.wav"));
    }

    #[test]
    fn variant_follows_format() {
        let input = Path::new("/x/clip.mp4");
        let out = cleaned_variant_path(input, "mp3");
        assert_eq!(out.as_path(), Path::new("/x/clip.cleaned.mp3"));
        assert_ne!(out.as_path(), input);
    }

    #[test]
    fn mime_maps_by_extension() {
        assert_eq!(mime_for_ext(Path::new("a.wav")), "audio/wav");
        assert_eq!(mime_for_ext(Path::new("a.MP3")), "audio/mpeg");
        assert_eq!(mime_for_ext(Path::new("a.mp4")), "video/mp4");
        assert_eq!(mime_for_ext(Path::new("a.webm")), "video/webm");
        assert_eq!(mime_for_ext(Path::new("a.xyz")), "application/octet-stream");
        assert_eq!(mime_for_ext(Path::new("noext")), "application/octet-stream");
    }

    #[test]
    fn validate_choice_accepts_none_and_allowed() {
        assert!(validate_choice("pause", &None, &["drop", "duck", "off"]).is_ok());
        assert!(validate_choice("pause", &Some("duck".into()), &["drop", "duck", "off"]).is_ok());
    }

    #[test]
    fn validate_choice_rejects_illegal() {
        let err =
            validate_choice("format", &Some("ogg".into()), &["wav", "mp3", "flac"]).unwrap_err();
        assert!(err.contains("format"));
        assert!(err.contains("ogg"));
    }
}
