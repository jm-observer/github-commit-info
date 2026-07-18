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

/// NSIS 会先把 bundle resource 解压到最终安装目录，再从该目录调用 `install`。因此重装时
/// `--mihomo` 很可能就是目标文件本身；Windows 上对同一文件执行 `copy` 会失败。
fn same_file_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    let normalize = |path: &std::path::Path| {
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .replace('/', "\\")
    };
    normalize(left).eq_ignore_ascii_case(&normalize(right))
}

/// 安装：**原子化**复制完整运行资产（agent + mihomo + 可选 wintun）到受保护目录 + 注册计划任务。
/// 须管理员。缺 mihomo 则**不注册任务、不返回 installed**（评审点 1：否则登录后 apply 必失败）。
///
/// `mitm_zip`：给了则**顺带部署 L4 MITM 引擎**（best-effort，失败不阻断核心网络安装——L4 是可选高风险
/// 能力）；不给则跳过（可事后 `install-mitm-engine` 补）。
pub fn install(
    mihomo_src: Option<&str>,
    wintun_src: Option<&str>,
    mitm_zip: Option<&str>,
) -> Result<()> {
    if !crate::win::is_windows() {
        bail!("仅支持 Windows");
    }
    if !crate::win::is_elevated() {
        bail!("安装需要管理员权限：请以管理员身份运行。");
    }

    // 0) **先验证所有源资产，再动 %ProgramFiles%**（评审点 7：避免 mihomo 缺失时留下半装的 agent）。
    let install_dir = paths::install_dir();
    let mihomo_dst = install_dir.join(paths::MIHOMO_BIN_NAME);
    let mihomo_pending_dst = paths::pending_mihomo_bin();
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
    if !same_file_path(&current, &target) {
        std::fs::copy(&current, &target)
            .with_context(|| format!("复制 agent 到 {} 失败", target.display()))?;
    }

    // 1b) mihomo：首次安装直接落正式路径；覆盖安装时旧 mihomo 仍承载数据面、不可覆盖，
    //     因而只暂存为 pending，等下一次受控 reapply 在 kill-switch 内完成切换。
    if let Some(src) = mihomo_src {
        let src_path = std::path::Path::new(src);
        if mihomo_dst.exists() {
            if !same_file_path(src_path, &mihomo_dst)
                && !same_file_path(src_path, &mihomo_pending_dst)
            {
                std::fs::copy(src_path, &mihomo_pending_dst).with_context(|| {
                    format!(
                        "暂存 mihomo（{src} → {}）失败",
                        mihomo_pending_dst.display()
                    )
                })?;
            }
        } else if !same_file_path(src_path, &mihomo_dst) {
            std::fs::copy(src_path, &mihomo_dst)
                .with_context(|| format!("安装 mihomo（{src} → {}）失败", mihomo_dst.display()))?;
            if same_file_path(src_path, &mihomo_pending_dst) {
                std::fs::remove_file(src_path)
                    .with_context(|| format!("清理 mihomo 暂存文件失败：{src}"))?;
            }
        }
    }
    // 1c) wintun.dll：给了源路径就复制过去（gvisor 栈通常内置，可选）。
    if let Some(src) = wintun_src {
        let wintun_dst = install_dir.join("wintun.dll");
        if !same_file_path(std::path::Path::new(src), &wintun_dst) {
            std::fs::copy(src, &wintun_dst)
                .with_context(|| format!("复制 wintun（{src} → {}）失败", wintun_dst.display()))?;
        }
    }

    // 1d) **原子性校验**：mihomo 必须已就位（复制来的 or 之前已放），否则安装未完成。
    if !mihomo_dst.exists() {
        bail!(
            "安装未完成：受保护目录缺少 mihomo（{}）。请用 --mihomo <源路径> 指定 mihomo-windows-amd64.exe（提权进程会复制过去），不要求用户事后手工放置。",
            mihomo_dst.display()
        );
    }

    // 2) 创建 Windows 服务：LocalSystem + 开机自启（Automatic）+ 崩溃自动重启。binPath 带 `run-service`
    //    参数（SCM 拉起 service::run）。服务在 Session 0 常驻、**不依赖用户登录**——根治 D-1（登录会话
    //    结束 → agent+mihomo 被杀 → 防火墙残留断网）。重装时原地更新旧服务，避免 delete 后服务仍处于
    //    “marked for deletion”窗口，导致紧接着 New-Service 返回失败。
    let exe = target.to_string_lossy().replace('\'', "''");
    let script = format!(
        r#"$exe='{exe}'
$bin = '"' + $exe + '" run-service'
$old = Get-Service -Name '{TASK_NAME}' -ErrorAction SilentlyContinue
if ($old) {{
    if ($old.Status -ne 'Stopped') {{
        Stop-Service -Name '{TASK_NAME}' -Force
        $old.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(30))
    }}
    sc.exe delete '{TASK_NAME}' | Out-Null
    if ($LASTEXITCODE -ne 0) {{ throw "删除旧 net-policy-agent 服务失败（sc.exe delete 返回 $LASTEXITCODE）" }}
    $deleteDeadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deleteDeadline -and (Get-Service -Name '{TASK_NAME}' -ErrorAction SilentlyContinue)) {{
        Start-Sleep -Milliseconds 300
    }}
    if (Get-Service -Name '{TASK_NAME}' -ErrorAction SilentlyContinue) {{
        throw '旧 net-policy-agent 服务仍处于删除中，无法重新创建'
    }}
}}
New-Service -Name '{TASK_NAME}' -BinaryPathName $bin -DisplayName 'Net Policy Agent' -StartupType Automatic -Description '网络出口策略守护（mihomo / 防火墙 / TUN；开机自启，Session 0 常驻）' | Out-Null
sc.exe failure '{TASK_NAME}' reset= 0 actions= restart/5000/restart/5000/restart/5000 | Out-Null
if ($LASTEXITCODE -ne 0) {{ throw "配置 net-policy-agent 恢复策略失败（sc.exe failure 返回 $LASTEXITCODE）" }}
Start-Service -Name '{TASK_NAME}'
$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline) {{
    $client = [System.IO.Pipes.NamedPipeClientStream]::new('.', 'net-policy-agent', [System.IO.Pipes.PipeDirection]::InOut)
    try {{
        $client.Connect(500)
        'OK'
        exit 0
    }} catch {{
        $svc = Get-Service -Name '{TASK_NAME}' -ErrorAction SilentlyContinue
        if (-not $svc -or $svc.Status -eq 'Stopped') {{ throw "net-policy-agent 服务启动失败或已停止" }}
    }} finally {{
        $client.Dispose()
    }}
    Start-Sleep -Milliseconds 300
}}
throw 'net-policy-agent 服务启动超时：30 秒内未创建命名管道'
"#
    );
    let out = run_ps(&script).context("创建 net-policy-agent 服务失败")?;
    if !out.trim().contains("OK") {
        bail!("创建服务返回异常：{}", out.trim());
    }

    // 3) L4 MITM 引擎（可选，best-effort）：给了 --mitm-zip 才部署；失败只记录，不回滚核心安装
    //    （网络策略是核心功能，L4 是独立高风险可选能力，不能因引擎部署失败让整机装不上）。
    let mitm = match mitm_zip {
        Some(zip) => match crate::mitm_engine::deploy(Some(zip)) {
            Ok(v) => v,
            Err(e) => serde_json::json!({"result": "engine_deploy_failed", "error": e.to_string()}),
        },
        None => serde_json::json!({
            "result": "skipped",
            "hint": "可事后 install-mitm-engine 补部署",
            "current": crate::mitm_engine::status(),
        }),
    };

    println!(
        "{}",
        serde_json::json!({
            "result": "installed",
            "service": TASK_NAME,
            "exe": target.to_string_lossy(),
            "mihomo": mihomo_dst.to_string_lossy(),
            "mitm_engine": mitm,
            "note": "已注册 Windows 服务（LocalSystem，开机自启 + Session 0 常驻 + 崩溃自动重启），并已启动"
        })
    );
    Ok(())
}

