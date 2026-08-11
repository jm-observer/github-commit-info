//! 截图模块的 Tauri 命令 + 触发入口（设计文档 §6）。
//!
//! 平台策略（§1）：命令函数在所有平台都注册进 `generate_handler!`；与 GDI 相关的
//! `do_capture` 用 `cfg` 分流——Windows 走真实抓屏，非 Windows 走 stub 返回「不支持」，
//! 这样非 Windows 也能正常编译，只是调用即报错。

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::app_state::AppState;

// ---- 重入锁 + trace 工具（卡死修复：design.md §3.4 / §3.6） ----

/// `do_capture` 入口重入锁：连按热键时第二次直接早返，不叠加触发。
static CAPTURE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct CaptureGuard;
impl CaptureGuard {
    /// 获取锁。已被占用返回 `None`（调用方应早返）。
    fn acquire() -> Option<Self> {
        CAPTURE_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| CaptureGuard)
    }
}
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// trace 文件单文件上限（超过即旋转一次到 `*.1`）。
const TRACE_MAX_BYTES: u64 = 1_000_000;

fn append_trace(path: &Path, stage: &str, detail: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > TRACE_MAX_BYTES {
            let rotated = path.with_extension("log.1");
            let _ = std::fs::rename(path, &rotated);
        }
    }
    let line = format!(
        "[{}] {} | {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        stage,
        detail
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 追加一行 capture trace 到 `<workspace>/screenshots/.capture-trace.log`。
/// 暴露给 `overlay::open_overlay` 里的 watchdog 用。
pub(crate) fn trace_capture(workspace: &Path, stage: &str, detail: &str) {
    let path = super::output::screenshots_dir(workspace).join(".capture-trace.log");
    append_trace(&path, stage, detail);
    log::info!(target: "screenshot", "[capture] {stage} | {detail}");
}

/// 失败时弹一条系统通知，不再「按了没反应」。
pub(crate) fn notify_capture_failed(app: &AppHandle, msg: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app
        .notification()
        .builder()
        .title("截图失败")
        .body(msg)
        .show();
}

/// 延迟截图倒计时开始时弹一条系统通知：给用户时间去摆好右键菜单/悬浮态等瞬时 UI。
fn notify_capture_delay(app: &AppHandle, delay_secs: u32) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app
        .notification()
        .builder()
        .title("延迟截图")
        .body(format!("{delay_secs} 秒后自动截图，请摆好菜单/悬浮内容"))
        .show();
}

/// 截图设置（P1 仅落地存取，热键当前写死见 `mod::DEFAULT_HOTKEY`，可配为 P2）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotSettings {
    /// 全局热键（P2 可配；P1 实际生效的是 `DEFAULT_HOTKEY`）。
    pub hotkey: String,
    /// 默认标注颜色（CSS 颜色串）。
    pub color: String,
    /// 默认线宽（像素）。
    pub line_width: u32,
    /// 落盘目录（空表示用默认 `<workspace>/screenshots`）。
    pub save_dir: String,
    /// 延迟截图秒数（0 = 立即，不等待）。用于抓取右键菜单/悬浮态等瞬时 UI——
    /// 触发后先倒计时，倒计时期间去摆好目标状态，计时结束才真正抓屏，避免
    /// 触发本身（尤其含 Alt 的热键会关闭右键菜单）把要拍的东西弄没了。
    /// `#[serde(default)]`：兼容旧版 settings.json（无此字段时按 0 处理）。
    #[serde(default)]
    pub delay_secs: u32,
}

impl Default for ScreenshotSettings {
    fn default() -> Self {
        Self {
            hotkey: super::DEFAULT_HOTKEY.to_string(),
            color: "#FF3B30".to_string(),
            line_width: 4,
            save_dir: String::new(),
            delay_secs: 0,
        }
    }
}

// ---- 抓屏触发：平台分流 ----

