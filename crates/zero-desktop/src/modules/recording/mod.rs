//! recording 模块：全局热键唤起 → 框选区域 → 定时抓帧喂给 ffmpeg → mp4 落盘。
//!
//! 职责切分与 screenshot 一致：Rust 做平台脏活（抓屏 / 全局热键 / 子进程 / 输出），
//! 前端做框选（复用截图叠加窗的 `mode=record`）与录制中的悬浮控制条。
//!
//! **编码交给外部 ffmpeg**（方案 ①）：抓到的 BGRA 原始帧直接写进 `ffmpeg` 的 stdin，
//! H.264 编码 / 封装 / 时间戳全由它负责。好处是本仓不引入任何编码器依赖，坏处是
//! 用户机器上要有 `ffmpeg.exe`——路径做成设置项（见 [`RecordingSettings::ffmpeg_path`]），
//! 留空则按 PATH + 常见安装位置自动找，找不到时前端明确提示而不是静默失败。
//!
//! 平台相关实现（`capture`）仅 Windows 编译；命令在所有平台注册（见 `commands`）。

pub mod bar;
pub mod commands;
pub mod ffmpeg;
pub mod output;
pub mod session;

#[cfg(windows)]
pub mod capture;

// 与 screenshot 同理：glob 再导出，让 `generate_handler!` 能解析到
// `#[tauri::command]` 生成的隐藏伴生项。
pub use commands::*;

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 录制矩形（物理像素，`x`/`y` 可为负——副屏在主屏左侧/上方时如此）。
///
/// 与 `screenshot::monitor::MonitorRect` 形状相同但**不复用**：那个类型是
/// `#[cfg(windows)]` 的，而录屏的命令层在所有平台都要能编译。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 默认全局热键：按一下开始录制，再按一下结束。
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+R";

/// 允许的帧率档位（前端下拉与后端校验共用）。
pub const FPS_CHOICES: [u32; 4] = [10, 15, 24, 30];

/// 录屏设置（落 `<workspace>/recordings/settings.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSettings {
    /// 全局热键（开始 / 结束共用一个）。
    pub hotkey: String,
    /// 帧率。越高越流畅也越吃 CPU，见 [`FPS_CHOICES`]。
    pub fps: u32,
    /// `ffmpeg.exe` 路径。**留空 = 自动查找**（PATH + 常见安装位置）。
    pub ffmpeg_path: String,
    /// 落盘目录（空表示用默认 `<workspace>/recordings`）。
    pub save_dir: String,
    /// 是否把鼠标指针画进画面。
    pub capture_cursor: bool,
    /// x264 质量（CRF，越小越清晰、文件越大；18~28 为常用区间）。
    pub crf: u32,
    /// 热键触发时是否先框选区域。false = 直接录鼠标所在的整块屏。
    pub select_region: bool,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.to_string(),
            fps: 15,
            ffmpeg_path: String::new(),
            save_dir: String::new(),
            capture_cursor: true,
            crf: 23,
            select_region: true,
        }
    }
}

impl RecordingSettings {
    /// 把越界值拽回合法区间。设置文件是人可以手改的，别让一个 0 帧率把抓帧线程转死。
    pub fn sanitized(mut self) -> Self {
        if !FPS_CHOICES.contains(&self.fps) {
            self.fps = self.fps.clamp(1, 60);
        }
        self.crf = self.crf.clamp(0, 51);
        if self.hotkey.trim().is_empty() {
            self.hotkey = DEFAULT_HOTKEY.to_string();
        }
        self
    }
}

/// 读设置（缺失/损坏一律回退默认，不让录屏因为一个坏 json 直接不可用）。
pub fn read_settings(workspace: &Path) -> RecordingSettings {
    std::fs::read_to_string(output::settings_path(workspace))
        .ok()
        .and_then(|s| serde_json::from_str::<RecordingSettings>(&s).ok())
        .unwrap_or_default()
        .sanitized()
}
