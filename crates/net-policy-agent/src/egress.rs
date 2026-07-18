//! 出口生命周期管理（`EgressManager`）——设计见
//! `docs/net-policy/net-policy-wg-egress-design.md`，纯模型在 `net_policy_core::egress`。
//!
//! ## 为什么它必须独立于 mihomo 导流
//!
//! mihomo 的 userspace WireGuard outbound 是**懒的**：没有业务流量命中 `wg-out`，它就不会去
//! 握手。于是「策略切到直连」在旧模型里等于「WG 静默掉线」，用户再切回来要重新等握手——而 UI
//! 上只有一个「已启用」，完全看不出这件事。
//!
//! 本管理器给每个出口一条**独立的探活循环**：无论策略当前是否选中它，都周期性经该出口做一次
//! 主动探测（mihomo controller 的 `/proxies/{name}/delay`，真实产生一次经隧道的请求）。这满足
//! 设计 §9 里与「业务流量」有关的两条——「没有业务流量时 WG 也报告握手成功」「切到直连后 WG
//! 仍是 Ready」。
//!
//! **它不满足与「引擎」有关的那几条**：mihomo 没跑就没有 wg-out，停掉 mihomo 出口即消失，
//! reload 会重建 outbound。准确说本阶段做的是**独立健康状态管理**，不是独立出口生命周期——
//! 这个落差由 `EgressManagement::MihomoManaged` 一路标注到 UI，不许在界面上含糊过去。
//!
//! ## 停用出口 = 真的数据面动作
//!
//! [`EgressManager::view`] 产出的 [`EgressRuntimeView`] 会喂给配置生成：被用户 Stop 的出口不再
//! 渲染 outbound，指向它的规则按 fallback 改写（默认 `REJECT-DROP`）。**探测失败不进这个视图**
//! ——否则一次网络抖动就会触发一轮 reload 风暴；连不上的出口由 mihomo 自然连接失败即可，同样
//! 不会泄漏到直连。
//!
//! 数据面选型（设计 §11 待决策 1）：第一阶段沿用 mihomo 内置 WG/代理承载，不引入独立
//! wireguard-go 进程；本模块拥有的是**健康与探活**，隧道本身的存亡仍归 mihomo。

use crate::store::now_ms;
use net_policy_core::config::{NetPolicySettings, RuleSet};
use net_policy_core::egress::{
    catalog, usage, DirectDetail, EgressDetail, EgressFallback, EgressKind, EgressLifecycle,
    EgressRuntimeView, EgressStatus, HealthReport, HealthState, ProxyDetail, WireGuardDetail,
    EGRESS_DIRECT, EGRESS_PROXY, EGRESS_WG,
};
use net_policy_core::mihomo::CONTROLLER;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

/// 探测目标（204 空响应，够轻且能穿透隧道验真）。
const PROBE_URL: &str = "https://www.gstatic.com/generate_204";
/// 单次探测超时。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 探活周期。比 persistent-keepalive(25s) 略短，保证隧道不因空闲被中间设备回收。
pub const PROBE_INTERVAL: Duration = Duration::from_secs(20);
/// 延迟超过此值判 `Degraded`（通但差）。
const DEGRADED_LATENCY_MS: u32 = 1500;
/// 连续失败多少次后从 `Reconnecting` 落到 `Failed`（放弃自动恢复，等人工介入）。
const FAILURES_BEFORE_FAILED: u32 = 3;
/// mihomo reload 后的**状态机静默窗口**。
///
/// 决议 §7.2：「不因普通规则变化把出口状态解释为已断线或已重连」。但 reload 会重建 outbound
/// （§4 承认这是当前架构的已知边界），窗口内的探测失败极可能只是 outbound 正在重建，把它算成
/// 掉线会让用户以为隧道出了问题。故窗口内失败**只记健康、不推状态机、不计重连**。
/// 取值大于一个探活周期，确保重建期至少被完整覆盖（带混淆的握手可能慢几秒）。
const RELOAD_QUIET_WINDOW: Duration = Duration::from_secs(30);

/// 单个出口的运行态。
#[derive(Debug, Clone)]
struct EgressRuntime {
    /// 用户意图：是否希望这个出口在线。false = 显式 Stop，会从 mihomo 配置摘除。
    desired_up: bool,
    lifecycle: EgressLifecycle,
    health: HealthReport,
    fallback: EgressFallback,
    reconnect_count: u32,
    consecutive_failures: u32,
    changed_at_ms: u64,
    last_error: Option<String>,
    /// 最近一次主动探测成功的时刻。**只是探测事实，不是隧道握手时间**（决议 §3.3）。
    last_probe_ok_ms: u64,
}

impl Default for EgressRuntime {
    fn default() -> Self {
        Self {
            // 默认期望在线：出口是「配了就该连着」的资源，不需要用户每次手动启动（设计 §6.1）。
            desired_up: true,
            lifecycle: EgressLifecycle::Stopped,
            health: HealthReport::default(),
            fallback: EgressFallback::default(),
            reconnect_count: 0,
            consecutive_failures: 0,
            changed_at_ms: 0,
            last_error: None,
            last_probe_ok_ms: 0,
        }
    }
}

/// 一次探测的结论（供状态机消费）。
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub latency_ms: Option<u32>,
    pub error: Option<String>,
}

impl ProbeOutcome {
    fn ok(latency_ms: u32) -> Self {
        Self {
            latency_ms: Some(latency_ms),
            error: None,
        }
    }
    fn failed(error: impl Into<String>) -> Self {
        Self {
            latency_ms: None,
            error: Some(error.into()),
        }
    }
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

/// 出口生命周期管理器。形状与 `CaptureManager` / `DecryptManager` 对齐：
/// 同步 `Mutex` 保运行态，异步副作用（探测/重连）由调用方在 tokio 上下文里驱动。
pub struct EgressManager {
    inner: Mutex<BTreeMap<String, EgressRuntime>>,
    /// reload 静默窗口的截止时刻（epoch 毫秒）。见 [`RELOAD_QUIET_WINDOW`]。
    quiet_until_ms: Mutex<u64>,
    /// 订阅状态缓存（决议 §3.4 的「订阅层」+「节点层」）。由后台采样与显式刷新写入。
    subscription: Mutex<SubscriptionSnapshot>,
    /// 各出口当前活跃连接数（「使用层」）。由连接采样器写入。
    active_conns: Mutex<BTreeMap<String, usize>>,
}

impl Default for EgressManager {
    fn default() -> Self {
        Self::new()
    }
}

impl EgressManager {
    pub fn new() -> Self {
        let mut map = BTreeMap::new();
        for id in [EGRESS_DIRECT, EGRESS_WG, EGRESS_PROXY] {
            map.insert(id.to_string(), EgressRuntime::default());
        }
        Self {
            inner: Mutex::new(map),
            quiet_until_ms: Mutex::new(0),
            subscription: Mutex::new(SubscriptionSnapshot::default()),
            active_conns: Mutex::new(BTreeMap::new()),
        }
    }

