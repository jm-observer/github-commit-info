//! 命名管道 DACL（设计 §7.1 + 评审点 2：OS 级强制访问控制）。
//!
//! 把管道访问限到 **SYSTEM + 当前进程 token 的用户 SID**（= 运行 agent 的那个用户；每用户任务下即
//! 桌面用户本人）。**不再用宽泛的 `IU`（交互用户组）**——快速用户切换/RDP/多会话下 IU 会放进其它
//! 交互用户。也**不默认放 Administrators**（每用户控制面无需交给其他管理员）。
//!
//! **fail-closed**：拿不到用户 SID / 构造安全描述符失败 → 返回 null，由 server 拒绝启动（不降级）。

use std::ffi::c_void;
use std::path::Path;

/// 把机器级数据目录收紧为仅 LocalSystem 与内建 Administrators 完全控制，并移除继承 ACL。
/// agent/CA/抓包/解密产物都在该目录下，服务启动时 fail-closed 执行，防止 ProgramData 父目录上
/// 意外的 Users 写权限传播进来。
///
/// **实现要点（否则会把 workspace 文件锁成空 DACL、连 SYSTEM 都读不了 → `record_store_degraded`）**：
/// 绝不用 `/grant:r "SID:(OI)(CI)F" /T` ——`(OI)(CI)` 是「仅供继承」标志，用 `/T` 显式打到**文件**上
/// 时文件本身得不到有效授权，结果是空 DACL 拒绝所有人（真机实测 settings.json/net-policy.db 被锁死）。
/// 正确做法分三步：① `takeown /R`（SYSTEM 有 SeTakeOwnership，可自愈此前被锁死、非自己 owner 的旧文件）
/// → ② `icacls /reset /T`（子节点改回「继承父目录」，清掉损坏的显式 DACL）→ ③ 只在**根目录**设保护性
/// 可继承授权（不带 `/T`），子文件靠继承拿到 `SYSTEM/Admins:(I)(F)`。
#[cfg(windows)]
pub fn harden_machine_data_dir(path: &Path) -> anyhow::Result<()> {
    use anyhow::{bail, Context};

    std::fs::create_dir_all(path)
        .with_context(|| format!("创建机器级数据目录失败：{}", path.display()))?;
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let sys32 = Path::new(&system_root).join("System32");
    let icacls = sys32.join("icacls.exe");
    let takeown = sys32.join("takeown.exe");

    let run = |program: &Path, args: &[&std::ffi::OsStr]| -> anyhow::Result<std::process::Output> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        crate::proc::hide_console(&mut cmd);
        cmd.output()
            .with_context(|| format!("启动 {} 失败", program.display()))
    };
    let os = |s: &str| std::ffi::OsString::from(s);
    let p = path.as_os_str();

    // ① 取得所有权（自愈：此前被 bug 锁成空 DACL 且 owner=Admins 的旧文件，SYSTEM 无 WRITE_DAC，
    //    须先 takeown 才能改 ACL）。best-effort：新装/健康树本就可写，失败不致命。
    let _ = run(
        &takeown,
        &[&os("/F"), p, &os("/R"), &os("/A"), &os("/D"), &os("Y")],
    );

    // ② 子节点改回继承（清掉损坏/遗留的显式 DACL）。best-effort。
    let _ = run(
        &icacls,
        &[p, &os("/reset"), &os("/T"), &os("/C"), &os("/Q")],
    );

    // ③ 只在根目录设保护性可继承授权（**不带 /T**，避免把 (OI)(CI) 显式打到文件上）。这是决定性步骤。
    let out = run(
        &icacls,
        &[
            p,
            &os("/inheritance:r"),
            &os("/grant:r"),
            &os("*S-1-5-18:(OI)(CI)F"),
            &os("*S-1-5-32-544:(OI)(CI)F"),
            &os("/C"),
            &os("/Q"),
        ],
    )?;
    if !out.status.success() {
        bail!(
            "收紧机器级数据目录 ACL 失败（exit={}）：{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn harden_machine_data_dir(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// best-effort 收紧目录到 SYSTEM + Administrators（CA 私钥 `private/` 子目录用，§17.4）。失败仅
/// 记日志——父 workspace 根已 [`harden_machine_data_dir`] 收紧，子目录继承的收紧是纵深防御。
pub fn lock_down_dir_system_only(path: &Path) {
    // 单测里不真跑 icacls：会把 Administrators 变 deny-only（UAC 过滤 token）→ 非提权测试进程
    // 反而读不回自己刚建的文件。生产（服务/CLI）正常收紧。
    if cfg!(test) {
        return;
    }
    if let Err(e) = harden_machine_data_dir(path) {
        log::warn!(
            "收紧 {} ACL 失败（best-effort，父目录已收紧）：{e:#}",
            path.display()
        );
    }
}

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

/// 从已连接命名管道取得客户端进程 token 的用户 SID。调用失败即拒绝该连接的高风险 L4 操作。
#[cfg(windows)]
pub fn pipe_client_sid(raw_pipe: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let mut pid = 0u32;
        if GetNamedPipeClientProcessId(raw_pipe, &mut pid) == 0 || pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            CloseHandle(process);
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = len != 0
            && GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) != 0;
        CloseHandle(token);
        CloseHandle(process);
        if !ok {
            return None;
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        sid_to_string(user.User.Sid)
    }
}

#[cfg(windows)]
unsafe fn sid_to_string(sid: windows_sys::Win32::Security::PSID) -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;

    let mut ptr: *mut u16 = std::ptr::null_mut();
    if ConvertSidToStringSidW(sid, &mut ptr) == 0 || ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let value = std::ffi::OsString::from_wide(std::slice::from_raw_parts(ptr, len))
        .to_string_lossy()
        .into_owned();
    LocalFree(ptr.cast());
    Some(value)
}

