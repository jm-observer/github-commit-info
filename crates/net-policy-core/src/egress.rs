//! 统一出口（Egress）模型 —— **出口生命周期与流量导流解耦**。
//!
//! 设计源：`docs/net-policy/net-policy-wg-egress-design.md`。核心约束：
//!
//! > 出口是否已启动、是否已连接，与当前是否有业务流量经过它，是两个独立问题。
//!
//! 本模块只放**纯模型**：出口身份、生命周期状态机、健康报告、由 settings/rules 派生的出口清单
//! 与占用统计、以及「停用出口如何 fail-closed 渲染」的解析器。真实的探活/重连副作用在
//! `net-policy-agent` 的 `EgressManager`（形状照抄 `CaptureManager`/`DecryptManager`）。
//!
//! ## 第一阶段做到了什么、没做到什么（**别把这两者搞混**）
//!
//! WireGuard 与代理订阅的数据面**仍由 mihomo 内置 outbound 承载**（[`EgressManagement::MihomoManaged`]），
//! 没有独立进程/接口。因此本阶段实现的准确名字是 **「独立健康状态管理」，不是「独立出口生命周期」**：
//!
//! | 设计目标 | 现状 |
//! |---|---|
//! | 出口的连接状态不由「有没有业务流量」推断 | ✅ 探活循环无视策略选中与否，持续经该出口打流量 |
//! | 切到直连后 WG 仍 Ready、不需重等握手 | ✅ 同上 |
//! | 出口不可用时不静默裸奔 | ✅ [`EgressFallback`] 默认阻断 |
//! | **agent 启动即连 WG（与 mihomo 无关）** | ❌ mihomo 没跑就没有 wg-out |
//! | **停掉 mihomo 后 WG 仍在线** | ❌ outbound 随进程消失 |
//! | **reload 规则不重建隧道** | ❌ mihomo 重载全量配置会重建 outbound |
//!
//! 打勾的三条要真正独立于引擎，必须把 WG 挪到独立引擎（amneziawg-go 等，即
//! [`EgressManagement::Independent`]），那是后续阶段。**在此之前，任何界面都不得把
//! `mihomo-managed` 出口的「已就绪」呈现成「这是个能独立存活的资源」。**
//!
//! `Stop` 一个出口会把它从生成的 mihomo 配置里摘掉，指向它的规则按 [`EgressFallback`] 处理
//! （默认阻断，**绝不静默回落直连**，设计 §6.1）。

use crate::config::{NetPolicySettings, Route, RuleSet};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// agent 声明的能力标记：支持统一出口生命周期面（`Egress*` 请求）。
pub const CAPABILITY_EGRESS_V1: &str = "egress_v1";

/// 直连出口 id（始终存在）。
pub const EGRESS_DIRECT: &str = "direct";
/// WireGuard 出口 id（第一阶段单实例；多实例见设计 §10 后续）。
pub const EGRESS_WG: &str = "wg";
/// 代理订阅出口 id（第一阶段单实例，承载「当前激活订阅」）。
pub const EGRESS_PROXY: &str = "proxy";

/// 出口类型。注意：`Blackhole` 不是出口而是**丢弃目标**，故不在此枚举内。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressKind {
    /// 物理网卡直连，无远端会话。
    Direct,
    /// WireGuard 隧道。
    WireGuard,
    /// 代理订阅（节点组）。
    Proxy,
}

impl EgressKind {
    /// 该类出口是否适合**周期性**主动探测。
    ///
    /// - `WireGuard`：是。隧道可保活，且 mihomo 的 userspace WG 懒握手，不定期探就会被误判掉线
    ///   （决议 §3.3 认可探活循环的这个作用）。
    /// - `Direct`：是。探的是本机物理网络，不建立远端会话，成本可忽略。
    /// - `Proxy`：**否**。代理订阅不是一条持续隧道而是「节点配置来源」，决议 §3.4 明确
    ///   「没有业务流量时，不应为了展示在线而强行维持每个代理节点的连接」。它只在用户显式
    ///   探测 / 切换节点 / 启动出口时探一次。
    pub fn probe_on_schedule(self) -> bool {
        !matches!(self, EgressKind::Proxy)
    }
}