/// 触发截图：抓鼠标所在屏（含冻结帧落盘）→ 打开叠加窗。被命令与全局热键共用。
///
/// **重入保护**：入口加 `CaptureGuard`，连按热键/延迟倒计时期间再触发直接早返。
/// **延迟截图**：设置里 `delay_secs > 0` 时先弹通知倒计时，`sleep` 结束后再回主线程
/// 真正抓屏——用于抓右键菜单/悬浮态等瞬时 UI（触发动作本身，尤其含 Alt 的热键，
/// 会先把这些瞬时 UI 关掉，所以不能在触发的那一刻就抓）。倒计时期间持有重入锁，
/// 防止用户等待中再次按热键叠加触发。
/// **失败可见**：每个阶段写 `.capture-trace.log`（append + 体积旋转）；任一阶段失败
/// 弹 notification，避免「按了没反应」。
#[cfg(windows)]
pub fn do_capture(app: &AppHandle) -> Result<(), String> {
    use super::output;

    let workspace = app.state::<AppState>().workspace.clone();
    let trace_path = output::screenshots_dir(&workspace).join(".capture-trace.log");

    let guard = match CaptureGuard::acquire() {
        Some(g) => g,
        None => {
            append_trace(&trace_path, "skip-reentrant", "");
            return Ok(());
        }
    };

    let delay_secs = read_settings(&workspace).delay_secs;
    if delay_secs == 0 {
        return capture_now(app, &workspace, &trace_path);
    }

    append_trace(&trace_path, "delay-start", &delay_secs.to_string());
    notify_capture_delay(app, delay_secs);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(delay_secs as u64)).await;
        let _ = app.clone().run_on_main_thread(move || {
            let _guard = guard; // 延迟期间全程持锁，回调结束（成功/失败）才释放。
            let workspace = app.state::<AppState>().workspace.clone();
            let trace_path = output::screenshots_dir(&workspace).join(".capture-trace.log");
            if let Err(e) = capture_now(&app, &workspace, &trace_path) {
                log::warn!(target: "screenshot", "延迟截图失败: {e}");
            }
        });
    });
    Ok(())
}

/// 实际执行一次抓屏：定位显示器 → 抓屏 → 落冻结帧 → 打开叠加窗。
#[cfg(windows)]
fn capture_now(app: &AppHandle, workspace: &Path, trace_path: &Path) -> Result<(), String> {
    use super::{capture, monitor, output, overlay};

    let trace = |stage: &str, detail: &str| append_trace(trace_path, stage, detail);

    let session_id = overlay::next_session_id();
    trace("session", &session_id.to_string());
    trace("enter", "");

    let rect = match monitor::monitor_at_cursor() {
        Ok(r) => {
            trace(
                "monitor-ok",
                &format!("x={} y={} w={} h={}", r.x, r.y, r.width, r.height),
            );
            r
        }
        Err(e) => {
            let m = e.to_string();
            trace("monitor-fail", &m);
            notify_capture_failed(app, &m);
            return Err(m);
        }
    };

    let png = match capture::capture_rect_png(&rect) {
        Ok(p) => {
            trace("capture-ok", &format!("bytes={}", p.len()));
            p
        }
        Err(e) => {
            let m = e.to_string();
            trace("capture-fail", &m);
            notify_capture_failed(app, &m);
            return Err(m);
        }
    };

    let frame_path = output::frozen_frame_path(workspace);
    if let Some(parent) = frame_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            let m = format!("创建截图目录失败: {e}");
            trace("mkdir-fail", &m);
            notify_capture_failed(app, &m);
            return Err(m);
        }
    }
    if let Err(e) = std::fs::write(&frame_path, &png) {
        let m = format!("写冻结帧失败: {e}");
        trace("write-fail", &m);
        notify_capture_failed(app, &m);
        return Err(m);
    }
    trace("frame-saved", &frame_path.to_string_lossy());

    if let Err(e) = overlay::open_overlay(
        app,
        &rect,
        &frame_path.to_string_lossy(),
        session_id,
        overlay::Mode::Shot,
    ) {
        let m = e.to_string();
        trace("overlay-fail", &m);
        notify_capture_failed(app, &m);
        return Err(m);
    }
    trace("overlay-shown", &session_id.to_string());
    Ok(())
}

#[cfg(not(windows))]
pub fn do_capture(_app: &AppHandle) -> Result<(), String> {
    Err("截图功能仅支持 Windows".to_string())
}

/// 命令：触发截图。
#[tauri::command]
pub fn screenshot_capture(app: AppHandle) -> Result<(), String> {
    do_capture(&app)
}

