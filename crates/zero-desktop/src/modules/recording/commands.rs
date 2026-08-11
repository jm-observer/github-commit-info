//! 录屏模块的 Tauri 命令 + 热键入口。
//!
//! 平台策略与 screenshot 一致：命令函数在所有平台都注册，Windows 独有的抓屏路径用
//! `cfg` 分流，非 Windows 直接返回「不支持」而不是编译不过。

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::output;
use super::session;
use super::{RecordingSettings, Rect};
use crate::app_state::AppState;

/// 关掉框选叠加窗后、真正开录前的等待：叠加窗是置顶透明窗，它还在屏上时开录会把
/// 那层半透明蒙版录进第一帧。等一下让窗口真正从合成器里消失。
const OVERLAY_SETTLE_MS: u64 = 180;

// ---- 设置 ----

/// 命令：读录屏设置（`save_dir` 为空时回填默认目录）。
#[tauri::command]
pub fn recording_get_settings(app: AppHandle) -> Result<RecordingSettings, String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let mut s = super::read_settings(&workspace);
    if s.save_dir.is_empty() {
        s.save_dir = output::recordings_dir(&workspace)
            .to_string_lossy()
            .into_owned();
    }
    Ok(s)
}

/// 命令：保存录屏设置。热键注册与 screenshot 同款处理——先纯解析校验，写盘后整体
/// 重注册，注册失败回滚设置，避免「界面显示新热键、实际按了没反应」。
#[tauri::command]
pub fn recording_save_settings(app: AppHandle, settings: RecordingSettings) -> Result<(), String> {
    let settings = settings.sanitized();
    crate::modules::hotkeys::parse_shortcut(&settings.hotkey)?;

    let workspace = app.state::<AppState>().workspace.clone();
    let path = output::settings_path(&workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败: {e}"))?;
    }
    let old = std::fs::read_to_string(&path).ok();
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写设置失败: {e}"))?;

    let failures = crate::modules::hotkeys::reregister_all(&app);
    if let Some((_, e)) = failures.iter().find(|(who, _)| *who == "recording") {
        match old {
            Some(prev) => {
                let _ = std::fs::write(&path, prev);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        let _ = crate::modules::hotkeys::reregister_all(&app);
        return Err(e.clone());
    }
    Ok(())
}

/// ffmpeg 探测结果（设置页显示「用的是哪一个」）。
#[derive(Debug, Clone, Serialize)]
pub struct FfmpegInfo {
    pub found: bool,
    /// 实际会用的可执行文件路径（未找到时为空串）。
    pub path: String,
    /// `ffmpeg version ...` 首行（未找到时为空串）。
    pub version: String,
    /// 未找到时的可操作提示。
    pub error: String,
}

/// 命令：按给定路径（或设置里的路径）探测 ffmpeg。`path_hint` 传空串 = 用设置里的值。
#[tauri::command]
pub fn recording_detect_ffmpeg(app: AppHandle, path_hint: String) -> Result<FfmpegInfo, String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let hint = if path_hint.trim().is_empty() {
        super::read_settings(&workspace).ffmpeg_path
    } else {
        path_hint
    };
    Ok(match super::ffmpeg::resolve(&hint) {
        Ok(exe) => FfmpegInfo {
            found: true,
            version: super::ffmpeg::version_line(&exe).unwrap_or_default(),
            path: exe.to_string_lossy().into_owned(),
            error: String::new(),
        },
        Err(e) => FfmpegInfo {
            found: false,
            path: String::new(),
            version: String::new(),
            error: e.to_string(),
        },
    })
}

// ---- 录制控制 ----

/// 命令：查询当前录制状态（前端与悬浮控制条都靠轮询它）。
#[tauri::command]
pub fn recording_status() -> session::RecordingStatus {
    session::status()
}

/// 命令：开始录制。`region` 为空 = 录鼠标所在的整块屏。
///
/// 由框选叠加窗（`mode=record`）与主窗口按钮共用。叠加窗调用时它自己还在屏上，
/// 这里负责关掉它并等合成器落定，再开录。
#[tauri::command]
pub async fn recording_start(app: AppHandle, region: Option<Rect>) -> Result<String, String> {
    // 框选窗（若在）是置顶透明窗，必须先关。
    crate::modules::screenshot::overlay::close_overlay(&app);
    tokio::time::sleep(std::time::Duration::from_millis(OVERLAY_SETTLE_MS)).await;
    do_start(&app, region)
}

/// 命令：唤起框选叠加窗，用户拉出区域后由前端回调 `recording_start` 开录。
/// 与热键那条路径走同一个实现，行为完全一致。
#[tauri::command]
pub fn recording_start_region(app: AppHandle) -> Result<(), String> {
    begin_region_select(&app)
}

/// 命令：暂停 / 继续。返回切换后的状态串。
#[tauri::command]
pub fn recording_set_paused(paused: bool) -> Result<String, String> {
    session::set_paused(paused).map(|s| s.to_string())
}

/// 命令：停止录制，返回落盘路径。
#[tauri::command]
pub fn recording_stop(app: AppHandle) -> Result<String, String> {
    super::bar::close(&app);
    match session::stop() {
        Ok(path) => {
            let s = path.to_string_lossy().into_owned();
            notify(&app, "录屏完成", &format!("已保存到 {s}"));
            Ok(s)
        }
        Err(e) => {
            notify(&app, "录屏失败", &e);
            Err(e)
        }
    }
}

/// 命令：取消录制并删除已产生的文件（录废了不想留）。
#[tauri::command]
pub fn recording_discard(app: AppHandle) -> Result<(), String> {
    super::bar::close(&app);
    let result = session::stop();
    if let Ok(path) = &result {
        let _ = std::fs::remove_file(path);
        output::remove_meta(path);
    }
    result.map(|_| ())
}

/// 热键入口：没在录就开始（按设置决定是否先框选），在录就停止。
pub fn toggle_from_hotkey(app: &AppHandle) -> Result<(), String> {
    if session::is_active() {
        recording_stop(app.clone()).map(|_| ())
    } else {
        let workspace = app.state::<AppState>().workspace.clone();
        let settings = super::read_settings(&workspace);
        if settings.select_region {
            begin_region_select(app)
        } else {
            do_start(app, None).map(|_| ())
        }
    }
}

/// 唤起框选叠加窗（`mode=record`）：抓一张冻结帧铺底，用户拉出区域后再调
/// `recording_start`。复用截图那套叠加窗，避免两份框选实现。
#[cfg(windows)]
fn begin_region_select(app: &AppHandle) -> Result<(), String> {
    use crate::modules::screenshot::{capture, monitor, output as shot_output, overlay};

    // 开录前先探一次 ffmpeg：不然用户框选半天，最后才被告知没装 ffmpeg。
    let workspace = app.state::<AppState>().workspace.clone();
    let settings = super::read_settings(&workspace);
    if let Err(e) = super::ffmpeg::resolve(&settings.ffmpeg_path) {
        let msg = e.to_string();
        notify(app, "录屏失败", &msg);
        return Err(msg);
    }

    let rect = monitor::monitor_at_cursor().map_err(|e| e.to_string())?;
    let png = capture::capture_rect_png(&rect).map_err(|e| e.to_string())?;
    let frame_path = shot_output::frozen_frame_path(&workspace);
    if let Some(parent) = frame_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(&frame_path, &png).map_err(|e| format!("写冻结帧失败: {e}"))?;

    let session_id = overlay::next_session_id();
    overlay::open_overlay(
        app,
        &rect,
        &frame_path.to_string_lossy(),
        session_id,
        overlay::Mode::Record,
    )
    .map_err(|e| {
        let m = e.to_string();
        notify(app, "录屏失败", &m);
        m
    })
}

#[cfg(not(windows))]
fn begin_region_select(_app: &AppHandle) -> Result<(), String> {
    Err("录屏功能仅支持 Windows".to_string())
}

/// 真正开录：解析区域（缺省 = 鼠标所在屏）→ 起会话 → 弹控制条。
#[cfg(windows)]
fn do_start(app: &AppHandle, region: Option<Rect>) -> Result<String, String> {
    use crate::modules::screenshot::monitor;

    let workspace = app.state::<AppState>().workspace.clone();
    let settings = super::read_settings(&workspace);

    let mon = monitor::monitor_at_cursor().map_err(|e| e.to_string())?;
    let screen = Rect {
        x: mon.x,
        y: mon.y,
        width: mon.width,
        height: mon.height,
    };
    let region = region.unwrap_or(screen);

    let dir = if settings.save_dir.trim().is_empty() {
        output::recordings_dir(&workspace)
    } else {
        PathBuf::from(settings.save_dir.trim())
    };
    let out_path = output::new_video_path(&dir).map_err(|e| e.to_string())?;

    match session::start(region, &settings, out_path) {
        Ok(status) => {
            super::bar::open(app, &region, &screen);
            Ok(status.path)
        }
        Err(e) => {
            notify(app, "录屏失败", &e);
            Err(e)
        }
    }
}

#[cfg(not(windows))]
fn do_start(_app: &AppHandle, _region: Option<Rect>) -> Result<String, String> {
    Err("录屏功能仅支持 Windows".to_string())
}

// ---- 历史列表 ----

/// 一条历史录屏（供主窗口列表展示）。
#[derive(Debug, Clone, Serialize)]
pub struct RecordingItem {
    pub name: String,
    pub path: String,
    pub modified_ms: i64,
    pub size: u64,
    /// 时长（毫秒；sidecar 缺失时为 0）。
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// 命令：列出历史录屏（按修改时间新→旧）。
#[tauri::command]
pub fn recording_list(app: AppHandle) -> Result<Vec<RecordingItem>, String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let dir = recordings_dir_effective(&workspace);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // 从未录过 → 空列表，不算错误。
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读取录屏目录失败: {e}")),
    };

    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_mp4 = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("mp4"))
            .unwrap_or(false);
        if !is_mp4 {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let side = output::read_meta(&path);
        items.push(RecordingItem {
            name,
            path: path.to_string_lossy().into_owned(),
            modified_ms,
            size: meta.len(),
            duration_ms: side.as_ref().map(|m| m.duration_ms).unwrap_or(0),
            width: side.as_ref().map(|m| m.width).unwrap_or(0),
            height: side.as_ref().map(|m| m.height).unwrap_or(0),
            fps: side.as_ref().map(|m| m.fps).unwrap_or(0),
        });
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.modified_ms));
    Ok(items)
}

