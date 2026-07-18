//! agent 运行态：Runtime（applied/pid/secret）+ 可观测 + **operation 作业追踪** + 状态计算。
//!
//! 进度事件从 Tauri `emit` 改为**框架无关的 broadcast channel**（设计 §8.2）；长操作作为**独立作业**
//! 执行、持 op 互斥、断线不亡（§8.1/D7）。

use crate::engine;
use crate::firewall;
use crate::observe::Observatory;
use crate::store::{now_ms, Store};
use crate::win;
use net_policy_core::config::{self, ProcessRef, TempDirect};
use net_policy_core::operation::{
    ApplyProgress, OperationInfo, OperationKind, OperationResult, OperationStatus,
};
use net_policy_core::protocol::Event as ProtoEvent;
use net_policy_core::types::{NetPolicyStatus, RequestLogEntry, TempDirectStatus};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// 临时直连运行态（`gen` 单调递增，供过期定时器辨认是否被后续操作取代）。
#[derive(Default)]
struct TempState {
    active: bool,
    until_ms: u64,
    except: Vec<ProcessRef>,
    gen: u64,
}

/// temp 态快照（事务化回滚用；不含 gen——restore 会 bump gen）。
pub struct TempSnapshot {
    active: bool,
    until_ms: u64,
    except: Vec<ProcessRef>,
}

/// 防火墙白名单模型是否已在新形态下真机验证。未验证前 `protected` 仅算"实验保护"。
const FIREWALL_MODEL_VALIDATED: bool = false;
// GUI 每 3 秒拉一次状态；缓存必须短于轮询周期，否则一次切换可能连续展示两轮旧值。
const STATUS_CACHE_TTL: Duration = Duration::from_secs(2);
/// broadcast 容量：慢订阅者落后会丢老事件（设计 §4.1：事件不保证不丢，以真实态对齐）。
const EVENT_CHANNEL_CAP: usize = 512;

#[derive(Default)]
pub struct Runtime {
    pub applied: bool,
    pub mihomo_pid: Option<u32>,
    pub secret: Option<String>,
    /// 最近一次成功落地的 kill-switch 配置指纹；接管旧进程时未知，首次 reload 会自愈重铺。
    pub firewall_signature: Option<String>,
}

pub struct AgentState {
    pub workspace: PathBuf,
    /// 开发模式（允许 `MIHOMO_BIN` 覆盖 + 跳过安装目录严格校验）。production 为 false。
    pub dev: bool,
    pub rt: Mutex<Runtime>,
    pub obs: Observatory,

    /// 长操作互斥标志（true = 有 apply/stop/reload/enabled 在跑）。同步 flag → 立即 operation_conflict。
    op_flag: Mutex<bool>,
    op_counter: AtomicU64,
    current_op: Mutex<Option<OperationInfo>>,
    events: broadcast::Sender<ProtoEvent>,
    status_cache: Mutex<Option<(Instant, NetPolicyStatus)>>,

    /// 持久化记录（请求历史 + 生命周期事件）。
    pub store: Arc<Store>,
    /// 临时直连（限时应急）运行态。
    temp: Mutex<TempState>,
    /// 抓包会话管理（单会话；真实 pktmon 后端，抓包设计 §7）。入口与 op_flag/L4 做互斥检查。
    pub capture: crate::capture::CaptureManager,
    /// L4 解密会话 + CA 管理（单会话；自研 net-policy-mitm 引擎，抓包设计 §17）。
    pub decrypt: crate::decrypt_manager::DecryptManager,
    /// 出口生命周期（Direct/WireGuard/Proxy）。**独立于流量导流**：出口是否连着与是否被策略
    /// 选中是两个维度（出口设计 §2.1）。
    pub egress: crate::egress::EgressManager,
}

