//! 防火墙残留修复（设计 §7 两级救援 + repair 四态分级）。
//!
//! `graded_repair` 是**在线/离线共用**的核心：只做最小、固定的恢复——不读复杂业务配置、不启动
//! mihomo、只删本产品防火墙规则、按已保存快照恢复原 Profile、幂等。结果分四态，不把所有情况都包装
//! 成"已回基线"：
//! - `repaired_exactly`：有可信快照 → 精确恢复原 Profile。
//! - `removed_owned_rules_only`：无快照且当前非 Block → 只清本产品规则，Profile 未动（安全默认）。
//! - `baseline_unknown`：无快照且当前仍 Block → 拒绝猜测，报危险态待用户决断。
//! - `forced_not_configured`：用户 `--force` 显式确认的最后手段，强设 NotConfigured。
//!
//! 两级救援：在线走 agent 管道（`Request::Repair`）；agent 连不上时走**离线提权入口**
//! `net-policy-agent repair-offline`（弹 UAC）。

use crate::firewall;
use crate::win::{firewall_default_outbound_domain, is_elevated, is_windows, run_ps};
use anyhow::{bail, Result};
use net_policy_core::types::{RepairKind, RepairResult};
use std::path::Path;

/// 分级修复核心（无提权检查、无打印；调用方决定语境）。
pub fn graded_repair(workspace: &Path, force: bool) -> Result<RepairResult> {
    let had_snapshot = firewall::snapshot_exists(workspace);
    let outbound_before = firewall_default_outbound_domain().unwrap_or_else(|_| "Unknown".into());

    let (kind, message) = if had_snapshot {
        firewall::remove(workspace)?;
        (
            RepairKind::RepairedExactly,
            "已按保存的快照精确恢复原防火墙 Profile 并清除本产品规则".to_string(),
        )
    } else if force {
        firewall::remove_owned_rules_only()?;
        run_ps("Set-NetFirewallProfile -Profile Domain,Private,Public -DefaultOutboundAction NotConfigured; 'OK'")?;
        (
            RepairKind::ForcedNotConfigured,
            "无可信快照；已按 --force 强设 DefaultOutboundAction=NotConfigured 并清本产品规则"
                .to_string(),
        )
    } else {
        firewall::remove_owned_rules_only()?;
        if outbound_before.eq_ignore_ascii_case("Block") {
            (
                RepairKind::BaselineUnknown,
                "已清除本产品规则，但**无可信快照且当前 DefaultOutboundAction 仍为 Block**——拒绝猜测原状态。网络可能仍受阻，确认要放开请加 --force".to_string(),
            )
        } else {
            (
                RepairKind::RemovedOwnedRulesOnly,
                format!("已清除本产品规则；Profile 未改（当前 DefaultOutboundAction={outbound_before}）"),
            )
        }
    };

    Ok(RepairResult {
        kind,
        message,
        had_snapshot,
        outbound_before,
    })
}

/// 离线提权救援入口（弹 UAC 后调用）：提权检查 + graded_repair + 打印紧凑 JSON。
pub fn repair_offline(workspace: &Path, force: bool) -> Result<()> {
    if !is_windows() {
        bail!("仅支持 Windows");
    }
    if !is_elevated() {
        bail!(
            "离线救援需要管理员权限：请以管理员身份运行（或用安装时生成的「恢复网络」快捷方式）。"
        );
    }
    let r = graded_repair(workspace, force)?;
    println!(
        "{}",
        serde_json::json!({
            "result": serde_json::to_value(r.kind).unwrap_or(serde_json::Value::Null),
            "message": r.message,
            "had_snapshot": r.had_snapshot,
            "outbound_before": r.outbound_before,
        })
    );
    Ok(())
}