/// 命令：删除一条录屏（连 sidecar 一起）。路径须在录屏目录内。
#[tauri::command]
pub fn recording_delete(app: AppHandle, path: String) -> Result<(), String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_recordings(&workspace, &path)?;
    std::fs::remove_file(&target).map_err(|e| format!("删除失败: {e}"))?;
    output::remove_meta(&target);
    Ok(())
}

/// 命令：在系统文件管理器中打开录屏目录。
#[tauri::command]
pub fn recording_open_folder(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    let workspace = app.state::<AppState>().workspace.clone();
    let dir = recordings_dir_effective(&workspace);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建录屏目录失败: {e}"))?;
    #[allow(deprecated)]
    app.shell()
        .open(dir.to_string_lossy().to_string(), None)
        .map_err(|e| format!("打开文件夹失败: {e}"))
}

/// 命令：用系统默认播放器打开一条录屏。
#[tauri::command]
pub fn recording_open_file(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_recordings(&workspace, &path)?;
    #[allow(deprecated)]
    app.shell()
        .open(target.to_string_lossy().to_string(), None)
        .map_err(|e| format!("打开视频失败: {e}"))
}

/// 命令：在文件管理器里定位到这条录屏。
#[tauri::command]
pub fn recording_reveal_in_folder(app: AppHandle, path: String) -> Result<(), String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_recordings(&workspace, &path)?;

    #[cfg(windows)]
    {
        let _ = &app;
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", target.to_string_lossy()))
            .spawn()
            .map_err(|e| format!("定位文件失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use tauri_plugin_shell::ShellExt;
        let dir = target.parent().unwrap_or(&target);
        #[allow(deprecated)]
        app.shell()
            .open(dir.to_string_lossy().to_string(), None)
            .map_err(|e| format!("打开文件夹失败: {e}"))
    }
}

/// 命令：把一条录屏另存到用户选择的位置。取消返回 `None`。
#[tauri::command]
pub async fn recording_save_as(app: AppHandle, path: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_recordings(&workspace, &path)?;
    let default_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("recording.mp4")
        .to_string();

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(&default_name)
        .add_filter("MP4 视频", &["mp4"])
        .save_file(move |f| {
            let _ = tx.send(f);
        });
    let chosen = rx.await.map_err(|e| format!("保存对话框失败: {e}"))?;
    let Some(fp) = chosen else { return Ok(None) };
    let dest = fp
        .into_path()
        .map_err(|e| format!("解析保存路径失败: {e}"))?;
    std::fs::copy(&target, &dest).map_err(|e| format!("保存失败: {e}"))?;
    Ok(Some(dest.to_string_lossy().into_owned()))
}

// ---- 内部辅助 ----

/// 生效的录屏目录：设置里指定了就用它，否则默认 `<workspace>/recordings`。
fn recordings_dir_effective(workspace: &Path) -> PathBuf {
    let s = super::read_settings(workspace);
    if s.save_dir.trim().is_empty() {
        output::recordings_dir(workspace)
    } else {
        PathBuf::from(s.save_dir.trim())
    }
}

/// 校验 `path` 落在录屏目录内，返回规范化路径（防越权删除/读取）。
fn ensure_in_recordings(workspace: &Path, path: &str) -> Result<PathBuf, String> {
    let base = recordings_dir_effective(workspace)
        .canonicalize()
        .map_err(|e| format!("录屏目录不可用: {e}"))?;
    let target = Path::new(path)
        .canonicalize()
        .map_err(|e| format!("文件不存在: {e}"))?;
    if !target.starts_with(&base) {
        return Err("非法路径：不在录屏目录内".to_string());
    }
    Ok(target)
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(title).body(body).show();
}
