//! 跨进程共享的状态 / 快照 / 报告类型。
//!
//! 这些是控制面响应体（protocol.rs 的 `Response`）的载荷。原 zero-desktop 里多数只派生
//! `Serialize`（Tauri command 只出不进）；这里统一 `Serialize + Deserialize`，因为
//! **client 需要反序列化 agent 的响应**（设计文档 §4）。

use crate::config::{ProcessRef, Route};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 防火墙 kill-switch 当前状态。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FirewallStatus {
    pub default_outbound: String,
    pub rule_count: u32,
    pub active: bool,
}

/// 状态快照（驱动全景图 + 保护横幅）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetPolicyStatus {
    pub platform_supported: bool,
    pub wg_configured: bool,
    pub killswitch_enabled: bool,
    pub applied: bool,
    pub mihomo_running: bool,
    /// mihomo TUN（Meta 适配器）是否已起栈并 Up。controller 可达但 TUN 未起栈时为 false。
    pub tun_ready: bool,
    /// 是否处于"受保护"状态：kill-switch 启用 + 防火墙默认出站已 Block + mihomo 在跑且 TUN 已起栈。
    pub protected: bool,
    /// 当前防火墙白名单模型是否已真机验证。false=实验保护，不能宣称 fail-closed。
    pub protection_validated: bool,
    pub firewall: Option<FirewallStatus>,
    /// 默认出口（未命中规则的兜底）。
    pub default_route: Route,
    /// 主开关：是否已启用「启动即生效」（持久化在 settings.enabled）。
    pub enabled: bool,
    /// 守护是否以管理员身份运行。
    pub elevated: bool,
    /// 记录库是否已降级为内存（磁盘打开失败，历史将在重启后丢失）。
    #[serde(default)]
    pub record_store_degraded: bool,
}

/// 单条活跃连接（取 UI 驱动全景图所需的最小集）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    /// mihomo 连接 ID（uuid）。观察器按它对「命中次数」去重。
    pub id: String,
    /// 出口链（mihomo `chains`）。第 0 个是最终出口。
    pub chains: Vec<String>,
    /// 命中的出口（`chains` 末项归一化）。
    pub outbound: String,
    /// 目标主机名（fake-ip 场景下为真实域名）。
    pub host: String,
    /// 目标 IP。
    pub destination_ip: String,
    /// 目标端口。
    pub destination_port: String,
    /// 发起进程名。
    pub process: String,
    /// 发起进程完整路径（mihomo `processPath`，可能为空）。
    #[serde(default)]
    pub process_path: String,
    /// 命中的 mihomo 规则。
    pub rule: String,
    /// 网络类型（tcp/udp）。
    pub network: String,
}

/// 活跃连接快照 + 按出口聚合。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionsSnapshot {
    pub available: bool,
    pub total: usize,
    pub wg_count: usize,
    pub direct_count: usize,
    pub other_count: usize,
    pub by_process: BTreeMap<String, usize>,
    pub connections: Vec<Connection>,
}

impl ConnectionsSnapshot {
    /// 空快照（mihomo 未跑 / secret 缺失 / 控制器不可达时返回）。
    pub fn empty() -> Self {
        Self {
            available: false,
            total: 0,
            wg_count: 0,
            direct_count: 0,
            other_count: 0,
            by_process: BTreeMap::new(),
            connections: Vec::new(),
        }
    }
}

/// 进程候选（供 UI 选作直连程序组）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessCandidate {
    pub pid: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub remotes: Vec<String>,
}

/// 一条被阻断尝试（按 network|host|port 去重）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockedEntry {
    pub network: String,
    pub host: String,
    pub dest_ip: String,
    pub dest_port: String,
    pub rule: String,
    pub outbound: String,
    pub count: u64,
    pub last_ms: u64,
}

/// 域名↔IP/进程 关联聚合的一行。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainAssoc {
    pub domain: String,
    pub ips: Vec<String>,
    pub processes: Vec<String>,
    pub count: u64,
    pub last_ms: u64,
}

/// 出口/泄漏验证的单个用例。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyCase {
    pub id: String,
    pub name: String,
    /// passed / failed / unknown
    pub status: String,
    pub observed: String,
}

/// 验证报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub mihomo_running: bool,
    pub cases: Vec<VerifyCase>,
}

/// repair 结果分级（设计 §7：不把所有情况都包装成"已回基线"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairKind {
    /// 有可信快照 → 精确恢复原 Profile。
    RepairedExactly,
    /// 无快照 → 只清本产品规则，Profile 未动（安全默认）。
    RemovedOwnedRulesOnly,
    /// 无快照且当前仍 Block → 拒绝猜测（报危险态待用户决断）。
    BaselineUnknown,
    /// 用户显式确认后的最后手段，强设 NotConfigured。
    ForcedNotConfigured,
}

/// repair 结果（在线/离线共用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairResult {
    pub kind: RepairKind,
    pub message: String,
    pub had_snapshot: bool,
    pub outbound_before: String,
}

/// 一条持久化的进程请求记录（`requests` 表一行；连接观测的历史留痕）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogEntry {
    pub ts_ms: u64,
    /// mihomo 连接 ID（去重键）。
    pub conn_id: String,
    pub process: String,
    #[serde(default)]
    pub process_path: String,
    pub host: String,
    pub dest_ip: String,
    pub dest_port: String,
    pub network: String,
    /// 命中出口（wg-out/DIRECT/REJECT…）。
    pub outbound: String,
    pub rule: String,
}

/// 生命周期事件（`events` 表一行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEvent {
    pub ts_ms: u64,
    /// agent_start / agent_stop / policy_applied / policy_stopped / temp_direct_on / temp_direct_off …
    pub kind: String,
    #[serde(default)]
    pub detail: String,
}

/// 进程树节点（`GetProcessTree` 返回；children 递归）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessNode {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub children: Vec<ProcessNode>,
}

/// 一条生效路由（含**优先级** = mihomo 匹配顺序；首个命中生效）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    /// 优先级 = 在 mihomo 规则列表中的位置（0 起，越小越先匹配）。
    pub priority: usize,
    /// 匹配维度：process-path / process-name / domain-suffix / ip-cidr / match。
    pub kind: String,
    /// 匹配值（`match` 兜底为空）。
    pub value: String,
    /// 命中后出口。
    pub route: Route,
    /// 来源：builtin_lan / temp_except / group / rule / default。
    pub source: String,
    /// 是否可删（用户规则可删；内置 LAN / 兜底 MATCH / temp 不可直接删）。
    pub deletable: bool,
}

/// 临时直连（限时应急）当前状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempDirectStatus {
    pub active: bool,
    /// 到期时间（epoch ms）；None = 未激活。
    pub until_ms: Option<u64>,
    /// 剩余秒数（未激活为 0）。
    pub remaining_secs: u64,
    /// 例外进程（这些**不**走临时直连，被强制 Blackhole 以防泄漏）。
    pub except: Vec<ProcessRef>,
}