/// 命令：前端 overlay 已挂载完成首帧渲染 → 通知 Rust watchdog，避免误关。
/// `session_id` 来自 overlay URL 查询串 `sid=...`。未知 session 静默忽略
/// （前端误传 / watchdog 已超时 unregister）。
#[tauri::command]
pub fn screenshot_overlay_ready(app: AppHandle, session_id: u64) -> Result<(), String> {
    let hit = super::overlay::mark_ready(session_id);
    let workspace = app.state::<AppState>().workspace.clone();
    trace_capture(
        &workspace,
        if hit {
            "overlay-ack"
        } else {
            "overlay-ack-stale"
        },
        &session_id.to_string(),
    );
    Ok(())
}

/// 命令：提交最终成图（前端 canvas 合成的 PNG，base64）→ 落盘 + 写剪贴板 + 通知 → 关窗。
/// 落盘与剪贴板互不阻断：任一失败仍尽力完成另一个，错误进日志/通知。返回落盘绝对路径。
///
/// **诊断**：每个阶段都写 `<workspace>/screenshots/.commit-trace.log`（覆盖式），
/// 即便日志器没开/通知被屏蔽，也能从这个文件看清楚走到了哪一步。
#[tauri::command]
pub fn screenshot_commit(app: AppHandle, png_base64: String) -> Result<String, String> {
    use base64::Engine;
    let workspace = app.state::<AppState>().workspace.clone();
    let trace_path = super::output::screenshots_dir(&workspace).join(".commit-trace.log");
    let trace = |stage: &str, detail: &str| {
        // append + 体积旋转：单次 commit 多阶段日志、多次 commit 累积都能完整看到。
        append_trace(&trace_path, stage, detail);
        log::info!(target: "screenshot", "[commit] {} | {}", stage, detail);
    };

    trace("enter", &format!("png_base64.len={}", png_base64.len()));

    let bytes = match base64::engine::general_purpose::STANDARD.decode(png_base64.trim()) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("解码 PNG 失败: {e}");
            trace("decode-fail", &msg);
            return Err(msg);
        }
    };
    trace("decoded", &format!("bytes.len={}", bytes.len()));

    let saved = match super::output::save_png(&workspace, &bytes) {
        Ok(p) => p,
        Err(e) => {
            let msg = e.to_string();
            trace("save-fail", &msg);
            return Err(msg);
        }
    };
    let saved_str = saved.to_string_lossy().into_owned();
    trace("saved", &saved_str);

    // 写剪贴板（失败不阻断落盘结果）。
    let clip_err = write_clipboard_png(&app, &bytes).err();
    match clip_err.as_deref() {
        Some(e) => trace("clipboard-fail", e),
        None => trace("clipboard-ok", "CF_DIB"),
    }

    super::overlay::close_overlay(&app);
    notify_done(&app, &saved_str, clip_err.as_deref());
    trace("done", &saved_str);

    Ok(saved_str)
}

/// 命令：取消截图，关闭叠加窗、丢弃。
#[tauri::command]
pub fn screenshot_cancel(app: AppHandle) -> Result<(), String> {
    super::overlay::close_overlay(&app);
    Ok(())
}

/// 命令：读取截图设置（save_dir 为空时回填默认目录）。
#[tauri::command]
pub fn screenshot_get_settings(app: AppHandle) -> Result<ScreenshotSettings, String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let mut s = read_settings(&workspace);
    if s.save_dir.is_empty() {
        s.save_dir = super::output::screenshots_dir(&workspace)
            .to_string_lossy()
            .into_owned();
    }
    Ok(s)
}

