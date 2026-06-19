//! 截图叠加窗口的创建/销毁：无边框、置顶、铺满目标显示器物理矩形（设计文档 §3.2）。
//! 叠加窗是**独立 webview**（`overlay.html` 多入口），主窗口保持原样。

use tauri::{AppHandle, Manager};

pub const OVERLAY_LABEL: &str = "screenshot-overlay";

/// 创建并显示叠加窗口；`frame_path` 为冻结帧绝对路径，连同显示器几何经查询串传给前端。
/// 仅 Windows：依赖 `monitor::MonitorRect`（GDI 抓屏链路的一部分）。
#[cfg(windows)]
pub fn open_overlay(
    app: &AppHandle,
    rect: &super::monitor::MonitorRect,
    frame_path: &str,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use tauri::{PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

    // 已存在则先关掉，避免重复触发残留窗口。
    if let Some(w) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = w.close();
    }

    let url = format!(
        "overlay.html?x={}&y={}&w={}&h={}&frame={}",
        rect.x,
        rect.y,
        rect.width,
        rect.height,
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

    // 精确铺满目标显示器（物理像素，副屏负坐标也适用）。
    win.set_position(PhysicalPosition::new(rect.x, rect.y))
        .context("定位叠加窗口失败")?;
    win.set_size(PhysicalSize::new(rect.width as u32, rect.height as u32))
        .context("设置叠加窗口尺寸失败")?;
    win.show().context("显示叠加窗口失败")?;
    let _ = win.set_focus();
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