/// 复核 L4 目标仍是同一进程实例（PID + 创建时间 + canonical path）。
#[cfg(windows)]
pub fn resolve_process_instance(
    target: &net_policy_core::decrypt::ProcessInstanceRef,
) -> anyhow::Result<net_policy_core::decrypt::ProcessInstanceRef> {
    use anyhow::{bail, Context};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, target.pid);
        if process.is_null() {
            bail!("目标进程不存在或不可查询：pid={}", target.pid);
        }
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        if GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            CloseHandle(process);
            bail!("读取目标进程创建时间失败");
        }
        let created = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        let mut path = vec![0u16; 32_768];
        let mut len = path.len() as u32;
        let path_ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, path.as_mut_ptr(), &mut len)
                != 0;
        CloseHandle(process);
        if !path_ok {
            bail!("读取目标进程映像路径失败");
        }
        if target.created_at_100ns != 0 && created != target.created_at_100ns {
            bail!("目标进程实例已变化（PID 复用或进程已重启）");
        }
        let actual = std::path::PathBuf::from(String::from_utf16_lossy(&path[..len as usize]));
        let actual = actual.canonicalize().context("canonicalize 当前进程路径")?;
        let expected = std::path::Path::new(&target.path)
            .canonicalize()
            .context("canonicalize 请求进程路径")?;
        if !actual
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected.to_string_lossy())
        {
            bail!("目标进程映像路径与请求不符");
        }
        Ok(net_policy_core::decrypt::ProcessInstanceRef {
            pid: target.pid,
            created_at_100ns: created,
            path: actual.to_string_lossy().into_owned(),
        })
    }
}

#[cfg(not(windows))]
pub fn resolve_process_instance(
    _target: &net_policy_core::decrypt::ProcessInstanceRef,
) -> anyhow::Result<net_policy_core::decrypt::ProcessInstanceRef> {
    anyhow::bail!("进程实例复核仅支持 Windows")
}

#[cfg(not(windows))]
pub fn build_pipe_security() -> *mut c_void {
    std::ptr::null_mut()
}