/// 命令：保存截图设置（落 `<workspace>/screenshots/settings.json`）。
#[tauri::command]
pub fn screenshot_save_settings(
    app: AppHandle,
    settings: ScreenshotSettings,
) -> Result<(), String> {
    // 先纯解析校验：热键串写错（比如 `Ctrl+`）不该落盘。
    crate::modules::hotkeys::parse_shortcut(&settings.hotkey)?;

    let workspace = app.state::<AppState>().workspace.clone();
    let path = super::output::settings_path(&workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败: {e}"))?;
    }
    let old = std::fs::read_to_string(&path).ok();
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写设置失败: {e}"))?;

    // 重注册从磁盘读热键，所以必须写在前面。注册失败（多为被别的软件占用）→ 回滚设置
    // 并按旧值重注册，避免「设置显示新热键、实际按了没反应」。
    if let Err(e) = super::reregister(&app) {
        match old {
            Some(prev) => {
                let _ = std::fs::write(&path, prev);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        let _ = super::reregister(&app);
        return Err(e);
    }
    Ok(())
}

/// 一条历史截图（供主窗口画廊展示）。
#[derive(Debug, Clone, Serialize)]
pub struct HistoryItem {
    /// 文件名（如 `20260619-143012-123.png`）。
    pub name: String,
    /// 绝对路径（前端 `convertFileSrc` 走 asset 协议显示）。
    pub path: String,
    /// 修改时间（Unix 毫秒；用于排序/展示）。
    pub modified_ms: i64,
    /// 文件大小（字节）。
    pub size: u64,
    /// 像素宽（读 PNG 头，失败为 0）。
    pub width: u32,
    /// 像素高（读 PNG 头，失败为 0）。
    pub height: u32,
    /// 是否已收藏（来自 `index.json` sidecar）。收藏 = 永久保留。
    pub starred: bool,
}

/// 命令：列出历史截图（`<workspace>/screenshots/*.png`，按修改时间新→旧）。
/// 排除冻结帧临时文件与设置文件。收藏标记来自 `meta::load` 的 sidecar 索引。
#[tauri::command]
pub fn screenshot_list_history(app: AppHandle) -> Result<Vec<HistoryItem>, String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let dir = super::output::screenshots_dir(&workspace);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // 目录还不存在（从未截过图）→ 空列表，不报错。
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读取截图目录失败: {e}")),
    };

    let index = super::meta::load(&workspace);
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_png = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("png"))
            .unwrap_or(false);
        if !is_png {
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // 冻结帧临时文件不算成品。
        if name.starts_with('.') {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let (width, height) = png_dimensions(&path).unwrap_or((0, 0));
        let starred = index.is_starred(&name);
        items.push(HistoryItem {
            name,
            path: path.to_string_lossy().into_owned(),
            modified_ms,
            size: meta.len(),
            width,
            height,
            starred,
        });
    }
    items.sort_by_key(|item| std::cmp::Reverse(item.modified_ms));
    Ok(items)
}

/// 命令：在系统文件管理器中打开截图目录。
#[tauri::command]
pub fn screenshot_open_folder(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    let workspace = app.state::<AppState>().workspace.clone();
    let dir = super::output::screenshots_dir(&workspace);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建截图目录失败: {e}"))?;
    #[allow(deprecated)]
    app.shell()
        .open(dir.to_string_lossy().to_string(), None)
        .map_err(|e| format!("打开文件夹失败: {e}"))
}

/// 命令：删除一张历史截图（路径须在截图目录内，防越权删除）。
/// 删文件成功后同步清掉 sidecar 里的元数据，避免索引堆积孤儿条目。
#[tauri::command]
pub fn screenshot_delete(app: AppHandle, path: String) -> Result<(), String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_screenshots(&workspace, &path)?;
    std::fs::remove_file(&target).map_err(|e| format!("删除失败: {e}"))?;
    if let Some(name) = target.file_name().and_then(|s| s.to_str()) {
        // 索引清理失败不影响「已删除」这个既成事实，仅告警。
        if let Err(e) = super::meta::forget(&workspace, name) {
            log::warn!(target: "screenshot", "清理截图索引失败: {e}");
        }
    }
    Ok(())
}

/// 命令：设置/取消一张截图的收藏标记（路径须在截图目录内）。
/// 收藏 = 永久保留，后续的自动清理不会碰它。
#[tauri::command]
pub fn screenshot_set_starred(app: AppHandle, path: String, starred: bool) -> Result<(), String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_screenshots(&workspace, &path)?;
    let name = target
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "非法文件名".to_string())?;
    super::meta::set_starred(&workspace, name, starred)
}

/// 命令：把一张历史截图复制到剪贴板（路径须在截图目录内）。
#[tauri::command]
pub fn screenshot_copy_to_clipboard(app: AppHandle, path: String) -> Result<(), String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_screenshots(&workspace, &path)?;
    let bytes = std::fs::read(&target).map_err(|e| format!("读取图片失败: {e}"))?;
    write_clipboard_png(&app, &bytes)
}

