//! Windows DPAPI machine-scope 加解密（抓包设计 §17.4：CA 私钥静态保护）。
//!
//! CA 私钥在磁盘上永不明文：`create` 时用 `CryptProtectData(CRYPTPROTECT_LOCAL_MACHINE)` 加密后落
//! `<ws>/mitm/private/ca.key.dpapi`，会话启动时 `CryptUnprotectData` 解到内存构造 `CertAuthority`
//! （从不写明文 `ca.key`）。machine scope 让以 SYSTEM 运行的服务能解密（与创建它的用户无关），配合
//! ProgramData 目录 ACL（SYSTEM + Administrators）双重保护。
//!
//! **非 Windows**（CI/`--exclude` 场景）没有 DPAPI：退化为「加个魔数前缀原样存」，仅保证 round-trip
//! 可跑单测，绝不当作真实保护——生产只在 Windows 服务下运行。

use anyhow::{bail, Result};

/// 非 Windows 退化格式的魔数前缀（明确标识「未加密」，防误当密文）。
#[cfg(not(windows))]
const PLAIN_MAGIC: &[u8] = b"NPDPAPI-PLAINTEXT-FALLBACK\0";

/// DPAPI machine-scope 加密（`CRYPTPROTECT_LOCAL_MACHINE`）。
#[cfg(windows)]
pub fn protect_machine(plaintext: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_LOCAL_MACHINE, CRYPT_INTEGER_BLOB,
    };
    // 输入 blob 需可变指针，但 CryptProtectData 不改它——拷一份避免对 &[u8] 取可变裸指针。
    let mut input = plaintext.to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: 两个 blob 均有效；成功后 pbData 由 LocalFree 释放。
    let ok = unsafe {
        CryptProtectData(
            &in_blob,
            std::ptr::null(),     // description
            std::ptr::null_mut(), // optional entropy
            std::ptr::null_mut(), // reserved
            std::ptr::null_mut(), // prompt struct
            CRYPTPROTECT_LOCAL_MACHINE,
            &mut out_blob,
        )
    };
    if ok == 0 {
        bail!("CryptProtectData 失败（GetLastError={}）", unsafe {
            windows_sys::Win32::Foundation::GetLastError()
        });
    }
    let result = copy_and_free(&out_blob);
    Ok(result)
}

/// DPAPI 解密（scope 由密文自带，machine-scope 密文任意本机进程可解）。
#[cfg(windows)]
pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let mut input = ciphertext.to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: 见 protect_machine。
    let ok = unsafe {
        CryptUnprotectData(
            &in_blob,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut out_blob,
        )
    };
    if ok == 0 {
        bail!("CryptUnprotectData 失败（GetLastError={}）", unsafe {
            windows_sys::Win32::Foundation::GetLastError()
        });
    }
    let result = copy_and_free(&out_blob);
    Ok(result)
}

/// 把 DPAPI 输出 blob 拷成 `Vec` 并 `LocalFree` 原缓冲。
#[cfg(windows)]
fn copy_and_free(blob: &windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: pbData/cbData 由 DPAPI 填；成功后必须 LocalFree。
    unsafe {
        let slice = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize);
        let out = slice.to_vec();
        windows_sys::Win32::Foundation::LocalFree(blob.pbData as *mut _);
        out
    }
}

#[cfg(not(windows))]
pub fn protect_machine(plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut out = PLAIN_MAGIC.to_vec();
    out.extend_from_slice(plaintext);
    Ok(out)
}

#[cfg(not(windows))]
pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>> {
    match ciphertext.strip_prefix(PLAIN_MAGIC) {
        Some(rest) => Ok(rest.to_vec()),
        None => bail!("非 Windows 退化解密：缺魔数前缀（密文格式不符）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let secret = b"-----BEGIN PRIVATE KEY-----\nMIIsecret\n-----END PRIVATE KEY-----\n";
        let enc = protect_machine(secret).unwrap();
        // 密文不得含明文私钥体（至少不等于原文）。
        assert_ne!(enc, secret.to_vec());
        let dec = unprotect(&enc).unwrap();
        assert_eq!(dec, secret.to_vec());
    }

    #[test]
    fn unprotect_rejects_garbage() {
        // 随机字节解密应失败（Windows: DPAPI 拒；非 Windows: 缺魔数）。
        assert!(unprotect(b"not a valid dpapi blob at all").is_err());
    }
}
