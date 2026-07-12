//! mihomo 引擎编排：从规则生成 mihomo 配置 + 进程生命周期。
//!
//! 落地的验证结论（docs/net-policy-validation-report.md）：
//! - TUN(gvisor) + WG userspace outbound，§9#1 实测无路由环，无需手动 route-exclude host route。
//! - `strict-route: true` + `dns-hijack: any:53`（§0.6 拦系统 DNS，含显式 8.8.8.8）。
//! - DNS 模式 A：上游 nameserver 走物理 bootstrap（§0.8.1），kill-switch 放行其 IP；
//!   零泄漏的 `respect-rules`（模式 B）未实测，默认不启用。
//! - 停 mihomo 必须优雅：先 API 关 TUN 再结束进程（§0.8.2bis，避免 wintun 路由残留断网）。

use super::config::{mihomo_config_path, net_policy_dir, NetPolicySettings, RuleSet};
use super::win::run_ps;
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// mihomo 外部控制器端口（loopback）。
pub const CONTROLLER: &str = "127.0.0.1:9090";

/// 解析 mihomo 可执行文件路径：优先 `MIHOMO_BIN`，否则 workspace 的 `net-policy/` 下（一层，
/// 该目录开机由 `ensure_workspace` 自动建好，直接把 exe 放进去即可）。
pub fn mihomo_bin(workspace: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("MIHOMO_BIN") {
        return PathBuf::from(p);
    }
    net_policy_dir(workspace).join("mihomo-windows-amd64.exe")
}

