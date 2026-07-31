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
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// 默认全局热键（保存设置后可改，见 `screenshot_save_settings`）。
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+A";

/// 启动时注册全局热键：从设置读取（缺失回退 `DEFAULT_HOTKEY`）。
/// 注册失败仅告警，不影响应用启动（设计文档 §8 热键冲突）。
pub fn setup(app: &AppHandle) -> Result<()> {
    let workspace = app.state::<crate::app_state::AppState>().workspace.clone();
    let hotkey = commands::read_settings(&workspace).hotkey;
    match register_hotkey(app, &hotkey) {
        Ok(_) => log::info!(target: "screenshot", "全局热键已注册: {hotkey}"),
        Err(e) => log::warn!(target: "screenshot", "注册全局热键失败（{hotkey}）: {e}"),
    }
    Ok(())
}

/// 注册（或替换）全局热键 → 触发截图。先解析校验，再清掉旧热键后注册新热键，保存设置即生效。
/// 解析失败时返回 Err 且**不**改动当前已注册的热键。
pub fn register_hotkey(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let shortcut = parse_shortcut(hotkey)?;
    let gs = app.global_shortcut();
    // screenshot 是本应用唯一的全局热键使用者，整体清掉再注册最简单可靠。
    let _ = gs.unregister_all();
    gs.on_shortcut(shortcut, move |app, _scut, event| {
        if event.state == ShortcutState::Pressed {
            // 抓屏 + 建窗须在主线程跑（热键回调在独立线程）。
            let app = app.clone();
            let _ = app.clone().run_on_main_thread(move || {
                if let Err(e) = commands::do_capture(&app) {
                    log::warn!(target: "screenshot", "触发截图失败: {e}");
                }
            });
        }
    })
    .map_err(|e| format!("注册热键失败: {e}"))?;
    Ok(())
}

/// 解析形如 `Ctrl+Alt+A` / `Shift+PrintScreen` / `Ctrl+F2` 的热键串。
fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in s.split('+') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "super" | "meta" | "cmd" | "command" | "win" => mods |= Modifiers::SUPER,
            other => {
                if code.is_some() {
                    return Err(format!("热键含多个主键: {s}"));
                }
                code = Some(parse_code(other)?);
            }
        }
    }
    let code = code.ok_or_else(|| format!("热键缺少主键: {s}"))?;
    Ok(Shortcut::new(
        if mods.is_empty() { None } else { Some(mods) },
        code,
    ))
}

/// 把单个按键名映射成 `Code`：A–Z / 0–9 / F1–F12 / 少量常用键。
fn parse_code(k: &str) -> Result<Code, String> {
    let lower = k.to_ascii_lowercase();
    let bytes = lower.as_bytes();

    if bytes.len() == 1 {
        let c = bytes[0];
        if c.is_ascii_lowercase() {
            // 'a' → KeyA … 'z' → KeyZ
            let idx = c - b'a';
            return Ok(LETTERS[idx as usize]);
        }
        if c.is_ascii_digit() {
            let idx = c - b'0';
            return Ok(DIGITS[idx as usize]);
        }
    }

    if let Some(n) = lower.strip_prefix('f') {
        if let Ok(num) = n.parse::<u8>() {
            if (1..=12).contains(&num) {
                return Ok(FKEYS[(num - 1) as usize]);
            }
        }
    }

    Ok(match lower.as_str() {
        "printscreen" | "prtsc" | "print" => Code::PrintScreen,
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "tab" => Code::Tab,
        "escape" | "esc" => Code::Escape,
        "insert" | "ins" => Code::Insert,
        "delete" | "del" => Code::Delete,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" | "pgup" => Code::PageUp,
        "pagedown" | "pgdn" => Code::PageDown,
        _ => return Err(format!("不支持的按键: {k}")),
    })
}

const LETTERS: [Code; 26] = [
    Code::KeyA,
    Code::KeyB,
    Code::KeyC,
    Code::KeyD,
    Code::KeyE,
    Code::KeyF,
    Code::KeyG,
    Code::KeyH,
    Code::KeyI,
    Code::KeyJ,
    Code::KeyK,
    Code::KeyL,
    Code::KeyM,
    Code::KeyN,
    Code::KeyO,
    Code::KeyP,
    Code::KeyQ,
    Code::KeyR,
    Code::KeyS,
    Code::KeyT,
    Code::KeyU,
    Code::KeyV,
    Code::KeyW,
    Code::KeyX,
    Code::KeyY,
    Code::KeyZ,
];
const DIGITS: [Code; 10] = [
    Code::Digit0,
    Code::Digit1,
    Code::Digit2,
    Code::Digit3,
    Code::Digit4,
    Code::Digit5,
    Code::Digit6,
    Code::Digit7,
    Code::Digit8,
    Code::Digit9,
];
const FKEYS: [Code; 12] = [
    Code::F1,
    Code::F2,
    Code::F3,
    Code::F4,
    Code::F5,
    Code::F6,
    Code::F7,
    Code::F8,
    Code::F9,
    Code::F10,
    Code::F11,
    Code::F12,
];