    /// 写入订阅状态快照（后台采样 / 显式刷新后调用）。
    pub fn set_subscription(&self, snap: SubscriptionSnapshot) {
        *self.subscription.lock().unwrap() = snap;
    }

    /// 更新各出口的活跃连接数（「使用状态」层，与「策略选中」严格区分）。
    pub fn set_active_connections(&self, counts: BTreeMap<String, usize>) {
        *self.active_conns.lock().unwrap() = counts;
    }

    /// 用持久化偏好初始化运行态（agent 启动时调用一次，决议 §7.3）。
    ///
    /// 「用户停掉了 WG 并要求阻断」这类决定必须跨重启存活——否则下次开机它会静默恢复成
    /// 「在线 + 放行」，是安全语义倒退。
    pub fn hydrate(&self, prefs: &net_policy_core::config::EgressPrefs) {
        let mut g = self.inner.lock().unwrap();
        for (id, rt) in g.iter_mut() {
            let pref = prefs.get(id);
            rt.desired_up = pref.up;
            rt.fallback = pref.fallback;
            // 尚未探测过，一律从 Stopped 起步：Ready 必须由真实探测挣来。
            rt.lifecycle = EgressLifecycle::Stopped;
        }
    }

    /// 当前运行态导出为可持久化偏好。
    pub fn prefs(&self) -> net_policy_core::config::EgressPrefs {
        use net_policy_core::config::{EgressPref, EgressPrefs};
        let g = self.inner.lock().unwrap();
        EgressPrefs {
            by_id: g
                .iter()
                .map(|(id, r)| {
                    (
                        id.clone(),
                        EgressPref {
                            up: r.desired_up,
                            fallback: r.fallback,
                        },
                    )
                })
                .collect(),
        }
    }

    /// mihomo 配置重载后调用：开启静默窗口，窗口内的探测失败不推进状态机（决议 §7.2）。
    pub fn begin_reload_quiet_window(&self) {
        *self.quiet_until_ms.lock().unwrap() = now_ms() + RELOAD_QUIET_WINDOW.as_millis() as u64;
    }

    /// 当前是否处于 reload 静默窗口内。
    pub fn in_reload_quiet_window(&self) -> bool {
        now_ms() < *self.quiet_until_ms.lock().unwrap()
    }

    fn with<T>(&self, id: &str, f: impl FnOnce(&mut EgressRuntime) -> T) -> Option<T> {
        let mut g = self.inner.lock().unwrap();
        g.get_mut(id).map(f)
    }

    pub fn exists(&self, id: &str) -> bool {
        self.inner.lock().unwrap().contains_key(id)
    }

    /// 配置生成用的出口运行态视图。**只有显式 Stop 的出口进 `unavailable`**（见模块文档）。
    pub fn view(&self) -> EgressRuntimeView {
        let g = self.inner.lock().unwrap();
        EgressRuntimeView {
            unavailable: g
                .iter()
                .filter(|(_, r)| !r.desired_up)
                .map(|(id, _)| id.clone())
                .collect(),
            fallback: g.iter().map(|(id, r)| (id.clone(), r.fallback)).collect(),
        }
    }

    /// 设置用户启停意图，返回**变更前**的值（供事务化回滚）。`None` = 出口不存在。
    pub fn set_desired_up(&self, id: &str, up: bool) -> Option<bool> {
        self.with(id, |r| {
            let prev = r.desired_up;
            r.desired_up = up;
            if !up {
                r.lifecycle = EgressLifecycle::Stopped;
                r.consecutive_failures = 0;
                r.last_error = None;
                r.changed_at_ms = now_ms();
            } else if prev != up {
                r.lifecycle = EgressLifecycle::Starting;
                r.changed_at_ms = now_ms();
            }
            prev
        })
    }

    pub fn desired_up(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(id)
            .map(|r| r.desired_up)
            .unwrap_or(false)
    }

    /// 读取出口当前生命周期，用于区分“用户意图仍为在线”和“运行态需要重新拉起”。
    pub fn lifecycle(&self, id: &str) -> Option<EgressLifecycle> {
        self.inner.lock().unwrap().get(id).map(|r| r.lifecycle)
    }

    /// mihomo 停止后立即对齐依赖引擎的出口状态，避免等待探活周期期间继续显示旧健康状态。
    pub fn mark_engine_stopped(&self) {
        let mut g = self.inner.lock().unwrap();
        for (id, rt) in g.iter_mut() {
            if id == EGRESS_DIRECT {
                continue;
            }
            rt.lifecycle = EgressLifecycle::Stopped;
            rt.health = HealthReport::default();
            rt.consecutive_failures = 0;
            rt.last_error = None;
            rt.changed_at_ms = now_ms();
        }
        self.active_conns.lock().unwrap().clear();
    }

    pub fn set_fallback(&self, id: &str, fallback: EgressFallback) -> Option<EgressFallback> {
        self.with(id, |r| std::mem::replace(&mut r.fallback, fallback))
    }

    /// 直接置生命周期（用于「引擎未跑 → 全部出口 Stopped」这类外部事实）。
    /// 返回 true 表示状态真的变了（调用方据此决定是否推事件）。
    pub fn set_lifecycle(&self, id: &str, next: EgressLifecycle) -> bool {
        self.with(id, |r| {
            if r.lifecycle == next {
                return false;
            }
            r.lifecycle = next;
            r.changed_at_ms = now_ms();
            true
        })
        .unwrap_or(false)
    }

