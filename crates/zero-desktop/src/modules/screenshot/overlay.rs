//! 截图叠加窗口的创建/销毁：无边框、置顶、铺满目标显示器物理矩形（设计文档 §3.2）。
//! 叠加窗是**独立 webview**（`overlay.html` 多入口），主窗口保持原样。
//!
//! 卡死修复（docs/2026-06-24-screenshot-freeze-fix/design.md §3.2/§3.3）：
//! - 旧窗未销毁前不重建，超时直接放弃本次截图（不复用旧窗，避免锁定故障态）。
//! - 建窗后启动 ready-ack watchdog：前端 `screenshot_overlay_ready` 未在 ~2.5s
//!   内回执则自动关窗 + 通知，覆盖「JS 起不来 / asset 协议失败」等前端兜底不到的路径。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Manager};

pub const OVERLAY_LABEL: &str = "screenshot-overlay";

/// watchdog 等待 ready ack 的时长。超过此值视为前端未就绪。
const READY_WATCHDOG: Duration = Duration::from_millis(2500);
/// 等待旧窗销毁的总时长（轮询 20ms 一次）。
const OLD_WINDOW_WAIT: Duration = Duration::from_millis(300);

// ---- per-session ready-ack 状态 ----

static NEXT_SID: AtomicU64 = AtomicU64::new(1);
static READY_FLAGS: OnceLock<Mutex<HashMap<u64, Arc<AtomicBool>>>> = OnceLock::new();

fn flags_map() -> &'static Mutex<HashMap<u64, Arc<AtomicBool>>> {
    READY_FLAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 分配新的截图会话 id（单调递增，进程内唯一）。
pub fn next_session_id() -> u64 {
    NEXT_SID.fetch_add(1, Ordering::Relaxed)
}

fn register_session(sid: u64) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    flags_map().lock().unwrap().insert(sid, flag.clone());
    flag
}

/// 前端汇报 overlay 已渲染就绪。返回是否命中已知 session（误传/迟到为 false）。
pub fn mark_ready(sid: u64) -> bool {
    if let Some(f) = flags_map().lock().unwrap().get(&sid) {
        f.store(true, Ordering::SeqCst);
        true
    } else {
        false
    }
}

fn unregister_session(sid: u64) {
    flags_map().lock().unwrap().remove(&sid);
}

/// 创建并显示叠加窗口；`frame_path` 为冻结帧绝对路径，连同显示器几何 + session id
/// 经查询串传给前端。仅 Windows：依赖 `monitor::MonitorRect`（GDI 抓屏链路的一部分）。
#[cfg(windows)]
pub fn open_overlay(
    app: &AppHandle,
    rect: &super::monitor::MonitorRect,
    frame_path: &str,
    session_id: u64,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use tauri::{PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

    // 旧窗存在 → 先关，轮询等其销毁。超时则放弃本次截图（不复用旧窗：旧窗本身
    // 可能就是「透明 + 空白 + 拦截输入」的故障态，set_focus 反而锁定故障）。
    if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = w.close();
        let step = Duration::from_millis(20);
        let mut waited = Duration::ZERO;
        while app.get_webview_window(OVERLAY_LABEL).is_some() && waited < OLD_WINDOW_WAIT {
            std::thread::sleep(step);
            waited += step;
        }
        if let Some(w2) = app.get_webview_window(OVERLAY_LABEL) {
            let _ = w2.close();
            anyhow::bail!("上一个截图叠加窗未释放，请稍后重试");
        }
    }

    let url = format!(
        "overlay.html?x={}&y={}&w={}&h={}&sid={}&frame={}",
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        session_id,
        percent_encode(frame_path)
    );

    let win = WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App(url.into()))
        .title("截图")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .transparent(true)
        .visible(false)
        .build()
        .context("创建截图叠加窗口失败")?;

    // 精确铺满目标显示器（物理像素，副屏负坐标也适用）。任一步失败必兜底 close，
    // 否则会留下「已 build 但未配置」的窗口卡在屏上。
    let configure = || -> anyhow::Result<()> {
        win.set_position(PhysicalPosition::new(rect.x, rect.y))
            .context("定位叠加窗口失败")?;
        win.set_size(PhysicalSize::new(rect.width as u32, rect.height as u32))
            .context("设置叠加窗口尺寸失败")?;
        win.show().context("显示叠加窗口失败")?;
        Ok(())
    };
    if let Err(e) = configure() {
        let _ = win.close();
        return Err(e);
    }
    let _ = win.set_focus();

    // ready-ack watchdog：~2.5s 没收到前端 ready 就强行关窗 + 通知。覆盖所有
    // 「window 已 show 但前端永远跑不到 ack」的路径（JS chunk 缺失 / asset 协议
    // 失败 / 根组件 mount 前崩溃 / preload 死循环）。
    let flag = register_session(session_id);
    let app_clone = app.clone();
    let workspace = app.state::<crate::app_state::AppState>().workspace.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(READY_WATCHDOG).await;
        if flag.load(Ordering::SeqCst) {
            super::commands::trace_capture(&workspace, "overlay-ready", &session_id.to_string());
        } else {
            super::commands::trace_capture(
                &workspace,
                "overlay-ready-timeout",
                &session_id.to_string(),
            );
            if let Some(w) = app_clone.get_webview_window(OVERLAY_LABEL) {
                let _ = w.close();
            }
            super::commands::notify_capture_failed(&app_clone, "叠加窗未就绪，已自动关闭");
        }
        unregister_session(session_id);
    });

    Ok(())
}

/// 关闭叠加窗口（若存在）。
pub fn close_overlay(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = w.close();
    }
}

/// 极简百分号编码：保留 unreserved 字符，其余按 UTF-8 字节编码（兼容 Windows 路径反斜杠/冒号）。
#[cfg(windows)]
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}
