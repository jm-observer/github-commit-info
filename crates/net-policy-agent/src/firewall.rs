//! Windows 防火墙 kill-switch（§0.4/§0.9 验证过的"默认 Block + 白名单"模型）。
//!
//! 白名单：R-mihomo（Program=mihomo.exe，放行 mihomo 出物理网卡）/ R-TUN(Meta) / R-LO / R-LAN /
//! R-IPv6Block。移除按状态文件还原原 DefaultOutboundAction（原值多为 NotConfigured，不可盲设 Allow）。

use crate::win::run_ps;
use anyhow::Result;
use net_policy_core::config::{killswitch_state_path, NetPolicySettings, RuleSet};
use net_policy_core::types::FirewallStatus;
use net_policy_core::valid;
use std::path::Path;

const GROUP: &str = "NetPolicy-KillSwitch";
/// mihomo TUN 适配器名（gvisor wintun，固定 "Meta"）。
const TUN_ALIAS: &str = "Meta";

fn ps_squote(s: &str) -> String {
    s.replace('\'', "''")
}

/// 阶段 A：**在启动 mihomo 之前** 建立 fail-closed——快照 + 不依赖 Meta 的白名单 + 设默认 Block。
pub fn apply_base(workspace: &Path, settings: &NetPolicySettings, mihomo_bin: &Path) -> Result<()> {
    run_ps(&build_base_script(workspace, settings, mihomo_bin)?)?;
    Ok(())
}

/// 阶段 B：mihomo 起栈、Meta 出现后，补 KS-TUN（放行应用流量进 TUN）。
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

/// CLI 预览用：完整脚本（base + KS-TUN）。真实 apply 走 apply_base + apply_tun 两阶段。
#[allow(dead_code)]
pub fn build_apply_script(
    workspace: &Path,
    settings: &NetPolicySettings,
    _rules: &RuleSet,
    mihomo_bin: &Path,
) -> Result<String> {
    let base = build_base_script(workspace, settings, mihomo_bin)?;
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
}}
# 无快照：只删本产品规则，**不动 Profile**（评审点 6：不擅自设 NotConfigured，避免覆盖用户原有 Block 策略；
# 需强设 NotConfigured 走 repair --force）。
'OK'
"#
    );
    run_ps(&script)?;
    Ok(())
}

/// 只删本产品的规则、**不动 Profile**（repair 的 `removed_owned_rules_only` 用；无可信快照时的安全默认）。
pub fn remove_owned_rules_only() -> Result<()> {
    run_ps(&format!(
        "Get-NetFirewallRule -Group '{GROUP}' -ErrorAction SilentlyContinue | Remove-NetFirewallRule; 'OK'"
    ))?;
    Ok(())
}

/// 是否存在可信的 Profile 快照（repair 分级判定用）。
pub fn snapshot_exists(workspace: &Path) -> bool {
    killswitch_state_path(workspace).exists()
}

/// 查询 kill-switch 当前状态。
pub fn status() -> Result<FirewallStatus> {
    native_status().or_else(|_| powershell_status())
}

fn native_status() -> Result<FirewallStatus> {
    let default_outbound = crate::win::firewall_default_outbound_domain()?;
    let rule_count = crate::win::firewall_rule_group_count(GROUP)?;
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