/// 出口生命周期状态（决议 §5 的七态）。**与「是否被策略选中」无关**。
///
/// 决议 §5 要求状态含义按出口类型解释：Direct 通常只在 `Ready` / 网络不可用之间；WG 的
/// `Ready` 表示 mihomo outbound 已加载且最近主动探测成功，**不代表独立网卡握手状态**；
/// Proxy 的 `Ready` 表示订阅与当前节点可用，**不代表节点存在持续连接**。
///
/// 没有 `Stopping` 态：本阶段停止一个出口只是翻一个意图标志 + 重生成配置，是瞬时的，
/// 没有需要向用户呈现的中间态（决议 §5 的状态清单同样只列七个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressLifecycle {
    /// 用户显式停用，或尚未启动。不接收新流量。
    #[default]
    Stopped,
    /// 正在拉起（配置渲染 / 引擎重载中）。
    Starting,
    /// 已拉起，正在建立远端会话（WG 握手 / 代理首次探测）。
    Connecting,
    /// 已可承载业务流量（**不只是进程存在或配置解析成功**）。
    Ready,
    /// 可用但质量下降（探测超时一次、延迟异常）。
    Degraded,
    /// 曾经 Ready，检测到断线，正在重连。
    Reconnecting,
    /// 连续失败，已放弃自动恢复，需人工介入。
    Failed,
}

impl EgressLifecycle {
    /// 该状态下是否允许承载业务流量（设计 §7 的行为表）。
    pub fn accepts_traffic(self) -> bool {
        matches!(self, EgressLifecycle::Ready | EgressLifecycle::Degraded)
    }

    /// 是否属于「用户/系统已把它关掉」，与「连不上」区分开。
    pub fn is_stopped(self) -> bool {
        matches!(self, EgressLifecycle::Stopped)
    }

    /// 稳定的展示串（GUI 直接用，避免各端各写一份映射）。
    pub fn label(self) -> &'static str {
        match self {
            EgressLifecycle::Stopped => "已停止",
            EgressLifecycle::Starting => "启动中",
            EgressLifecycle::Connecting => "连接中",
            EgressLifecycle::Ready => "已就绪",
            EgressLifecycle::Degraded => "降级",
            EgressLifecycle::Reconnecting => "重连中",
            EgressLifecycle::Failed => "失败",
        }
    }
}

/// 最近一次主动探测的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    /// 尚未探测过。
    #[default]
    Unknown,
    Healthy,
    /// 通了但慢/抖动。
    Degraded,
    Unhealthy,
}

/// 一次主动探测的结果（设计 §8.2「健康状态」列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HealthReport {
    pub state: HealthState,
    /// 探测完成时刻（epoch 毫秒）；`0` = 从未探测。
    #[serde(default)]
    pub checked_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    /// 探测目标（脱敏后的 URL / 主机），供 UI 展示「探测目标、延迟和结果」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// **出口数据面归谁所有** —— 决定「停掉 mihomo 后这个出口还在不在」。
///
/// 这是第一阶段与设计目标之间**最重要的落差**，必须一路透传到 UI，否则就是产品语义欺骗：
/// 用户看到「出口：已就绪」会以为它是个能独立存活的资源，而实际上停掉引擎它就消失了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EgressManagement {
    /// **数据面由 mihomo 承载**：outbound 随 mihomo 进程存亡，reload 会重建它。
    /// 本管理器拥有的只是**健康状态与探活**，不是隧道本身的生命周期。
    MihomoManaged,
    /// 由系统承载（物理网卡 / 系统路由），无远端会话，不依赖任何引擎进程。
    System,
    /// 真正独立的受管出口（自有进程/接口，引擎停了它照活）。**第一阶段尚无此类出口**，
    /// 留作后续把 WG 挪到独立引擎（amneziawg-go 等）时使用。
    Independent,
}