impl AgentState {
    pub fn new(workspace: PathBuf, dev: bool) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAP);
        let store = Arc::new(Store::open_or_memory(&workspace));
        let capture = crate::capture::CaptureManager::new(&workspace);
        let decrypt = crate::decrypt_manager::DecryptManager::new(&workspace);
        Self {
            workspace,
            dev,
            rt: Mutex::new(Runtime::default()),
            obs: Observatory::default(),
            op_flag: Mutex::new(false),
            op_counter: AtomicU64::new(0),
            current_op: Mutex::new(None),
            events,
            status_cache: Mutex::new(None),
            store,
            temp: Mutex::new(TempState::default()),
            capture,
            decrypt,
            egress: crate::egress::EgressManager::new(),
        }
    }

    /// 配置生成用的出口运行态视图（被显式停用的出口 + 各自 fallback）。
    pub fn egress_view(&self) -> net_policy_core::egress::EgressRuntimeView {
        self.egress.view()
    }

    /// 当前出口清单（合并磁盘上的 settings/rules 与运行态）。设置损坏时退回默认值——
    /// 出口面是只读观测，不该因配置损坏而整个不可见。
    pub fn egress_list(&self) -> Vec<net_policy_core::egress::EgressStatus> {
        let settings = config::try_load_settings(&self.workspace).unwrap_or_default();
        let rules = config::try_load_rules(&self.workspace).unwrap_or_default();
        self.egress.list(&settings, &rules)
    }

    /// 推送单个出口的生命周期变迁。**与策略事件分开**：前端收到它只更新出口卡片，
    /// 不得据此推断导流已变（出口设计 §8.8）。
    pub fn publish_egress(&self, id: &str) {
        if let Some(egress) = self.egress_list().into_iter().find(|e| e.id == id) {
            self.emit(ProtoEvent::EgressChanged {
                egress: Box::new(egress),
            });
        }
    }

    // ── 生命周期事件 / 请求记录 ──────────────────────────────────────────────

    pub fn record_event(&self, kind: &str, detail: &str) {
        self.store.record_event(kind, detail);
    }

    pub fn record_request(&self, e: &RequestLogEntry) {
        self.store.record_request(e);
    }

    // ── 临时直连（限时应急） ─────────────────────────────────────────────────

    /// 配置生成用的临时直连视图（过期视为未激活）。
    pub fn temp_for_config(&self) -> TempDirect {
        let t = self.temp.lock().unwrap();
        if t.active && now_ms() < t.until_ms {
            TempDirect {
                active: true,
                except: t.except.clone(),
            }
        } else {
            TempDirect::default()
        }
    }

    /// 临时直连状态（剩余时间等）。
    pub fn temp_status(&self) -> TempDirectStatus {
        let t = self.temp.lock().unwrap();
        let now = now_ms();
        let active = t.active && now < t.until_ms;
        TempDirectStatus {
            active,
            until_ms: if active { Some(t.until_ms) } else { None },
            remaining_secs: if active { (t.until_ms - now) / 1000 } else { 0 },
            except: if active { t.except.clone() } else { Vec::new() },
        }
    }

    /// 开启临时直连，返回本次 `gen`（供过期定时器辨认）。
    pub fn set_temp(&self, duration_secs: u64, except: Vec<ProcessRef>) -> u64 {
        let mut t = self.temp.lock().unwrap();
        t.active = true;
        t.until_ms = now_ms() + duration_secs.saturating_mul(1000);
        t.except = except;
        t.gen += 1;
        t.gen
    }

    /// 解除临时直连；返回是否原本处于激活态。
    pub fn clear_temp(&self) -> bool {
        let mut t = self.temp.lock().unwrap();
        let was = t.active;
        t.active = false;
        t.gen += 1;
        was
    }

    /// 当前 temp 代次（过期定时器用它判断是否被后续 set/clear 取代）。
    pub fn temp_generation(&self) -> u64 {
        self.temp.lock().unwrap().gen
    }

    pub fn temp_active_flag(&self) -> bool {
        self.temp.lock().unwrap().active
    }

    /// 快照当前 temp 态（供事务化操作失败时回滚，评审点 1）。
    pub fn temp_snapshot(&self) -> TempSnapshot {
        let t = self.temp.lock().unwrap();
        TempSnapshot {
            active: t.active,
            until_ms: t.until_ms,
            except: t.except.clone(),
        }
    }

    /// 回滚到快照（bump `gen` 使失败那次 set 挂的过期定时器失效）。
    pub fn restore_temp(&self, s: TempSnapshot) {
        let mut t = self.temp.lock().unwrap();
        t.active = s.active;
        t.until_ms = s.until_ms;
        t.except = s.except;
        t.gen += 1;
    }

    // ── 事件 ─────────────────────────────────────────────────────────────────

    pub fn subscribe_events(&self) -> broadcast::Receiver<ProtoEvent> {
        self.events.subscribe()
    }

    fn emit(&self, ev: ProtoEvent) {
        // 无订阅者时 send 返回 Err，忽略（进度仅可观测性）。
        let _ = self.events.send(ev);
    }

    /// 发一条 apply 进度（同时更新 current_op 的 step/name）。
    pub fn emit_progress(&self, step: usize, status: &str, detail: Option<String>) {
        trace_step(step, status, detail.as_deref()); // 同步落 trace 文件，绕过缓冲日志——卡死/崩溃也能看到进度到了哪步
        let p = ApplyProgress::new(step, status, detail);
        if let Some(op) = self.current_op.lock().unwrap().as_mut() {
            op.step = Some(p.step);
            op.name = Some(p.name.clone());
        }
        self.emit(ProtoEvent::ApplyProgress { progress: p });
    }

    // ── operation 作业追踪 ───────────────────────────────────────────────────

    /// 尝试开始一个长操作。返回 `Some(id)` 表示占用成功；`None` = 已有操作在跑或有活跃抓包/解密会话
    /// （立即 conflict）。用户发起的 apply/reload/stop 走这个（§7：不干扰活跃会话）。
    pub fn try_begin_op(&self, kind: OperationKind) -> Option<u64> {
        self.try_begin_op_inner(kind, false)
    }

    /// **会话生命周期内部**用（如 L4 解密起停时的自动导流 reload）：只与其它长操作互斥，**不**因
    /// 自身的活跃抓包/解密会话而 conflict——否则 reload 会被它要服务的那个会话挡死（§17.3 导流）。
    pub fn try_begin_op_internal(&self, kind: OperationKind) -> Option<u64> {
        self.try_begin_op_inner(kind, true)
    }

    fn try_begin_op_inner(&self, kind: OperationKind, allow_active_session: bool) -> Option<u64> {
        let mut flag = self.op_flag.lock().unwrap();
        if *flag {
            return None;
        }
        if !allow_active_session
            && (self.capture.active_id().is_some() || self.decrypt.active_id().is_some())
        {
            return None;
        }
        *flag = true;
        let id = self.op_counter.fetch_add(1, Ordering::SeqCst) + 1;
        *self.current_op.lock().unwrap() = Some(OperationInfo {
            id,
            kind,
            status: OperationStatus::Running,
            step: None,
            name: None,
            error: None,
        });
        Some(id)
    }

    /// 结束一个长操作：写终态 + 广播 OperationFinished + 释放互斥。
    pub fn end_op(
        &self,
        id: u64,
        kind: OperationKind,
        status: OperationStatus,
        error: Option<String>,
    ) {
        {
            let mut cur = self.current_op.lock().unwrap();
            if let Some(op) = cur.as_mut() {
                if op.id == id {
                    op.status = status;
                    op.error = error.clone();
                }
            }
        }
        self.emit(ProtoEvent::OperationFinished {
            result: OperationResult {
                id,
                kind,
                status,
                error,
            },
        });
        *self.op_flag.lock().unwrap() = false;
    }

    pub fn current_operation(&self) -> Option<OperationInfo> {
        self.current_op.lock().unwrap().clone()
    }

    /// 是否有长操作在跑（写配置请求据此返回 operation_conflict，评审点 7）。
    pub fn op_in_flight(&self) -> bool {
        *self.op_flag.lock().unwrap()
    }

    // ── 状态计算 ─────────────────────────────────────────────────────────────

    pub fn invalidate_status_cache(&self) {
        *self.status_cache.lock().unwrap() = None;
    }

    fn cached_status(&self) -> Option<NetPolicyStatus> {
        self.status_cache
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(at, _)| at.elapsed() < STATUS_CACHE_TTL)
            .map(|(_, v)| v.clone())
    }

    fn store_status(&self, s: &NetPolicyStatus) {
        *self.status_cache.lock().unwrap() = Some((Instant::now(), s.clone()));
    }

    /// 计算当前状态快照（**含多次 PowerShell/注册表冷探测**，阻塞——调用方须在 spawn_blocking 内）。
    pub fn compute_status(&self) -> NetPolicyStatus {
        let settings = config::load_settings(&self.workspace);
        let (applied, secret) = {
            let rt = self.rt.lock().unwrap();
            (rt.applied, rt.secret.clone())
        };
        let (fw, mihomo_running, tun_up, elevated) = if win::is_windows() {
            let secret = secret.clone().unwrap_or_default();
            std::thread::scope(|s| {
                let fw = s.spawn(firewall::status);
                let run = s.spawn(|| engine::running(&secret));
                let tun = s.spawn(engine::tun_up);
                let el = s.spawn(win::is_elevated);
                (
                    fw.join().ok().and_then(|r| r.ok()),
                    run.join().unwrap_or(false),
                    tun.join().unwrap_or(false),
                    el.join().unwrap_or(false),
                )
            })
        } else {
            (None, false, false, false)
        };
        let tun_ready = mihomo_running && tun_up;
        let protected = settings.killswitch_enabled
            && fw.as_ref().map(|f| f.active).unwrap_or(false)
            && mihomo_running
            && tun_ready;
        let status = NetPolicyStatus {
            platform_supported: win::is_windows(),
            wg_configured: settings.wg.validate().is_ok(),
            killswitch_enabled: settings.killswitch_enabled,
            applied,
            mihomo_running,
            tun_ready,
            protected,
            protection_validated: FIREWALL_MODEL_VALIDATED,
            firewall: fw,
            default_route: settings.default_route,
            enabled: settings.enabled,
            elevated,
            record_store_degraded: self.store.is_degraded(),
        };
        self.store_status(&status);
        status
    }

    /// 带 5s 缓存的状态（异步；阻塞探测挪出当前线程）。
    pub async fn status_cached(self: std::sync::Arc<Self>) -> NetPolicyStatus {
        if let Some(s) = self.cached_status() {
            return s;
        }
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.compute_status())
            .await
            .unwrap_or_else(|_| fallback_status())
    }

    /// 强制新鲜状态（写操作后用）。
    pub async fn status_fresh(self: std::sync::Arc<Self>) -> NetPolicyStatus {
        self.invalidate_status_cache();
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.compute_status())
            .await
            .unwrap_or_else(|_| fallback_status())
    }
}

