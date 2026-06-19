//! [cfg(windows)] 定位鼠标当前所在显示器的物理矩形。
//!
//! P1 限定「鼠标所在那块屏」：抓屏与叠加窗都按这块显示器的物理矩形对齐
//! （含负坐标的副屏）。详见设计文档 §3.1 / §8「多屏几何」。

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// 显示器物理矩形（像素）。`x`/`y` 可能为负（副屏在主屏左侧/上方）。
#[derive(Debug, Clone, Copy)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 返回鼠标当前所在显示器的物理矩形。
pub fn monitor_at_cursor() -> Result<MonitorRect> {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) == 0 {
            bail!("GetCursorPos 失败");
        }
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        if hmon.is_null() {
            bail!("MonitorFromPoint 未找到显示器");
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hmon, &mut mi) == 0 {
            bail!("GetMonitorInfoW 失败");
        }
        let r = mi.rcMonitor;
        let width = r.right - r.left;
        let height = r.bottom - r.top;
        if width <= 0 || height <= 0 {
            bail!("显示器矩形非法: {width}x{height}");
        }
        Ok(MonitorRect {
            x: r.left,
            y: r.top,
            width,
            height,
        })
    }
}