    pub fn bump_reconnect(&self, id: &str) {
        self.with(id, |r| {
            r.reconnect_count += 1;
            r.lifecycle = EgressLifecycle::Reconnecting;
            r.changed_at_ms = now_ms();
        });
    }

    /// 消化一次探测结果，推进生命周期状态机。返回 true 表示生命周期发生了变迁。
    ///
    /// 规则（设计 §4/§7）：
    /// - 通过 → `Ready`（慢 → `Degraded`），清零连续失败计数，刷新 `last_probe_ok_ms`；
    /// - 失败且此前可承载流量 → `Reconnecting` 并计一次重连；
    /// - 连续失败达阈值 → `Failed`（不再自称在重连，逼 UI 显式报错）。
    pub fn record_probe(&self, id: &str, outcome: &ProbeOutcome) -> bool {
        // reload 静默窗口内的失败只记健康：outbound 很可能正在重建，不是出口断了（§7.2）。
        if !outcome.is_ok() && self.in_reload_quiet_window() {
            self.record_test_only(id, outcome);
            return false;
        }
        self.with(id, |r| {
            let before = r.lifecycle;
            let now = now_ms();
            match (&outcome.error, outcome.latency_ms) {
                (None, latency) => {
                    let slow = latency.is_some_and(|ms| ms > DEGRADED_LATENCY_MS);
                    r.consecutive_failures = 0;
                    r.last_error = None;
                    r.last_probe_ok_ms = now;
                    r.lifecycle = if slow {
                        EgressLifecycle::Degraded
                    } else {
                        EgressLifecycle::Ready
                    };
                    r.health = HealthReport {
                        state: if slow {
                            HealthState::Degraded
                        } else {
                            HealthState::Healthy
                        },
                        checked_at_ms: now,
                        latency_ms: latency,
                        target: Some(PROBE_URL.into()),
                        error: None,
                    };
                }
                (Some(err), _) => {
                    if before.accepts_traffic() {
                        r.reconnect_count += 1;
                    }
                    r.consecutive_failures += 1;
                    r.last_error = Some(err.clone());
                    r.lifecycle = if r.consecutive_failures >= FAILURES_BEFORE_FAILED {
                        EgressLifecycle::Failed
                    } else {
                        EgressLifecycle::Reconnecting
                    };
                    r.health = HealthReport {
                        state: HealthState::Unhealthy,
                        checked_at_ms: now,
                        latency_ms: None,
                        target: Some(PROBE_URL.into()),
                        error: Some(err.clone()),
                    };
                }
            }
            if r.lifecycle != before {
                r.changed_at_ms = now;
            }
            r.lifecycle != before
        })
        .unwrap_or(false)
    }

    /// 仅记录一次「测试连接」的健康结果，**不改生命周期**（设计 §8.3：测试连接 / 启动并连接 /
    /// 设为默认出口是三个不同操作）。
    pub fn record_test_only(&self, id: &str, outcome: &ProbeOutcome) {
        self.with(id, |r| {
            r.health = HealthReport {
                state: if outcome.is_ok() {
                    HealthState::Healthy
                } else {
                    HealthState::Unhealthy
                },
                checked_at_ms: now_ms(),
                latency_ms: outcome.latency_ms,
                target: Some(PROBE_URL.into()),
                error: outcome.error.clone(),
            };
        });
    }

    /// 合成对外 DTO：静态清单（settings 派生）+ 策略占用（rules 派生）+ 运行态。
    pub fn list(&self, settings: &NetPolicySettings, rules: &RuleSet) -> Vec<EgressStatus> {
        let use_map = usage(settings, rules);
        let sub = self.subscription.lock().unwrap().clone();
        let conns = self.active_conns.lock().unwrap().clone();
        let g = self.inner.lock().unwrap();
        catalog(settings)
            .into_iter()
            .map(|d| {
                let rt = g.get(&d.id).cloned().unwrap_or_default();
                let u = use_map.get(&d.id).copied().unwrap_or_default();
                // 配置不完整的出口不许自称 Ready——避免「显示在线但一用就废」。
                let lifecycle = if !d.configured {
                    EgressLifecycle::Stopped
                } else {
                    rt.lifecycle
                };
                EgressStatus {
                    id: d.id.clone(),
                    name: d.name,
                    kind: d.kind,
                    management: d.management,
                    lifecycle,
                    selected: u.selected(),
                    usage: u,
                    active_connections: conns.get(&d.id).copied().unwrap_or(0),
                    configured: d.configured,
                    unconfigured_reason: d.unconfigured_reason,
                    detail: detail_of(d.kind, settings, &rt, &sub),
                    health: rt.health,
                    route: d.route,
                    route_target: d.route_target,
                    fallback: rt.fallback,
                    reconnect_count: rt.reconnect_count,
                    changed_at_ms: rt.changed_at_ms,
                    last_error: rt.last_error,
                }
            })
            .collect()
    }

    /// 取单个出口 DTO。生产路径统一走 `AgentState::egress_list`（要带上磁盘 settings/rules），
    /// 这里主要给单测断言单个出口用。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get(
        &self,
        id: &str,
        settings: &NetPolicySettings,
        rules: &RuleSet,
    ) -> Option<EgressStatus> {
        self.list(settings, rules).into_iter().find(|e| e.id == id)
    }
}

