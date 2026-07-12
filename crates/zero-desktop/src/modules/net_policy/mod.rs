//! net-policy 模块：**观察者优先**。默认出口三档姿态（直连观察 / 海外VPN / 阻断收紧，缺省直连），
//! 与 SBN 解耦；防火墙 kill-switch 仅在海外/阻断姿态挂（fail-closed），观察姿态不挂（透明）。
//! 关窗/崩溃保持运行、下次启动接管；仅手动「停止」拆除并清 enabled。
//!
//! 设计见 docs/net-policy-observer-first-design.md（行为模型）+ docs/unified-desktop-shell-design.md §14
//! （原始设计）；落地依据的真机验证见 docs/net-policy-validation-report.md（§0.x 实测结论）。**仅 Windows**。
//!
//! 所有 Tauri command 名称以 `net_policy_` 开头。

mod config;
mod connections;
mod engine;
mod firewall;
mod observe;
mod process_watch;
mod valid;
mod verify;
mod win;

use crate::app_state::AppState;
use anyhow::{bail, Context, Result};
use config::{NetPolicySettings, Route, Rule, RuleSet};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::State;

/// net-policy 模块状态。
pub struct NetPolicyState {
    pub workspace: PathBuf,
    rt: Mutex<Runtime>,
    status_cache: Mutex<Option<StatusCache>>,
    status_compute: Mutex<()>,
    /// **操作级互斥**：apply / stop / reload 三类会「写 config.yaml + 动防火墙 + 改 rt」的操作
    /// 必须串行。这些 command 都是 async 可并发进入——没有它，双击「应用」会起两个 mihomo 抢
    /// 9090，apply 步 3 轮询的 7s 窗口里穿插 reload 会交叉覆盖 config.yaml、防火墙状态错乱。
    /// tokio Mutex（跨 await 持有）；只在 do_apply/do_stop/net_policy_reload 顶部各获取一次，
    /// `net_policy_set_enabled` 不自己持锁（它只是委托，避免重入死锁）。
    ops: tokio::sync::Mutex<()>,
    /// Phase 4 可观测：被阻断尝试 feed + 域名↔IP/进程 关联（内部 Mutex，跨 command 共享）。
    obs: observe::Observatory,
}

#[derive(Default)]
struct Runtime {
    /// 是否已 apply（mihomo + 可选 kill-switch）。
    applied: bool,
    mihomo_pid: Option<u32>,
    /// 本次 apply 生成的 external-controller secret（鉴权 mihomo API，P0-1）。
    secret: Option<String>,
}

struct StatusCache {
    at: Instant,
    value: NetPolicyStatus,
}

/// 新防火墙白名单模型（`Program=mihomo.exe`，§0.10.1）是否已在新模型下重跑 VP-08/09/10 通过。
/// 在重新真机验证通过前为 `false`——`protected` 仅算"实验保护"，前端须如实标注（P0-2）。
const FIREWALL_MODEL_VALIDATED: bool = false;
const STATUS_CACHE_TTL: Duration = Duration::from_secs(5);

impl NetPolicyState {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            rt: Mutex::new(Runtime::default()),
            status_cache: Mutex::new(None),
            status_compute: Mutex::new(()),
            ops: tokio::sync::Mutex::new(()),
            obs: observe::Observatory::default(),
        }
    }
}

/// 初始化：确保 workspace 子目录存在 + 恢复存活旧实例 +（**启动即生效**）按 `enabled` 自动应用。
///
/// **P1（secret 恢复）**：上次 apply 的 mihomo + kill-switch 可能在应用重启后仍存活（防火墙规则
/// 跨重启持久、mihomo 若注册为服务/计划任务也可能仍在）。此时从生成的 `config.yaml` 解析出
/// controller secret，并在确认 mihomo 仍可鉴权访问时恢复运行态——否则重启后 `secret` 丢失会导致
/// `get_status`/`emergency_stop` 用空 secret 鉴权失败、无法管理或停掉旧实例（防火墙被卡死）。
///
/// **自动应用（与首版不同）**：仅当 `settings.enabled == true`（用户曾在 UI 显式开启主开关）且当前
/// 无存活旧实例时，后台异步跑一遍 apply 恢复上次策略（首次=默认黑洞、全阻断）。`enabled` 默认 false，
/// 故全新安装**不会**擅自改全局防火墙；用户开启后此后每次启动自动恢复。
pub fn setup(app: &tauri::AppHandle, state: Arc<NetPolicyState>) -> Result<()> {
    let dir = config::net_policy_dir(&state.workspace);
    std::fs::create_dir_all(dir.join("generated")).ok();
    if win::is_windows() {
        let mut recovered = false;
        if let Some(secret) = config::read_generated_secret(&state.workspace) {
            // 仅在旧实例确实仍在（且 secret 有效可鉴权）时恢复，避免把陈旧配置误判为已应用。
            if engine::running(&secret) {
                let mut rt = state.rt.lock().unwrap();
                rt.applied = true;
                rt.secret = Some(secret);
                // pid 跨进程重启不可知；graceful_stop 在 pid=None 时回退按二进制名停。
                rt.mihomo_pid = None;
                recovered = true;
            }
        }
        // 启动即生效：enabled 且没有存活旧实例 → 后台自动 apply（不阻塞 setup / UI 启动）。
        let auto_apply = match config::try_load_settings(&state.workspace) {
            Ok(settings) => settings.enabled,
            Err(e) => {
                log::error!("net-policy 设置损坏，拒绝自动应用：{e:#}");
                false
            }
        };
        if !recovered && auto_apply {
            let np = state.clone();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = do_apply(np, app).await {
                    log::warn!("net-policy 启动自动应用失败：{e}");
                }
            });
        }

        // Phase 4：常驻「被阻断尝试」采集器。读 rt.secret，mihomo 在跑就连 /logs WS 流；
        // WS 断/换 secret（重 apply）后回到循环、3s 后重连。缓冲跨重连保留，自愈无需显式停。
        let np = state.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let secret = { np.rt.lock().unwrap().secret.clone() };
                match secret {
                    Some(s) if !s.is_empty() => {
                        let _ = observe::stream_logs(&s, &np.obs).await;
                    }
                    _ => {}
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }
    Ok(())
}

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

