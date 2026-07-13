//! 安装/卸载：登录触发的**最高权限计划任务，以交互用户身份运行**（设计 §5/D1）。
//!
//! - 二进制完整性（§5.3/D6）：把 agent.exe 复制到 `%ProgramFiles%\net-policy\`（管理员可写、普通
//!   用户只读；ProgramFiles 默认 ACL 即满足），任务 Action 用该绝对路径。mihomo/wintun 亦应置于此。
//! - 崩溃拉起（§5.2）：`New-ScheduledTaskSettingsSet -RestartCount/-RestartInterval`（`ONLOGON` 本身
//!   不含崩溃重启）。
//! - 前提（§5.1）：仅支持「目标交互用户 = 本机管理员」；安装时须由该用户提权同意（弹一次 UAC）。

use crate::paths;
use crate::win::run_ps;
use anyhow::{bail, Context, Result};

const TASK_NAME: &str = "net-policy-agent";

/// 安装：**原子化**复制完整运行资产（agent + mihomo + 可选 wintun）到受保护目录 + 注册计划任务。
/// 须管理员。缺 mihomo 则**不注册任务、不返回 installed**（评审点 1：否则登录后 apply 必失败）。
pub fn install(mihomo_src: Option<&str>, wintun_src: Option<&str>) -> Result<()> {
    if !crate::win::is_windows() {
        bail!("仅支持 Windows");
    }
    if !crate::win::is_elevated() {
        bail!("安装需要管理员权限：请以管理员身份运行。");
    }

    // 0) **先验证所有源资产，再动 %ProgramFiles%**（评审点 7：避免 mihomo 缺失时留下半装的 agent）。
    let install_dir = paths::install_dir();
    let mihomo_dst = install_dir.join(paths::MIHOMO_BIN_NAME);
    match mihomo_src {
        Some(src) if !std::path::Path::new(src).exists() => {
            bail!("--mihomo 源文件不存在：{src}（安装中止，未改动任何目录）")
        }
        None if !mihomo_dst.exists() => bail!(
            "未提供 --mihomo 且受保护目录无 mihomo（{}）——拒绝安装以免留下无法运行的半装状态。请用 --mihomo <源路径> 指定。",
            mihomo_dst.display()
        ),
        _ => {}
    }
    if let Some(src) = wintun_src {
        if !std::path::Path::new(src).exists() {
            bail!("--wintun 源文件不存在：{src}（安装中止）");
        }
    }

    // 1) 复制可执行资产到 %ProgramFiles%\net-policy\（受保护，普通用户不可写 = 防提权替换，D6）。
    std::fs::create_dir_all(&install_dir)
        .with_context(|| format!("创建安装目录失败：{}", install_dir.display()))?;

    // 1a) agent.exe。
    let target = paths::agent_exe();
    let current = std::env::current_exe().context("取当前 exe 路径失败")?;
    if current != target {
        std::fs::copy(&current, &target)
            .with_context(|| format!("复制 agent 到 {} 失败", target.display()))?;
    }

    // 1b) mihomo：给了源路径就复制过去（源已在步 0 校验存在）。
    if let Some(src) = mihomo_src {
        std::fs::copy(src, &mihomo_dst)
            .with_context(|| format!("复制 mihomo（{src} → {}）失败", mihomo_dst.display()))?;
    }
    // 1c) wintun.dll：给了源路径就复制过去（gvisor 栈通常内置，可选）。
    if let Some(src) = wintun_src {
        let wintun_dst = install_dir.join("wintun.dll");
        std::fs::copy(src, &wintun_dst)
            .with_context(|| format!("复制 wintun（{src} → {}）失败", wintun_dst.display()))?;
    }

    // 1d) **原子性校验**：mihomo 必须已就位（复制来的 or 之前已放），否则安装未完成。
    if !mihomo_dst.exists() {
        bail!(
            "安装未完成：受保护目录缺少 mihomo（{}）。请用 --mihomo <源路径> 指定 mihomo-windows-amd64.exe（提权进程会复制过去），不要求用户事后手工放置。",
            mihomo_dst.display()
        );
    }

    // 2) 注册计划任务：AtLogOn + Highest + 交互用户 + 崩溃重启 + 无限执行时长。
    let exe = target.to_string_lossy().replace('\'', "''");
    let script = format!(
        r#"$exe='{exe}'
$action = New-ScheduledTaskAction -Execute $exe -Argument 'run'
$trigger = New-ScheduledTaskTrigger -AtLogOn
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Seconds 0)
Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
'OK'
"#
    );
    let out = run_ps(&script).context("注册计划任务失败")?;
    if !out.trim().contains("OK") {
        bail!("注册计划任务返回异常：{}", out.trim());
    }
    println!(
        "{}",
        serde_json::json!({
            "result": "installed",
            "task": TASK_NAME,
            "exe": target.to_string_lossy(),
            "mihomo": mihomo_dst.to_string_lossy(),
            "note": "登录时以最高权限（交互用户）自启；全部可执行资产已置于受保护目录"
        })
    );
    Ok(())
}

/// 卸载：注销计划任务（数据保留）。
pub fn uninstall() -> Result<()> {
    if !crate::win::is_windows() {
        bail!("仅支持 Windows");
    }
    if !crate::win::is_elevated() {
        bail!("卸载需要管理员权限。");
    }
    let out = run_ps(&format!(
        "try{{ Unregister-ScheduledTask -TaskName '{TASK_NAME}' -Confirm:$false -ErrorAction Stop; 'OK' }}catch{{ 'ERR:'+$_.Exception.Message }}"
    ))?;
    let trimmed = out.trim();
    if trimmed.contains("OK") {
        println!(
            "{}",
            serde_json::json!({"result": "uninstalled", "task": TASK_NAME})
        );
    } else {
        println!(
            "{}",
            serde_json::json!({"result": "not_found_or_error", "detail": trimmed})
        );
    }
    Ok(())
}
