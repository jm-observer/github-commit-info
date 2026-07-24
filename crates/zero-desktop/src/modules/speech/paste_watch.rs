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
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::modules::speech::lock_utils::mutex_lock;

static PASTE_SIGNAL: AtomicBool = AtomicBool::new(false);

/// 取走「自上次取走以来发生过粘贴」的信号，并清零。返回 true 表示期间用户按过 Ctrl+V。
pub fn take_paste_signal() -> bool {
    PASTE_SIGNAL.swap(false, Ordering::Relaxed)
}

/// 一次交付动作发生时对前台窗口的抓拍（同音字纠错的数据收集期，宽记为主）。
///
/// **抓拍时刻 = 交付动作真正执行的那一刻**：auto_paste 是打字前、auto_copy 是用户按下
/// `Ctrl+V` 的钩子回调里；**不是**收到 LLM 优化结果的时刻——两者之间隔着 1~2 秒，
/// 用户完全可能已经切换了窗口，那时取值就记错了 app。
///
/// 各字段取不到就留 `None`（进程权限不足、窗口无标题等），不阻塞交付本身。
#[derive(Debug, Clone)]
pub struct DeliveryApp {
    /// 可执行文件名，如 `Code.exe`。
    pub exe: Option<String>,
    /// 可执行文件全路径，用于区分同名 exe。
    pub path: Option<String>,
    /// 窗口标题。浏览器场景下是区分具体站点的唯一线索（exe 一律是 `chrome.exe`）。
    pub title: Option<String>,
    /// 窗口类名，如 `Chrome_WidgetWin_1`。
    pub class: Option<String>,
    /// 交付模式：`"auto_paste"` | `"auto_copy"`。
    pub mode: &'static str,
}

/// 最近一次交付抓拍 + 抓拍时刻。只保留最近一次：一个 burst 内多次交付通常在同一个 app，
/// 且最后一次最接近用户随后按下采集快捷键的时刻。
static LAST_DELIVERY: Mutex<Option<(DeliveryApp, Instant)>> = Mutex::new(None);

/// 记录一次交付抓拍（覆盖式）。抓不到前台信息时不写，保留上一次的值。
fn record_delivery_app(mode: &'static str) {
    let Some(mut app) = foreground_app() else {
        return;
    };
    app.mode = mode;
    *mutex_lock(&LAST_DELIVERY) = Some((app, Instant::now()));
}

/// 读取最近一次交付抓拍；超过 `max_age` 视为过期返回 `None`。
///
/// 不清除，允许重复读——同一次交付可能既被采集样本引用、又被别处引用。
pub fn last_delivery_app(max_age: Duration) -> Option<DeliveryApp> {
    let guard = mutex_lock(&LAST_DELIVERY);
    let (app, at) = guard.as_ref()?;
    (at.elapsed() <= max_age).then(|| app.clone())
}

