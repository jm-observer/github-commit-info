//! 截图叠加窗口的创建/销毁：无边框、置顶、铺满目标显示器物理矩形（设计文档 §3.2）。
//! 叠加窗是**独立 webview**（`overlay.html` 多入口），主窗口保持原样。
//!
//! 卡死修复（docs/2026-06-24-screenshot-freeze-fix/design.md §3.2/§3.3）：
//! - 旧窗未销毁前不重建，超时直接放弃本次截图（不复用旧窗，避免锁定故障态）。
//! - 建窗后启动 ready-ack watchdog：前端 `screenshot_overlay_ready` 未在 ~2.5s
//!   内回执则自动关窗 + 通知，覆盖「JS 起不来 / asset 协议失败」等前端兜底不到的路径。
//! - **滞留兜底**（推翻原 design.md §6「不做长期巡检」的结论）：ready-ack watchdog 只
//!   覆盖「show() 之后到就绪之前」这个窗口期，一旦前端 ack 过了，之后再出问题（选区
//!   交互中 JS 崩、commit 卡住、进程被外部挂起）就再没人管，而残留代价是整个桌面失去
//!   输入。故给每个 overlay 加一条上限 `LINGER_LIMIT` 的存活线，超时无条件关窗。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Manager};

pub const OVERLAY_LABEL: &str = "screenshot-overlay";

/// 叠加窗的用途。同一套框选交互服务两个功能，靠这个参数分流前端行为：
/// 截图选完进标注、录屏选完直接开录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 截图：框选 → 标注 → 提交 PNG。
    Shot,
    /// 录屏：框选 → 开始录制（无标注环节）。
    Record,
}

impl Mode {
    /// 写进叠加窗 URL 查询串的值。
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Shot => "shot",
            Mode::Record => "record",
        }
    }
}

/// watchdog 等待 ready ack 的时长。超过此值视为前端未就绪。
const READY_WATCHDOG: Duration = Duration::from_millis(2500);
/// 等待旧窗销毁的总时长（轮询 20ms 一次）。
const OLD_WINDOW_WAIT: Duration = Duration::from_millis(300);
/// overlay 存活上限：超过这个时间还没 commit/cancel，一律视为滞留并强制关窗。
/// 取值考虑「人正常框选 + 标注」的耗时上限——两分钟足够从容操作，又不至于让一层
/// 隐身玻璃罩着桌面太久。
const LINGER_LIMIT: Duration = Duration::from_secs(120);

// ---- per-session ready-ack 状态 ----

static NEXT_SID: AtomicU64 = AtomicU64::new(1);
static READY_FLAGS: OnceLock<Mutex<HashMap<u64, Arc<AtomicBool>>>> = OnceLock::new();

/// 当前屏上 overlay 归属的 session id；0 = 当前无 overlay。
/// 滞留 watchdog 到期时据此确认「屏上这个窗还是不是我建的那个」，避免误关后续截图。
static CURRENT_SESSION: AtomicU64 = AtomicU64::new(0);

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
    mode: Mode,
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
        "overlay.html?x={}&y={}&w={}&h={}&sid={}&mode={}&frame={}",
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        session_id,
        mode.as_str(),
        percent_encode(frame_path)
    );

    let win = WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App(url.into()))
        .title(if mode == Mode::Record {
            "录屏选区"
        } else {
            "截图"
        })
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
    CURRENT_SESSION.store(session_id, Ordering::SeqCst);

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
            // 只关自己那一扇：屏上若已换成后续截图的 overlay，无条件 close_overlay 会连它一起
            // 关掉，还会把 CURRENT_SESSION 清零——新 overlay 的滞留 watchdog 就会认不出自己、
            // 提前放弃，等于新窗失去 120s 兜底，正是本文件要加的那道闸。
            if close_overlay_if_current(&app_clone, session_id) {
                super::commands::notify_capture_failed(&app_clone, "叠加窗未就绪，已自动关闭");
            }
        }
        unregister_session(session_id);
    });

    // 滞留 watchdog：ready 之后的所有故障路径的最后一道闸。到期时若屏上 overlay
    // 仍是本次 session 的（commit/cancel 会把 CURRENT_SESSION 清零，正常路径不会命中），
    // 无条件关窗——宁可丢一次没做完的截图，也不能让隐身玻璃继续罩着整个桌面。
    let app_linger = app.clone();
    let workspace_linger = app.state::<crate::app_state::AppState>().workspace.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(LINGER_LIMIT).await;
        if CURRENT_SESSION.load(Ordering::SeqCst) != session_id {
            return; // 已正常收尾，或已被后续截图接管
        }
        if app_linger.get_webview_window(OVERLAY_LABEL).is_none() {
            CURRENT_SESSION.store(0, Ordering::SeqCst);
            return;
        }
        super::commands::trace_capture(
            &workspace_linger,
            "overlay-linger-timeout",
            &session_id.to_string(),
        );
        if close_overlay_if_current(&app_linger, session_id) {
            super::commands::notify_capture_failed(&app_linger, "截图窗停留过久，已自动关闭");
        }
    });

    Ok(())
}

/// 关闭叠加窗口（若存在）。所有关窗路径（commit / cancel / 两个 watchdog / 主窗退出）
/// 都走这里，顺带清掉 `CURRENT_SESSION`，让滞留 watchdog 到期时认得出「已收尾」。
pub fn close_overlay(app: &AppHandle) {
    CURRENT_SESSION.store(0, Ordering::SeqCst);
    if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = w.close();
    }
}

/// 仅当屏上 overlay 仍归属 `session_id` 时才关窗，返回是否真的关了。
///
/// **watchdog 专用**：定时器是「过去某一刻排下的队」，到期时屏上那扇窗可能已经不是当初那扇。
/// 无条件 [`close_overlay`] 会误关后继截图，还会把 `CURRENT_SESSION` 清零、连带废掉它的滞留
/// 兜底。用户主动触发的路径（commit / cancel / 关主窗）不用这个——那些就是要关掉屏上任何一扇。
fn close_overlay_if_current(app: &AppHandle, session_id: u64) -> bool {
    if CURRENT_SESSION
        .compare_exchange(session_id, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }
    if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = w.close();
    }
    true
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