fn detail_of(
    kind: EgressKind,
    settings: &NetPolicySettings,
    rt: &EgressRuntime,
    sub: &SubscriptionSnapshot,
) -> EgressDetail {
    use net_policy_core::egress::redact_host;
    match kind {
        EgressKind::Direct => {
            let (interface, gateway) = physical_default_route();
            EgressDetail {
                direct: Some(DirectDetail { interface, gateway }),
                ..Default::default()
            }
        }
        EgressKind::WireGuard => EgressDetail {
            wireguard: Some(WireGuardDetail {
                // 脱敏：出口服务器地址不进 UI/截图（设计 §8.8）。
                endpoint: format!("{}:{}", redact_host(&settings.wg.server), settings.wg.port),
                local_ip: settings.wg.ip.clone(),
                mtu: settings.wg.mtu,
                obfuscation: settings.wg.amnezia.is_some(),
                via_dialer_proxy: settings.wg.dialer_proxy.is_some(),
                last_probe_ok_at_ms: rt.last_probe_ok_ms,
            }),
            ..Default::default()
        },
        EgressKind::Proxy => {
            let active = settings.proxy_subscriptions.active_subscription();
            let interval_secs = active.map(|(_, s)| s.interval_secs).unwrap_or(0);
            // 过期 = 距上次刷新已超过约定间隔。未刷新过（0）不算过期，只算「未知」。
            let expired = sub.refreshed_at_ms > 0
                && interval_secs > 0
                && now_ms().saturating_sub(sub.refreshed_at_ms) > interval_secs * 1000;
            EgressDetail {
                proxy: Some(ProxyDetail {
                    subscription: active.map(|(_, s)| s.name.clone()).unwrap_or_default(),
                    // 只留 host：订阅 URL 的 path/query 常自带 token，绝不能进 UI。
                    source: active
                        .and_then(|(_, s)| reqwest::Url::parse(&s.url).ok())
                        .and_then(|u| u.host_str().map(|h| h.to_string()))
                        .unwrap_or_default(),
                    refreshed_at_ms: sub.refreshed_at_ms,
                    interval_secs,
                    expired,
                    refresh_error: sub.error.clone(),
                    node: sub.node.clone(),
                    node_delay_ms: sub.node_delay_ms,
                    node_alive: sub.node_alive,
                    node_count: sub.node_count,
                }),
                ..Default::default()
            }
        }
    }
}

// ── 代理订阅状态（决议 §3.4：订阅 / 节点 / 使用三层分开）────────────────────

/// 从 mihomo controller 读到的订阅 + 当前节点快照。agent 负责聚合，mihomo 负责数据面。
#[derive(Debug, Clone, Default)]
pub struct SubscriptionSnapshot {
    pub refreshed_at_ms: u64,
    pub node: String,
    pub node_delay_ms: Option<u32>,
    pub node_alive: bool,
    pub node_count: usize,
    pub error: Option<String>,
}

/// 订阅 provider 名（与 `mihomo.rs` 的渲染保持一致）。
fn provider_name(settings: &NetPolicySettings) -> Option<String> {
    settings
        .proxy_subscriptions
        .active_subscription()
        .map(|(slot, _)| format!("net-policy-sub-{}", slot + 1))
}

/// 读取订阅与当前节点状态。**只读，不触发任何节点连接**——决议 §3.4 禁止为了展示「在线」
/// 而强行建连，所以这里不做 delay 探测，只取 mihomo 已有的信息。
pub async fn subscription_snapshot(
    secret: &str,
    settings: &NetPolicySettings,
) -> SubscriptionSnapshot {
    #[derive(serde::Deserialize)]
    struct Providers {
        #[serde(default)]
        providers: BTreeMap<String, Provider>,
    }
    #[derive(serde::Deserialize)]
    struct Provider {
        #[serde(default)]
        proxies: Vec<Node>,
        /// mihomo 回的是 RFC3339 时间串。
        #[serde(rename = "updatedAt", default)]
        updated_at: String,
    }
    #[derive(serde::Deserialize, Clone)]
    struct Node {
        #[serde(default)]
        name: String,
        #[serde(default)]
        alive: bool,
        #[serde(default)]
        history: Vec<History>,
    }
    #[derive(serde::Deserialize, Clone)]
    struct History {
        #[serde(default)]
        delay: u32,
    }
    #[derive(serde::Deserialize)]
    struct Group {
        /// select 组当前选中的节点名。
        #[serde(default)]
        now: String,
    }

    let mut snap = SubscriptionSnapshot::default();
    let Some(provider) = provider_name(settings) else {
        snap.error = Some("尚未配置并激活代理订阅".into());
        return snap;
    };
    let client = match controller_client(secret) {
        Ok(c) => c,
        Err(e) => {
            snap.error = Some(format!("{e:#}"));
            return snap;
        }
    };

    match client
        .get(format!("http://{CONTROLLER}/providers/proxies"))
        .send()
        .await
    {
        Err(e) => snap.error = Some(format!("读取订阅失败：{e}")),
        Ok(resp) => match resp.json::<Providers>().await {
            Err(e) => snap.error = Some(format!("解析订阅响应失败：{e}")),
            Ok(all) => match all.providers.get(&provider) {
                None => snap.error = Some("当前订阅尚未加载；请保存并应用代理设置后重试".into()),
                Some(p) => {
                    snap.node_count = p.proxies.len();
                    snap.refreshed_at_ms = parse_rfc3339_ms(&p.updated_at);
                    // 当前选中节点由 select 组决定。
                    if let Ok(g) = client
                        .get(format!("http://{CONTROLLER}/proxies/subscription-out"))
                        .send()
                        .await
                    {
                        if let Ok(g) = g.json::<Group>().await {
                            snap.node = g.now;
                        }
                    }
                    // 当前节点的历史延迟：读已有记录，不主动打探测。
                    if let Some(n) = p.proxies.iter().find(|n| n.name == snap.node) {
                        snap.node_alive = n.alive;
                        snap.node_delay_ms = n
                            .history
                            .iter()
                            .rev()
                            .find_map(|h| (h.delay > 0).then_some(h.delay));
                    }
                }
            },
        },
    }
    snap
}

/// 强制刷新订阅（`PUT /providers/proxies/{name}`）。
///
/// 决议 §3.4/§6.3：刷新订阅与连接节点是两个动作——本函数**只刷配置来源**，不重连节点，
/// 也不打断当前可用连接。
pub async fn refresh_subscription(
    secret: &str,
    settings: &NetPolicySettings,
) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    let provider = provider_name(settings).context("尚未配置并激活代理订阅")?;
    let resp = controller_client(secret)?
        .put(format!("http://{CONTROLLER}/providers/proxies/{provider}"))
        .send()
        .await
        .context("请求 mihomo 刷新订阅失败")?;
    if !resp.status().is_success() {
        bail!("mihomo 刷新订阅失败：控制器返回 {}", resp.status());
    }
    Ok(())
}

