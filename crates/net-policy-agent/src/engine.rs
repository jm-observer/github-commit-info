//! mihomo 引擎进程生命周期（副作用层）。配置生成的纯逻辑在 `net_policy_core::mihomo`。
//!
//! 落地的验证结论（docs/net-policy-validation-report.md）：
//! - 停 mihomo 必须优雅：先 API 关 TUN 再结束进程（§0.8.2bis，避免 wintun 路由残留断网）。

use crate::win::run_ps;
use anyhow::{bail, Context, Result};
use net_policy_core::config::{mihomo_config_path, NetPolicySettings, RuleSet, TempDirect};
use net_policy_core::mihomo::{generate_config, CONTROLLER};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 生成随机 controller secret（hex）。威胁模型：明文写在 generated/config.yaml，防的是盲打
/// 127.0.0.1:9090 的进程，非同用户强隔离（详见 core config 头注释）。
pub fn gen_secret() -> String {
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn auth_header(secret: &str) -> String {
    if secret.is_empty() {
        "@{}".to_string()
    } else {
        // secret 内插进单引号 PS 字符串——转义 `'` 作纵深防御（恢复路径已在 core
        // read_generated_secret 校验为 hex，此处再兜一层，不依赖上游净化）。
        let s = secret.replace('\'', "''");
        format!("@{{ Authorization = 'Bearer {s}' }}")
    }
}

/// 写出生成的 mihomo 配置文件，返回其路径。
pub fn write_config(
    workspace: &Path,
    settings: &NetPolicySettings,
    rules: &RuleSet,
    secret: &str,
    temp: &TempDirect,
) -> Result<PathBuf> {
    let path = mihomo_config_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, generate_config(settings, rules, secret, temp))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// 启动 mihomo（配置须已写）。`bin` 是**已完整性校验**的绝对路径（见 paths::resolve_mihomo_bin），
/// `home` 是 `-d` 家目录，`cfg` 是 `-f` 配置路径。返回 pid。
pub fn start(bin: &Path, home: &Path, cfg: &Path) -> Result<u32> {
    if !bin.exists() {
        bail!("mihomo 可执行文件不存在：{}", bin.display());
    }
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("-d").arg(home).arg("-f").arg(cfg);
    crate::proc::hide_console(&mut cmd);
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn mihomo: {}", bin.display()))?;
    Ok(child.id())
}

/// 热重载配置（逐项放行不重启隧道）：调用方须**先** `write_config` 再调本函数。
pub fn reload(workspace: &Path, secret: &str) -> Result<()> {
    let h = auth_header(secret);
    let cfg = mihomo_config_path(workspace);
    let path = cfg.to_string_lossy().replace('\\', "\\\\");
    let out = run_ps(&format!(
        r#"$h={h}
$body='{{"path":"{path}"}}'
try{{ Invoke-RestMethod 'http://{CONTROLLER}/configs' -Method PUT -Headers $h -ContentType 'application/json' -Body $body -TimeoutSec 8 | Out-Null; 'OK' }}
catch{{ "ERR:$($_.Exception.Message)" }}
"#
    ))?;
    if !out.trim().starts_with("OK") {
        bail!("mihomo 热重载失败：{}", out.trim());
    }
    Ok(())
}

/// 优雅停 mihomo（§0.8.2bis）：先 API 关 TUN，轮询确认 Meta 拆除再按 pid 结束。TUN 未拆除则 bail
/// （不强杀），让调用方保持防火墙生效。pid 未知时按进程名回退。
pub fn graceful_stop(pid: Option<u32>, secret: &str, fallback_name: &str) -> Result<()> {
    let h = auth_header(secret);
    let kill = match pid {
        Some(p) => format!(
            "Stop-Process -Id {p} -Force -ErrorAction SilentlyContinue; \
             if(Get-Process -Id {p} -ErrorAction SilentlyContinue){{ throw 'mihomo pid {p} 未能结束' }}"
        ),
        None => format!(
            "Get-Process '{}' -ErrorAction SilentlyContinue | Stop-Process -Force",
            fallback_name.replace('\'', "''")
        ),
    };
    run_ps(&format!(
        r#"$h={h}
try{{ Invoke-RestMethod 'http://{CONTROLLER}/configs' -Method PATCH -Headers $h -Body '{{"tun":{{"enable":false}}}}' -TimeoutSec 4 | Out-Null }}catch{{}}
$gone=$false
for($i=0;$i -lt 14;$i++){{ if(-not (Get-NetAdapter -Name Meta -ErrorAction SilentlyContinue)){{ $gone=$true; break }}; Start-Sleep -Milliseconds 500 }}
if(-not $gone){{ throw 'TUN(Meta) 未在超时内优雅拆除——拒绝强杀以避免 wintun 路由残留；防火墙保持生效（请重试或本地排查）' }}
{kill}
'OK'
"#
    ))?;
    if controller_present() {
        bail!(
            "mihomo 进程未能结束（controller 仍在响应）——请确认进程名 {fallback_name}，或手动结束进程"
        );
    }
    Ok(())
}

/// 9090 上是否有**任何** external-controller 在响应（不校验 secret）。
pub fn controller_present() -> bool {
    let addr: SocketAddr = match CONTROLLER.parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let timeout = Duration::from_millis(900);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    let req = format!("GET /version HTTP/1.1\r\nHost: {CONTROLLER}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 32];
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => std::str::from_utf8(&buf[..n])
            .map(|s| s.starts_with("HTTP/"))
            .unwrap_or(false),
        _ => false,
    }
}

/// mihomo 是否在运行（查外部控制器，带鉴权）。
pub fn running(secret: &str) -> bool {
    controller_get_ok("/version", secret).unwrap_or_else(|_| {
        let h = auth_header(secret);
        run_ps(&format!(
            "$h={h}; try{{ Invoke-RestMethod 'http://{CONTROLLER}/version' -Headers $h -TimeoutSec 3 | Out-Null; 'yes' }}catch{{ 'no' }}"
        ))
        .map(|s| s.trim() == "yes")
        .unwrap_or(false)
    })
}

/// mihomo TUN（Meta 适配器）是否已起栈并 Up。
pub fn tun_up() -> bool {
    crate::win::adapter_up("Meta").unwrap_or_else(|_| {
        run_ps(
            "try{ if((Get-NetAdapter -Name 'Meta' -ErrorAction Stop).Status -eq 'Up'){'yes'}else{'no'} }catch{ 'no' }",
        )
        .map(|s| s.trim() == "yes")
        .unwrap_or(false)
    })
}

fn controller_get_ok(path: &str, secret: &str) -> Result<bool> {
    let addr: SocketAddr = CONTROLLER.parse().context("parse mihomo controller addr")?;
    let timeout = Duration::from_millis(900);
    let mut stream =
        TcpStream::connect_timeout(&addr, timeout).context("connect mihomo controller")?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let auth = if secret.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {secret}\r\n")
    };
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: {CONTROLLER}\r\n{auth}Connection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .context("write mihomo controller request")?;

    let mut buf = [0u8; 128];
    let n = stream
        .read(&mut buf)
        .context("read mihomo controller response")?;
    let head = std::str::from_utf8(&buf[..n]).unwrap_or_default();
    Ok(head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200"))
}