/// 离线生成产物（供 CLI `net-policy-gen` 预览 / 真机验证用，不执行任何副作用）。
/// 返回 `(mihomo_config_yaml, firewall_apply_script)`，输入取自 workspace 的 settings/rules。
pub fn gen_artifacts(workspace: &std::path::Path) -> Result<(String, String)> {
    let settings = config::try_load_settings(workspace)?;
    let rules = config::try_load_rules(workspace)?;
    let cfg = engine::generate_config(&settings, &rules, "<runtime-secret>");
    let fw =
        firewall::build_apply_script(workspace, &settings, &rules, &engine::mihomo_bin(workspace))?;
    Ok((cfg, fw))
}

// ============ 状态 ============

#[derive(Clone, Debug, Serialize)]
pub struct NetPolicyStatus {
    pub platform_supported: bool,
    pub wg_configured: bool,
    pub killswitch_enabled: bool,
    pub applied: bool,
    pub mihomo_running: bool,
    /// mihomo TUN（Meta 适配器）是否已起栈并 Up（P2）。controller 可达但 TUN 未起栈时为 false——
    /// 用于区分"kill-switch 已阻断但隧道未连通"（fail-closed 仍成立，但应用无法联网）与真正连通。
    /// 真实出口可达性 / DNS 劫持等更重的探测放在按需的 `net_policy_verify`（避免每次轮询打网络）。
    pub tun_ready: bool,
    /// 是否处于"受保护"状态：kill-switch 启用 + 防火墙默认出站已 Block + mihomo 在跑 **且 TUN 已起栈**。
    /// 若 false 且 applied=true，前端据 firewall.active 区分"不受保护预览"与"已阻断但未连通"（P0-1/P2）。
    pub protected: bool,
    /// 当前防火墙白名单模型是否已真机验证（P0-2）。false=实验保护，不能宣称 fail-closed。
    pub protection_validated: bool,
    pub firewall: Option<firewall::FirewallStatus>,
    /// 默认出口（未命中规则的兜底）：`blackhole`（全阻断）或 `wg`（全走海外）。前端据此渲染。
    pub default_route: Route,
    /// 主开关:是否已启用「启动即生效」（持久化在 settings.enabled）。
    pub enabled: bool,
    /// 当前进程是否以管理员身份运行。false → 改防火墙/建 TUN 会被拒，前端据此提示并禁用「开始观察」。
    pub elevated: bool,
}

fn cached_status(np: &NetPolicyState) -> Option<NetPolicyStatus> {
    let cache = np.status_cache.lock().unwrap();
    cache
        .as_ref()
        .filter(|c| c.at.elapsed() < STATUS_CACHE_TTL)
        .map(|c| c.value.clone())
}

fn store_status_cache(np: &NetPolicyState, status: &NetPolicyStatus) {
    let mut cache = np.status_cache.lock().unwrap();
    *cache = Some(StatusCache {
        at: Instant::now(),
        value: status.clone(),
    });
}

fn invalidate_status_cache(np: &NetPolicyState) {
    let mut cache = np.status_cache.lock().unwrap();
    *cache = None;
}

fn compute_status_cached(np: &NetPolicyState) -> NetPolicyStatus {
    if let Some(status) = cached_status(np) {
        return status;
    }

    let _guard = np.status_compute.lock().unwrap();
    if let Some(status) = cached_status(np) {
        return status;
    }

    let status = compute_status(np);
    store_status_cache(np, &status);
    status
}

fn compute_status_fresh(np: &NetPolicyState) -> NetPolicyStatus {
    let _guard = np.status_compute.lock().unwrap();
    let status = compute_status(np);
    store_status_cache(np, &status);
    status
}