/// 卸载：停止并删除服务（数据保留）。
pub fn uninstall() -> Result<()> {
    if !crate::win::is_windows() {
        bail!("仅支持 Windows");
    }
    if !crate::win::is_elevated() {
        bail!("卸载需要管理员权限。");
    }
    let out = run_ps(&format!(
        r#"$svc = Get-Service -Name '{TASK_NAME}' -ErrorAction SilentlyContinue
if ($svc) {{ try {{ Stop-Service -Name '{TASK_NAME}' -Force -ErrorAction SilentlyContinue }} catch {{}}; sc.exe delete '{TASK_NAME}' | Out-Null; 'OK' }} else {{ 'NOTFOUND' }}"#
    ))?;
    let trimmed = out.trim();
    // L4 引擎清理（ADR §5）：撤 Defender 排除 + 删引擎目录（best-effort，不影响服务卸载结果）。
    let mitm = crate::mitm_engine::cleanup();
    if trimmed.contains("OK") {
        println!(
            "{}",
            serde_json::json!({"result": "uninstalled", "service": TASK_NAME, "mitm_engine": mitm})
        );
    } else {
        println!(
            "{}",
            serde_json::json!({"result": "not_found_or_error", "detail": trimmed, "mitm_engine": mitm})
        );
    }
    Ok(())
}