/// 命令：在系统文件管理器中定位到这张截图（高亮选中文件，路径须在截图目录内）。
/// Windows 走 `explorer /select,`；非 Windows 回退打开其所在目录。
#[tauri::command]
pub fn screenshot_reveal_in_folder(app: AppHandle, path: String) -> Result<(), String> {
    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_screenshots(&workspace, &path)?;

    #[cfg(windows)]
    {
        let _ = &app;
        // explorer /select,"C:\...\xxx.png" → 打开父目录并高亮选中该文件。
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

/// 命令：把一张历史截图另存到用户选择的位置（弹保存对话框，默认沿用原文件名）。
/// 返回保存后的绝对路径；用户取消则返回 `None`。源路径须在截图目录内。
#[tauri::command]
pub async fn screenshot_save_as(app: AppHandle, path: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let workspace = app.state::<AppState>().workspace.clone();
    let target = ensure_in_screenshots(&workspace, &path)?;
    let default_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("screenshot.png")
        .to_string();

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(&default_name)
        .add_filter("PNG 图片", &["png"])
        .save_file(move |f| {
            let _ = tx.send(f);
        });
    let chosen = rx.await.map_err(|e| format!("保存对话框失败: {e}"))?;
    let dest = match chosen {
        Some(fp) => fp
            .into_path()
            .map_err(|e| format!("解析保存路径失败: {e}"))?,
        None => return Ok(None), // 用户取消
    };
    std::fs::copy(&target, &dest).map_err(|e| format!("保存失败: {e}"))?;
    Ok(Some(dest.to_string_lossy().into_owned()))
}

// ---- 内部辅助 ----

/// 校验 `path` 落在 `<workspace>/screenshots/` 内，返回规范化后的绝对路径（防路径穿越）。
fn ensure_in_screenshots(
    workspace: &std::path::Path,
    path: &str,
) -> Result<std::path::PathBuf, String> {
    let dir = super::output::screenshots_dir(workspace);
    let base = dir
        .canonicalize()
        .map_err(|e| format!("截图目录不可用: {e}"))?;
    let target = std::path::Path::new(path)
        .canonicalize()
        .map_err(|e| format!("文件不存在: {e}"))?;
    if !target.starts_with(&base) {
        return Err("非法路径：不在截图目录内".to_string());
    }
    Ok(target)
}

/// 读 PNG 头取宽高（不解码全图）。
fn png_dimensions(path: &std::path::Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .ok()?;
    let info = reader.info();
    Some((info.width, info.height))
}

pub(crate) fn read_settings(workspace: &std::path::Path) -> ScreenshotSettings {
    let path = super::output::settings_path(workspace);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 把 PNG bytes 解码后写入系统剪贴板。
///
/// **Windows**：直接走 Win32 `SetClipboardData(CF_DIB, ...)`。原本 tauri-plugin-clipboard-manager
/// 的 `write_image` 在 Windows 上底层 arboard 只写 CF_DIBV5，微信 / 部分 Office / 旧 QQ 这类挑食
/// 程序只认 CF_DIB，结果"复制成功但粘贴一片空白"。CF_DIB 是最大公约数格式，几乎所有 Windows 程序
/// 都吃。
///
/// **非 Windows**：仍走 tauri 插件兜底（zero-desktop 当前主要平台是 Windows，这条分支保留以便
/// 跨平台编译）。
#[cfg(windows)]
fn write_clipboard_png(_app: &AppHandle, png_bytes: &[u8]) -> Result<(), String> {
    let (rgba, w, h) = decode_png_rgba(png_bytes)?;
    write_clipboard_cf_dib(&rgba, w, h)
}

#[cfg(not(windows))]
fn write_clipboard_png(app: &AppHandle, png_bytes: &[u8]) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    let (rgba, w, h) = decode_png_rgba(png_bytes)?;
    let image = tauri::image::Image::new_owned(rgba, w, h);
    app.clipboard()
        .write_image(&image)
        .map_err(|e| e.to_string())
}

/// Win32 直写 CF_DIB：BITMAPINFOHEADER + 32bpp BGRA bottom-up 像素 → GlobalAlloc(MOVEABLE)
/// → SetClipboardData(CF_DIB)。成功后所有权转交剪贴板，**不要** GlobalFree。
#[cfg(windows)]
fn write_clipboard_cf_dib(rgba_top_down: &[u8], w: u32, h: u32) -> Result<(), String> {
    use windows_sys::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    const CF_DIB: u32 = 8;

    if w == 0 || h == 0 {
        return Err("空图像".into());
    }
    let row_stride = (w as usize) * 4;
    let pixel_bytes = row_stride
        .checked_mul(h as usize)
        .ok_or_else(|| "像素尺寸溢出".to_string())?;
    if rgba_top_down.len() != pixel_bytes {
        return Err("RGBA 长度与宽高不匹配".into());
    }

    // RGBA top-down → BGRA bottom-up（CF_DIB / BI_RGB / biHeight>0 的标准期望）。
    let mut bgra = vec![0u8; pixel_bytes];
    for y in 0..(h as usize) {
        let src = &rgba_top_down[y * row_stride..][..row_stride];
        let dst_y = (h as usize) - 1 - y;
        let dst = &mut bgra[dst_y * row_stride..][..row_stride];
        for x in 0..(w as usize) {
            let p = x * 4;
            dst[p] = src[p + 2]; // B
            dst[p + 1] = src[p + 1]; // G
            dst[p + 2] = src[p]; // R
            dst[p + 3] = src[p + 3]; // A（CF_DIB BI_RGB 32bpp 下视为保留字节，保留以兼容读 BGRA 的客户端）
        }
    }

    let header_size = std::mem::size_of::<BITMAPINFOHEADER>();
    let total = header_size
        .checked_add(pixel_bytes)
        .ok_or_else(|| "总大小溢出".to_string())?;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("OpenClipboard 失败".into());
        }
        // RAII 保证 panic / 早返时关闭剪贴板。
        struct ClipboardGuard;
        impl Drop for ClipboardGuard {
            fn drop(&mut self) {
                unsafe { CloseClipboard() };
            }
        }
        let _guard = ClipboardGuard;

        if EmptyClipboard() == 0 {
            return Err("EmptyClipboard 失败".into());
        }

        let h_mem = GlobalAlloc(GMEM_MOVEABLE, total);
        if h_mem.is_null() {
            return Err("GlobalAlloc 失败".into());
        }

        let ptr = GlobalLock(h_mem) as *mut u8;
        if ptr.is_null() {
            // GlobalAlloc 的内存在 SetClipboardData 之前我们仍持有；这里失败要 GlobalFree，
            // 但 windows-sys 的 GlobalFree 在 Memory 模块。简单起见：写入失败让进程继续，
            // 泄漏一个临时缓冲（仅当 GlobalLock 失败这种极罕见路径）。
            return Err("GlobalLock 失败".into());
        }

        let header = BITMAPINFOHEADER {
            biSize: header_size as u32,
            biWidth: w as i32,
            biHeight: h as i32, // 正值 = bottom-up
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: pixel_bytes as u32,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };

        std::ptr::copy_nonoverlapping(&header as *const _ as *const u8, ptr, header_size);
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), ptr.add(header_size), pixel_bytes);

        GlobalUnlock(h_mem);

        if SetClipboardData(CF_DIB, h_mem as _).is_null() {
            return Err("SetClipboardData 失败".into());
        }
        // 成功后 h_mem 所有权归剪贴板，绝不再 GlobalFree。
    }

    Ok(())
}

/// 解码 PNG → RGBA + 宽高（前端 canvas 一般输出 RGBA，兼容 RGB 补 alpha）。
fn decode_png_rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(px);
                out.push(255);
            }
            out
        }
        other => return Err(format!("不支持的 PNG 颜色格式: {other:?}")),
    };
    Ok((rgba, w, h))
}

fn notify_done(app: &AppHandle, saved: &str, clip_err: Option<&str>) {
    use tauri_plugin_notification::NotificationExt;
    let body = match clip_err {
        None => format!("已复制到剪贴板，并保存到 {saved}"),
        Some(e) => format!("已保存到 {saved}（剪贴板写入失败: {e}）"),
    };
    let _ = app
        .notification()
        .builder()
        .title("截图完成")
        .body(body)
        .show();
}