/// 计算当前状态快照。**含多次 PowerShell 冷启动**（firewall::status / engine::running /
/// engine::tun_up），是阻塞操作——**绝不能在主线程直接跑**，否则 webview 卡死（见
/// `net_policy_get_status` 注释）。调用方必须在 `spawn_blocking` 或已有的阻塞任务里调它。
fn compute_status(np: &NetPolicyState) -> NetPolicyStatus {
    let settings = config::load_settings(&np.workspace);
    let (applied, secret) = {
        let rt = np.rt.lock().unwrap();
        (rt.applied, rt.secret.clone())
    };
    // 三个 Windows 探测各自冷启动一个 powershell（firewall::status 的 Get-NetFirewallRule、
    // engine::running 的 controller /version、engine::tun_up 的 Get-NetAdapter），单个就 ~0.3–1s。
    // 串行叠加是「全景图不秒出」的根因——改为**并发**跑，墙钟降到最慢的那一个。
    // （原先 tun_up 在 !running 时跳过省一次 spawn，并发后无此必要；语义仍是 running && tun_up。）
    let (firewall, mihomo_running, tun_up, elevated) = if win::is_windows() {
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
        && firewall.as_ref().map(|f| f.active).unwrap_or(false)
        && mihomo_running
        && tun_ready;
    NetPolicyStatus {
        platform_supported: win::is_windows(),
        // 精确反映「SBN 是否已配」——与默认出口/enabled 解耦（不再借 settings.validate 间接判断）。
        wg_configured: settings.wg.validate().is_ok(),
        killswitch_enabled: settings.killswitch_enabled,
        applied,
        mihomo_running,
        tun_ready,
        protected,
        protection_validated: FIREWALL_MODEL_VALIDATED,
        firewall,
        default_route: settings.default_route,
        enabled: settings.enabled,
        elevated,
    }
}

/// 跨表一致性校验（settings + rules）：除各自的格式校验外，确认「指向海外(SBN)的默认出口 / 规则 /
/// 程序组」都有合法 WG 兜底——否则 mihomo 加载会引用到不存在的 `wg-out`，给出可读报错而非起栈超时。
fn validate_combined(settings: &NetPolicySettings, rules: &RuleSet) -> Result<()> {
    settings.validate()?;
    rules.validate()?;
    let wg_needed = settings.default_route == Route::Wg
        || rules.rules.iter().any(|r| r.route() == Route::Wg)
        || rules.groups.iter().any(|g| g.route == Route::Wg);
    if wg_needed && settings.wg.validate().is_err() {
        bail!("有「默认出口/规则」指向海外(SBN)，但 WireGuard 未配置或无效——请先配置 SBN，或把它们改为「阻断/直连」");
    }
    Ok(())
}

/// 状态快照（驱动全景图 + 保护横幅）。**异步 command**：内部 `compute_status` 会串行冷启动
/// 多个 `powershell.exe`（firewall/controller/TUN 探测，~1–2s），若作为同步 command 会在
/// **主线程**执行并卡死 webview（全景图渲染延迟、界面周期性"未响应"——该页 3s 快轮询会反复触发）。
/// 故改为 async + `spawn_blocking`，把阻塞探测挪出 UI 线程。
#[tauri::command]
pub async fn net_policy_get_status(state: State<'_, AppState>) -> Result<NetPolicyStatus, String> {
    let np = state.net_policy.clone();
    tokio::task::spawn_blocking(move || compute_status_cached(&np))
        .await
        .map_err(err)
}

/// 活跃连接快照（P0-1，设计 §3.1/§5）：代理运行中 mihomo 的 external-controller `/connections`，
/// 复用 runtime 里的 controller secret 做 Bearer 鉴权。驱动全景图双分支聚合与「当前活跃连接」活数据。
/// mihomo 未跑 / secret 缺失 / 控制器不可达 → 返回**空快照**（非错误），供 3s 快轮询平滑降级。
#[tauri::command]
pub async fn net_policy_connections(
    state: State<'_, AppState>,
) -> Result<connections::ConnectionsSnapshot, String> {
    if !win::is_windows() {
        // 非 Windows：net-policy 不可用，返回空快照而非错误（与失败语义一致，便于前端统一处理）。
        return Ok(connections::ConnectionsSnapshot::empty_snapshot());
    }
    let secret = {
        let rt = state.net_policy.rt.lock().unwrap();
        rt.secret.clone().unwrap_or_default()
    };
    let snap = connections::fetch(&secret).await;
    // Phase 4：每次拉到的活跃连接累积进「域名↔IP/进程」关联（provider 3s 轮询即持续喂养）。
    state.net_policy.obs.ingest_connections(&snap.connections);
    Ok(snap)
}

// ============ Phase 4 可观测：被阻断 feed + 域名↔IP/进程 关联 ============

/// 被阻断尝试快照（默认黑洞下「什么被挡了」）。按 network|host|port 去重、按最近时间倒序。
/// 数据源是常驻 `/logs` 采集器；mihomo 未跑时为空。
#[tauri::command]
pub fn net_policy_blocked(state: State<'_, AppState>) -> Vec<observe::BlockedEntry> {
    state.net_policy.obs.blocked_snapshot()
}

/// 清空被阻断 feed（用户「已处理/忽略」后清屏）。
#[tauri::command]
pub fn net_policy_clear_blocked(state: State<'_, AppState>) {
    state.net_policy.obs.clear_blocked();
}

/// 域名↔IP/进程 关联快照（累积自历次活跃连接）。按最近时间倒序。
#[tauri::command]
pub fn net_policy_dns_map(state: State<'_, AppState>) -> Vec<observe::DomainAssoc> {
    state.net_policy.obs.dns_snapshot()
}

// ============ 设置（含 WG 配置） ============

#[tauri::command]
pub fn net_policy_get_settings(state: State<'_, AppState>) -> Result<NetPolicySettings, String> {
    config::try_load_settings(&state.net_policy.workspace).map_err(err)
}

#[tauri::command]
pub fn net_policy_save_settings(
    state: State<'_, AppState>,
    settings: NetPolicySettings,
) -> Result<(), String> {
    // 校验后再存（P1-3：拒绝非法/可注入的 WG/DNS/LAN 值）。
    settings.validate().map_err(err)?;
    config::save_settings(&state.net_policy.workspace, &settings).map_err(err)
}

/// 解析用户选择的 WireGuard `.conf` 文本，返回填好的 WG 出口配置（**不落盘**）。
/// 前端读取文件内容后调用，拿到结果合并进当前设置，由用户确认后再走
/// `net_policy_save_settings` 校验保存（Endpoint 为域名等问题在保存时报错）。
#[tauri::command]
pub fn net_policy_parse_wg_conf(content: String) -> Result<config::WgConfig, String> {
    config::WgConfig::from_wg_quick(&content).map_err(err)
}

// ============ 规则 ============

#[tauri::command]
pub fn net_policy_list_rules(state: State<'_, AppState>) -> Result<RuleSet, String> {
    config::try_load_rules(&state.net_policy.workspace).map_err(err)
}

/// **Upsert** 一条规则并持久化：同目标（kind+value）的旧规则先移除再追加——改路即替换，
/// 单命令原子完成（原「前端先删后加」两步在删成功、加失败时会把规则弄丢且状态分叉）。
#[tauri::command]
pub fn net_policy_save_rule(state: State<'_, AppState>, rule: Rule) -> Result<RuleSet, String> {
    rule.validate().map_err(err)?; // P1-3：校验规则值，拒绝注入
    let ws = &state.net_policy.workspace;
    let mut rs = config::try_load_rules(ws).map_err(err)?;
    rs.rules.retain(|r| !r.same_target(&rule));
    rs.rules.push(rule);
    config::save_rules(ws, &rs).map_err(err)?;
    Ok(rs)
}

/// 按值删除规则（匹配 kind+value，忽略 route）。原按数组下标删除在多行并发操作时会因
/// 下标前移删错规则（规则无稳定 ID，下标只是某次渲染的快照）。
#[tauri::command]
pub fn net_policy_delete_rule(state: State<'_, AppState>, rule: Rule) -> Result<RuleSet, String> {
    let ws = &state.net_policy.workspace;
    let mut rs = config::try_load_rules(ws).map_err(err)?;
    let before = rs.rules.len();
    rs.rules.retain(|r| !r.same_target(&rule));
    if rs.rules.len() == before {
        return Err("未找到该规则（可能已被删除）".into());
    }
    config::save_rules(ws, &rs).map_err(err)?;
    Ok(rs)
}

// ============ 进程候选 ============

#[tauri::command]
pub async fn net_policy_list_process_candidates(
) -> Result<Vec<process_watch::ProcessCandidate>, String> {
    if !win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    tokio::task::spawn_blocking(process_watch::list_candidates)
        .await
        .map_err(err)?
        .map_err(err)
}

// ============ 应用进度事件（Phase 2，设计 §3.3） ============

/// `net_policy_apply` 逐阶段进度事件的频道名。前端 `listen('net-policy://apply-progress')` 订阅。
pub const APPLY_PROGRESS_EVENT: &str = "net-policy://apply-progress";

/// apply 的 6 个阶段（与设计 §3.3 stepper 对齐，索引从 0 起）。
const APPLY_STEPS: [&str; 6] = [
    "校验配置",
    "装防火墙基线",
    "启动引擎",
    "等待 TUN 起栈",
    "补 TUN 白名单",
    "验证连通",
];

/// 单步进度。`status` ∈ {running, ok, fail}；`detail` 为可选补充（如 TUN 轮询 N/14、错误原文）。
#[derive(Debug, Clone, Serialize)]
pub struct ApplyProgress {
    /// 步索引（0..6），对应 `APPLY_STEPS`。
    pub step: usize,
    /// 步名（冗余给前端，省得对索引表）。
    pub name: String,
    /// running / ok / fail。
    pub status: String,
    /// 可选补充信息（进度、错误原文等）。
    pub detail: Option<String>,
}

/// 发一条进度事件（emit 失败不影响主流程——进度仅为可观测性）。
fn emit_progress(app: &tauri::AppHandle, step: usize, status: &str, detail: Option<String>) {
    use tauri::Emitter;
    let _ = app.emit(
        APPLY_PROGRESS_EVENT,
        ApplyProgress {
            step,
            name: APPLY_STEPS.get(step).copied().unwrap_or("").to_string(),
            status: status.to_string(),
            detail,
        },
    );
}

// ============ 应用 / 急停 ============

/// 应用策略：校验 → 生成配置 → 启动 mihomo → 应用 kill-switch（默认开启）。
/// **事务化（P0-2）**：任一步失败按安全顺序回滚（先停引擎再撤防火墙），不留半应用状态。
/// **受控 reapply（P0-1）**：已应用过则先用旧 secret/pid 优雅停旧引擎再起新引擎，旧 kill-switch
/// 在交换窗口内保持生效（fail-closed 不破），杜绝"重复 apply → 旧 mihomo 仍在跑/占端口、新引擎
/// 起不来、防火墙却被撤掉"。kill-switch 关闭时为"不受保护预览"，status.protected=false（P0-1）。
/// `rt` 在每个中间/失败点都被更新为与机器实际状态一致（避免重启/急停拿到陈旧 pid/secret）。
#[tauri::command]
pub async fn net_policy_apply(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<NetPolicyStatus, String> {
    do_apply(state.net_policy.clone(), app).await
}

/// apply 的核心实现（命令与 setup 自动应用共用）。`np` 为模块状态句柄，`app` 用于发进度事件。
async fn do_apply(
    np: Arc<NetPolicyState>,
    app: tauri::AppHandle,
) -> Result<NetPolicyStatus, String> {
    if !win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    // 操作互斥：与 stop/reload/另一个 apply 串行（apply 步 3 有 ~7s 轮询窗口，穿插操作会
    // 交叉写 config.yaml / 防火墙 / rt）。
    let np_lock = np.clone();
    let _ops = np_lock.ops.lock().await;
    invalidate_status_cache(&np);
    let np_final = np.clone(); // 闭包会 move 掉 np；末尾刷新状态用这个存活句柄。
    let ws = np.workspace.clone();
    let settings = config::try_load_settings(&ws).map_err(err)?;
    let rules = config::try_load_rules(&ws).map_err(err)?;

    // 步 0：校验配置（进副作用前先校验全部输入 + 跨表一致性，P1-3）。
    emit_progress(&app, 0, "running", None);
    if let Err(e) = validate_combined(&settings, &rules) {
        emit_progress(&app, 0, "fail", Some(err(&e)));
        return Err(err(e));
    }
    emit_progress(&app, 0, "ok", None);

    // 观察/Direct 模式下 kill-switch 无安全意义：mihomo 崩溃只是流量不走代理，不会泄密；
    // 若此时强行装 DefaultOutboundAction=Block 反而会在 mihomo 挂掉后砖掉网络。
    // 故仅在 Wg/Blackhole 姿态下启用 kill-switch（fail-closed 只对"应当封锁的出口"有意义）。
    let killswitch = settings.killswitch_enabled && settings.default_route != Route::Direct;
    let mihomo_bin = engine::mihomo_bin(&ws);
    let secret = engine::gen_secret(); // P0-1：每次 apply 随机 controller secret
    let app_bg = app.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let app = app_bg;
        // 前置:必须管理员。改全局防火墙 + 建 TUN 网卡都需提权;未提权时**先于任何副作用**给出
        // 可读错误,而非中途撞 `New-NetFirewallRule: Access is denied` 的 CLIXML。
        if !win::is_elevated() {
            let e = "需要以管理员身份运行 Zero Desktop:网络策略要改全局防火墙、建 TUN 网卡,均需管理员权限。请右键「以管理员身份运行」重启后再试。";
            emit_progress(&app, 1, "fail", Some(e.into()));
            bail!("{e}");
        }
        // 当前已应用状态快照——受控 reapply 的依据（P0-1）。
        let (was_applied, old_pid, old_secret) = {
            let rt = np.rt.lock().unwrap();
            (
                rt.applied,
                rt.mihomo_pid,
                rt.secret.clone().unwrap_or_default(),
            )
        };
        // graceful_stop 按名回退时用的进程名——从实际二进制推导（MIHOMO_BIN 可能指向自定义
        // 文件名，硬编码 mihomo-windows-amd64 会杀不到还假成功）。
        let engine_name = mihomo_bin
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "mihomo-windows-amd64".into());

        // 端口预检：本会话未应用但 9090 已有 external-controller 在响应 → 要么是别的
        // clash/mihomo，要么是丢失 secret 的孤儿实例。直接起新实例只会抢端口失败且报错误导
        //（"控制器不可达"），这里先给可读诊断。
        if !was_applied && engine::controller_present() {
            let e = format!(
                "127.0.0.1:9090 已有 external-controller 在响应，但不受本会话管理——可能是本机其他 clash/mihomo，或上次残留的 {engine_name} 实例（配置已丢失无法接管）。请先手动结束该进程再应用。"
            );
            emit_progress(&app, 1, "fail", Some(e.clone()));
            bail!("{e}");
        }

        // 受控 reapply：先用旧 secret/pid 优雅停旧引擎。旧 kill-switch 此刻仍生效 → 交换窗口
        // fail-closed 不破。停旧失败则中止（旧实例 + 旧 kill-switch 保留，仍受保护），不动配置/不起新引擎。
        if was_applied {
            engine::graceful_stop(old_pid, &old_secret, &engine_name).context(
                "受控 reapply：优雅停旧 mihomo 失败（保留原 kill-switch 与旧实例，未改配置）",
            )?;
            // 旧引擎已停；kill-switch 仍在（applied 维持 true）。清掉失效的旧 pid/secret。
            let mut rt = np.rt.lock().unwrap();
            rt.mihomo_pid = None;
            rt.secret = None;
        }

        // 安全回滚（P0-2，与 emergency_stop 同序）：有新 pid 时**先优雅停引擎**，确认停下后**才**
        // 动防火墙；停不掉则**保留防火墙**维持 fail-closed，并把 rt 记成"仍受保护"以便后续 emergency_stop。
        //
        // **fail-closed 优先**：受控 reapply（was_applied，用户此前处于受保护状态）失败时**不撤**
        // kill-switch——宁可断网也不把 Blackhole/Wg 用户静默降到"全放开"；防火墙保留 + 引擎已停
        // 会呈现"防火墙残留"危险态，由前端保护横幅提示、用户可用「紧急停止」显式解除。
        // 全新 apply（was_applied=false，用户本就无保护）失败才撤掉半装的防火墙回到基线。
        let rollback = |new_pid: Option<u32>| -> Result<()> {
            if let Some(p) = new_pid {
                if let Err(e) = engine::graceful_stop(Some(p), &secret, &engine_name) {
                    let mut rt = np.rt.lock().unwrap();
                    rt.applied = true;
                    rt.mihomo_pid = Some(p);
                    rt.secret = Some(secret.clone());
                    return Err(e)
                        .context("回滚：停 mihomo 失败，kill-switch 保持生效以维持 fail-closed");
                }
            }
            if killswitch && !was_applied {
                let _ = firewall::remove(&ws);
            }
            let mut rt = np.rt.lock().unwrap();
            rt.applied = false;
            rt.mihomo_pid = None;
            rt.secret = None;
            Ok(())
        };
        // 回滚后是否仍处于"防火墙保留、需用户显式解除"状态（拼进错误信息，让失败不静默）。
        let rollback_note = if killswitch && was_applied {
            "；kill-switch 已保留（fail-closed 优先，网络将保持阻断），重试应用或用「紧急停止」显式解除"
        } else {
            ""
        };

        // 步 1（阶段 A，P1-2）：先建 fail-closed（不依赖 Meta 的白名单 + 默认 Block），再起 mihomo。
        emit_progress(&app, 1, "running", None);
        if killswitch {
            if let Err(e) = firewall::apply_base(&ws, &settings, &mihomo_bin) {
                emit_progress(&app, 1, "fail", Some(format!("{}{rollback_note}", err(&e))));
                rollback(None)?; // 全新 apply 才撤半装的 kill-switch；reapply 保留（见 rollback 注释）。
                return Err(e.context(format!("apply kill-switch（阶段A）{rollback_note}")));
            }
        } else if firewall::status()
            .map(|f| f.rule_count > 0 || f.active)
            .unwrap_or(false)
        {
            // 本次姿态不需要 kill-switch（观察/直连 或 关了 killswitch），但机器上可能残留上次
            // Blackhole/Wg 会话的痕迹（异常退出未清）——包括「规则已删但 DefaultOutboundAction
            // 仍为 Block」的半残留（只看 rule_count 会漏掉，网络照样砖）。主动撤掉还原。
            let _ = firewall::remove(&ws);
        }
        if let Err(e) = engine::write_config(&ws, &settings, &rules, &secret) {
            emit_progress(&app, 1, "fail", Some(format!("{}{rollback_note}", err(&e))));
            rollback(None)?;
            return Err(e.context(format!("write mihomo config{rollback_note}")));
        }
        emit_progress(
            &app,
            1,
            "ok",
            Some(if killswitch {
                "kill-switch 基线已就位".into()
            } else {
                "不受保护预览（未装 kill-switch）".into()
            }),
        );

        // 步 2：启动 mihomo 引擎。
        emit_progress(&app, 2, "running", None);
        let pid = match engine::start(&ws) {
            Ok(p) => p,
            Err(e) => {
                emit_progress(&app, 2, "fail", Some(format!("{}{rollback_note}", err(&e))));
                rollback(None)?;
                return Err(e.context(format!("start mihomo{rollback_note}")));
            }
        };
        // 新引擎已起：立刻把 pid/secret 落进 rt，确保即便后续失败，回滚/急停也能按 pid 安全停。
        {
            let mut rt = np.rt.lock().unwrap();
            rt.mihomo_pid = Some(pid);
            rt.secret = Some(secret.clone());
        }
        emit_progress(&app, 2, "ok", Some(format!("pid {pid}")));

        // 步 3：等待 TUN(Meta) 起栈（真·轮询，复用 graceful_stop 的 14×500ms 范式；诚实分步，
        // 替代旧的固定 sleep(6s)+单次控制器探测）。控制器可达且 Meta Up 才算就绪。
        emit_progress(&app, 3, "running", Some("0/14".into()));
        let mut tun_ready = false;
        for i in 0..14 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if engine::running(&secret) && engine::tun_up() {
                tun_ready = true;
                emit_progress(&app, 3, "running", Some(format!("{}/14 已起栈", i + 1)));
                break;
            }
            emit_progress(&app, 3, "running", Some(format!("{}/14", i + 1)));
        }
        if !tun_ready {
            // 控制器可达但 TUN 没起栈，或控制器都不可达——区分提示。
            let detail = if engine::running(&secret) {
                "控制器可达但 TUN(Meta) 未在超时内起栈"
            } else {
                "mihomo 外部控制器不可达"
            };
            emit_progress(&app, 3, "fail", Some(format!("{detail}{rollback_note}")));
            rollback(Some(pid))?;
            bail!("等待 TUN 起栈超时（{detail}），已回滚（检查 WG 配置 / 管理员权限）{rollback_note}");
        }
        emit_progress(&app, 3, "ok", None);

        // 步 4（阶段 B）：Meta 已出现，补 KS-TUN 放行应用流量进隧道。
        emit_progress(&app, 4, "running", None);
        if killswitch {
            if let Err(e) = firewall::apply_tun(&ws) {
                emit_progress(&app, 4, "fail", Some(format!("{}{rollback_note}", err(&e))));
                rollback(Some(pid))?;
                return Err(e.context(format!("apply kill-switch（阶段B KS-TUN）{rollback_note}")));
            }
        }
        emit_progress(&app, 4, "ok", None);

        // 步 5：验证连通（控制器可达 + TUN 起栈，已在步 3 确认；此处终确认并标终态）。
        emit_progress(&app, 5, "running", None);
        {
            let mut rt = np.rt.lock().unwrap();
            rt.applied = true;
            rt.mihomo_pid = Some(pid);
            rt.secret = Some(secret.clone());
        }
        emit_progress(&app, 5, "ok", Some("引擎在线 · TUN 已起栈".into()));
        Ok(())
    })
    .await
    .map_err(err)?
    .map_err(err)?;

    tokio::task::spawn_blocking(move || compute_status_fresh(&np_final))
        .await
        .map_err(err)
}