impl EgressManagement {
    /// 该出口的可用性是否依赖 mihomo 进程在跑。
    pub fn depends_on_engine(self) -> bool {
        matches!(self, EgressManagement::MihomoManaged)
    }

    pub fn label(self) -> &'static str {
        match self {
            EgressManagement::MihomoManaged => "由 mihomo 承载",
            EgressManagement::System => "系统承载",
            EgressManagement::Independent => "独立进程",
        }
    }
}

/// mihomo 侧的导流目标（设计 §5.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteTargetKind {
    /// mihomo 的 outbound / proxy-group 名。
    MihomoOutbound,
    /// mihomo 内建直连。
    Direct,
    /// 丢弃（fail-closed 时的目标）。
    Reject,
}

/// 出口向路由器暴露的稳定目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteTarget {
    pub kind: RouteTargetKind,
    pub name: String,
}

/// 出口不可用（被停用 / Failed）时，指向它的规则如何处理（设计 §6.1、§8.5）。
///
/// **默认 fail-closed**：不允许隐式回落直连——那会让用户以为流量还在隧道里而实际裸奔。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EgressFallback {
    /// 阻断（`REJECT-DROP`）。
    #[default]
    Block,
    /// 用户**明确配置**允许时才回落直连。
    Direct,
}

/// 由 settings 派生的一个出口的静态描述（不含运行态）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressDescriptor {
    pub id: String,
    /// 用户可识别名称（如「直连」「家庭 WG」「海外代理」）。
    pub name: String,
    pub kind: EgressKind,
    /// 数据面归属。第一阶段 WG/Proxy 恒为 `MihomoManaged`。
    pub management: EgressManagement,
    /// 配置是否完整可用（WG 校验通过 / 订阅已激活）。false 时不应进入 Ready。
    pub configured: bool,
    /// 该出口对应的策略取值（规则里写 `route` 时用它）。
    pub route: Route,
    pub route_target: RouteTarget,
    /// 配置不完整的原因（configured=false 时给出，供 UI 直接展示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unconfigured_reason: Option<String>,
}

/// 策略对某出口的占用情况（设计 §7：`selected` 与 `lifecycle_status` 必须分开展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EgressUsage {
    /// 是否是当前默认出口（兜底 MATCH）。
    pub is_default: bool,
    /// 有多少条规则/程序组指向它。
    pub rule_count: usize,
}

impl EgressUsage {
    /// 当前是否有任何策略把流量导向它。
    pub fn selected(self) -> bool {
        self.is_default || self.rule_count > 0
    }
}

/// WireGuard 出口详情（脱敏后，设计 §8.3）。**私钥永不出现在这里**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WireGuardDetail {
    /// 脱敏 endpoint（`1.2.3.x:51820`）。
    pub endpoint: String,
    /// 隧道内本地地址。
    pub local_ip: String,
    pub mtu: u32,
    /// 是否启用 AmneziaWG 混淆。
    pub obfuscation: bool,
    /// endpoint 是否经上游代理拨号。
    #[serde(default)]
    pub via_dialer_proxy: bool,
    /// 最近一次**经该 outbound 主动探测成功**的时刻（epoch 毫秒，0=从未）。
    ///
    /// 这**不是** WireGuard 的 latest-handshake：数据面在 mihomo 进程内，是 userspace WG，
    /// 不创建 Windows 网卡也不暴露 peer 握手时间。决议 §3.3 明确禁止由探测结果推断或伪造
    /// latest-handshake / rx-tx / 网卡状态，故此处只报探测事实本身。
    #[serde(default)]
    pub last_probe_ok_at_ms: u64,
}

