//! [cfg(windows)] GDI 抓屏 → PNG bytes（P1 不含光标）。
//!
//! 链路（设计文档 §3.1）：`GetDC` → `CreateCompatibleDC`/`CreateCompatibleBitmap`
//! → `BitBlt` 把屏幕拷进位图句柄 → `GetDIBits`（`biHeight` 取负得 top-down、32bpp BGRA）
//! 读出像素 → BGRA 翻 RGBA → `png` 编码。光标进图为 P2 可选项，本文件不实现。

use anyhow::{bail, Result};
use std::ffi::c_void;
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};

use super::monitor::MonitorRect;

/// 抓取指定显示器物理矩形，返回编码后的 PNG bytes（RGBA）。
pub fn capture_rect_png(rect: &MonitorRect) -> Result<Vec<u8>> {
    let w = rect.width;
    let h = rect.height;
    if w <= 0 || h <= 0 {
        bail!("非法的截屏尺寸 {w}x{h}");
    }

    let rgba = unsafe { grab_rgba(rect, w, h)? };
    encode_png(&rgba, w as u32, h as u32)
}

/// 用 GDI 抓屏并返回 top-down RGBA 像素（已把 BGRA 翻成 RGBA、alpha 置 255）。
unsafe fn grab_rgba(rect: &MonitorRect, w: i32, h: i32) -> Result<Vec<u8>> {
    let screen_dc = GetDC(std::ptr::null_mut());
    if screen_dc.is_null() {
        bail!("GetDC 失败");
    }
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_null() {
        ReleaseDC(std::ptr::null_mut(), screen_dc);
        bail!("CreateCompatibleDC 失败");
    }
    let bitmap = CreateCompatibleBitmap(screen_dc, w, h);
    if bitmap.is_null() {
        DeleteDC(mem_dc);
        ReleaseDC(std::ptr::null_mut(), screen_dc);
        bail!("CreateCompatibleBitmap 失败");
    }

    let old = SelectObject(mem_dc, bitmap);
    let blt_ok = BitBlt(mem_dc, 0, 0, w, h, screen_dc, rect.x, rect.y, SRCCOPY);

    let result: Result<Vec<u8>> = if blt_ok == 0 {
        Err(anyhow::anyhow!("BitBlt 抓屏失败"))
    } else {
        // GetDIBits：biHeight 取负 → top-down 行序；32bpp、BI_RGB（无压缩）→ 缓冲为 BGRA。
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB as u32;

        let stride = (w as usize) * 4;
        let mut buf = vec![0u8; stride * (h as usize)];
        let lines = GetDIBits(
            mem_dc,
            bitmap,
            0,
            h as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut bmi,
            DIB_RGB_COLORS,
        );
        if lines == 0 {
            Err(anyhow::anyhow!("GetDIBits 读取像素失败"))
        } else {
            // BGRA → RGBA，alpha 强制 255（GDI 抓屏 alpha 通道不可靠）。
            for px in buf.chunks_exact_mut(4) {
                px.swap(0, 2);
                px[3] = 255;
            }
            Ok(buf)
        }
    };

    // 释放 GDI 资源（无论成功与否）。
    SelectObject(mem_dc, old);
    DeleteObject(bitmap);
    DeleteDC(mem_dc);
    ReleaseDC(std::ptr::null_mut(), screen_dc);

    result
}

/// 把 RGBA 像素编码成 PNG bytes（轻量 `png` crate）。
fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}
