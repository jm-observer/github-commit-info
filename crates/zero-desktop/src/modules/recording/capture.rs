//! [cfg(windows)] 可复用的 GDI 抓帧器：给录屏用的「反复抓同一块矩形」。
//!
//! 与 screenshot 的 `capture.rs` 的区别在于**生命周期**：截图是抓一次就释放，录屏
//! 每秒要抓十几次，如果每帧都 `CreateCompatibleDC`/`CreateCompatibleBitmap` 再销毁，
//! GDI 句柄的申请释放会成为固定开销。这里把 DC 与位图握在 [`Grabber`] 里，整场录制
//! 只建一次，每帧只做 `BitBlt` + `GetDIBits`。
//!
//! 另一处区别：**不做 BGRA→RGBA 翻转**。ffmpeg 直接吃 `-pix_fmt bgra`，GDI 给什么
//! 就喂什么，省掉每帧一遍全画面的字节交换。

use anyhow::{bail, Result};
use std::ffi::c_void;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, SRCCOPY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, ICONINFO,
};

use super::Rect;

/// 持有整场录制期间复用的 GDI 资源。
///
/// **不是 `Send` 友好的类型吗？** GDI 的 DC 可以跨线程用，只要同一时刻只有一个线程碰它。
/// 这里由抓帧线程独占（`session` 里在线程内构造、线程内销毁），不跨线程共享。
pub struct Grabber {
    rect: Rect,
    screen_dc: HDC,
    mem_dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    draw_cursor: bool,
}

impl Grabber {
    /// 为指定矩形建好复用资源。
    pub fn new(rect: Rect, draw_cursor: bool) -> Result<Self> {
        if rect.width <= 0 || rect.height <= 0 {
            bail!("非法的录制尺寸 {}x{}", rect.width, rect.height);
        }
        unsafe {
            let screen_dc = GetDC(std::ptr::null_mut());
            if screen_dc.is_null() {
                bail!("GetDC 失败");
            }
            let mem_dc = CreateCompatibleDC(screen_dc);
            if mem_dc.is_null() {
                ReleaseDC(std::ptr::null_mut(), screen_dc);
                bail!("CreateCompatibleDC 失败");
            }
            let bitmap = CreateCompatibleBitmap(screen_dc, rect.width, rect.height);
            if bitmap.is_null() {
                DeleteDC(mem_dc);
                ReleaseDC(std::ptr::null_mut(), screen_dc);
                bail!("CreateCompatibleBitmap 失败");
            }
            let old = SelectObject(mem_dc, bitmap);
            Ok(Self {
                rect,
                screen_dc,
                mem_dc,
                bitmap,
                old,
                draw_cursor,
            })
        }
    }

    /// 每帧的字节数（BGRA，无行填充）。
    pub fn frame_bytes(&self) -> usize {
        (self.rect.width as usize) * (self.rect.height as usize) * 4
    }

    /// 抓一帧到 `buf`（top-down BGRA）。`buf` 长度须等于 [`Self::frame_bytes`]。
    pub fn grab_into(&mut self, buf: &mut [u8]) -> Result<()> {
        if buf.len() != self.frame_bytes() {
            bail!("帧缓冲长度不匹配");
        }
        unsafe {
            if BitBlt(
                self.mem_dc,
                0,
                0,
                self.rect.width,
                self.rect.height,
                self.screen_dc,
                self.rect.x,
                self.rect.y,
                SRCCOPY,
            ) == 0
            {
                bail!("BitBlt 抓屏失败");
            }

            if self.draw_cursor {
                // 画不上不算录制失败——指针没了总比整场录制中断强。
                if let Err(e) = self.blend_cursor() {
                    log::debug!(target: "recording", "绘制鼠标指针失败: {e}");
                }
            }

            // biHeight 取负 → top-down 行序；32bpp BI_RGB → 缓冲即 BGRA。
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = self.rect.width;
            bmi.bmiHeader.biHeight = -self.rect.height;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;

            let lines = GetDIBits(
                self.mem_dc,
                self.bitmap,
                0,
                self.rect.height as u32,
                buf.as_mut_ptr() as *mut c_void,
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if lines == 0 {
                bail!("GetDIBits 读取像素失败");
            }
        }
        Ok(())
    }

    /// 把当前鼠标指针画进内存位图（坐标要减去录制矩形原点，再减去热点偏移）。
    unsafe fn blend_cursor(&self) -> Result<()> {
        let mut ci: CURSORINFO = std::mem::zeroed();
        ci.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut ci) == 0 {
            bail!("GetCursorInfo 失败");
        }
        if ci.flags != CURSOR_SHOWING || ci.hCursor.is_null() {
            return Ok(()); // 指针隐藏（比如全屏视频播放中）→ 本就不该画
        }

        // 热点：光标位图里代表「实际点击位置」的那个像素，画的时候要按它对齐。
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(ci.hCursor, &mut ii) == 0 {
            bail!("GetIconInfo 失败");
        }
        // GetIconInfo 会创建两个位图，责任在调用方释放。
        if !ii.hbmMask.is_null() {
            DeleteObject(ii.hbmMask as HGDIOBJ);
        }
        if !ii.hbmColor.is_null() {
            DeleteObject(ii.hbmColor as HGDIOBJ);
        }

        let pt = POINT {
            x: ci.ptScreenPos.x - self.rect.x - ii.xHotspot as i32,
            y: ci.ptScreenPos.y - self.rect.y - ii.yHotspot as i32,
        };
        if DrawIconEx(
            self.mem_dc,
            pt.x,
            pt.y,
            ci.hCursor,
            0,
            0,
            0,
            std::ptr::null_mut(),
            DI_NORMAL,
        ) == 0
        {
            bail!("DrawIconEx 失败");
        }
        Ok(())
    }
}

impl Drop for Grabber {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem_dc, self.old);
            DeleteObject(self.bitmap as HGDIOBJ);
            DeleteDC(self.mem_dc);
            ReleaseDC(std::ptr::null_mut(), self.screen_dc);
        }
    }
}