/// 切换 select 组当前节点（`PUT /proxies/subscription-out`）。
pub async fn select_node(secret: &str, node: &str) -> anyhow::Result<()> {
    use anyhow::{bail, Context};
    let body = serde_json::json!({ "name": node });
    let resp = controller_client(secret)?
        .put(format!("http://{CONTROLLER}/proxies/subscription-out"))
        .json(&body)
        .send()
        .await
        .context("请求 mihomo 切换节点失败")?;
    if !resp.status().is_success() {
        bail!("mihomo 切换节点失败：控制器返回 {}", resp.status());
    }
    Ok(())
}

/// 极简 RFC3339 → epoch 毫秒。mihomo 回 `2026-07-18T12:00:00.123456789+08:00` 这类串；
/// 解析失败返回 0（= 未知），不因为一个展示字段引入日期库依赖。
fn parse_rfc3339_ms(s: &str) -> u64 {
    parse_rfc3339_ms_opt(s).unwrap_or(0)
}

fn parse_rfc3339_ms_opt(s: &str) -> Option<u64> {
    fn num(s: &str) -> Option<i64> {
        s.parse().ok()
    }
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let (date, rest) = s.split_at(10);
    let time = &rest[1..9]; // 跳过 'T'
    let (y, m, d) = (num(&date[0..4])?, num(&date[5..7])?, num(&date[8..10])?);
    let (hh, mm, ss) = (num(&time[0..2])?, num(&time[3..5])?, num(&time[6..8])?);
    // 范围校验：畸形串必须退化为「未知」，不能算出一个看似合理的时间去误导 UI。
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // 民用历 → 天数（Howard Hinnant days_from_civil）。
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    // 时区偏移：末尾 Z 或 ±HH:MM。
    if let Some(idx) = s.rfind(['+', '-']).filter(|&i| i > 10) {
        let sign = if s.as_bytes()[idx] == b'+' { -1 } else { 1 };
        let tz = &s[idx + 1..];
        if tz.len() >= 5 {
            if let (Some(h), Some(m)) = (num(&tz[0..2]), num(&tz[3..5])) {
                secs += sign * (h * 3600 + m * 60);
            }
        }
    }
    Some((secs.max(0) as u64) * 1000)
}

// ── 物理默认路由（Direct 出口详情，决议 §6.4）──────────────────────────────