/// 代理订阅出口详情（脱敏；**不含订阅 URL 与凭据**）。
///
/// 决议 §3.4 要求把三层状态分开表达，别糊成一个「在线」：
///
/// | 层 | 字段 |
/// |---|---|
/// | 订阅状态（配置来源） | [`Self::subscription`] / [`Self::source`] / [`Self::refreshed_at_ms`] / [`Self::refresh_error`] / [`Self::expired`] |
/// | 节点状态（当前节点） | [`Self::node`] / [`Self::node_delay_ms`] / [`Self::node_alive`] / [`Self::node_count`] |
/// | 使用状态（是否真在跑流量） | `EgressStatus::active_connections` + `EgressStatus::usage` |
///
/// 代理订阅**不是一条持续隧道**，而是「节点配置来源」——所以这里没有、也不该有「隧道在线」
/// 之类的字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProxyDetail {
    // ── 订阅状态 ────────────────────────────────────────────────────────────
    /// 订阅显示名。
    pub subscription: String,
    /// 脱敏来源（只留 host，**丢弃 path/query/凭据**——订阅 URL 常自带 token）。
    #[serde(default)]
    pub source: String,
    /// 最近一次订阅刷新时刻（epoch 毫秒，0=未知/未刷新）。
    #[serde(default)]
    pub refreshed_at_ms: u64,
    /// 约定的刷新间隔（秒），用于判定过期与展示「下次刷新」。
    #[serde(default)]
    pub interval_secs: u64,
    /// 是否已过期（距上次刷新超过 `interval_secs`）。过期不等于不可用，只是该刷了。
    #[serde(default)]
    pub expired: bool,
    /// 最近一次刷新的失败原因（成功为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_error: Option<String>,

    // ── 节点状态 ────────────────────────────────────────────────────────────
    /// 当前选中的节点名（未知时空串）。
    #[serde(default)]
    pub node: String,
    /// 当前节点最近一次探测延迟。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_delay_ms: Option<u32>,
    /// 当前节点最近一次探测是否可用。
    #[serde(default)]
    pub node_alive: bool,
    /// 该订阅解析出的节点总数。
    #[serde(default)]
    pub node_count: usize,
}

/// 直连出口详情（决议 §6.4：展示物理网卡、默认网关、基础连通性）。
///
/// 注意这里刻意**排除 mihomo 自己的 TUN 适配器**：策略生效时默认路由会指向 `Meta`，若照搬
/// 就会把「直连出口」显示成走隧道，与它的语义完全相反。取的是最优的**物理**默认路由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DirectDetail {
    /// 物理出口网卡名（未知时空串）。
    #[serde(default)]
    pub interface: String,
    /// 默认网关（未知时空串）。
    #[serde(default)]
    pub gateway: String,
}

/// 类型相关的详情（扁平三选一，缺省全 None 也合法）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EgressDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireGuardDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct: Option<DirectDetail>,
}

/// 前端出口 DTO（设计 §8.8）。生命周期与策略选中**分两个字段**，前端不得由其一推断其二。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressStatus {
    pub id: String,
    pub name: String,
    pub kind: EgressKind,
    /// 数据面归属。`mihomo-managed` 时「已就绪」只代表**当前引擎里这条出口通**，
    /// 不代表它能脱离引擎存活——UI 必须把这点讲清楚。
    #[serde(default = "default_management")]
    pub management: EgressManagement,
    pub lifecycle: EgressLifecycle,
    /// 当前是否被策略导流（= `usage.selected()`）。
    pub selected: bool,
    pub usage: EgressUsage,
    /// 当前**实际**经该出口的活跃连接数（决议 §3.4 的「使用状态」层）。
    ///
    /// 与 [`Self::usage`] 是两回事：usage 是「策略计划把什么导给它」，这个是「此刻真的有多少
    /// 连接在走它」。规则指了但没人访问 → usage 非空而这里为 0，属正常。
    #[serde(default)]
    pub active_connections: usize,
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unconfigured_reason: Option<String>,
    pub health: HealthReport,
    #[serde(default)]
    pub detail: EgressDetail,
    pub route: Route,
    pub route_target: RouteTarget,
    /// 出口不可用时该出口上的规则的处理方式。
    pub fallback: EgressFallback,
    /// **探测驱动的**重连计数（决议 §6.2 的措辞）：主动探测从「通」跌到「不通」的次数。
    ///
    /// 它统计的是探测视角的可用性跌落，**不是**隧道真的断开重连了多少次——数据面在 mihomo 里，
    /// 我们看不到那个层面。mihomo reload 引起的 outbound 重建不计入（见 agent 侧静默窗口）。
    #[serde(default)]
    pub reconnect_count: u32,
    /// 最近一次生命周期变迁时刻（epoch 毫秒）。
    #[serde(default)]
    pub changed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