/// 紧急停止 / 撤销。**顺序（P1-2）**：先在 kill-switch 仍生效时优雅停引擎（API 关 TUN
/// 清理路由，§0.8.2bis），确认引擎停下后才撤防火墙——避免"防火墙先撤、引擎还在/卡住"的
/// 泄漏窗口。若引擎停不下来，防火墙保持生效（继续 fail-closed），返回错误。
#[tauri::command]
pub async fn net_policy_emergency_stop(
    state: State<'_, AppState>,
) -> Result<NetPolicyStatus, String> {
    do_stop(state.net_policy.clone()).await
}

/// 停止核心（命令与「关主开关」共用）：优雅停引擎 → 撤防火墙 → 清 rt。
async fn do_stop(np: Arc<NetPolicyState>) -> Result<NetPolicyStatus, String> {
    if !win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    // 操作互斥：与 apply/reload 串行。
    let np_lock = np.clone();
    let _ops = np_lock.ops.lock().await;
    invalidate_status_cache(&np);
    let np_final = np.clone();
    let ws = np.workspace.clone();
    let (pid, secret) = {
        let rt = np.rt.lock().unwrap();
        (rt.mihomo_pid, rt.secret.clone().unwrap_or_default())
    };
    tokio::task::spawn_blocking(move || -> Result<()> {
        let engine_name = engine::mihomo_bin(&ws)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "mihomo-windows-amd64".into());
        // 1) 先优雅停引擎（关 TUN + 确认 Meta 拆除后才杀，按 pid，P1-1/P2-2）；
        //    失败则不撤防火墙，保持 fail-closed。
        engine::graceful_stop(pid, &secret, &engine_name)
            .context("优雅停 mihomo 失败（防火墙保持生效以维持 fail-closed）")?;
        // 2) 引擎已停、路由已清，再撤防火墙恢复联网。
        firewall::remove(&ws).context("移除 kill-switch")?;
        Ok(())
    })
    .await
    .map_err(err)?
    .map_err(err)?;

    {
        let mut rt = np.rt.lock().unwrap();
        rt.applied = false;
        rt.mihomo_pid = None;
        rt.secret = None;
    }
    // 手动停止（紧急停止按钮 / 主开关关闭）= "停了就别再自动恢复"。
    // close/crash 不走 do_stop，保留 enabled=true，下次启动 setup 会自动恢复（预期行为）。
    // net_policy_set_enabled(false) 已在调用前把 enabled 落盘；此处覆盖 emergency_stop 路径，
    // 使两条路径一致：紧急停止后也不会在下次启动时自动重新应用。
    {
        if let Ok(mut s) = config::try_load_settings(&np.workspace) {
            if s.enabled {
                s.enabled = false;
                let _ = config::save_settings(&np.workspace, &s);
            }
        }
    }
    tokio::task::spawn_blocking(move || compute_status_fresh(&np_final))
        .await
        .map_err(err)
}

