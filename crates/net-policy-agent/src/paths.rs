//! 路径解析 + **二进制完整性**（设计文档 §5.3 / D6）。
//!
//! 关键安全约束：agent 以最高权限自启，**绝不能**执行/加载用户可写目录里的东西。
//! - 可执行资产（agent.exe / mihomo.exe / wintun.dll）在 `%ProgramFiles%\net-policy\`（管理员可写、
//!   普通用户只读）。
//! - **production 完全忽略 `MIHOMO_BIN`**；仅前台开发模式（`run --dev`）允许覆盖。
//! - 启动 mihomo 前 canonicalize 其路径并校验落在受保护安装目录内（reparse point/symlink 防护）。
//! - workspace（settings/rules/generated/killswitch-state）可属用户，但其中内容绝不当代码执行。

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// mihomo 二进制文件名（Windows amd64 -compatible 变体）。
pub const MIHOMO_BIN_NAME: &str = "mihomo-windows-amd64.exe";

/// 受保护安装目录：`%ProgramFiles%\net-policy\`。可执行资产只放这里。
pub fn install_dir() -> PathBuf {
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
    Path::new(&pf).join("net-policy")
}

/// agent 自身 exe 的安装路径（计划任务 Action 用绝对路径指向它）。
pub fn agent_exe() -> PathBuf {
    install_dir().join("net-policy-agent.exe")
}

/// 用户可写的 workspace（数据目录）：`-w` / `NET_POLICY_WORKSPACE` / **`~/.config/net-policy-agent`**。
/// 所有配置/规则/生成配置/SQLite 记录都放这里（与 zero-desktop `.config/<app>` 约定一致）。
pub fn workspace_dir(arg: Option<&str>) -> PathBuf {
    if let Some(a) = arg {
        if !a.trim().is_empty() {
            return PathBuf::from(a);
        }
    }
    if let Ok(env) = std::env::var("NET_POLICY_WORKSPACE") {
        if !env.trim().is_empty() {
            return PathBuf::from(env);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".config").join("net-policy-agent")
}

/// 解析 mihomo 可执行路径，**带完整性校验**。
///
/// - `dev=false`（production/已安装）：**忽略 `MIHOMO_BIN`**，固定 `install_dir()/mihomo`，且必须存在、
///   canonicalize 后仍落在 `install_dir()` 内（防 symlink/reparse 逃逸）。
/// - `dev=true`（前台开发）：允许 `MIHOMO_BIN` 覆盖，否则回退 `install_dir()/mihomo`；不做严格目录校验
///   （开发便利），但仍要求文件存在。
pub fn resolve_mihomo_bin(dev: bool) -> Result<PathBuf> {
    if dev {
        if let Ok(p) = std::env::var("MIHOMO_BIN") {
            let p = PathBuf::from(p);
            if !p.exists() {
                bail!("MIHOMO_BIN 指向的文件不存在：{}", p.display());
            }
            return Ok(p);
        }
        let p = install_dir().join(MIHOMO_BIN_NAME);
        if !p.exists() {
            bail!("mihomo 不存在：{}（开发模式可设 MIHOMO_BIN）", p.display());
        }
        return Ok(p);
    }

    // production：固定受保护安装目录，忽略任何环境覆盖。
    let dir = install_dir();
    let bin = dir.join(MIHOMO_BIN_NAME);
    if !bin.exists() {
        bail!(
            "mihomo 不存在于受保护安装目录：{}（请重新安装/更新 net-policy）",
            bin.display()
        );
    }
    // canonicalize 两端后校验 bin 落在 install_dir 内——防用户用软链把 install_dir 下的名字
    // 指到别处的可写文件（reparse point/symlink 逃逸）。
    let canon_dir = dir
        .canonicalize()
        .with_context(|| format!("canonicalize 安装目录失败：{}", dir.display()))?;
    let canon_bin = bin
        .canonicalize()
        .with_context(|| format!("canonicalize mihomo 失败：{}", bin.display()))?;
    if !canon_bin.starts_with(&canon_dir) {
        bail!(
            "mihomo 路径逃逸出受保护安装目录（{} 不在 {} 内）——拒绝以管理员启动",
            canon_bin.display(),
            canon_dir.display()
        );
    }
    Ok(canon_bin)
}

/// mihomo 的家目录（`-d`，放缓存/wintun）。用 workspace 下的 net-policy/ 即可（数据，非可执行）。
pub fn mihomo_home(workspace: &Path) -> PathBuf {
    net_policy_core::config::net_policy_dir(workspace)
}
