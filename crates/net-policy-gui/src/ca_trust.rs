//! CA 信任库安装（抓包设计 §17.4/§17.8）：GUI 在**当前用户上下文**把 net-policy 专用调试 CA 的
//! **公钥证书**装进 `CurrentUser\Root`，装完按指纹**实查**证书库验证（不只信退出码），再把
//! 指纹 + owner SID 交回 agent 复核。私钥永不经过 GUI（DPAPI 密文留在 agent 的 ProgramData）。
//!
//! 指纹口径与 agent 一致：**证书 DER 的 SHA-256（大写 hex）**。这里用 PowerShell 对
//! `X509Certificate2.RawData` 现算 SHA-256 与 agent 指纹比对，天然按「本产品这张证书」精确定位，
//! 不依赖 Windows 默认的 SHA-1 thumbprint。
//!
//! 仅 Windows 有实义；非 Windows 返回明确错误（GUI 只在 Windows 分发）。

#[cfg(windows)]
use anyhow::{bail, Context, Result};
#[cfg(not(windows))]
use anyhow::{bail, Result};

/// 把公钥证书 PEM 装进 `CurrentUser\Root`（触发 Windows 根证书信任确认对话框），装完按 `thumbprint`
/// （DER SHA-256 大写）实查证书库确认存在。成功返回 `()`；用户在确认框取消或未装成功 → 错误。
#[cfg(windows)]
pub fn install_current_user_root(cert_pem: &str, thumbprint: &str) -> Result<()> {
    let thumb = normalize_thumb(thumbprint)?;
    // 写临时 .crt（仅公钥，非秘密）。用进程 id + 指纹前 8 位命名，避免碰撞。
    let tmp = std::env::temp_dir().join(format!(
        "net-policy-ca-{}-{}.crt",
        std::process::id(),
        &thumb[..8.min(thumb.len())]
    ));
    std::fs::write(&tmp, cert_pem).with_context(|| format!("写临时证书 {}", tmp.display()))?;
    let _guard = TmpGuard(tmp.clone());

    // certutil -user -addstore Root：装进当前用户 Root，会弹 Windows 根证书安全警告（用户须确认）。
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let certutil = std::path::Path::new(&system_root)
        .join("System32")
        .join("certutil.exe");
    let mut cmd = std::process::Command::new(&certutil);
    cmd.args(["-user", "-addstore", "Root"]).arg(&tmp);
    hide_console(&mut cmd);
    let out = cmd.output().context("启动 certutil 失败")?;
    if !out.status.success() {
        bail!(
            "装入 CurrentUser\\Root 失败（certutil exit={}）：{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    // 实查证书库：按 DER SHA-256 == thumbprint 确认确实装上了（§17.4：不只信退出码）。
    if !is_installed_current_user_root(&thumb)? {
        bail!("certutil 报成功但证书库中未按指纹找到本产品 CA（可能用户在确认框取消）");
    }
    Ok(())
}

/// 从 `CurrentUser\Root` 精确删除本产品 CA（按 DER SHA-256 == thumbprint 匹配）。幂等：不存在也返回 Ok。
#[cfg(windows)]
pub fn remove_current_user_root(thumbprint: &str) -> Result<()> {
    let thumb = normalize_thumb(thumbprint)?;
    let script = format!(
        r#"$t='{thumb}';
Get-ChildItem Cert:\CurrentUser\Root | Where-Object {{
  ([System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash($_.RawData)).Replace('-','')) -ieq $t
}} | Remove-Item -Force -ErrorAction SilentlyContinue
'OK'"#
    );
    let out = run_ps(&script)?;
    if !out.contains("OK") {
        bail!("从 CurrentUser\\Root 删除 CA 失败：{out}");
    }
    Ok(())
}

/// 当前进程用户的 SID 字符串（`S-1-5-...`）。CA owner 绑定用（§17.8）。
#[cfg(windows)]
pub fn current_user_sid() -> Result<String> {
    let sid = run_ps("[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value")?;
    let sid = sid.trim().to_string();
    if !sid.starts_with("S-1-") {
        bail!("取当前用户 SID 失败：{sid}");
    }
    Ok(sid)
}

/// 证书库里是否存在 DER SHA-256 == `thumb` 的证书（CurrentUser\Root）。
#[cfg(windows)]
fn is_installed_current_user_root(thumb: &str) -> Result<bool> {
    let script = format!(
        r#"$t='{thumb}';$f=$false;
Get-ChildItem Cert:\CurrentUser\Root | ForEach-Object {{
  $h=[System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash($_.RawData)).Replace('-','')
  if ($h -ieq $t) {{ $f=$true }}
}}
if ($f) {{ 'FOUND' }} else {{ 'MISSING' }}"#
    );
    Ok(run_ps(&script)?.contains("FOUND"))
}

/// 校验 thumbprint 为 64 位 hex（DER SHA-256），防注入进 PowerShell 脚本。
#[cfg(windows)]
fn normalize_thumb(thumbprint: &str) -> Result<String> {
    let t: String = thumbprint.trim().to_uppercase();
    if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(t)
    } else {
        bail!("非法指纹（应为 64 位 hex 的 DER SHA-256）：{thumbprint}")
    }
}

/// 跑一段 PowerShell（-NoProfile -NonInteractive），返回 stdout。
#[cfg(windows)]
fn run_ps(script: &str) -> Result<String> {
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    hide_console(&mut cmd);
    let out = cmd.output().context("启动 powershell 失败")?;
    if !out.status.success() {
        bail!(
            "powershell 失败（exit={}）：{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 隐藏子进程控制台窗口（CREATE_NO_WINDOW）。
#[cfg(windows)]
fn hide_console(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// 临时证书文件守卫：作用域结束即删。
#[cfg(windows)]
struct TmpGuard(std::path::PathBuf);
#[cfg(windows)]
impl Drop for TmpGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ── 非 Windows：GUI 仅 Windows 分发；给出明确错误而非静默 ──────────────────────

#[cfg(not(windows))]
pub fn install_current_user_root(_cert_pem: &str, _thumbprint: &str) -> Result<()> {
    bail!("CA 信任库安装仅支持 Windows")
}
#[cfg(not(windows))]
pub fn remove_current_user_root(_thumbprint: &str) -> Result<()> {
    bail!("CA 信任库操作仅支持 Windows")
}
#[cfg(not(windows))]
pub fn current_user_sid() -> Result<String> {
    bail!("取用户 SID 仅支持 Windows")
}
