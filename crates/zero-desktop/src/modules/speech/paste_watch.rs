//! 全局 Ctrl+V 观察器（仅观察，不拦截）。
//!
//! 自动复制把时间上相邻的多段优化「拼接」成整体写进剪贴板（见
//! [`commands::remote::next_clipboard_text`]），本意是结尾一次性粘贴能拿到完整长句。
//! 但用户若「每段优化即时粘贴」，下一段仍会带上已粘贴过的前一段 → 重复粘贴。
//!
//! 解法：装一个 Windows 低级键盘钩子（`WH_KEYBOARD_LL`）**只观察** Ctrl+V，钩子里照常
//! `CallNextHookEx` 不吞按键。观察到一次粘贴就置位 [`PASTE_SIGNAL`]；remote 接收循环在
//! 下次写剪贴板前 `take` 这个信号，若已粘贴则清空拼接累加器，使下一段重新从空开始。
//!
//! 信号是「边沿」语义：置位后由消费方一次性取走（swap-to-false）。多次 Ctrl+V 只要被取走一次
//! 即可——重置只影响下一段的累加，不动当前剪贴板内容，故不会误伤连续粘贴同一文本。

use std::sync::atomic::{AtomicBool, Ordering};

static PASTE_SIGNAL: AtomicBool = AtomicBool::new(false);

/// 取走「自上次取走以来发生过粘贴」的信号，并清零。返回 true 表示期间用户按过 Ctrl+V。
pub fn take_paste_signal() -> bool {
    PASTE_SIGNAL.swap(false, Ordering::Relaxed)
}

/// 启动全局 Ctrl+V 观察线程。幂等：重复调用只会装第一个钩子（后续线程会因装钩失败而退出）。
/// 非 Windows 平台为 no-op（本应用面向 Windows）。
pub fn start_paste_watcher() {
    #[cfg(windows)]
    win::start();
    #[cfg(not(windows))]
    {
        tracing::info!(target: "speech", "[paste_watch] non-windows build: paste watcher disabled");
    }
}

/// 把 `text` 作为 Unicode 键盘事件直接「打字」进当前前台窗口的焦点控件（自动粘贴）。
///
/// 走 `SendInput` 的 `KEYEVENTF_UNICODE`，等同模拟键盘逐字输入：**不碰剪贴板**，
/// 因此与自动复制那条「写完整拼接文本进剪贴板供手动 Ctrl+V 兜底」的链路彻底解耦。
/// 若前台窗口属于本进程（用户正看着 Zero Desktop 自己），则不输入并返回 false，
/// 把内容留给剪贴板兜底——用户回头点别处的输入框再 Ctrl+V 即可。
///
/// 返回是否实际向外部窗口输入了文本。非 Windows 平台恒为 no-op。
pub fn type_text_to_foreground(text: &str) -> bool {
    type_text_with_backspaces_to_foreground(0, text)
}

/// 与 [`type_text_to_foreground`] 同语义，但在打字前先发 `backspaces` 个退格键，
/// 用于「同段优化稿被 LLM 改写」时按公共前缀长度回退已输入字符再补打新尾巴。
///
/// 与普通自动粘贴共用「前台属于本进程时不动」的安全闸；退格只在外部窗口发生。
pub fn type_text_with_backspaces_to_foreground(backspaces: usize, text: &str) -> bool {
    #[cfg(windows)]
    {
        win::type_text_with_backspaces(backspaces, text)
    }
    #[cfg(not(windows))]
    {
        let _ = (backspaces, text);
        false
    }
}

#[cfg(windows)]
mod win {
    use super::PASTE_SIGNAL;
    use std::ptr;
    use std::sync::atomic::Ordering;
    use tracing::{error, info};
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_BACK, VK_CONTROL};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    };

    const VK_V: u32 = 0x56;
    const KEY_DOWN_MASK: u16 = 0x8000;

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32
            && (wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN)
        {
            let kb = lparam as *const KBDLLHOOKSTRUCT;
            if !kb.is_null() && (*kb).vkCode == VK_V {
                let ctrl_down = (GetAsyncKeyState(VK_CONTROL as i32) as u16 & KEY_DOWN_MASK) != 0;
                if ctrl_down {
                    PASTE_SIGNAL.store(true, Ordering::Relaxed);
                }
            }
        }
        // 始终放行，绝不吞 Ctrl+V。
        CallNextHookEx(ptr::null_mut(), code, wparam, lparam)
    }

    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };

    /// 构造一个「Unicode 码元」键盘事件（按下或抬起）。`wVk=0` + `KEYEVENTF_UNICODE`
    /// 表示这是字符注入而非虚拟键，`wScan` 携带 UTF-16 码元。
    fn unicode_input(unit: u16, key_up: bool) -> INPUT {
        let mut flags = KEYEVENTF_UNICODE;
        if key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0,
                    wScan: unit,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// 构造一个虚拟键（如退格）的「按下/抬起」事件。`wVk` 非 0 时 Windows 将其作为
    /// 普通按键处理，目标控件按自身规则消费（输入框退格 = 删一个字符或一个组合字符）。
    fn vk_input(vk: u16, key_up: bool) -> INPUT {
        let mut flags: u32 = 0;
        if key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    pub fn type_text_with_backspaces(backspaces: usize, text: &str) -> bool {
        if backspaces == 0 && text.is_empty() {
            return false;
        }
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_null() {
                return false;
            }
            // 前台是本应用自己的窗口时不自动输入（也不退格），避免误删自己的界面文本。
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(fg, &mut pid);
            if pid == GetCurrentProcessId() {
                return false;
            }
            let mut inputs: Vec<INPUT> =
                Vec::with_capacity(backspaces * 2 + text.encode_utf16().count() * 2);
            for _ in 0..backspaces {
                inputs.push(vk_input(VK_BACK, false));
                inputs.push(vk_input(VK_BACK, true));
            }
            // 每个 UTF-16 码元发「按下 + 抬起」两个事件；代理对（emoji 等）按序发亦可。
            for unit in text.encode_utf16() {
                inputs.push(unicode_input(unit, false));
                inputs.push(unicode_input(unit, true));
            }
            if inputs.is_empty() {
                return false;
            }
            let sent = SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
            sent as usize == inputs.len()
        }
    }

    pub fn start() {
        // 低级键盘钩子要求装钩线程自带消息循环，故起一个专用线程常驻。
        std::thread::Builder::new()
            .name("paste-watch".into())
            .spawn(|| unsafe {
                let hook = SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(hook_proc),
                    ptr::null_mut(),
                    0,
                );
                if hook.is_null() {
                    error!(target: "speech", "[paste_watch] SetWindowsHookExW failed; Ctrl+V reset disabled");
                    return;
                }
                info!(target: "speech", "[paste_watch] global Ctrl+V watcher installed");
                // 阻塞跑消息循环让钩子保活；进程退出时随线程一并结束。
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {}
            })
            .map_err(|e| {
                error!(target: "speech", "[paste_watch] spawn watcher thread failed: {e}");
            })
            .ok();
    }
}