fn fallback_status() -> NetPolicyStatus {
    NetPolicyStatus {
        platform_supported: win::is_windows(),
        wg_configured: false,
        killswitch_enabled: false,
        applied: false,
        mihomo_running: false,
        tun_ready: false,
        protected: false,
        protection_validated: FIREWALL_MODEL_VALIDATED,
        firewall: None,
        default_route: net_policy_core::config::Route::Direct,
        enabled: false,
        elevated: false,
        record_store_degraded: false,
    }
}

/// 同步把 apply/reload 的每步进度追加到 `{USERPROFILE}\log\net-policy-agent-trace.log`（**同步写+flush**，
/// 绕过 flexi 的缓冲日志）。用途：定位「切姿态 / auto 时卡死或崩溃」——缓冲日志卡死时看不到，这个能实时
/// 落盘看到进度到了哪一步（最后一行即卡/崩的位置）。失败静默（诊断设施不应影响主流程）。
fn trace_step(step: usize, status: &str, detail: Option<&str>) {
    use std::io::Write;
    let Ok(profile) = std::env::var("USERPROFILE") else {
        return;
    };
    let dir = std::path::Path::new(&profile).join("log");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("net-policy-agent-trace.log"))
    {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "[{ms}] step={step} {status} {}", detail.unwrap_or(""));
        let _ = f.flush();
    }
}

