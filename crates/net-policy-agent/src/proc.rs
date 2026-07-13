//! 子进程创建辅助：Windows 上隐藏控制台窗口（避免频繁起 powershell/mihomo 时闪黑窗）。

/// `CREATE_NO_WINDOW`：不为子进程分配控制台窗口。
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
