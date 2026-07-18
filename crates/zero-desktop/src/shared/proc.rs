//! 子进程创建辅助：Windows 上隐藏控制台窗口。
//!
//! 桌面端频繁起 `git` / `powershell` / `pwsh` / `mihomo` 等控制台程序；若不加
//! `CREATE_NO_WINDOW`，Windows 会为每个子进程弹一个（一闪而过的）黑色控制台窗口，
//! 体验很差（例如 G10 部署会"闪很多窗口"）。
//! 统一经此模块加无窗口标志；非 Windows 平台为 no-op。

/// `CREATE_NO_WINDOW`：不为子进程分配控制台窗口（GUI 进程仍可正常 I/O 重定向）。
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 给 `std::process::Command` 加无窗口标志（仅 Windows 生效）。
pub fn hide_console(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// 给 `tokio::process::Command` 加无窗口标志（仅 Windows 生效）。
pub fn hide_console_tokio(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}