/// 生成随机 controller secret（hex，P0-1）。
///
/// **威胁模型写实**：secret 明文写在 `generated/config.yaml`（重启恢复路径也依赖读它），
/// 同用户任何有文件读取能力的进程都拿得到——它防的只是「不知道 workspace 路径、只会盲打
/// 127.0.0.1:9090 的进程」，**不是**同用户强隔离边界（WG 私钥同样明文落盘，见 config.rs
/// 头注释）。要强边界得迁 Windows Credential Manager / DPAPI，首版明确不做。
pub fn gen_secret() -> String {
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// 构造带 Authorization 头的 PS hashtable 片段（secret 空则无头）。
fn auth_header(secret: &str) -> String {
    if secret.is_empty() {
        "@{}".to_string()
    } else {
        format!("@{{ Authorization = 'Bearer {secret}' }}")
    }
}

/// 从规则集 + 设置生成 mihomo `config.yaml` 文本。`secret` 是 external-controller 鉴权口令。
///
/// **与 SBN 解耦**：仅当 WG 配置合法时才输出 `wg-out` proxy；否则 `proxies: []`（纯黑洞/直连
/// 也能跑，TUN 照常起）。默认出口（兜底 MATCH）由 `settings.default_route` 决定（黑洞或海外）。
pub fn generate_config(settings: &NetPolicySettings, rules: &RuleSet, secret: &str) -> String {
    let wg = &settings.wg;
    let dns_bootstrap = settings
        .dns_bootstrap
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let lan_exclude = settings
        .lan_ranges
        .iter()
        .map(|s| format!("    - {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    let rule_lines = rules.to_mihomo_rules(settings.default_route).join("\n");
    let ipv6 = !settings.block_ipv6; // block_ipv6=true → mihomo ipv6:false

    // WG 已配 → 输出 wg-out outbound；未配 → 空 proxies（黑洞/直连仍可运行，不强制 SBN）。
    let proxies = if wg.validate().is_ok() {
        format!(
            "\n  - name: wg-out\n    type: wireguard\n    server: {server}\n    port: {port}\n    ip: {ip}\n    private-key: {priv}\n    public-key: {pubk}\n    pre-shared-key: {psk}\n    udp: true\n    mtu: {mtu}\n    remote-dns-resolve: false",
            server = wg.server,
            port = wg.port,
            ip = wg.ip,
            priv = wg.private_key,
            pubk = wg.public_key,
            psk = wg.pre_shared_key,
            mtu = wg.mtu,
        )
    } else {
        " []".to_string()
    };

    format!(
        r#"# 由 zero-desktop net_policy 模块生成，请勿手改（改规则用 UI / net_policy_apply）。
mixed-port: 7890
allow-lan: false
mode: rule
log-level: info
ipv6: {ipv6}
find-process-mode: always
external-controller: {CONTROLLER}
secret: "{secret}"

dns:
  enable: true
  listen: 127.0.0.1:1053
  ipv6: {ipv6}
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-filter:
    - '*.lan'
    - '*.local'
    - '+.msftconnecttest.com'
    - '+.msftncsi.com'
  # DNS 模式 A：上游 bootstrap 走物理（§0.8.1，kill-switch 放行其 IP）。
  default-nameserver: [{dns_bootstrap}]
  nameserver: [{dns_bootstrap}]
  direct-nameserver: [{dns_bootstrap}]
  fallback: []

tun:
  enable: true
  stack: gvisor
  dns-hijack:
    - any:53
    - tcp://any:53
  auto-route: true
  auto-detect-interface: true
  strict-route: true
  route-exclude-address:
{lan_exclude}

proxies:{proxies}

proxy-groups: []

rules:
{rule_lines}
"#,
    )
}

/// 写出生成的 mihomo 配置文件，返回其路径。
pub fn write_config(
    workspace: &Path,
    settings: &NetPolicySettings,
    rules: &RuleSet,
    secret: &str,
) -> Result<PathBuf> {
    let path = mihomo_config_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, generate_config(settings, rules, secret))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// 启动 mihomo（以管理员/当前进程上下文 spawn 子进程，配置须已写）。返回 pid。
pub fn start(workspace: &Path) -> Result<u32> {
    let bin = mihomo_bin(workspace);
    if !bin.exists() {
        bail!(
            "mihomo 可执行文件不存在：{}（设 MIHOMO_BIN 或放到 workspace 的 net-policy/ 下）",
            bin.display()
        );
    }
    // `-d` 家目录（缓存/wintun）+ 显式 `-f` 指向我们生成的配置（在 generated/ 下，
    // 不能只给 -d，否则 mihomo 会去 <dir>/config.yaml 找不到 → 跑默认配置 → 外部控制器/ TUN 都不对）。
    let dir = net_policy_dir(workspace);
    let cfg = mihomo_config_path(workspace);
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("-d").arg(&dir).arg("-f").arg(&cfg);
    crate::shared::proc::hide_console(&mut cmd); // 不弹控制台窗口
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn mihomo: {}", bin.display()))?;
    Ok(child.id())
}

/// 热重载配置（逐项放行不重启隧道）：重写后的 `config.yaml` 经 mihomo `PUT /configs` 原地生效，
/// TUN(Meta) 与现有连接不中断。调用方须**先** `write_config` 再调本函数。tun 段不变 → mihomo 软重载，
/// 不重建适配器。失败（控制器不可达 / 配置非法）即 bail，由调用方提示。
pub fn reload(workspace: &Path, secret: &str) -> Result<()> {
    let h = auth_header(secret);
    let cfg = mihomo_config_path(workspace);
    // JSON 字符串里的 Windows 反斜杠需转义为 \\。
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

/// 优雅停 mihomo（§0.8.2bis + 审查 P1-1）：先 API 关 TUN，**轮询确认 Meta 适配器已拆除**
/// 再按 pid 结束进程。若 TUN 未在超时内拆除则 **bail（不强杀）**，让调用方保持防火墙生效、
/// 不进入"强杀残留路由"的泄漏/断网路径。
///
/// pid 未知（应用重启后接管的孤儿）时回退按进程名停——`fallback_name` 由调用方从实际
/// `mihomo_bin()` 的文件名推导（`MIHOMO_BIN` 可指向自定义文件名，硬编码会杀不到）。按名
/// 回退命不中时 `Get-Process | Stop-Process` 是 no-op 且照样输出 OK，故杀完**再验证控制器
/// 已不可达**，否则报错——避免"假成功 → 撤了防火墙/清了 rt，进程和 9090 端口却还在"。
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
    // 杀后验证：控制器仍在响应说明进程没死（按名回退没命中 / pid 复用等），不能当成功。
    if controller_present() {
        bail!(
            "mihomo 进程未能结束（controller 仍在响应）——若设置了 MIHOMO_BIN，请确认进程名 {fallback_name} 与之一致，或手动结束进程"
        );
    }
    Ok(())
}

/// 9090 上是否有**任何** external-controller 在响应（不校验 secret，401 也算「在」）。
/// 用于 apply 前的端口预检：区分「端口空闲可起新实例」与「已被其他 clash/mihomo 或失联孤儿
/// 占用」——后者直接起新实例只会抢端口失败且报错误导。
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
        // 任何 HTTP 响应（200/401/404…）都说明有 controller 在听。
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

/// mihomo TUN（Meta 适配器）是否已起栈并 Up（P2：区分"controller 活着但 TUN 没起/隧道未连通"
/// 与"真正受保护"）。controller 可达 ≠ TUN 起栈（起栈可能失败而进程/控制器仍在）。
pub fn tun_up() -> bool {
    super::win::adapter_up("Meta").unwrap_or_else(|_| {
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
