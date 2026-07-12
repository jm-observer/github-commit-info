//! Windows 防火墙 kill-switch（§0.4/§0.9 验证过的"默认 Block + 白名单"模型）。
//!
//! 模型：`Set-NetFirewallProfile -DefaultOutboundAction Block` + 纯 Allow 白名单
//! （v1 的「Allow + Block 兜底」在 New-NetFirewallRule 层不工作，§0.2②实测 Block 永远赢 Allow）。
//!
//! 白名单（v2.2 重设计，修复审查 P1-1：原 RemoteAddress 白名单使域名/程序 DIRECT 在
//! kill-switch 下被拦死）：
//! - **R-mihomo（Program=mihomo.exe）**：放行 mihomo 进程出物理网卡的全部流量
//!   —— 覆盖 WG 握手、上游 DNS（§0.8.1）、**以及 DIRECT 命中后 mihomo 替程序/域名拨号**。
//!   fail-closed 不破：mihomo 崩溃 → 进程没了 → 此规则不匹配任何流量 → 物理全 Block。
//! - R-TUN(InterfaceAlias=Meta)：应用流量进 TUN。
//! - R-LO(127.0.0.0/8) / R-LAN(物理 NIC 的 LAN 段，非 mihomo 的本机互访)。
//! - R-IPv6Block（block_ipv6 时）：显式 Block 2000::/3，真正阻断 IPv6 公网（修 P2-1）。
//!
//! 移除（-Remove）按状态文件还原原 DefaultOutboundAction（实测原值 NotConfigured，不可盲设 Allow）。

use super::config::{killswitch_state_path, NetPolicySettings, RuleSet};
use super::valid;
use super::win::run_ps;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

const GROUP: &str = "NetPolicy-KillSwitch";
/// mihomo TUN 适配器名（gvisor wintun，固定 "Meta"）。
const TUN_ALIAS: &str = "Meta";

#[derive(Clone, Debug, Serialize)]
pub struct FirewallStatus {
    pub default_outbound: String,
    pub rule_count: u32,
    pub active: bool,
}

fn ps_squote(s: &str) -> String {
    s.replace('\'', "''")
}

/// 阶段 A（审查 P1-2）：**在启动 mihomo 之前** 建立 fail-closed——快照 + 不依赖 Meta 的白名单
/// （KS-mihomo / KS-LO / KS-LAN / KS-IPv6Block）+ 设默认 Block。这样 mihomo 起栈期间已有兜底，
/// 消除"先起 mihomo 再补防火墙"的无保护窗口。
pub fn apply_base(workspace: &Path, settings: &NetPolicySettings, mihomo_bin: &Path) -> Result<()> {
    run_ps(&build_base_script(workspace, settings, mihomo_bin)?)?;
    Ok(())
}

/// 阶段 B：mihomo 起栈、Meta 适配器出现后，补 KS-TUN（放行应用流量进 TUN）。
/// 此前 app→Meta 被默认 Block 拦（不泄漏，仅短暂不通），符合 fail-closed。
pub fn apply_tun(_workspace: &Path) -> Result<()> {
    run_ps(&format!(
        "New-NetFirewallRule -Group '{GROUP}' -Name 'KS-TUN' -DisplayName 'KS TUN' -Direction Outbound -Action Allow -InterfaceAlias '{TUN_ALIAS}' -Enabled True | Out-Null; 'OK'"
    ))?;
    Ok(())
}

fn base_rules_ps(settings: &NetPolicySettings, mihomo_bin: &Path) -> String {
    let lan = settings.lan_ranges.join(",");
    let mihomo = ps_squote(&mihomo_bin.to_string_lossy());
    let mut s = String::new();
    // R-mihomo：放行 mihomo 进程出物理网卡（覆盖 WG 握手 / 上游 DNS / DIRECT 拨号）。
    s.push_str(&format!(
        "New-NetFirewallRule -Group $G -Name 'KS-mihomo' -DisplayName 'KS mihomo egress' -Direction Outbound -Action Allow -Program '{mihomo}' -InterfaceAlias $eth -Enabled True | Out-Null\n"
    ));
    s.push_str(
        "New-NetFirewallRule -Group $G -Name 'KS-LO' -DisplayName 'KS Loopback v4' -Direction Outbound -Action Allow -RemoteAddress 127.0.0.0/8 -Enabled True | Out-Null\n",
    );
    s.push_str(&format!(
        "New-NetFirewallRule -Group $G -Name 'KS-LAN' -DisplayName 'KS LAN' -Direction Outbound -Action Allow -RemoteAddress {lan} -InterfaceAlias $eth -Enabled True | Out-Null\n"
    ));
    if settings.block_ipv6 {
        s.push_str(
            "New-NetFirewallRule -Group $G -Name 'KS-IPv6Block' -DisplayName 'KS block IPv6 public' -Direction Outbound -Action Block -RemoteAddress 2000::/3 -Enabled True | Out-Null\n",
        );
    }
    s
}

