//! screenshot 模块：全局热键唤起 → 抓屏冻结 → 叠加窗框选/标注 → 剪贴板 + 落盘。
//!
//! 设计文档：`docs/zero-desktop-screenshot-design.md`。职责切分（§1）：Rust 做平台脏活
//! （抓屏 / 全局热键 / 叠加窗 / 输出），React 做框选 + canvas 标注 + 合成 PNG。
//! 平台相关实现（`capture` / `monitor`）仅 Windows 编译；命令在所有平台注册（见 `commands`）。

pub mod commands;
pub mod meta;
pub mod output;
pub mod overlay;

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod monitor;

// 用 glob 再导出：除命令函数本身，`#[tauri::command]` 生成的隐藏伴生项
// （`__cmd__*` / `__tauri_command_name_*`）也随之导出，`generate_handler!` 才能在
// `modules::screenshot::*` 下解析到它们。
pub use commands::*;

use anyhow::Result;
use tauri::AppHandle;

/// 默认全局热键（保存设置后可改，见 `screenshot_save_settings`）。
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+A";

/// 启动时注册全局热键：实际注册在 [`crate::modules::hotkeys`] 里统一做
/// （截图与录屏共用同一张全局热键表，见那里的模块注释）。
pub fn setup(app: &AppHandle) -> Result<()> {
    crate::modules::hotkeys::setup(app);
    Ok(())
}

/// 按新热键重注册（供 `screenshot_save_settings` 校验用）。
/// 注意：注册前设置文件必须已写入——[`hotkeys::reregister_all`] 是从磁盘读热键的。
pub fn reregister(app: &AppHandle) -> Result<(), String> {
    let failures = crate::modules::hotkeys::reregister_all(app);
    match failures.iter().find(|(who, _)| *who == "screenshot") {
        Some((_, e)) => Err(e.clone()),
        None => Ok(()),
    }
}