/// 抓拍当前前台窗口所属应用。前台属于本进程时返回 `None`（用户在看 Zero Desktop 自己，
/// 不是一次对外交付）。非 Windows 平台恒为 `None`。
pub fn foreground_app() -> Option<DeliveryApp> {
    #[cfg(windows)]
    {
        win::foreground_app()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冒烟：实际调一次 Win32 抓拍链路。
    ///
    /// 断言只能到「不崩 + 拿到的字段不是垃圾」这一层——测试环境下前台窗口是什么无法预期
    /// （无窗口会话下合法地返回 `None`）。但这足以暴露 unsafe 部分最容易犯的错：
    /// 缓冲区越界、把 `n` 当字节数用、UTF-16 解码错位导致的乱码/空串。
    #[test]
    fn foreground_app_smoke() {
        let snapshot = foreground_app();
        // 打印实际抓到的内容：若为 None（无窗口会话），本测试等于没覆盖 Win32 部分，
        // 得知道这一点，别把 early return 当成「验证通过」。
        eprintln!("[foreground_app_smoke] {snapshot:?}");
        let Some(app) = snapshot else {
            return; // 无前台窗口 / 前台是本进程，合法。
        };
        for (name, value) in [
            ("exe", &app.exe),
            ("path", &app.path),
            ("title", &app.title),
            ("class", &app.class),
        ] {
            if let Some(v) = value {
                assert!(!v.is_empty(), "{name} 取到了空串，应为 None 而非 Some(\"\")");
                assert!(
                    !v.contains('\0'),
                    "{name} 含 NUL，说明按 UTF-16 长度截断有误: {v:?}"
                );
            }
        }
        if let (Some(exe), Some(path)) = (&app.exe, &app.path) {
            assert!(
                path.ends_with(exe),
                "exe 应是 path 的最后一段: exe={exe:?} path={path:?}"
            );
        }
    }

    /// 过期抓拍不应被读出来——收集期宁可留空也不猜一个 app。
    #[test]
    fn last_delivery_app_respects_max_age() {
        *mutex_lock(&LAST_DELIVERY) = Some((
            DeliveryApp {
                exe: Some("X.exe".into()),
                path: None,
                title: None,
                class: None,
                mode: "auto_paste",
            },
            Instant::now(),
        ));
        assert!(last_delivery_app(Duration::from_secs(60)).is_some());
        assert!(last_delivery_app(Duration::ZERO).is_none());
    }
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

/// 向当前前台窗口的焦点控件发一次回车键（语音命令"发送" → Enter）。
///
/// 与 [`type_text_to_foreground`] 共用「前台属于本进程时不动」的安全闸——避免用户正在
/// Zero Desktop 自己的输入里时被误触发。返回是否真的发出了按键。
pub fn press_enter_to_foreground() -> bool {
    #[cfg(windows)]
    {
        win::press_enter()
    }
    #[cfg(not(windows))]
    {
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
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_BACK, VK_CONTROL, VK_MENU, VK_RETURN,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    };

    const VK_V: u32 = 0x56;
    /// 语音纠错一键采集的专用快捷键组合（`Ctrl+Alt+C`）中的 `C` 键。注意 `C` 同时也是普通
    /// 复制快捷键的一部分，但只有同时按下 Ctrl **和** Alt 时才吞键/触发；纯 Ctrl+C 不受影响。
    const VK_C: u32 = 0x43;
    const KEY_DOWN_MASK: u16 = 0x8000;

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32
            && (wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN)
        {
            let kb = lparam as *const KBDLLHOOKSTRUCT;
            if !kb.is_null() {
                let vk_code = (*kb).vkCode;
                if vk_code == VK_V {
                    let ctrl_down =
                        (GetAsyncKeyState(VK_CONTROL as i32) as u16 & KEY_DOWN_MASK) != 0;
                    if ctrl_down {
                        PASTE_SIGNAL.store(true, Ordering::Relaxed);
                        // auto_copy 链路的「实际交付时刻」就是这一下 Ctrl+V：此刻前台窗口
                        // 即粘贴目标，抓拍最准。钩子回调有超时限制，抓拍只做几个本地
                        // Win32 调用且全程失败即放弃，不会阻塞。
                        super::record_delivery_app("auto_copy");
                    }
                } else if vk_code == VK_C {
                    let ctrl_down =
                        (GetAsyncKeyState(VK_CONTROL as i32) as u16 & KEY_DOWN_MASK) != 0;
                    let alt_down = (GetAsyncKeyState(VK_MENU as i32) as u16 & KEY_DOWN_MASK) != 0;
                    if ctrl_down && alt_down {
                        crate::modules::speech::capture::signal_capture();
                        // 吞掉 Ctrl+Alt+C：不放行给前台窗口，避免它当普通按键处理。
                        return 1;
                    }
                }
            }
        }
        // 其余按键（含普通 Ctrl+V / Ctrl+C）始终放行。
        CallNextHookEx(ptr::null_mut(), code, wparam, lparam)
    }

    use windows_sys::Win32::Foundation::{CloseHandle, HWND};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };

    /// 窗口标题 / 类名的读取上限（UTF-16 码元）。标题超长直接截断——收集期够用，
    /// 也避免在键盘钩子里搬运过大的缓冲。
    const MAX_TEXT_UNITS: usize = 256;
    /// 进程路径读取上限。常规 Win32 路径远小于此。
    const MAX_PATH_UNITS: usize = 512;

    /// 抓拍当前前台窗口所属应用。
    ///
    /// **会在低级键盘钩子回调里被调用**（auto_copy 的 Ctrl+V 抓拍），因此：全部是本地
    /// 快速 Win32 调用、任一步失败只让对应字段为 `None`、不分配大缓冲、不 panic。
    pub fn foreground_app() -> Option<super::DeliveryApp> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            // 前台是本进程 = 用户在看 Zero Desktop 自己，不是一次对外交付。
            if pid == 0 || pid == GetCurrentProcessId() {
                return None;
            }
            let path = process_image_path(pid);
            let exe = path
                .as_deref()
                .and_then(|p| p.rsplit(['\\', '/']).next())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Some(super::DeliveryApp {
                exe,
                path,
                title: window_text(hwnd, TextKind::Title),
                class: window_text(hwnd, TextKind::Class),
                mode: "",
            })
        }
    }

    enum TextKind {
        Title,
        Class,
    }

    /// 读窗口标题或类名。返回 `None` 表示空或读取失败。
    unsafe fn window_text(hwnd: HWND, kind: TextKind) -> Option<String> {
        let mut buf = [0u16; MAX_TEXT_UNITS];
        let n = match kind {
            TextKind::Title => GetWindowTextW(hwnd, buf.as_mut_ptr(), MAX_TEXT_UNITS as i32),
            TextKind::Class => GetClassNameW(hwnd, buf.as_mut_ptr(), MAX_TEXT_UNITS as i32),
        };
        if n <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..n as usize]))
    }

    /// 由 pid 取可执行文件全路径。用 `PROCESS_QUERY_LIMITED_INFORMATION` 打开——
    /// 它是查询路径所需的最小权限，对多数非同权限进程也能成功（提权进程仍会失败，
    /// 此时返回 `None`，不影响其余字段）。
    unsafe fn process_image_path(pid: u32) -> Option<String> {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; MAX_PATH_UNITS];
        let mut len = buf.len() as u32;
        // dwFlags = 0 (PROCESS_NAME_WIN32)：返回 Win32 路径而非设备路径。
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }

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

    /// 向焦点窗口发一次回车键。共用 [`type_text_with_backspaces`] 的本进程护栏。
    pub fn press_enter() -> bool {
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_null() {
                return false;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(fg, &mut pid);
            if pid == GetCurrentProcessId() {
                return false;
            }
            let inputs = [vk_input(VK_RETURN, false), vk_input(VK_RETURN, true)];
            let sent = SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
            sent as usize == inputs.len()
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
            // auto_paste 链路的「实际交付时刻」= 即将打字进这个窗口的此刻（已过本进程
            // 护栏，目标必为外部 app）。抓拍放在 SendInput 之前，避免打字过程中窗口变化。
            super::record_delivery_app("auto_paste");
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