fn default_management() -> EgressManagement {
    EgressManagement::MihomoManaged
}

impl EgressStatus {
    /// 「策略选了它但它承载不了」——UI 必须显式警告的危险组合（设计 §8.4）。
    pub fn selected_but_unusable(&self) -> bool {
        self.selected && !self.lifecycle.accepts_traffic()
    }
}

/// 把 [`Route`] 映射到出口 id；`Blackhole` 不是出口，返回 `None`。
pub fn route_egress_id(route: Route) -> Option<&'static str> {
    match route {
        Route::Direct => Some(EGRESS_DIRECT),
        Route::Wg => Some(EGRESS_WG),
        Route::Proxy => Some(EGRESS_PROXY),
        Route::Blackhole => None,
    }
}

/// 出口 id → [`Route`]。未知 id 返回 `None`。
pub fn egress_id_route(id: &str) -> Option<Route> {
    match id {
        EGRESS_DIRECT => Some(Route::Direct),
        EGRESS_WG => Some(Route::Wg),
        EGRESS_PROXY => Some(Route::Proxy),
        _ => None,
    }
}

fn route_target(route: Route) -> RouteTarget {
    match route {
        Route::Direct => RouteTarget {
            kind: RouteTargetKind::Direct,
            name: route.outbound().to_string(),
        },
        Route::Blackhole => RouteTarget {
            kind: RouteTargetKind::Reject,
            name: route.outbound().to_string(),
        },
        other => RouteTarget {
            kind: RouteTargetKind::MihomoOutbound,
            name: other.outbound().to_string(),
        },
    }
}

/// 脱敏 endpoint：保留网段但抹掉末段，避免出口服务器地址随截图外泄（设计 §8.8）。
pub fn redact_host(host: &str) -> String {
    if let Some(idx) = host.rfind('.') {
        // IPv4 形态：抹末段。域名形态同样只留父域，效果一致且不会误导。
        if host[idx + 1..].chars().all(|c| c.is_ascii_digit()) {
            return format!("{}.x", &host[..idx]);
        }
    }
    if host.contains(':') {
        // IPv6：只留前两段。
        let head: Vec<&str> = host.split(':').take(2).collect();
        return format!("{}::…", head.join(":"));
    }
    host.to_string()
}

/// 由设置派生当前存在的出口清单（顺序稳定：Direct → WireGuard → Proxy）。
///
/// **未配置的出口也会出现在清单里**（`configured: false` + 原因），这样 GUI 能引导用户去配，
/// 而不是让出口凭空消失。
pub fn catalog(settings: &NetPolicySettings) -> Vec<EgressDescriptor> {
    let wg_reason = settings.wg.validate().err().map(|e| e.to_string());
    let proxy_active = settings.proxy_subscriptions.active_subscription();
    vec![
        EgressDescriptor {
            id: EGRESS_DIRECT.into(),
            name: "直连".into(),
            kind: EgressKind::Direct,
            // 直连没有远端会话，可用性由物理网卡/系统路由决定，不依赖任何引擎进程。
            management: EgressManagement::System,
            configured: true,
            route: Route::Direct,
            route_target: route_target(Route::Direct),
            unconfigured_reason: None,
        },
        EgressDescriptor {
            id: EGRESS_WG.into(),
            name: "WireGuard".into(),
            kind: EgressKind::WireGuard,
            // 第一阶段：隧道由 mihomo 的 userspace WG outbound 承载，随引擎存亡。
            management: EgressManagement::MihomoManaged,
            configured: wg_reason.is_none(),
            route: Route::Wg,
            route_target: route_target(Route::Wg),
            unconfigured_reason: wg_reason,
        },
        EgressDescriptor {
            id: EGRESS_PROXY.into(),
            name: "代理订阅".into(),
            kind: EgressKind::Proxy,
            management: EgressManagement::MihomoManaged,
            configured: proxy_active.is_some(),
            route: Route::Proxy,
            route_target: route_target(Route::Proxy),
            unconfigured_reason: proxy_active
                .is_none()
                .then(|| "尚未配置并激活代理订阅".to_string()),
        },
    ]
}

