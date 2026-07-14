//! Windows PowerShell 调用助手 + 原生防火墙/网卡状态读取。
//!
//! 通过 `-EncodedCommand`（base64 of UTF-16LE）把脚本整体传给 powershell，避开多层转义与多行块被
//! stdin 增量解析吞掉输出的坑（见 docs/net-policy-validation-report.md §0.2.1）。

use anyhow::{bail, Context, Result};

#[cfg(windows)]
const FIREWALL_POLICY_KEY: &str =
    r"SYSTEM\CurrentControlSet\Services\SharedAccess\Parameters\FirewallPolicy";

#[cfg(windows)]
const RUN_PS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// 在 Windows 上执行一段 PowerShell 脚本，返回 stdout（UTF-8）。超时（120s）杀进程并报错。
#[cfg(windows)]
pub fn run_ps(script: &str) -> Result<String> {
    use base64::Engine;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let wrapped = format!(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8\n$ErrorActionPreference='Stop'\n{script}"
    );
    let utf16: Vec<u8> = wrapped
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);

    let mut cmd = Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        &encoded,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    crate::proc::hide_console(&mut cmd);
    let mut child = cmd.spawn().context("spawn powershell")?;
    let deadline = Instant::now() + RUN_PS_TIMEOUT;
    loop {
        match child.try_wait().context("wait powershell")? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "powershell 执行超时（{}s），已强制结束",
                    RUN_PS_TIMEOUT.as_secs()
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let out = child
        .wait_with_output()
        .context("collect powershell output")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("powershell failed ({}): {}", out.status, stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(not(windows))]
pub fn run_ps(_script: &str) -> Result<String> {
    bail!("net-policy 仅支持 Windows（当前非 Windows 平台）")
}

/// 当前是否为 Windows。
pub fn is_windows() -> bool {
    cfg!(windows)
}

/// 当前进程是否以管理员（提权）身份运行。进程生命周期内不变 → `OnceLock` 缓存。
#[cfg(windows)]
pub fn is_elevated() -> bool {
    static ELEVATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ELEVATED.get_or_init(|| {
        run_ps(
            "if(([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)){'yes'}else{'no'}",
        )
        .map(|s| s.trim() == "yes")
        .unwrap_or(false)
    })
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

/// 原生读取网卡友好名是否处于 Up；用于状态轮询热路径，避免 `Get-NetAdapter` 冷启动 PowerShell。
#[cfg(windows)]
pub fn adapter_up(alias: &str) -> Result<bool> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_INCLUDE_ALL_INTERFACES, GAA_FLAG_SKIP_ANYCAST,
        GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

    fn wide_ptr_to_string(ptr: *const u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        // SAFETY: Windows returns a NUL-terminated PWSTR for FriendlyName.
        unsafe {
            while *ptr.add(len) != 0 {
                len += 1;
            }
            OsString::from_wide(std::slice::from_raw_parts(ptr, len))
                .to_string_lossy()
                .into_owned()
        }
    }

    let flags = GAA_FLAG_INCLUDE_ALL_INTERFACES
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 15_000u32;
    let mut buf = vec![0u8; size as usize];

    // SAFETY: Buffer is sized according to GetAdaptersAddresses contract.
    let mut ret = unsafe {
        GetAdaptersAddresses(
            0,
            flags,
            std::ptr::null(),
            buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
            &mut size,
        )
    };
    if ret == ERROR_BUFFER_OVERFLOW {
        buf.resize(size as usize, 0);
        // SAFETY: Buffer resized to requested length.
        ret = unsafe {
            GetAdaptersAddresses(
                0,
                flags,
                std::ptr::null(),
                buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &mut size,
            )
        };
    }
    if ret != ERROR_SUCCESS {
        bail!("GetAdaptersAddresses failed: {ret}");
    }

    let mut cur = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !cur.is_null() {
        // SAFETY: `cur` walks the linked list returned inside `buf`.
        let adapter = unsafe { &*cur };
        let name = wide_ptr_to_string(adapter.FriendlyName);
        if name.eq_ignore_ascii_case(alias) {
            return Ok(adapter.OperStatus == IfOperStatusUp);
        }
        cur = adapter.Next;
    }
    Ok(false)
}

#[cfg(not(windows))]
pub fn adapter_up(_alias: &str) -> Result<bool> {
    bail!("net-policy 仅支持 Windows（当前非 Windows 平台）")
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain([0]).collect()
}

#[cfg(windows)]
pub fn firewall_default_outbound_domain() -> Result<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, REG_DWORD, RRF_RT_REG_DWORD,
    };

    let subkey = to_wide(&format!("{FIREWALL_POLICY_KEY}\\DomainProfile"));
    let value = to_wide("DefaultOutboundAction");
    let mut ty = 0u32;
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: All pointers valid for the call; data/size match REG_DWORD.
    let ret = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            &mut ty,
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    if ret != ERROR_SUCCESS {
        return Ok("NotConfigured".to_string());
    }
    if ty != REG_DWORD {
        bail!("DefaultOutboundAction registry type is {ty}, expected REG_DWORD");
    }
    // 注册表 DefaultOutboundAction REG_DWORD：**0=Allow、1=Block**（0.228 真机实测 0x1 = CIM Block）。
    // 早期这里 0/1 写反了 → kill-switch 实际 Block 却被读成 Allow → status.firewall.active=false →
    // GUI 误报「不受保护预览」，而实际是受保护的。此处对调修正。
    Ok(match data {
        0 => "Allow".to_string(),
        1 => "Block".to_string(),
        n => format!("Unknown({n})"),
    })
}