fn validate_fw_inputs(settings: &NetPolicySettings) -> Result<()> {
    // kill-switch 白名单不直接用 WG server IP（放行的是 mihomo.exe 整进程），故 WG 留空（纯黑洞、
    // 不依赖 SBN）时不应卡防火墙——仅当填了 server 才校验其为合法 IP。
    if !settings.wg.server.trim().is_empty() {
        valid::ip(&settings.wg.server)?;
    }
    for l in &settings.lan_ranges {
        valid::ip_or_cidr(l)?;
    }
    Ok(())
}

/// 构造阶段 A 脚本（快照 + base 白名单 + Set Block，不含 KS-TUN）。
pub fn build_base_script(
    workspace: &Path,
    settings: &NetPolicySettings,
    mihomo_bin: &Path,
) -> Result<String> {
    validate_fw_inputs(settings)?;
    let state = killswitch_state_path(workspace);
    let state_s = ps_squote(&state.to_string_lossy());
    let state_dir = state
        .parent()
        .map(|p| ps_squote(&p.to_string_lossy()))
        .unwrap_or_default();
    let rules_ps = base_rules_ps(settings, mihomo_bin);
    Ok(format!(
        r#"$G='{GROUP}'
$state='{state_s}'
if(-not (Test-Path $state)){{
  $snap=[ordered]@{{}}
  foreach($p in 'Domain','Private','Public'){{ $snap[$p]=(Get-NetFirewallProfile -Profile $p).DefaultOutboundAction.ToString() }}
  New-Item -ItemType Directory -Path '{state_dir}' -Force | Out-Null
  $snap | ConvertTo-Json | Set-Content -Path $state -Encoding UTF8
}}
$eth=@(Get-NetAdapter -Physical | Where-Object {{ $_.Status -eq 'Up' }} | Select-Object -ExpandProperty Name)
if($eth.Count -eq 0){{ throw '没有处于 Up 的物理网卡' }}
Get-NetFirewallRule -Group $G -ErrorAction SilentlyContinue | Remove-NetFirewallRule
{rules_ps}Set-NetFirewallProfile -Profile Domain,Private,Public -DefaultOutboundAction Block
'OK'
"#
    ))
}

/// CLI 预览用：完整脚本（base + KS-TUN，展示全部规则）。实际 apply 走 apply_base + apply_tun 两阶段。
pub fn build_apply_script(
    workspace: &Path,
    settings: &NetPolicySettings,
    _rules: &RuleSet,
    mihomo_bin: &Path,
) -> Result<String> {
    let base = build_base_script(workspace, settings, mihomo_bin)?;
    // 在 Set Block 前插入 KS-TUN（仅为预览完整性；真实流程 KS-TUN 在 mihomo 起栈后补）。
    let tun = format!(
        "New-NetFirewallRule -Group $G -Name 'KS-TUN' -DisplayName 'KS TUN' -Direction Outbound -Action Allow -InterfaceAlias '{TUN_ALIAS}' -Enabled True | Out-Null\n"
    );
    Ok(base.replace(
        "Set-NetFirewallProfile",
        &format!("{tun}Set-NetFirewallProfile"),
    ))
}

/// 移除 kill-switch：删白名单 + 按状态文件还原 DefaultOutboundAction。
pub fn remove(workspace: &Path) -> Result<()> {
    let state = killswitch_state_path(workspace);
    let state_s = ps_squote(&state.to_string_lossy());
    let script = format!(
        r#"$G='{GROUP}'
$state='{state_s}'
Get-NetFirewallRule -Group $G -ErrorAction SilentlyContinue | Remove-NetFirewallRule
if(Test-Path $state){{
  $s=Get-Content $state -Raw | ConvertFrom-Json
  foreach($p in 'Domain','Private','Public'){{ if($s.$p){{ Set-NetFirewallProfile -Profile $p -DefaultOutboundAction $s.$p }} }}
  Remove-Item $state -Force
}}else{{
  Set-NetFirewallProfile -Profile Domain,Private,Public -DefaultOutboundAction NotConfigured
}}
'OK'
"#
    );
    run_ps(&script)?;
    Ok(())
}

/// 查询 kill-switch 当前状态。
pub fn status() -> Result<FirewallStatus> {
    native_status().or_else(|_| powershell_status())
}

fn native_status() -> Result<FirewallStatus> {
    let default_outbound = super::win::firewall_default_outbound_domain()?;
    let rule_count = super::win::firewall_rule_group_count(GROUP)?;
    Ok(FirewallStatus {
        active: default_outbound.eq_ignore_ascii_case("Block"),
        default_outbound,
        rule_count,
    })
}

fn powershell_status() -> Result<FirewallStatus> {
    let out = run_ps(&format!(
        r#"$o=(Get-NetFirewallProfile -Profile Domain).DefaultOutboundAction
$c=(Get-NetFirewallRule -Group '{GROUP}' -ErrorAction SilentlyContinue | Measure-Object).Count
"$o|$c"
"#
    ))?;
    let line = out.trim();
    let mut parts = line.split('|');
    let default_outbound = parts.next().unwrap_or("Unknown").trim().to_string();
    let rule_count: u32 = parts
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    Ok(FirewallStatus {
        active: default_outbound.eq_ignore_ascii_case("Block"),
        default_outbound,
        rule_count,
    })
}