/// 统计每个出口被策略占用的情况（默认出口 + 规则数 + 程序组数）。
pub fn usage(settings: &NetPolicySettings, rules: &RuleSet) -> BTreeMap<String, EgressUsage> {
    let mut map: BTreeMap<String, EgressUsage> = BTreeMap::new();
    for d in catalog(settings) {
        map.insert(d.id, EgressUsage::default());
    }
    let mut bump = |route: Route, is_default: bool| {
        if let Some(id) = route_egress_id(route) {
            let e = map.entry(id.to_string()).or_default();
            if is_default {
                e.is_default = true;
            } else {
                e.rule_count += 1;
            }
        }
    };
    bump(settings.default_route, true);
    for r in &rules.rules {
        bump(r.route(), false);
    }
    for g in &rules.groups {
        bump(g.route, false);
    }
    map
}

/// 出口运行态视图：配置生成/路由渲染时的输入，决定「哪些出口当前不可承载流量」。
///
/// 与 `TempDirect` / `DecryptDivert` 同属「agent 运行态，不落 settings.json」。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EgressRuntimeView {
    /// 当前不可承载业务流量的出口 id（用户 Stop 或 Failed）。
    pub unavailable: BTreeSet<String>,
    /// 每个出口的 fallback 策略；缺省 [`EgressFallback::Block`]。
    pub fallback: BTreeMap<String, EgressFallback>,
}

impl EgressRuntimeView {
    /// 全部出口可用（默认；等价于「未引入 Egress 生命周期」时的旧行为）。
    pub fn all_available() -> Self {
        Self::default()
    }

    pub fn is_unavailable(&self, id: &str) -> bool {
        self.unavailable.contains(id)
    }

    pub fn fallback_of(&self, id: &str) -> EgressFallback {
        self.fallback.get(id).copied().unwrap_or_default()
    }

    /// 把「策略计划的出口」解析为「实际可渲染的出口」。
    ///
    /// 出口可用 → 原样返回；不可用 → 按 fallback（默认阻断）。**直连出口不可用时只能阻断**，
    /// 不存在「回落到自己」。`Blackhole` 不是出口，恒等返回。
    pub fn resolve(&self, route: Route) -> Route {
        let Some(id) = route_egress_id(route) else {
            return route;
        };
        if !self.is_unavailable(id) {
            return route;
        }
        match self.fallback_of(id) {
            EgressFallback::Direct
                if id != EGRESS_DIRECT && !self.is_unavailable(EGRESS_DIRECT) =>
            {
                Route::Direct
            }
            _ => Route::Blackhole,
        }
    }

    /// 该次解析是否发生了降级（UI 需要显式告警：计划出口 ≠ 实际出口）。
    pub fn degraded(&self, route: Route) -> bool {
        self.resolve(route) != route
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProgramGroup, Rule, WgConfig};

