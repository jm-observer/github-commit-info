//! 全局热键统一注册处。
//!
//! 存在的理由：`tauri-plugin-global-shortcut` 的注册是「全局一张表」，而唯一可靠的
//! 清场手段是 `unregister_all()`。screenshot 原先独占热键，直接 `unregister_all()` 再注册
//! 自己即可；recording 加进来之后，谁保存设置谁清场，就会把对方的热键一起抹掉。
//! 所以改成：**任何一方改热键，都从这里整体重注册两个**。
//!
//! 失败策略与 screenshot 原有行为一致——注册失败只告警不中断启动（热键可能被别的软件
//! 占用），并把失败清单返回给调用方，让「保存设置」这种交互路径能把原因给到用户。

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// 一次重注册里某个热键的失败原因（`("screenshot", "热键冲突: ...")`）。
pub type Failure = (&'static str, String);

/// 从各模块的设置里读出热键，整体重注册。返回失败清单（空 = 全部成功）。
///
/// 逐个注册、互不阻断：截图热键被占用不该连累录屏热键。
pub fn reregister_all(app: &AppHandle) -> Vec<Failure> {
    let workspace = app
        .try_state::<crate::app_state::AppState>()
        .map(|s| s.workspace.clone());
    let Some(workspace) = workspace else {
        return vec![("state", "AppState 尚未就绪".to_string())];
    };

    let shot_key = crate::modules::screenshot::commands::read_settings(&workspace).hotkey;
    let rec_key = crate::modules::recording::read_settings(&workspace).hotkey;

    // 全局一张表，只能整体清场后重建。
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let mut failures = Vec::new();

    match parse_shortcut(&shot_key) {
        Ok(scut) => {
            let r = gs.on_shortcut(scut, move |app, _s, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }
                // 抓屏 + 建窗须在主线程跑（热键回调在独立线程）。
                let app = app.clone();
                let _ = app.clone().run_on_main_thread(move || {
                    if let Err(e) = crate::modules::screenshot::commands::do_capture(&app) {
                        log::warn!(target: "screenshot", "触发截图失败: {e}");
                    }
                });
            });
            if let Err(e) = r {
                failures.push(("screenshot", format!("注册热键失败: {e}")));
            } else {
                log::info!(target: "screenshot", "全局热键已注册: {shot_key}");
            }
        }
        Err(e) => failures.push(("screenshot", e)),
    }

    match parse_shortcut(&rec_key) {
        Ok(scut) => {
            let r = gs.on_shortcut(scut, move |app, _s, event| {
                if event.state != ShortcutState::Pressed {
                    return;
                }
                let app = app.clone();
                let _ = app.clone().run_on_main_thread(move || {
                    // 同一个热键既开始也结束：录制中按下即停，符合「按一下开、按一下关」的直觉。
                    if let Err(e) = crate::modules::recording::commands::toggle_from_hotkey(&app) {
                        log::warn!(target: "recording", "触发录屏失败: {e}");
                    }
                });
            });
            if let Err(e) = r {
                failures.push(("recording", format!("注册热键失败: {e}")));
            } else {
                log::info!(target: "recording", "全局热键已注册: {rec_key}");
            }
        }
        Err(e) => failures.push(("recording", e)),
    }

    failures
}

/// 启动时注册：失败只告警，不影响应用启动。
pub fn setup(app: &AppHandle) {
    for (who, err) in reregister_all(app) {
        log::warn!(target: "hotkeys", "注册 {who} 全局热键失败: {err}");
    }
}

/// 解析形如 `Ctrl+Alt+A` / `Shift+PrintScreen` / `Ctrl+F2` 的热键串。
pub fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
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
            return Ok(LETTERS[(c - b'a') as usize]);
        }
        if c.is_ascii_digit() {
            return Ok(DIGITS[(c - b'0') as usize]);
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