/// 初始化：确保 workspace 子目录 + 接管存活旧 mihomo（读 generated secret）+ 按 `enabled` 自动恢复 +
/// 常驻被阻断采集器。**operations/current 在 agent 重启后无内存态**，由此处按真实机器状态对齐（不谎报）。
pub fn setup(state: std::sync::Arc<AgentState>) {
    let dir = config::net_policy_dir(&state.workspace);
    std::fs::create_dir_all(dir.join("generated")).ok();
    state.record_event("agent_start", "");
    if !win::is_windows() {
        return;
    }

    // 请求记录采样器：mihomo 在跑时每 3s 拉一次活跃连接，新连接（按 conn_id 去重）写库 +
    // 喂 obs 的域名关联。周期性 prune 控制表大小。
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick: u64 = 0;
            loop {
                let secret = { st.rt.lock().unwrap().secret.clone() };
                if let Some(s) = secret {
                    if !s.is_empty() {
                        let snap = crate::connections::fetch(&s).await;
                        st.obs.ingest_connections(&snap.connections);
                        // 出口的「使用状态」层（决议 §3.4）：此刻真的有多少连接在走它。
                        // 与「策略选中」严格区分——规则指了但没人访问时这里就是 0。
                        st.egress.set_active_connections(
                            [
                                (
                                    net_policy_core::egress::EGRESS_WG.to_string(),
                                    snap.wg_count,
                                ),
                                (
                                    net_policy_core::egress::EGRESS_PROXY.to_string(),
                                    snap.proxy_count,
                                ),
                                (
                                    net_policy_core::egress::EGRESS_DIRECT.to_string(),
                                    snap.direct_count,
                                ),
                            ]
                            .into_iter()
                            .collect(),
                        );
                        let ts = now_ms();
                        for c in &snap.connections {
                            if c.host.trim().is_empty() && c.destination_ip.trim().is_empty() {
                                continue;
                            }
                            st.record_request(&RequestLogEntry {
                                ts_ms: ts,
                                conn_id: c.id.clone(),
                                process: c.process.clone(),
                                process_path: c.process_path.clone(),
                                host: c.host.clone(),
                                dest_ip: c.destination_ip.clone(),
                                dest_port: c.destination_port.clone(),
                                network: c.network.clone(),
                                outbound: c.outbound.clone(),
                                rule: c.rule.clone(),
                            });
                        }
                    }
                }
                tick += 1;
                if tick.is_multiple_of(200) {
                    st.store.prune();
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    let mut recovered = false;
    if let Some(secret) = config::read_generated_secret(&state.workspace) {
        if engine::running(&secret) {
            let mut rt = state.rt.lock().unwrap();
            rt.applied = true;
            rt.secret = Some(secret);
            rt.mihomo_pid = None; // pid 跨重启不可知；graceful_stop 回退按名停。
            recovered = true;
        }
    }

    // 出口偏好回填（决议 §7.3）：必须在自动 apply **之前**，否则首次生成的配置会带上被用户
    // 停用的出口，等于开机瞬间恢复了一条用户已经关掉的出口。
    if let Ok(s) = config::try_load_settings(&state.workspace) {
        state.egress.hydrate(&s.egress_prefs);
    }

    let auto_apply = match config::try_load_settings(&state.workspace) {
        Ok(s) => s.enabled,
        Err(e) => {
            log::error!("net-policy 设置损坏，拒绝自动应用：{e:#}");
            false
        }
    };
    if !recovered && auto_apply {
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::ops::run_apply_operation(st).await {
                log::warn!("net-policy 启动自动应用失败：{e}");
            }
        });
    }

    // 常驻**出口探活循环**（决议 §3.3）：与策略是否选中该出口无关，解决 mihomo userspace WG
    // 「没流量就不握手」导致的误判——切到直连后 WG 仍会被定期探测，状态不会假性掉线。
    //
    // **只探周期探活有意义的出口**（[`EgressKind::probe_on_schedule`]）：
    // - WG：隧道可保活，周期探测正是它的价值所在；
    // - Direct：探的是本机物理网络，不建立任何远端会话，成本可忽略；
    // - Proxy：**排除**。决议 §3.4 明确「没有业务流量时，不应为了展示在线而强行维持每个代理
    //   节点的连接」——周期打 `/proxies/subscription-out/delay` 恰恰是在强行建连。代理出口改为
    //   按需探测（用户点「探测节点」/ 切换节点 / 启动出口时各探一次）。
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(crate::egress::PROBE_INTERVAL).await;
                let settings = match config::try_load_settings(&st.workspace) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let secret = { st.rt.lock().unwrap().secret.clone() };
                let engine_up = secret.as_deref().is_some_and(|s| !s.is_empty());

                // 订阅状态采样（决议 §3.4 的「订阅层 / 节点层」）：**纯读 mihomo 已有信息**，
                // 不做 delay 探测、不触发任何节点连接——代理订阅是配置来源，不是持续隧道。
                if engine_up && settings.proxy_subscriptions.active_subscription().is_some() {
                    if let Some(s) = secret.as_deref() {
                        st.egress.set_subscription(
                            crate::egress::subscription_snapshot(s, &settings).await,
                        );
                    }
                }

                for d in net_policy_core::egress::catalog(&settings) {
                    // 未配置 / 已停用 / 不适合周期探活的出口一律跳过（不制造无意义的失败噪声，
                    // 也不为了「看起来在线」而替代理订阅强行建连）。
                    if !d.configured || !d.kind.probe_on_schedule() || !st.egress.desired_up(&d.id)
                    {
                        continue;
                    }
                    // 隧道类出口的数据面由 mihomo 承载：引擎没跑就是 Stopped，不是 Failed
                    // ——把「没启动」误报成「连不上」会把用户引向错误的排查方向。
                    if !engine_up && d.kind != net_policy_core::egress::EgressKind::Direct {
                        if st
                            .egress
                            .set_lifecycle(&d.id, net_policy_core::egress::EgressLifecycle::Stopped)
                        {
                            st.publish_egress(&d.id);
                        }
                        continue;
                    }
                    let outcome = crate::egress::probe(&d.id, &settings, secret.as_deref()).await;
                    if st.egress.record_probe(&d.id, &outcome) {
                        st.publish_egress(&d.id);
                    }
                }
            }
        });
    }

    // 常驻「被阻断尝试」采集器（读 rt.secret，mihomo 在跑就连 /logs WS；断线 3s 重连）。
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let secret = { st.rt.lock().unwrap().secret.clone() };
            if let Some(s) = secret {
                if !s.is_empty() {
                    let _ = crate::observe::stream_logs(&s, &st.obs).await;
                }
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    });
}
