//! 录制中的悬浮控制条窗口（独立 webview，`recorder.html` 多入口）。
//!
//! 为什么非要有它：录屏一旦开始，画面上没有任何东西表明「正在录」——既容易忘关，
//! 也没法在不切窗口的前提下暂停/停止。控制条给的是这三件事：**计时、暂停、停止**。
//!
//! **摆放**：控制条本身也在屏幕上，会被录进画面。所以优先放到录制区域**外面**
//! （下方 → 上方），实在放不下（整屏录制）才压在区域内的右下角——此时会被录进去，
//! 这是整屏录制无法回避的取舍，不做假装解决。

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

use super::Rect;

pub const BAR_LABEL: &str = "recording-bar";

/// 控制条尺寸（物理像素）。
const BAR_W: i32 = 232;
const BAR_H: i32 = 44;
/// 与录制区域的间隙。
const GAP: i32 = 10;

/// 打开控制条。失败只告警——控制条起不来不该让已经开始的录制跟着废掉，
/// 用户仍可用热键或主窗口停止。
pub fn open(app: &AppHandle, region: &Rect, screen: &Rect) {
    if let Err(e) = try_open(app, region, screen) {
        log::warn!(target: "recording", "打开录制控制条失败: {e}");
    }
}

fn try_open(app: &AppHandle, region: &Rect, screen: &Rect) -> anyhow::Result<()> {
    close(app);

    let (x, y) = place(region, screen);

    let win = WebviewWindowBuilder::new(app, BAR_LABEL, WebviewUrl::App("recorder.html".into()))
        .title("录制中")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .transparent(true)
        .shadow(false)
        .focused(false)
        .visible(false)
        .build()?;

    let configure = || -> anyhow::Result<()> {
        win.set_position(PhysicalPosition::new(x, y))?;
        win.set_size(PhysicalSize::new(BAR_W as u32, BAR_H as u32))?;
        win.show()?;
        Ok(())
    };
    if let Err(e) = configure() {
        let _ = win.close();
        return Err(e);
    }
    Ok(())
}

/// 选摆放位置：区域下方 → 区域上方 → 区域内右下角（整屏录制时的兜底）。
fn place(region: &Rect, screen: &Rect) -> (i32, i32) {
    let center_x = region.x + (region.width - BAR_W) / 2;
    let x = center_x.clamp(screen.x, screen.x + screen.width - BAR_W);

    let below = region.y + region.height + GAP;
    if below + BAR_H <= screen.y + screen.height {
        return (x, below);
    }
    let above = region.y - GAP - BAR_H;
    if above >= screen.y {
        return (x, above);
    }
    // 无处可躲：压在区域内右下角，会被录进画面。
    (
        region.x + region.width - BAR_W - GAP,
        region.y + region.height - BAR_H - GAP,
    )
}

/// 关闭控制条（不存在时无副作用）。
pub fn close(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(BAR_LABEL) {
        let _ = w.close();
    }
}