// ============ 主开关 / 热重载 ============

/// 设主开关（启动即生效）。开 → 暂存 enabled=true 后 apply，失败则回滚原值；关 → 若在运行则 stop。
/// 只有成功启用才会在下次启动时由 `setup` 自动应用。
#[tauri::command]
pub async fn net_policy_set_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<NetPolicyStatus, String> {
    if !win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    let np = state.net_policy.clone();
    let ws = np.workspace.clone();
    let mut settings = config::try_load_settings(&ws).map_err(err)?;
    let previous_enabled = settings.enabled;
    settings.enabled = enabled;
    settings.validate().map_err(err)?; // 开关切换前先确认配置自洽（默认黑洞下 WG 可空）。
    config::save_settings(&ws, &settings).map_err(err)?;
    invalidate_status_cache(&np);

    if enabled {
        match do_apply(np.clone(), app).await {
            Ok(status) => Ok(status),
            Err(apply_error) => {
                settings.enabled = previous_enabled;
                invalidate_status_cache(&np);
                match config::save_settings(&ws, &settings) {
                    Ok(()) => Err(apply_error),
                    Err(rollback_error) => Err(format!(
                        "{apply_error}；同时回滚 enabled 失败：{}",
                        err(rollback_error)
                    )),
                }
            }
        }
    } else {
        let applied = { np.rt.lock().unwrap().applied };
        if applied {
            do_stop(np).await
        } else {
            // 本就未应用，仅落盘 enabled=false，刷新状态即可。
            tokio::task::spawn_blocking(move || compute_status_fresh(&np))
                .await
                .map_err(err)
        }
    }
}

