//! 命名管道 DACL（设计 §7.1 + 评审点 2：OS 级强制访问控制）。
//!
//! 把管道访问限到 **SYSTEM + 当前进程 token 的用户 SID**（= 运行 agent 的那个用户；每用户任务下即
//! 桌面用户本人）。**不再用宽泛的 `IU`（交互用户组）**——快速用户切换/RDP/多会话下 IU 会放进其它
//! 交互用户。也**不默认放 Administrators**（每用户控制面无需交给其他管理员）。
//!
//! **fail-closed**：拿不到用户 SID / 构造安全描述符失败 → 返回 null，由 server 拒绝启动（不降级）。

use std::ffi::c_void;

/// 构建管道 `SECURITY_ATTRIBUTES`。失败返回 null（server 必须据此**拒绝启动**，评审点 2）。
#[cfg(windows)]
pub fn build_pipe_security() -> *mut c_void {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    let sid = match current_user_sid_string() {
        Some(s) if !s.is_empty() => s,
        _ => return std::ptr::null_mut(), // fail-closed
    };
    // D:P = protected，无继承。两种运行形态：
    // - **服务模式**（LocalSystem，SID=S-1-5-18）：GUI 在用户会话，管道必须**跨会话可达**，故放行
    //   交互用户组 IU（+ 内建管理员 BA，便于救援 CLI 提权运行）。代价：多用户/RDP 下 IU 含其他交互
    //   用户；单用户机即桌面用户本人。
    // - **每用户模式**（计划任务/手动 run）：仍严格限到 SYSTEM + 该用户本人（原设计，最小暴露面）。
    let sddl = if sid == "S-1-5-18" {
        "D:P(A;;GA;;;SY)(A;;GA;;;IU)(A;;GA;;;BA)".to_string()
    } else {
        format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})")
    };
    const SDDL_REVISION_1: u32 = 1;
    let wide: Vec<u16> = std::ffi::OsStr::new(&sddl)
        .encode_wide()
        .chain([0])
        .collect();
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `wide` 是 NUL 结尾 UTF-16；psd 为 out 参数。
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut psd,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 || psd.is_null() {
        return std::ptr::null_mut();
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd,
        bInheritHandle: 0,
    };
    Box::into_raw(Box::new(sa)) as *mut c_void
}

/// 取当前进程 token 的用户 SID 字符串（`S-1-5-...`）。失败返回 None。
#[cfg(windows)]
fn current_user_sid_string() -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        // 先探大小。
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        if len == 0 {
            CloseHandle(token);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr() as *mut c_void,
            len,
            &mut len,
        );
        CloseHandle(token);
        if ok == 0 {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut str_ptr: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(tu.User.Sid, &mut str_ptr) == 0 || str_ptr.is_null() {
            return None;
        }
        // 读回 NUL 结尾宽字符串。
        let mut n = 0usize;
        while *str_ptr.add(n) != 0 {
            n += 1;
        }
        let s = std::ffi::OsString::from_wide(std::slice::from_raw_parts(str_ptr, n))
            .to_string_lossy()
            .into_owned();
        LocalFree(str_ptr as *mut c_void);
        Some(s)
    }
}

#[cfg(not(windows))]
pub fn build_pipe_security() -> *mut c_void {
    std::ptr::null_mut()
}
