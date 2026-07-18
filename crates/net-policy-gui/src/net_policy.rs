//! net-policy 模块（**瘦客户端桥接**）：GUI 普通权限，经命名管道把 17 个 `net_policy_*` command 委托给
//! 提权的 `net-policy-agent`（唯一副作用所有者）。设计见 docs/net-policy-daemon-gui-split-design.md（收敛版）。
//!
//! **本模块不再持有任何提权副作用**——mihomo/防火墙/观察器/operation 全在 net-policy-agent；配置/规则/
//! 校验/mihomo 配置生成等纯逻辑在 net-policy-core。旧的进程内 engine/firewall/observe/... 子文件已停用
//! （不再 `mod` 声明，不参与编译），迁移稳定后可物理删除。
//!
//! **agent 生命周期（当前推荐）**：GUI **不自动拉起/安装 agent**（那需要 UAC 提权）；agent 由
//! `net-policy-agent install` 注册的登录任务自启，或开发时 `net-policy-agent run --dev` 手动起。连不上
//! agent 时命令返回可读错误，前端提示"请先安装并启动网络策略服务"。

use crate::app_state::AppState;
use anyhow::Result;
use net_policy_client::Client;
use net_policy_core::capture::{CaptureOpts, CaptureSession, CaptureTarget};
use net_policy_core::config::{NetPolicySettings, ProcessRef, Rule, RuleSet, WgConfig};
use net_policy_core::decrypt::{
    CaStatus, DecryptArtifact, DecryptOpts, DecryptSession, DecryptTarget,
};
use net_policy_core::egress::{EgressFallback, EgressStatus};
use net_policy_core::types::{
    BlockedEntry, ConnectionsSnapshot, DomainAssoc, LifecycleEvent, NetPolicyStatus,
    ProcessCandidate, ProcessNode, ProxyNode, RequestLogEntry, RouteEntry, TempDirectStatus,
    VerifyReport,
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{Emitter, State};

/// 前端 `listen('net-policy://apply-progress')` 订阅的频道名（与既有前端约定一致）。
pub const APPLY_PROGRESS_EVENT: &str = "net-policy://apply-progress";

/// 前端 `listen('net-policy://egress-changed')` 订阅的频道名（minor 8，出口生命周期变迁）。
/// **与 [`APPLY_PROGRESS_EVENT`] 分开推送**：前端不得由其一推断另一个（出口设计 §8.8 末段）。
pub const EGRESS_CHANGED_EVENT: &str = "net-policy://egress-changed";

/// 模块状态：瘦客户端下几乎无状态（保留结构以维持 AppState 装配不变）。
#[allow(dead_code)]
pub struct NetPolicyState {
    pub workspace: PathBuf,
}

impl NetPolicyState {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

fn estr<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

/// 连接 agent 管道，连不上给可读引导。
async fn connect() -> Result<Client, String> {
    Client::connect().await.map_err(|e| {
        format!(
            "连不上网络策略服务（net-policy-agent）：{e:#}——请先安装并启动它（net-policy-agent install / run）"
        )
    })
}

/// 初始化：订阅 agent 的长操作事件流，把 `ApplyProgress` re-emit 成 Tauri 事件
/// `net-policy://apply-progress`（前端 ApplyStepper 消费）。断线 3s 重连；`OperationFinished`/
/// `ResyncRequired` 前端靠轮询 `get_status` / `GetCurrentOperation` 对齐，这里不额外处理。
pub fn setup(app: &tauri::AppHandle, _state: Arc<NetPolicyState>) -> Result<()> {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if let Ok(mut stream) = net_policy_client::subscribe_events().await {
                while let Some(ev) = stream.next().await {
                    match ev {
                        Ok(net_policy_core::protocol::Event::ApplyProgress { progress }) => {
                            let _ = app.emit(APPLY_PROGRESS_EVENT, progress);
                        }
                        Ok(net_policy_core::protocol::Event::EgressChanged { egress }) => {
                            let _ = app.emit(EGRESS_CHANGED_EVENT, egress);
                        }
                        Ok(_) => {} // OperationFinished / ResyncRequired：前端以真实态对齐
                        Err(_) => break,
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    });
    Ok(())
}

/// 离线生成产物（CLI `net-policy-gen` 预览用；纯 core，不触 agent）。返回 `(mihomo_config, note)`。
/// 防火墙脚本现由 net-policy-agent 生成，预览侧只给 mihomo 配置 + 说明。
pub fn gen_artifacts(workspace: &std::path::Path) -> Result<(String, String)> {
    let settings = net_policy_core::config::try_load_settings(workspace)?;
    let rules = net_policy_core::config::try_load_rules(workspace)?;
    let cfg = net_policy_core::mihomo::generate_config(
        &settings,
        &rules,
        "<runtime-secret>",
        &net_policy_core::config::TempDirect::default(),
        &net_policy_core::config::DecryptDivert::default(),
    );
    let note = "（防火墙脚本现由 net-policy-agent 提权生成/应用；预览仅 mihomo 配置）".to_string();
    Ok((cfg, note))
}

// ============ 17 个 command → 委托 agent ============

#[tauri::command]
pub async fn net_policy_get_status(_state: State<'_, AppState>) -> Result<NetPolicyStatus, String> {
    connect().await?.status().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_connections(
    _state: State<'_, AppState>,
) -> Result<ConnectionsSnapshot, String> {
    connect().await?.connections().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_proxy_nodes(_state: State<'_, AppState>) -> Result<Vec<ProxyNode>, String> {
    connect().await?.proxy_nodes().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_test_proxy_node(
    _state: State<'_, AppState>,
    name: String,
) -> Result<ProxyNode, String> {
    connect().await?.test_proxy_node(name).await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_get_settings(
    _state: State<'_, AppState>,
) -> Result<NetPolicySettings, String> {
    connect().await?.get_settings().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_save_settings(
    _state: State<'_, AppState>,
    settings: NetPolicySettings,
) -> Result<(), String> {
    connect().await?.save_settings(settings).await.map_err(estr)
}

/// WG 解析是纯函数——本地做，不必过 agent（`.conf` 明文不进管道）。
#[tauri::command]
pub async fn net_policy_parse_wg_conf(content: String) -> Result<WgConfig, String> {
    WgConfig::from_wg_quick(&content).map_err(estr)
}

#[tauri::command]
pub async fn net_policy_list_rules(_state: State<'_, AppState>) -> Result<RuleSet, String> {
    connect().await?.list_rules().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_save_rule(
    _state: State<'_, AppState>,
    rule: Rule,
) -> Result<RuleSet, String> {
    connect().await?.save_rule(rule).await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_delete_rule(
    _state: State<'_, AppState>,
    rule: Rule,
) -> Result<RuleSet, String> {
    connect().await?.delete_rule(rule).await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_list_process_candidates() -> Result<Vec<ProcessCandidate>, String> {
    connect()
        .await?
        .list_process_candidates()
        .await
        .map_err(estr)
}

#[tauri::command]
pub async fn net_policy_apply(_state: State<'_, AppState>) -> Result<NetPolicyStatus, String> {
    connect().await?.apply().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_emergency_stop(
    _state: State<'_, AppState>,
) -> Result<NetPolicyStatus, String> {
    connect().await?.stop().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_set_enabled(
    _state: State<'_, AppState>,
    enabled: bool,
) -> Result<NetPolicyStatus, String> {
    connect().await?.set_enabled(enabled).await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_reload(_state: State<'_, AppState>) -> Result<NetPolicyStatus, String> {
    connect().await?.reload().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_blocked(_state: State<'_, AppState>) -> Result<Vec<BlockedEntry>, String> {
    connect().await?.blocked().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_clear_blocked(_state: State<'_, AppState>) -> Result<(), String> {
    connect().await?.clear_blocked().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_dns_map(_state: State<'_, AppState>) -> Result<Vec<DomainAssoc>, String> {
    connect().await?.dns_map().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_verify(_state: State<'_, AppState>) -> Result<VerifyReport, String> {
    connect().await?.verify().await.map_err(estr)
}

// ── 记录 / 观测 / 临时直连 / 路由（minor 2） ──────────────────────────────────

#[tauri::command]
pub async fn net_policy_requests(limit: u32) -> Result<Vec<RequestLogEntry>, String> {
    connect().await?.requests(limit).await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_events(limit: u32) -> Result<Vec<LifecycleEvent>, String> {
    connect().await?.events(limit).await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_routes() -> Result<Vec<RouteEntry>, String> {
    connect().await?.routes().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_process_tree() -> Result<Vec<ProcessNode>, String> {
    connect().await?.process_tree().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_temp_status() -> Result<TempDirectStatus, String> {
    connect().await?.temp_direct().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_temp_direct_on(
    duration_secs: u64,
    except: Vec<ProcessRef>,
) -> Result<TempDirectStatus, String> {
    connect()
        .await?
        .set_temp_direct(duration_secs, except)
        .await
        .map_err(estr)
}

#[tauri::command]
pub async fn net_policy_temp_direct_off() -> Result<TempDirectStatus, String> {
    connect().await?.clear_temp_direct().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_clear_requests() -> Result<(), String> {
    connect().await?.clear_requests().await.map_err(estr)
}

#[tauri::command]
pub async fn net_policy_clear_events() -> Result<(), String> {
    connect().await?.clear_events().await.map_err(estr)
}

// ── 连接重置 / 运行日志（minor 3） ────────────────────────────────────────

/// 切姿态后强制旧连接用新出口重连（best-effort：前端失败只提示不阻塞）。
#[tauri::command]
pub async fn net_policy_reset_connections(_state: State<'_, AppState>) -> Result<(), String> {
    connect().await?.reset_connections().await.map_err(estr)
}

/// mihomo / WireGuard 运行日志（最近 `lines` 行）。
#[tauri::command]
pub async fn net_policy_get_mihomo_log(lines: u32) -> Result<Vec<String>, String> {
    connect().await?.mihomo_log(lines).await.map_err(estr)
}

// ── 抓包（minor 5，抓包设计 §10/§12） ─────────────────────────────────────

/// pcapng 分块（前端用浏览器原生 atob 解码组装 Blob 下载，避免走磁盘/新增依赖）。
#[derive(serde::Serialize)]
pub struct CaptureChunkDto {
    pub offset: u64,
    pub data_base64: String,
    pub eof: bool,
}

/// 开始抓包（`All` 全 TUN，或定向 Process/Domain/Ip）。
#[tauri::command]
pub async fn net_policy_capture_start(
    target: CaptureTarget,
    opts: CaptureOpts,
) -> Result<CaptureSession, String> {
    connect()
        .await?
        .capture_start(target, opts)
        .await
        .map_err(estr)
}

/// 停止抓包（幂等）。
#[tauri::command]
pub async fn net_policy_capture_stop(id: String) -> Result<CaptureSession, String> {
    connect().await?.capture_stop(id).await.map_err(estr)
}

/// 取单个会话当前态。
#[tauri::command]
pub async fn net_policy_capture_get(id: String) -> Result<CaptureSession, String> {
    connect().await?.capture_get(id).await.map_err(estr)
}

/// 列出所有抓包会话。
#[tauri::command]
pub async fn net_policy_capture_list() -> Result<Vec<CaptureSession>, String> {
    connect().await?.capture_list().await.map_err(estr)
}

/// 删除会话（运行态返回 capture_busy）。
#[tauri::command]
pub async fn net_policy_capture_delete(id: String) -> Result<(), String> {
    connect().await?.capture_delete(id).await.map_err(estr)
}

/// 分块读取 done 会话 pcapng（前端循环调用直至 eof，组装保存）。
#[tauri::command]
pub async fn net_policy_capture_read(
    id: String,
    offset: u64,
    len: u32,
) -> Result<CaptureChunkDto, String> {
    let (offset, data_base64, eof) = connect()
        .await?
        .capture_read(id, offset, len)
        .await
        .map_err(estr)?;
    Ok(CaptureChunkDto {
        offset,
        data_base64,
        eof,
    })
}

// ============ L4 应用明文（Decrypt*/DecryptCa*，抓包设计 §17）============

/// 查 CA 信任状态。
#[tauri::command]
pub async fn net_policy_decrypt_ca_status() -> Result<CaStatus, String> {
    connect().await?.decrypt_ca_status().await.map_err(estr)
}

/// 生成专用调试 CA（agent 侧生成 + DPAPI 私钥保护；此步**不装信任库**）。
#[tauri::command]
pub async fn net_policy_decrypt_ca_create() -> Result<CaStatus, String> {
    connect().await?.decrypt_ca_create().await.map_err(estr)
}

/// 把 CA 公钥装进当前用户 `CurrentUser\Root`（弹 Windows 确认框），实查验证后把指纹 + SID 交回 agent
/// 复核（§17.4/§17.8）。返回复核后的 CA 状态。
#[tauri::command]
pub async fn net_policy_decrypt_ca_install() -> Result<CaStatus, String> {
    let mut client = connect().await?;
    // 1. 取 agent 侧 CA 指纹（DER SHA-256）与公钥 PEM。
    let status = client.decrypt_ca_status().await.map_err(estr)?;
    let thumbprint = status
        .thumbprint
        .ok_or_else(|| "CA 尚未创建（先点『创建调试 CA』）".to_string())?;
    let cert_pem = client.decrypt_ca_export_public().await.map_err(estr)?;
    // 2. 在当前用户上下文装信任库 + 实查验证（阻塞调用放 spawn_blocking）。
    let tp = thumbprint.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::ca_trust::install_current_user_root(&cert_pem, &tp)
    })
    .await
    .map_err(|e| format!("安装任务失败：{e}"))?
    .map_err(estr)?;
    // 3. 取当前用户 SID。
    let sid = tauri::async_runtime::spawn_blocking(crate::ca_trust::current_user_sid)
        .await
        .map_err(|e| format!("取 SID 任务失败：{e}"))?
        .map_err(estr)?;
    // 4. 交回 agent 复核（指纹须与 agent 本地一致）。
    client
        .decrypt_ca_confirm(thumbprint, sid)
        .await
        .map_err(estr)
}

/// 移除本产品 CA：先从 `CurrentUser\Root` 精确删证书（best-effort），再让 agent 删私钥/记录。
#[tauri::command]
pub async fn net_policy_decrypt_ca_remove() -> Result<CaStatus, String> {
    let mut client = connect().await?;
    // 先拿指纹以便精确删信任库证书。
    if let Ok(status) = client.decrypt_ca_status().await {
        if let Some(tp) = status.thumbprint {
            let _ = tauri::async_runtime::spawn_blocking(move || {
                crate::ca_trust::remove_current_user_root(&tp)
            })
            .await;
        }
    }
    client.decrypt_ca_remove().await.map_err(estr)
}

/// 开始明文会话（精确进程实例 + 必填域名 allowlist）。
#[tauri::command]
pub async fn net_policy_decrypt_start(
    target: DecryptTarget,
    opts: DecryptOpts,
) -> Result<DecryptSession, String> {
    connect()
        .await?
        .decrypt_start(target, opts)
        .await
        .map_err(estr)
}

/// 停止明文会话（幂等）。
#[tauri::command]
pub async fn net_policy_decrypt_stop(id: String) -> Result<DecryptSession, String> {
    connect().await?.decrypt_stop(id).await.map_err(estr)
}

/// 取单个会话当前态（含每域名计数）。
#[tauri::command]
pub async fn net_policy_decrypt_get(id: String) -> Result<DecryptSession, String> {
    connect().await?.decrypt_get(id).await.map_err(estr)
}

/// 列出所有明文会话。
#[tauri::command]
pub async fn net_policy_decrypt_list() -> Result<Vec<DecryptSession>, String> {
    connect().await?.decrypt_list().await.map_err(estr)
}

/// 删除会话（运行态返回冲突）。
#[tauri::command]
pub async fn net_policy_decrypt_delete(id: String) -> Result<(), String> {
    connect().await?.decrypt_delete(id).await.map_err(estr)
}

/// 分块读取 done 会话产物（manifest / http.jsonl）。
#[tauri::command]
pub async fn net_policy_decrypt_read(
    id: String,
    artifact: DecryptArtifact,
    offset: u64,
    len: u32,
) -> Result<CaptureChunkDto, String> {
    let (offset, data_base64, eof) = connect()
        .await?
        .decrypt_read(id, artifact, offset, len)
        .await
        .map_err(estr)?;
    Ok(CaptureChunkDto {
        offset,
        data_base64,
        eof,
    })
}

// ============ 统一出口（Egress*，minor 8，出口设计 §8.8）============
//
// 六个操作语义互不重叠：list 只读；probe 只测不改状态；start/stop 改生命周期；reconnect
// 重建会话；set_fallback 改不可用时的行为。改导流策略（谁用哪个出口）不在这里，走既有的
// `net_policy_save_settings` / `net_policy_save_rule`。全部回出口全量清单，供前端整表刷新。

/// 出口全量清单（生命周期与策略选中分两个字段，前端不得由其一推断另一个）。
#[tauri::command]
pub async fn net_policy_egress_list(
    _state: State<'_, AppState>,
) -> Result<Vec<EgressStatus>, String> {
    connect().await?.egress_list().await.map_err(estr)
}

/// 启动出口（渲染进配置 + 立即探测）。**不改变任何导流规则。**
#[tauri::command]
pub async fn net_policy_egress_start(
    _state: State<'_, AppState>,
    id: String,
) -> Result<Vec<EgressStatus>, String> {
    connect().await?.egress_start(id).await.map_err(estr)
}

/// 停止出口（从配置摘除；指向它的规则按 fallback 处理，默认阻断）。
#[tauri::command]
pub async fn net_policy_egress_stop(
    _state: State<'_, AppState>,
    id: String,
) -> Result<Vec<EgressStatus>, String> {
    connect().await?.egress_stop(id).await.map_err(estr)
}

/// 立即重连（重建会话，不改导流）。
#[tauri::command]
pub async fn net_policy_egress_reconnect(
    _state: State<'_, AppState>,
    id: String,
) -> Result<Vec<EgressStatus>, String> {
    connect().await?.egress_reconnect(id).await.map_err(estr)
}

/// 仅测试连接：探测一次，不改生命周期也不改导流策略。
#[tauri::command]
pub async fn net_policy_egress_probe(
    _state: State<'_, AppState>,
    id: String,
) -> Result<Vec<EgressStatus>, String> {
    connect().await?.egress_probe(id).await.map_err(estr)
}

/// 设置出口不可用时的处理方式（阻断 / 明确允许回落直连）。
#[tauri::command]
pub async fn net_policy_egress_set_fallback(
    _state: State<'_, AppState>,
    id: String,
    fallback: EgressFallback,
) -> Result<Vec<EgressStatus>, String> {
    connect()
        .await?
        .egress_set_fallback(id, fallback)
        .await
        .map_err(estr)
}

/// 刷新当前代理订阅，不主动重连当前节点。
#[tauri::command]
pub async fn net_policy_egress_refresh_subscription(
    _state: State<'_, AppState>,
    id: String,
) -> Result<Vec<EgressStatus>, String> {
    connect()
        .await?
        .egress_refresh_subscription(id)
        .await
        .map_err(estr)
}

/// 切换当前代理订阅节点。
#[tauri::command]
pub async fn net_policy_egress_select_node(
    _state: State<'_, AppState>,
    id: String,
    node: String,
) -> Result<Vec<EgressStatus>, String> {
    connect()
        .await?
        .egress_select_node(id, node)
        .await
        .map_err(estr)
}
