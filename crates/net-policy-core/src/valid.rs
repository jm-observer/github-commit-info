//! 输入校验：所有进 PowerShell / mihomo YAML 的值必须先过这里。
//!
//! **这是一条跨完整性级别的安全边界，不只是业务校验**（设计文档 §3.1）：agent 以管理员身份
//! 消费用户可写的 settings/rules，所有值都可能被拿来拼 PowerShell / YAML / 命令行。
//!
//! 防注入（审查 P1-3）：IP/CIDR/域名/进程名严格白名单字符；拒绝换行、逗号、引号、
//! 反引号、`$`、`;`、`|` 等会改写 PS 命令或 YAML 规则行的字符。

use anyhow::{bail, Result};
use std::net::{Ipv4Addr, Ipv6Addr};

/// 严格集：IP/CIDR/域名/key 用。括号等一律拒绝。
fn no_meta(s: &str) -> Result<()> {
    if s.is_empty() {
        bail!("空值");
    }
    if s.chars()
        .any(|c| c.is_control() || "\n\r,;|&`$'\"<>(){}".contains(c))
    {
        bail!("含非法字符：{s:?}");
    }
    Ok(())
}

/// 路径/进程名用的放宽集：**允许括号/方括号/空格**（常见 Windows 路径如
/// `C:\Program Files (x86)\...`，P2 修正），只拦真正会改写 PS/YAML 的字符：
/// 换行、逗号、引号、反引号、`;|&$<>`。
fn no_inject_path(s: &str) -> Result<()> {
    if s.is_empty() {
        bail!("空值");
    }
    if s.chars()
        .any(|c| c.is_control() || "\n\r,;|&`$'\"<>".contains(c))
    {
        bail!("含非法字符：{s:?}");
    }
    Ok(())
}

/// 校验 IPv4 或 IPv6 地址。
pub fn ip(s: &str) -> Result<()> {
    no_meta(s)?;
    if s.parse::<Ipv4Addr>().is_ok() || s.parse::<Ipv6Addr>().is_ok() {
        Ok(())
    } else {
        bail!("非法 IP：{s}")
    }
}

/// 校验 CIDR（`ip/prefix`）。
pub fn cidr(s: &str) -> Result<()> {
    no_meta(s)?;
    let (addr, prefix) = s
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("非法 CIDR（缺 /）：{s}"))?;
    let p: u32 = prefix
        .parse()
        .map_err(|_| anyhow::anyhow!("非法前缀：{s}"))?;
    if addr.parse::<Ipv4Addr>().is_ok() {
        if p > 32 {
            bail!("IPv4 前缀越界：{s}");
        }
    } else if addr.parse::<Ipv6Addr>().is_ok() {
        if p > 128 {
            bail!("IPv6 前缀越界：{s}");
        }
    } else {
        bail!("非法 CIDR 地址：{s}");
    }
    Ok(())
}

/// IP 或 CIDR 都接受。
pub fn ip_or_cidr(s: &str) -> Result<()> {
    if s.contains('/') {
        cidr(s)
    } else {
        ip(s)
    }
}

/// 校验 mihomo 出站代理服务器地址（IP 或域名）。
pub fn host(s: &str) -> Result<()> {
    if s.parse::<Ipv4Addr>().is_ok() || s.parse::<Ipv6Addr>().is_ok() {
        return ip(s);
    }
    domain(s)
}

/// 校验 mihomo 订阅地址。订阅由 mihomo 自己下载，因此这里只接受 HTTP(S)，并拒绝
/// 会改变 YAML 或跨权限命令语义的控制字符/引号。
pub fn http_url(s: &str) -> Result<()> {
    if s.len() > 2048
        || s.is_empty()
        || s.chars()
            .any(|c| c.is_control() || "\r\n\\\"'`<>".contains(c))
    {
        bail!("订阅地址非法");
    }
    if !(s.starts_with("http://") || s.starts_with("https://")) {
        bail!("订阅地址必须以 http:// 或 https:// 开头");
    }
    Ok(())
}

/// 校验域名（DOMAIN-SUFFIX 用）。
pub fn domain(s: &str) -> Result<()> {
    no_meta(s)?;
    if s.len() > 253 {
        bail!("域名过长：{s}");
    }
    let ok = s.split('.').all(|label| {
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    if !ok {
        bail!("非法域名：{s}");
    }
    Ok(())
}

/// 校验域名关键词（DOMAIN-KEYWORD 用）。它不是完整域名，只允许能安全写进 mihomo YAML 的短关键词。
pub fn domain_keyword(s: &str) -> Result<()> {
    no_meta(s)?;
    if s.len() > 128 {
        bail!("域名关键词过长：{s}");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("非法域名关键词：{s}");
    }
    Ok(())
}

/// 校验进程名（如 `steam.exe`）。允许括号（少见但合法），拒注入元字符。
pub fn process_name(s: &str) -> Result<()> {
    no_inject_path(s)?;
    if s.contains('/') || s.contains('\\') {
        bail!("进程名不应含路径分隔符：{s}");
    }
    if s.len() > 260 {
        bail!("进程名过长");
    }
    Ok(())
}

/// 校验进程完整路径（Windows 路径，允许空格/反斜杠/盘符/括号，拒绝注入元字符与换行）。
pub fn process_path(s: &str) -> Result<()> {
    no_inject_path(s)?; // 允许 (){}[] 等，拒换行/逗号/引号/反引号/;|&$<>
    if s.len() > 260 {
        bail!("路径过长");
    }
    Ok(())
}

/// base64 密钥粗校验（WireGuard key 是 44 字符 base64）。
pub fn wg_key(s: &str) -> Result<()> {
    no_meta(s)?;
    if s.len() < 40
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    {
        bail!("非法 WireGuard 密钥");
    }
    Ok(())
}