/// 热重载（逐项放行不重启隧道）：重写配置 → mihomo `PUT /configs` 原地生效。未应用时为 no-op。
/// 前端在 applied 状态下增删规则 / 改默认出口后调用，避免每次都走 6 步整链重启而瞬断。
#[tauri::command]
pub async fn net_policy_reload(state: State<'_, AppState>) -> Result<NetPolicyStatus, String> {
    if !win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    let np = state.net_policy.clone();
    // 操作互斥：与 apply/stop 串行（apply 中途穿插 reload 会交叉写 config.yaml）。
    let _ops = np.ops.lock().await;
    invalidate_status_cache(&np);
    let np_final = np.clone();
    let ws = np.workspace.clone();
    let (applied, secret) = {
        let rt = np.rt.lock().unwrap();
        (rt.applied, rt.secret.clone().unwrap_or_default())
    };
    if !applied {
        // 未应用：无需热载，直接回当前状态（前端据此决定是否走完整 apply）。
        return tokio::task::spawn_blocking(move || compute_status_fresh(&np_final))
            .await
            .map_err(err);
    }
    let settings = config::try_load_settings(&ws).map_err(err)?;
    let rules = config::try_load_rules(&ws).map_err(err)?;
    validate_combined(&settings, &rules).map_err(err)?;
    tokio::task::spawn_blocking(move || -> Result<()> {
        // 切入受保护姿态必须先装防火墙基线，再让 mihomo 接受新配置，避免 reload 成功后、
        // kill-switch 安装前引擎恰好退出而回落物理出口。切回 Direct 则相反：先 reload，再撤围栏。
        let want = settings.killswitch_enabled && settings.default_route != Route::Direct;
        // 「在挂」的判定含半残留：规则被清但 DefaultOutboundAction 仍 Block（上次 remove 半程
        // 失败）也算——切回直连时必须触发 remove 还原，否则网络照样砖。
        let up = firewall::status()
            .map(|f| f.rule_count > 0 || f.active)
            .unwrap_or(false);
        if want {
            let bin = engine::mihomo_bin(&ws);
            firewall::apply_base(&ws, &settings, &bin)?;
        }
        if let Err(e) = engine::write_config(&ws, &settings, &rules, &secret)
            .and_then(|_| engine::reload(&ws, &secret))
        {
            // 原先未受保护时，切换失败应恢复原直连基线；原先已有围栏则保留，继续 fail-closed。
            if want && !up {
                let _ = firewall::remove(&ws);
            }
            return Err(e);
        }
        if want {
            // Meta 适配器已在运行（applied 路径保证），补回 apply_base 刷掉的 KS-TUN。
            firewall::apply_tun(&ws)?;
        } else if !want && up {
            firewall::remove(&ws)?;
        }
        Ok(())
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    tokio::task::spawn_blocking(move || compute_status_fresh(&np_final))
        .await
        .map_err(err)
}

// ============ 验证 ============

#[tauri::command]
pub async fn net_policy_verify(state: State<'_, AppState>) -> Result<verify::VerifyReport, String> {
    if !win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    let secret = state
        .net_policy
        .rt
        .lock()
        .unwrap()
        .secret
        .clone()
        .unwrap_or_default();
    // 姿态决定各用例的期望方向（黑洞下"出不去"才是通过）。
    let default_route = config::try_load_settings(&state.net_policy.workspace)
        .map_err(err)?
        .default_route;
    tokio::task::spawn_blocking(move || verify::run(&secret, default_route))
        .await
        .map_err(err)?
        .map_err(err)
}