    fn wg_settings() -> NetPolicySettings {
        NetPolicySettings {
            wg: WgConfig {
                server: "203.0.113.7".into(),
                port: 51820,
                ip: "10.7.0.2/32".into(),
                private_key: "A".repeat(43) + "=",
                public_key: "B".repeat(43) + "=",
                pre_shared_key: String::new(),
                mtu: 1420,
                amnezia: None,
                dialer_proxy: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn catalog_lists_three_kinds_with_configured_flags() {
        let cat = catalog(&NetPolicySettings::default());
        assert_eq!(cat.len(), 3, "Direct/WireGuard/Proxy 三个出口恒在清单里");
        assert!(cat[0].configured, "直连始终可用");
        assert!(
            !cat[1].configured && cat[1].unconfigured_reason.is_some(),
            "默认设置无 WG，应标未配置并给出原因"
        );
        assert!(!cat[2].configured, "默认设置无激活订阅");

        let cat = catalog(&wg_settings());
        assert!(cat[1].configured, "WG 校验通过后应标已配置");
        assert!(cat[1].unconfigured_reason.is_none());
    }

    #[test]
    fn usage_separates_default_from_rule_count() {
        let settings = NetPolicySettings {
            default_route: Route::Direct,
            ..wg_settings()
        };
        let rules = RuleSet {
            rules: vec![
                Rule::DomainSuffix {
                    value: "a.com".into(),
                    route: Route::Wg,
                },
                Rule::DomainSuffix {
                    value: "b.com".into(),
                    route: Route::Wg,
                },
                Rule::DomainSuffix {
                    value: "c.com".into(),
                    route: Route::Blackhole,
                },
            ],
            groups: vec![ProgramGroup {
                id: "g1".into(),
                name: "组".into(),
                root_paths: vec![r"C:\app\a.exe".into()],
                known_children: vec![],
                route: Route::Wg,
            }],
        };
        let u = usage(&settings, &rules);
        let wg = u[EGRESS_WG];
        assert!(!wg.is_default, "默认出口是直连，WG 不该被标默认");
        assert_eq!(wg.rule_count, 3, "两条规则 + 一个程序组");
        assert!(wg.selected(), "有规则指向即算被选中");
        assert!(u[EGRESS_DIRECT].is_default);
        assert_eq!(
            u[EGRESS_DIRECT].rule_count, 0,
            "Blackhole 不是出口，不该计到任何出口头上"
        );
        assert!(!u[EGRESS_PROXY].selected());
    }

    #[test]
    fn resolve_blocks_unavailable_egress_by_default() {
        let mut view = EgressRuntimeView::default();
        view.unavailable.insert(EGRESS_WG.into());
        assert_eq!(
            view.resolve(Route::Wg),
            Route::Blackhole,
            "停用出口默认 fail-closed，绝不隐式回落直连"
        );
        assert!(view.degraded(Route::Wg));
        assert_eq!(
            view.resolve(Route::Direct),
            Route::Direct,
            "其它出口不受影响"
        );
        assert_eq!(view.resolve(Route::Blackhole), Route::Blackhole);
    }

    #[test]
    fn resolve_falls_back_to_direct_only_when_explicitly_configured() {
        let mut view = EgressRuntimeView::default();
        view.unavailable.insert(EGRESS_WG.into());
        view.fallback
            .insert(EGRESS_WG.into(), EgressFallback::Direct);
        assert_eq!(
            view.resolve(Route::Wg),
            Route::Direct,
            "用户明确允许时才回落直连"
        );

        // 直连自己不可用时，允许回落也只能阻断。
        view.unavailable.insert(EGRESS_DIRECT.into());
        assert_eq!(view.resolve(Route::Wg), Route::Blackhole);
        view.fallback
            .insert(EGRESS_DIRECT.into(), EgressFallback::Direct);
        assert_eq!(
            view.resolve(Route::Direct),
            Route::Blackhole,
            "直连出口不可用时不存在回落到自己"
        );
    }

    #[test]
    fn wg_and_proxy_are_disclosed_as_mihomo_managed() {
        // 这条断言守的是**产品诚实性**，不是实现细节：第一阶段 WG/代理的隧道随 mihomo 存亡，
        // 出口清单必须如实声明，UI 才能避免把「已就绪」呈现成「独立存活的资源」。
        // 若哪天真把 WG 挪到独立引擎，改这里的期望值 + 同步改 UI 文案，别偷偷放宽。
        let cat = catalog(&wg_settings());
        assert_eq!(
            cat[0].management,
            EgressManagement::System,
            "直连不依赖引擎"
        );
        assert!(!cat[0].management.depends_on_engine());
        for d in &cat[1..] {
            assert_eq!(
                d.management,
                EgressManagement::MihomoManaged,
                "{} 的数据面仍由 mihomo 承载，必须如实标注",
                d.id
            );
            assert!(d.management.depends_on_engine());
        }
    }

    #[test]
    fn management_survives_serialization() {
        let json = serde_json::to_string(&catalog(&wg_settings())[1]).unwrap();
        assert!(
            json.contains("\"management\":\"mihomo-managed\""),
            "标注必须真的上线协议，否则 UI 拿不到：{json}"
        );
    }

    #[test]
    fn proxy_is_excluded_from_scheduled_probing() {
        // 决议 §3.4：不得为了展示「在线」而周期性强行连代理节点。
        assert!(!EgressKind::Proxy.probe_on_schedule());
        assert!(
            EgressKind::WireGuard.probe_on_schedule(),
            "WG 靠探活对抗懒握手"
        );
        assert!(EgressKind::Direct.probe_on_schedule());
    }

    #[test]
    fn lifecycle_traffic_admission() {
        assert!(EgressLifecycle::Ready.accepts_traffic());
        assert!(EgressLifecycle::Degraded.accepts_traffic());
        for s in [
            EgressLifecycle::Stopped,
            EgressLifecycle::Starting,
            EgressLifecycle::Connecting,
            EgressLifecycle::Reconnecting,
            EgressLifecycle::Failed,
        ] {
            assert!(!s.accepts_traffic(), "{s:?} 不得承载业务流量");
        }
    }

    #[test]
    fn route_id_mapping_roundtrip() {
        for route in [Route::Direct, Route::Wg, Route::Proxy] {
            let id = route_egress_id(route).unwrap();
            assert_eq!(egress_id_route(id), Some(route));
        }
        assert!(route_egress_id(Route::Blackhole).is_none(), "黑洞不是出口");
        assert!(egress_id_route("nope").is_none());
    }

    #[test]
    fn redact_host_hides_last_octet() {
        assert_eq!(redact_host("203.0.113.7"), "203.0.113.x");
        assert_eq!(redact_host("2001:db8::1"), "2001:db8::…");
        assert_eq!(redact_host("vpn.example.com"), "vpn.example.com");
    }

    #[test]
    fn status_flags_selected_but_unusable() {
        let d = &catalog(&wg_settings())[1];
        let mut s = EgressStatus {
            id: d.id.clone(),
            name: d.name.clone(),
            kind: d.kind,
            management: d.management,
            lifecycle: EgressLifecycle::Failed,
            selected: true,
            usage: EgressUsage {
                is_default: true,
                rule_count: 3,
            },
            active_connections: 0,
            configured: d.configured,
            unconfigured_reason: None,
            health: HealthReport::default(),
            detail: EgressDetail::default(),
            route: d.route,
            route_target: d.route_target.clone(),
            fallback: EgressFallback::Block,
            reconnect_count: 0,
            changed_at_ms: 0,
            last_error: Some("握手超时".into()),
        };
        assert!(s.selected_but_unusable(), "策略选中但 Failed 必须报警");
        s.lifecycle = EgressLifecycle::Ready;
        assert!(!s.selected_but_unusable());
        // 已连接但没被选中 —— 正是目标行为，不该被当成异常。
        s.selected = false;
        assert!(!s.selected_but_unusable());
    }

    #[test]
    fn status_serialization_keeps_lifecycle_and_selected_separate() {
        let json = serde_json::to_string(&EgressStatus {
            id: EGRESS_WG.into(),
            name: "WireGuard".into(),
            kind: EgressKind::WireGuard,
            management: EgressManagement::MihomoManaged,
            lifecycle: EgressLifecycle::Ready,
            selected: false,
            usage: EgressUsage::default(),
            active_connections: 0,
            configured: true,
            unconfigured_reason: None,
            health: HealthReport::default(),
            detail: EgressDetail::default(),
            route: Route::Wg,
            route_target: route_target(Route::Wg),
            fallback: EgressFallback::Block,
            reconnect_count: 0,
            changed_at_ms: 0,
            last_error: None,
        })
        .unwrap();
        assert!(json.contains("\"lifecycle\":\"ready\""));
        assert!(json.contains("\"selected\":false"), "已连接≠正在承载流量");
        assert!(!json.contains("private"), "详情里绝不能出现私钥字段");
    }
}