#[cfg(not(windows))]
pub fn firewall_default_outbound_domain() -> Result<String> {
    bail!("net-policy 仅支持 Windows（当前非 Windows 平台）")
}

#[cfg(windows)]
pub fn firewall_rule_group_count(group: &str) -> Result<u32> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
        REG_EXPAND_SZ, REG_SZ,
    };

    let subkey = to_wide(&format!("{FIREWALL_POLICY_KEY}\\FirewallRules"));
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: `subkey` is NUL-terminated UTF-16; `key` is an out param.
    let ret = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey.as_ptr(), 0, KEY_READ, &mut key) };
    if ret != ERROR_SUCCESS {
        return Ok(0);
    }

    let mut count = 0u32;
    let mut index = 0u32;
    loop {
        let mut name = vec![0u16; 512];
        let mut name_len = name.len() as u32;
        let mut ty = 0u32;
        let mut data = vec![0u8; 16 * 1024];
        let mut data_len = data.len() as u32;
        // SAFETY: Buffers valid, lengths describe capacities.
        let ret = unsafe {
            RegEnumValueW(
                key,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                std::ptr::null(),
                &mut ty,
                data.as_mut_ptr(),
                &mut data_len,
            )
        };
        match ret {
            ERROR_SUCCESS => {
                if ty == REG_SZ || ty == REG_EXPAND_SZ {
                    let words = data_len as usize / 2;
                    let raw = data.as_ptr().cast::<u16>();
                    // SAFETY: REG_SZ data is UTF-16; data_len is byte length from Windows.
                    let value = unsafe {
                        let slice = std::slice::from_raw_parts(raw, words);
                        let end = slice.iter().position(|c| *c == 0).unwrap_or(slice.len());
                        OsString::from_wide(&slice[..end])
                            .to_string_lossy()
                            .into_owned()
                    };
                    if value.contains(group) {
                        count += 1;
                    }
                }
                index += 1;
            }
            ERROR_MORE_DATA => {
                index += 1;
            }
            ERROR_NO_MORE_ITEMS => break,
            other => {
                // SAFETY: `key` was opened by RegOpenKeyExW.
                unsafe { RegCloseKey(key) };
                bail!("RegEnumValueW failed: {other}");
            }
        }
    }
    // SAFETY: `key` was opened by RegOpenKeyExW.
    unsafe { RegCloseKey(key) };
    Ok(count)
}

#[cfg(not(windows))]
pub fn firewall_rule_group_count(_group: &str) -> Result<u32> {
    bail!("net-policy 仅支持 Windows（当前非 Windows 平台）")
}