/// 当前**物理**默认路由的（网卡别名, 网关）。
///
/// 刻意跳过 mihomo 的 TUN 适配器：策略生效时 `Meta` 会以极低 metric 抢占 `0.0.0.0/0`，
/// 照搬就会把「直连出口」显示成走隧道——与它的语义正好相反。用 IP Helper 直接读表，
/// 不拉 PowerShell（无闪窗、无 OEM 代码页把中文网卡名解成乱码的问题）。
#[cfg(windows)]
fn physical_default_route() -> (String, String) {
    use std::net::{IpAddr, Ipv4Addr};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceLuidToAlias, FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_TABLE2,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    fn alias(luid: NET_LUID_LH) -> String {
        let mut buf = [0u16; 257];
        // SAFETY: 缓冲区按 ConvertInterfaceLuidToAlias 契约定长；失败时保持全零。
        if unsafe { ConvertInterfaceLuidToAlias(&luid, buf.as_mut_ptr(), buf.len()) }
            != ERROR_SUCCESS
        {
            return String::new();
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    }

    let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    // SAFETY: 传出参指针，成功后必须 FreeMibTable（下方无条件释放）。
    if unsafe { GetIpForwardTable2(AF_INET, &mut table) } != ERROR_SUCCESS || table.is_null() {
        return (String::new(), String::new());
    }
    let mut best: Option<(u32, String, String)> = None;
    // SAFETY: GetIpForwardTable2 成功即保证 Table 有 NumEntries 个有效行。
    unsafe {
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        for r in rows {
            // 只要默认路由（0.0.0.0/0）。
            if r.DestinationPrefix.PrefixLength != 0
                || r.DestinationPrefix.Prefix.si_family != AF_INET
            {
                continue;
            }
            let name = alias(r.InterfaceLuid);
            // 跳过 mihomo TUN：它不是物理出口。
            if name.eq_ignore_ascii_case("Meta") {
                continue;
            }
            let gw = IpAddr::V4(Ipv4Addr::from(
                r.NextHop.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes(),
            ));
            if gw.is_unspecified() {
                continue;
            }
            // metric 越小越优先。
            if best.as_ref().is_none_or(|(m, _, _)| r.Metric < *m) {
                best = Some((r.Metric, name, gw.to_string()));
            }
        }
        FreeMibTable(table as *const _);
    }
    best.map(|(_, iface, gw)| (iface, gw)).unwrap_or_default()
}

#[cfg(not(windows))]
fn physical_default_route() -> (String, String) {
    (String::new(), String::new())
}

// ── 主动探测（真实副作用）──────────────────────────────────────────────────

fn controller_client(secret: &str) -> anyhow::Result<reqwest::Client> {
    use anyhow::Context;
    let mut headers = reqwest::header::HeaderMap::new();
    if !secret.is_empty() {
        let value = format!("Bearer {secret}")
            .parse()
            .context("mihomo controller secret 非法")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(PROBE_TIMEOUT + Duration::from_secs(3))
        .build()
        .context("创建 mihomo controller 客户端失败")
}

/// 经指定 mihomo outbound 做一次延迟探测。
///
/// 这实现的是**「独立于业务流量」的保活**：mihomo 会真的经该 outbound 发起一次请求，从而驱动
/// WG 握手 / 代理节点连接，无论当前有没有业务流量命中它。
///
/// **注意它不是「独立于 mihomo」**：整条探测链路都跑在 mihomo 进程里，引擎没跑就无从探测，
/// 引擎重载会重建 outbound。要真正独立于引擎，得把隧道挪到独立进程（见
/// `EgressManagement::Independent`）。
async fn probe_outbound(secret: &str, outbound: &str) -> ProbeOutcome {
    #[derive(serde::Deserialize)]
    struct DelayResponse {
        #[serde(default)]
        delay: u32,
    }
    let client = match controller_client(secret) {
        Ok(c) => c,
        Err(e) => return ProbeOutcome::failed(format!("{e:#}")),
    };
    let mut url = match reqwest::Url::parse(&format!("http://{CONTROLLER}/")) {
        Ok(u) => u,
        Err(e) => return ProbeOutcome::failed(format!("构造控制器地址失败：{e}")),
    };
    url.set_path(&format!("/proxies/{outbound}/delay"));
    url.query_pairs_mut()
        .append_pair("url", PROBE_URL)
        .append_pair("timeout", &PROBE_TIMEOUT.as_millis().to_string());
    match client.get(url).send().await {
        Err(e) => ProbeOutcome::failed(format!("探测请求失败：{e}")),
        Ok(resp) if !resp.status().is_success() => {
            ProbeOutcome::failed(format!("出口不可达（控制器 {}）", resp.status()))
        }
        Ok(resp) => match resp.json::<DelayResponse>().await {
            Err(e) => ProbeOutcome::failed(format!("解析探测响应失败：{e}")),
            Ok(d) if d.delay == 0 => ProbeOutcome::failed("探测超时"),
            Ok(d) => ProbeOutcome::ok(d.delay),
        },
    }
}

/// 直连出口探测：**不经 mihomo**，直接 TCP 连 DNS bootstrap，反映物理网络本身是否通
/// （设计 §4.3：Direct 的可用性由物理网卡/系统路由决定）。
async fn probe_direct(settings: &NetPolicySettings) -> ProbeOutcome {
    let Some(host) = settings.dns_bootstrap.first() else {
        return ProbeOutcome::failed("未配置 DNS bootstrap，无法探测物理网络");
    };
    let target = format!("{host}:53");
    let started = std::time::Instant::now();
    match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(&target)).await {
        Err(_) => ProbeOutcome::failed("物理网络探测超时"),
        Ok(Err(e)) => ProbeOutcome::failed(format!("物理网络不可达：{e}")),
        Ok(Ok(_)) => ProbeOutcome::ok(started.elapsed().as_millis().min(u32::MAX as u128) as u32),
    }
}

/// 按出口 id 执行一次主动探测。`secret` 为 None 或空 = mihomo 未跑（隧道类出口无从探测）。
pub async fn probe(id: &str, settings: &NetPolicySettings, secret: Option<&str>) -> ProbeOutcome {
    match id {
        EGRESS_DIRECT => probe_direct(settings).await,
        EGRESS_WG | EGRESS_PROXY => {
            let Some(secret) = secret.filter(|s| !s.is_empty()) else {
                return ProbeOutcome::failed("mihomo 未运行，无法经该出口探测");
            };
            let Some(outbound) = net_policy_core::egress::egress_id_route(id).map(|r| r.outbound())
            else {
                return ProbeOutcome::failed("未知出口");
            };
            probe_outbound(secret, outbound).await
        }
        _ => ProbeOutcome::failed("未知出口"),
    }
}

/// 重置某个出口上的存量连接（逼它们重新握手/重连），**不影响其它出口的连接**。
/// best-effort：拿不到连接列表就当没有可重置的。
pub async fn reset_egress_connections(secret: &str, outbound: &str) -> anyhow::Result<usize> {
    #[derive(serde::Deserialize)]
    struct ConnList {
        #[serde(default)]
        connections: Vec<Conn>,
    }
    #[derive(serde::Deserialize)]
    struct Conn {
        #[serde(default)]
        id: String,
        #[serde(default)]
        chains: Vec<String>,
    }
    let client = controller_client(secret)?;
    let list: ConnList = client
        .get(format!("http://{CONTROLLER}/connections"))
        .send()
        .await?
        .json()
        .await?;
    let mut closed = 0usize;
    for c in list.connections {
        if c.id.is_empty() || !c.chains.iter().any(|x| x == outbound) {
            continue;
        }
        if client
            .delete(format!("http://{CONTROLLER}/connections/{}", c.id))
            .send()
            .await
            .is_ok()
        {
            closed += 1;
        }
    }
    Ok(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_policy_core::config::{Route, Rule};

    fn wg_settings() -> NetPolicySettings {
        use net_policy_core::config::WgConfig;
        NetPolicySettings {
            default_route: Route::Wg,
            wg: WgConfig {
                server: "38.209.122.38".into(),
                port: 29987,
                ip: "10.66.66.2".into(),
                private_key: "aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvMTI=".into(),
                public_key: "cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1Yj0=".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn probe_success_marks_ready_without_any_business_traffic() {
        let m = EgressManager::new();
        assert!(m.record_probe(EGRESS_WG, &ProbeOutcome::ok(82)));
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(s.lifecycle, EgressLifecycle::Ready);
        assert_eq!(s.health.state, HealthState::Healthy);
        assert_eq!(s.health.latency_ms, Some(82));
    }

    #[test]
    fn ready_egress_can_be_unselected_by_policy() {
        // 设计 §7 的目标行为：WG 连着，但当前策略没把流量导给它。
        let settings = NetPolicySettings {
            default_route: Route::Direct,
            ..wg_settings()
        };
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(50));
        let s = m.get(EGRESS_WG, &settings, &RuleSet::default()).unwrap();
        assert_eq!(
            s.lifecycle,
            EgressLifecycle::Ready,
            "策略不选它不影响生命周期"
        );
        assert!(!s.selected, "没有任何规则指向它");
        assert!(!s.selected_but_unusable(), "在线且未被使用不是异常");
    }

    #[test]
    fn slow_probe_degrades_but_still_carries_traffic() {
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(DEGRADED_LATENCY_MS + 1));
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(s.lifecycle, EgressLifecycle::Degraded);
        assert!(s.lifecycle.accepts_traffic());
    }

    #[test]
    fn failures_escalate_reconnecting_then_failed() {
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(30));
        m.record_probe(EGRESS_WG, &ProbeOutcome::failed("握手超时"));
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(s.lifecycle, EgressLifecycle::Reconnecting);
        assert_eq!(s.reconnect_count, 1, "从可用跌落才算一次重连");

        m.record_probe(EGRESS_WG, &ProbeOutcome::failed("握手超时"));
        m.record_probe(EGRESS_WG, &ProbeOutcome::failed("握手超时"));
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(s.lifecycle, EgressLifecycle::Failed);
        assert_eq!(s.reconnect_count, 1, "持续失败不重复计重连");
        assert_eq!(s.last_error.as_deref(), Some("握手超时"));

        // 恢复：一次成功即回 Ready 并清零。
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(40));
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(s.lifecycle, EgressLifecycle::Ready);
        assert!(s.last_error.is_none());
    }

    fn proxy_settings() -> NetPolicySettings {
        use net_policy_core::config::{ProxySubscription, ProxySubscriptions};
        NetPolicySettings {
            proxy_subscriptions: ProxySubscriptions {
                first: Some(ProxySubscription {
                    name: "机场A".into(),
                    // 订阅 URL 常自带 token，脱敏后不得出现在 DTO 里。
                    url: "https://sub.example.com/link/SECRET_TOKEN?flag=clash".into(),
                    interval_secs: 3600,
                }),
                second: None,
                active: Some(0),
            },
            ..Default::default()
        }
    }

    #[test]
    fn proxy_detail_separates_subscription_and_node_layers() {
        // 决议 §3.4：订阅层 / 节点层 / 使用层必须分开表达。
        let m = EgressManager::new();
        m.set_subscription(SubscriptionSnapshot {
            refreshed_at_ms: now_ms() - 60_000,
            node: "香港01".into(),
            node_delay_ms: Some(120),
            node_alive: true,
            node_count: 37,
            error: None,
        });
        m.set_active_connections([(EGRESS_PROXY.to_string(), 5)].into_iter().collect());

        let s = m
            .get(EGRESS_PROXY, &proxy_settings(), &RuleSet::default())
            .unwrap();
        let p = s.detail.proxy.as_ref().unwrap();
        // 订阅层
        assert_eq!(p.subscription, "机场A");
        assert_eq!(p.source, "sub.example.com", "只留 host");
        assert!(!p.expired, "刚刷新过不该判过期");
        assert_eq!(p.interval_secs, 3600);
        // 节点层
        assert_eq!(p.node, "香港01");
        assert_eq!(p.node_delay_ms, Some(120));
        assert!(p.node_alive);
        assert_eq!(p.node_count, 37);
        // 使用层：与「策略选中」是两回事。
        assert_eq!(s.active_connections, 5);
        assert!(!s.selected, "没有任何规则指向它，但它确实有连接在跑");
    }

    #[test]
    fn subscription_source_never_leaks_credentials() {
        let m = EgressManager::new();
        let s = m
            .get(EGRESS_PROXY, &proxy_settings(), &RuleSet::default())
            .unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("SECRET_TOKEN"),
            "订阅 URL 的 token 绝不能出管道：{json}"
        );
        assert!(!json.contains("/link/"), "path 也不该出现：{json}");
    }

    #[test]
    fn stale_subscription_is_flagged_expired() {
        let m = EgressManager::new();
        m.set_subscription(SubscriptionSnapshot {
            // interval 是 3600s，这里已过去 2 小时。
            refreshed_at_ms: now_ms() - 7_200_000,
            node_count: 3,
            ..Default::default()
        });
        let s = m
            .get(EGRESS_PROXY, &proxy_settings(), &RuleSet::default())
            .unwrap();
        assert!(s.detail.proxy.as_ref().unwrap().expired);

        // 从未刷新过 = 未知，不该谎报「过期」。
        let m2 = EgressManager::new();
        let s2 = m2
            .get(EGRESS_PROXY, &proxy_settings(), &RuleSet::default())
            .unwrap();
        assert!(!s2.detail.proxy.as_ref().unwrap().expired);
        assert_eq!(s2.detail.proxy.as_ref().unwrap().refreshed_at_ms, 0);
    }

    #[test]
    fn rfc3339_parsing_handles_offsets_and_garbage() {
        // 1970-01-01T00:00:00Z
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), 0);
        // 2026-07-18T12:00:00Z = 1784548800
        assert_eq!(parse_rfc3339_ms("2026-07-18T12:00:00Z"), 1_784_376_000_000);
        // 同一时刻的 +08:00 表示应比 UTC 表示早 8 小时。
        assert_eq!(
            parse_rfc3339_ms("2026-07-18T20:00:00.123+08:00"),
            1_784_376_000_000
        );
        // 垃圾输入退化为「未知」，不 panic。
        assert_eq!(parse_rfc3339_ms(""), 0);
        assert_eq!(parse_rfc3339_ms("not a time"), 0);
        assert_eq!(parse_rfc3339_ms("2026-13-99T99:99:99Z"), 0);
    }

    #[test]
    fn prefs_survive_restart() {
        // 决议 §7.3：「停掉 WG 且要求阻断」必须跨重启存活，绝不能静默恢复成在线+放行。
        let m = EgressManager::new();
        m.set_desired_up(EGRESS_WG, false);
        m.set_fallback(EGRESS_PROXY, EgressFallback::Direct);
        let saved = m.prefs();

        // 模拟 agent 重启：全新 manager + 回填。
        let restarted = EgressManager::new();
        restarted.hydrate(&saved);
        assert!(!restarted.desired_up(EGRESS_WG), "停用意图必须被记住");
        assert!(restarted.view().is_unavailable(EGRESS_WG));
        assert_eq!(
            restarted.view().fallback_of(EGRESS_PROXY),
            EgressFallback::Direct,
            "fallback 是安全语义，同样必须被记住"
        );
        assert!(
            restarted.desired_up(EGRESS_DIRECT),
            "未动过的出口保持默认在线"
        );
        assert_eq!(
            restarted.view().fallback_of(EGRESS_DIRECT),
            EgressFallback::Block,
            "缺省仍是 fail-closed"
        );
        // 重启后生命周期一律从 Stopped 起步：Ready 必须由真实探测挣来。
        assert_eq!(
            restarted
                .get(EGRESS_DIRECT, &wg_settings(), &RuleSet::default())
                .unwrap()
                .lifecycle,
            EgressLifecycle::Stopped
        );
    }

    #[test]
    fn hydrate_from_empty_prefs_keeps_fail_closed_defaults() {
        let m = EgressManager::new();
        m.hydrate(&net_policy_core::config::EgressPrefs::default());
        for id in [EGRESS_DIRECT, EGRESS_WG, EGRESS_PROXY] {
            assert!(m.desired_up(id), "空偏好 = 全部默认在线");
            assert_eq!(m.view().fallback_of(id), EgressFallback::Block);
        }
    }

    #[test]
    fn reload_quiet_window_does_not_read_rebuild_as_disconnect() {
        // 决议 §7.2：普通规则变化触发的 reload 会重建 outbound，期间探测失败不得被解释为
        // 「出口已断线 / 已重连」。
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(40));
        m.begin_reload_quiet_window();
        assert!(m.in_reload_quiet_window());

        for _ in 0..5 {
            assert!(
                !m.record_probe(EGRESS_WG, &ProbeOutcome::failed("outbound 重建中")),
                "静默窗口内不得推进状态机"
            );
        }
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(s.lifecycle, EgressLifecycle::Ready, "生命周期保持不变");
        assert_eq!(s.reconnect_count, 0, "不得计为重连");
        assert_eq!(
            s.health.state,
            HealthState::Unhealthy,
            "但健康探测的事实要如实记录，不隐瞒"
        );

        // 窗口内的**成功**探测照常生效（恢复得越早越好）。
        assert!(!m.record_probe(EGRESS_WG, &ProbeOutcome::ok(35)));
        assert_eq!(
            m.get(EGRESS_WG, &wg_settings(), &RuleSet::default())
                .unwrap()
                .health
                .state,
            HealthState::Healthy
        );
    }

    #[test]
    fn failures_outside_quiet_window_still_escalate() {
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(40));
        assert!(!m.in_reload_quiet_window(), "默认不在静默窗口内");
        m.record_probe(EGRESS_WG, &ProbeOutcome::failed("真的断了"));
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(s.lifecycle, EgressLifecycle::Reconnecting);
        assert_eq!(s.reconnect_count, 1);
    }

    #[test]
    fn probe_failure_does_not_rewrite_config() {
        // 探测失败不进 view——否则一次抖动就触发 reload 风暴。
        let m = EgressManager::new();
        for _ in 0..5 {
            m.record_probe(EGRESS_WG, &ProbeOutcome::failed("超时"));
        }
        assert_eq!(
            m.get(EGRESS_WG, &wg_settings(), &RuleSet::default())
                .unwrap()
                .lifecycle,
            EgressLifecycle::Failed
        );
        assert!(
            m.view().unavailable.is_empty(),
            "只有用户显式 Stop 才改写配置"
        );
    }

    #[test]
    fn stop_marks_unavailable_and_start_restores() {
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(30));
        assert_eq!(m.set_desired_up(EGRESS_WG, false), Some(true));
        let view = m.view();
        assert!(view.is_unavailable(EGRESS_WG));
        assert!(!view.is_unavailable(EGRESS_DIRECT), "只影响被停的那个出口");
        assert_eq!(
            m.get(EGRESS_WG, &wg_settings(), &RuleSet::default())
                .unwrap()
                .lifecycle,
            EgressLifecycle::Stopped
        );

        assert_eq!(m.set_desired_up(EGRESS_WG, true), Some(false));
        assert!(m.view().unavailable.is_empty());
        assert_eq!(
            m.get(EGRESS_WG, &wg_settings(), &RuleSet::default())
                .unwrap()
                .lifecycle,
            EgressLifecycle::Starting,
            "重新启动后先进 Starting，等探测结果才敢称 Ready"
        );
        assert_eq!(m.set_desired_up("nope", true), None);
    }

    #[test]
    fn engine_stop_clears_mihomo_managed_runtime_state() {
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(30));
        m.set_active_connections([(EGRESS_WG.to_string(), 2)].into_iter().collect());
        assert_eq!(m.lifecycle(EGRESS_WG), Some(EgressLifecycle::Ready));

        m.mark_engine_stopped();

        let status = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(status.lifecycle, EgressLifecycle::Stopped);
        assert_eq!(status.health.state, HealthState::Unknown);
        assert_eq!(status.active_connections, 0);
        assert!(m.desired_up(EGRESS_WG), "引擎停止不应覆盖用户的在线意图");
    }

    #[test]
    fn unconfigured_egress_never_reports_ready() {
        // 默认设置没有 WG：即便（异常地）记了一次成功探测，对外也不能显示 Ready。
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(10));
        let s = m
            .get(
                EGRESS_WG,
                &NetPolicySettings::default(),
                &RuleSet::default(),
            )
            .unwrap();
        assert!(!s.configured);
        assert_eq!(s.lifecycle, EgressLifecycle::Stopped);
        assert!(s.unconfigured_reason.is_some());
    }

    #[test]
    fn test_only_probe_updates_health_but_not_lifecycle() {
        let m = EgressManager::new();
        m.record_probe(EGRESS_WG, &ProbeOutcome::ok(30));
        m.record_test_only(EGRESS_WG, &ProbeOutcome::failed("测试失败"));
        let s = m
            .get(EGRESS_WG, &wg_settings(), &RuleSet::default())
            .unwrap();
        assert_eq!(
            s.lifecycle,
            EgressLifecycle::Ready,
            "「仅测试连接」不得改变出口生命周期"
        );
        assert_eq!(s.health.state, HealthState::Unhealthy);
    }

    #[test]
    fn usage_counts_flow_into_status() {
        let rules = RuleSet {
            rules: vec![Rule::DomainSuffix {
                value: "a.com".into(),
                route: Route::Wg,
            }],
            groups: vec![],
        };
        let m = EgressManager::new();
        let s = m.get(EGRESS_WG, &wg_settings(), &rules).unwrap();
        assert!(s.selected);
        assert!(s.usage.is_default, "默认出口是 WG");
        assert_eq!(s.usage.rule_count, 1);
        // 停用后策略仍指向它 → UI 必须报警。
        m.set_desired_up(EGRESS_WG, false);
        let s = m.get(EGRESS_WG, &wg_settings(), &rules).unwrap();
        assert!(s.selected_but_unusable());
    }

    #[test]
    fn fallback_defaults_to_block_and_is_per_egress() {
        let m = EgressManager::new();
        assert_eq!(
            m.set_fallback(EGRESS_WG, EgressFallback::Direct),
            Some(EgressFallback::Block),
            "默认必须 fail-closed"
        );
        let view = m.view();
        assert_eq!(view.fallback_of(EGRESS_WG), EgressFallback::Direct);
        assert_eq!(view.fallback_of(EGRESS_PROXY), EgressFallback::Block);
    }
}
