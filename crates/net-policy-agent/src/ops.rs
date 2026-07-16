//! 长操作（apply/stop/reload/set_enabled）作业逻辑（设计 §8）。
//!
//! - **独立作业 + 断线不亡（D7/§8.1）**：`run_operation` spawn 一个 detached 任务执行、持 op 互斥；
//!   请求 handler await 其 JoinHandle 拿终态 Status——若 handler future 被 drop（客户端断线），
//!   detached 任务**继续跑完**（JoinHandle drop 不 abort spawn 的任务）。
//! - **并发即 conflict（不排队）**：`try_begin_op` 已有操作在跑 → 立即返回 `Conflict`。
//! - 事务化回滚 + fail-closed 优先：忠实移植自原 zero-desktop `mod.rs::do_apply`。

use crate::engine;
use crate::firewall;
use crate::paths;
use crate::state::AgentState;
use net_policy_core::config::{self, ProcessRef, Route};
use net_policy_core::operation::{OperationKind, OperationStatus};
use net_policy_core::types::{NetPolicyStatus, TempDirectStatus};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// 长操作错误。
pub enum OpError {
    /// 已有操作在跑。
    Conflict,
    /// 执行失败（可读消息）。
    Failed(String),
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::Conflict => write!(f, "已有网络策略操作在进行中"),
            OpError::Failed(m) => write!(f, "{m}"),
        }
    }
}

type WorkFut = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// 统一作业包装：占用 op 互斥 → detached 执行 → await 终态 → 回新鲜 Status。
async fn run_operation<F>(
    state: Arc<AgentState>,
    kind: OperationKind,
    work: F,
) -> Result<NetPolicyStatus, OpError>
where
    F: FnOnce(Arc<AgentState>) -> WorkFut + Send + 'static,
{
    let id = match state.try_begin_op(kind) {
        Some(i) => i,
        None => return Err(OpError::Conflict),
    };
    let st = state.clone();
    // detached：即便下面的 await 被 drop（断线），本任务照跑完。
    let handle = tokio::spawn(async move {
        let result = work(st.clone()).await;
        let (status, err) = match &result {
            Ok(()) => (OperationStatus::Succeeded, None),
            Err(e) => (OperationStatus::Failed, Some(e.clone())),
        };
        st.end_op(id, kind, status, err);
        result
    });
    match handle.await {
        Ok(Ok(())) => Ok(state.status_fresh().await),
        Ok(Err(msg)) => Err(OpError::Failed(msg)),
        Err(_) => Err(OpError::Failed("操作任务异常终止".into())),
    }
}

/// 应用策略（供请求 handler 与 setup 自动恢复共用）。
pub async fn run_apply_operation(state: Arc<AgentState>) -> Result<NetPolicyStatus, OpError> {
    run_operation(state, OperationKind::Apply, |st| Box::pin(do_apply(st))).await
}

/// 停止 / 急停。
pub async fn run_stop_operation(state: Arc<AgentState>) -> Result<NetPolicyStatus, OpError> {
    run_operation(state, OperationKind::Stop, |st| Box::pin(do_stop(st))).await
}

/// 热重载。
pub async fn run_reload_operation(state: Arc<AgentState>) -> Result<NetPolicyStatus, OpError> {
    run_operation(state, OperationKind::Reload, |st| Box::pin(do_reload(st))).await
}

/// 主开关（开→apply、关→stop；失败回滚 enabled）。
pub async fn run_set_enabled_operation(
    state: Arc<AgentState>,
    enabled: bool,
) -> Result<NetPolicyStatus, OpError> {
    run_operation(state, OperationKind::SetEnabled, move |st| {
        Box::pin(do_set_enabled(st, enabled))
    })
    .await
}

fn e<T: std::fmt::Display>(x: T) -> String {
    format!("{x:#}")
}

/// 合并「主错误」与「回滚结果」：回滚成功只返回主错误；回滚失败把两者拼接返回，
/// **不让回滚错误掩盖真正的病因**（评审：`rollback(..)?` 会用回滚错替换原始 err）。
fn with_rollback(primary: String, rollback: Result<(), String>) -> String {
    match rollback {
        Ok(()) => primary,
        Err(rb) => format!("{primary}；回滚亦失败：{rb}"),
    }
}

/// 开启临时直连（限时应急）——**事务化**（评审点 1）：置 temp 态 → reload 使配置生效；
/// **reload 失败即回滚 temp 态并返回错误**（不制造「状态说开、配置没变」的分裂）。成功才记事件 +
/// 起过期定时器。
pub async fn run_set_temp_direct(
    state: Arc<AgentState>,
    duration_secs: u64,
    except: Vec<ProcessRef>,
) -> Result<TempDirectStatus, OpError> {
    let prev = state.temp_snapshot();
    let gen = state.set_temp(duration_secs, except.clone());
    // reload 使配置生效（未应用时 no-op 返回 Ok，下次 apply 自然带上）。失败回滚。
    if let Err(err) = run_reload_operation(state.clone()).await {
        let primary = err.to_string();
        state.restore_temp(prev);
        return match run_reload_operation(state.clone()).await {
            Ok(_) => Err(OpError::Failed(primary)),
            Err(rollback_err) => Err(OpError::Failed(format!(
                "{primary}；恢复原临时直连配置亦失败：{rollback_err}"
            ))),
        };
    }
    state.record_event(
        "temp_direct_on",
        &format!("{duration_secs}s, except={}", except.len()),
    );
    // 过期定时器：到期后重试 reload 还原（reload 读 temp_for_config 已按时间判定为未激活）。
    let st = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(duration_secs)).await;
        expire_temp(st, gen).await;
    });
    Ok(state.temp_status())
}

/// 提前解除临时直连——**事务化**：清 temp 态 → reload 还原；reload 失败则**保留 active 并返回错误**
/// （避免「状态说关、mihomo 仍 DIRECT」的反向分裂），用户可重试。
pub async fn run_clear_temp_direct(state: Arc<AgentState>) -> Result<TempDirectStatus, OpError> {
    if !state.temp_active_flag() {
        return Ok(state.temp_status());
    }
    let prev = state.temp_snapshot();
    state.clear_temp();
    if let Err(err) = run_reload_operation(state.clone()).await {
        let primary = err.to_string();
        state.restore_temp(prev);
        return match run_reload_operation(state.clone()).await {
            Ok(_) => Err(OpError::Failed(primary)),
            Err(rollback_err) => Err(OpError::Failed(format!(
                "{primary}；恢复临时直连状态亦失败：{rollback_err}"
            ))),
        };
    }
    state.record_event("temp_direct_off", "manual");
    Ok(state.temp_status())
}

/// 过期还原：仅当未被后续 set/clear 取代（gen 未变）时执行；reload 带重试，持续失败则记危险事件
/// （不谎报已关闭——`temp_status` 按时间已呈现未激活，但 mihomo 配置可能仍 DIRECT，须可见）。
async fn expire_temp(state: Arc<AgentState>, gen: u64) {
    if state.temp_generation() != gen {
        return; // 已被后续操作取代
    }
    for attempt in 0..6 {
        if state.temp_generation() != gen {
            return;
        }
        match run_reload_operation(state.clone()).await {
            Ok(_) => {
                if state.temp_generation() == gen {
                    state.clear_temp();
                    state.record_event("temp_direct_off", "expired");
                }
                return;
            }
            Err(_) => {
                state.record_event(
                    "temp_direct_expire_reload_failed",
                    &format!("attempt {}", attempt + 1),
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
    state.record_event(
        "temp_direct_expire_gave_up",
        "reload 持续失败，mihomo 可能仍处于临时直连配置——请手动 reload/apply 或检查引擎",
    );
}

/// apply 的核心实现（忠实移植原 do_apply：事务化 + fail-closed 优先 + 受控 reapply）。
async fn do_apply(state: Arc<AgentState>) -> Result<(), String> {
    if !crate::win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    state.invalidate_status_cache();
    let ws = state.workspace.clone();
    let settings = config::try_load_settings(&ws).map_err(e)?;
    let rules = config::try_load_rules(&ws).map_err(e)?;

    // 步 0：校验配置（含跨表一致性）。
    state.emit_progress(0, "running", None);
    if let Err(err) = config::validate_combined(&settings, &rules) {
        state.emit_progress(0, "fail", Some(e(&err)));
        return Err(e(err));
    }
    state.emit_progress(0, "ok", None);

    // 观察/Direct 姿态 kill-switch 无安全意义（崩溃只是不走代理，不泄密）；强装反而会砖网。
    let killswitch = settings.killswitch_enabled && settings.default_route != Route::Direct;
    let mihomo_bin = paths::resolve_mihomo_bin(state.dev).map_err(e)?;
    let firewall_signature = if killswitch {
        Some(firewall::configuration_signature(&settings, &mihomo_bin))
    } else {
        None
    };
    let mihomo_home = paths::mihomo_home(&ws);
    let secret = engine::gen_secret();
    let temp = state.temp_for_config();
    let ev_route = settings.default_route; // 事件详情用（settings 随后被 move 进闭包）。

    let st = state.clone();
    let ws2 = ws.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let state = st;
        let ws = ws2;
        // 前置：必须管理员。
        if !crate::win::is_elevated() {
            let msg = "需要以管理员身份运行守护：改全局防火墙、建 TUN 网卡均需管理员权限。";
            state.emit_progress(1, "fail", Some(msg.into()));
            return Err(msg.into());
        }
        let (was_applied, old_pid, old_secret) = {
            let rt = state.rt.lock().unwrap();
            (rt.applied, rt.mihomo_pid, rt.secret.clone().unwrap_or_default())
        };
        let engine_name = mihomo_bin
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "mihomo-windows-amd64".into());

        // 端口预检：未受管却已有 controller → 报可读诊断，不盲起抢端口。
        if !was_applied && engine::controller_present() {
            let msg = format!(
                "127.0.0.1:9090 已有 external-controller 在响应但不受本守护管理——可能是其他 clash/mihomo 或残留 {engine_name}。请先手动结束该进程再应用。"
            );
            state.emit_progress(1, "fail", Some(msg.clone()));
            return Err(msg);
        }

        // 受控 reapply：先优雅停旧引擎（旧 kill-switch 仍生效，交换窗口 fail-closed 不破）。
        if was_applied {
            engine::graceful_stop(old_pid, &old_secret, &engine_name)
                .map_err(|err| e(err.context("受控 reapply：优雅停旧 mihomo 失败（保留原 kill-switch）")))?;
            let mut rt = state.rt.lock().unwrap();
            rt.mihomo_pid = None;
            rt.secret = None;
            rt.firewall_signature = None;
        }

        // 回滚：有新 pid 时先优雅停引擎；停不掉则保留防火墙维持 fail-closed。
        let rollback = |new_pid: Option<u32>| -> Result<(), String> {
            if let Some(p) = new_pid {
                if let Err(err) = engine::graceful_stop(Some(p), &secret, &engine_name) {
                    let mut rt = state.rt.lock().unwrap();
                    rt.applied = true;
                    rt.mihomo_pid = Some(p);
                    rt.secret = Some(secret.clone());
                    rt.firewall_signature = firewall_signature.clone();
                    return Err(e(err));
                }
            }
            if killswitch && !was_applied {
                let _ = firewall::remove(&ws);
            }
            let mut rt = state.rt.lock().unwrap();
            rt.applied = false;
            rt.mihomo_pid = None;
            rt.secret = None;
            rt.firewall_signature = None;
            Ok(())
        };
        let rollback_note = if killswitch && was_applied {
            "；kill-switch 已保留（fail-closed 优先），重试或用「停止」显式解除"
        } else {
            ""
        };

        // 安装器覆盖升级时不会杀数据面，而是把新 mihomo 留为 pending。只有来到本来就会
        // 受控重启引擎的 reapply 窗口，才在旧进程已停、kill-switch 仍保持时切换二进制。
        if let Err(err) = paths::activate_pending_mihomo() {
            state.emit_progress(1, "fail", Some(format!("{}{rollback_note}", e(&err))));
            return Err(with_rollback(
                format!("切换暂存 mihomo：{}{rollback_note}", e(err)),
                rollback(None),
            ));
        }

        // 步 1：先建 fail-closed 基线（不依赖 Meta），再写配置。
        state.emit_progress(1, "running", None);
        if killswitch {
            if let Err(err) = firewall::apply_base(&ws, &settings, &mihomo_bin) {
                state.emit_progress(1, "fail", Some(format!("{}{rollback_note}", e(&err))));
                return Err(with_rollback(
                    format!("apply kill-switch（阶段A）：{}{rollback_note}", e(err)),
                    rollback(None),
                ));
            }
        } else if firewall::status()
            .map(|f| f.rule_count > 0 || f.active)
            .unwrap_or(false)
        {
            // 本次不需要 kill-switch，但可能残留上次 Blackhole/Wg 的痕迹（含半残留），主动撤掉。
            let _ = firewall::remove(&ws);
        }
        if let Err(err) = engine::write_config(&ws, &settings, &rules, &secret, &temp) {
            state.emit_progress(1, "fail", Some(format!("{}{rollback_note}", e(&err))));
            return Err(with_rollback(
                format!("write mihomo config：{}{rollback_note}", e(err)),
                rollback(None),
            ));
        }
        state.emit_progress(
            1,
            "ok",
            Some(if killswitch {
                "kill-switch 基线已就位".into()
            } else {
                "不受保护预览（未装 kill-switch）".into()
            }),
        );

        // 步 2：启动 mihomo。
        state.emit_progress(2, "running", None);
        let cfg = config::mihomo_config_path(&ws);
        let pid = match engine::start(&mihomo_bin, &mihomo_home, &cfg) {
            Ok(p) => p,
            Err(err) => {
                state.emit_progress(2, "fail", Some(format!("{}{rollback_note}", e(&err))));
                return Err(with_rollback(
                    format!("start mihomo：{}{rollback_note}", e(err)),
                    rollback(None),
                ));
            }
        };
        {
            let mut rt = state.rt.lock().unwrap();
            rt.mihomo_pid = Some(pid);
            rt.secret = Some(secret.clone());
        }
        state.emit_progress(2, "ok", Some(format!("pid {pid}")));

        // 步 3：等待 TUN 起栈（14×500ms）。
        state.emit_progress(3, "running", Some("0/14".into()));
        let mut tun_ready = false;
        for i in 0..14 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if engine::running(&secret) && engine::tun_up() {
                tun_ready = true;
                state.emit_progress(3, "running", Some(format!("{}/14 已起栈", i + 1)));
                break;
            }
            state.emit_progress(3, "running", Some(format!("{}/14", i + 1)));
        }
        if !tun_ready {
            let detail = if engine::running(&secret) {
                "控制器可达但 TUN(Meta) 未在超时内起栈"
            } else {
                "mihomo 外部控制器不可达"
            };
            state.emit_progress(3, "fail", Some(format!("{detail}{rollback_note}")));
            return Err(with_rollback(
                format!("等待 TUN 起栈超时（{detail}），已回滚{rollback_note}"),
                rollback(Some(pid)),
            ));
        }
        state.emit_progress(3, "ok", None);

        // 步 4：补 KS-TUN。
        state.emit_progress(4, "running", None);
        if killswitch {
            if let Err(err) = firewall::apply_tun(&ws) {
                state.emit_progress(4, "fail", Some(format!("{}{rollback_note}", e(&err))));
                return Err(with_rollback(
                    format!("apply kill-switch（阶段B KS-TUN）：{}{rollback_note}", e(err)),
                    rollback(Some(pid)),
                ));
            }
        }
        state.emit_progress(4, "ok", None);

        // 步 5：终确认。
        state.emit_progress(5, "running", None);
        {
            let mut rt = state.rt.lock().unwrap();
            rt.applied = true;
            rt.mihomo_pid = Some(pid);
            rt.secret = Some(secret.clone());
            rt.firewall_signature = firewall_signature;
        }
        state.emit_progress(5, "ok", Some("引擎在线 · TUN 已起栈".into()));
        Ok(())
    })
    .await
    .map_err(|_| "apply 阻塞任务 panic".to_string())??;
    state.record_event("policy_applied", &format!("default_route={ev_route:?}"));
    Ok(())
}

/// 停止核心：优雅停引擎 → 撤防火墙 → 清 rt → 清 enabled（手动停 = 下次不自动恢复）。
async fn do_stop(state: Arc<AgentState>) -> Result<(), String> {
    if !crate::win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    state.invalidate_status_cache();
    let ws = state.workspace.clone();
    let (pid, secret) = {
        let rt = state.rt.lock().unwrap();
        (rt.mihomo_pid, rt.secret.clone().unwrap_or_default())
    };
    let dev = state.dev;
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let engine_name = paths::resolve_mihomo_bin(dev)
            .ok()
            .and_then(|b| b.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "mihomo-windows-amd64".into());
        engine::graceful_stop(pid, &secret, &engine_name).map_err(|err| {
            e(err.context("优雅停 mihomo 失败（防火墙保持生效以维持 fail-closed）"))
        })?;
        firewall::remove(&ws).map_err(|err| e(err.context("移除 kill-switch")))?;
        Ok(())
    })
    .await
    .map_err(|_| "stop 阻塞任务 panic".to_string())??;

    {
        let mut rt = state.rt.lock().unwrap();
        rt.applied = false;
        rt.mihomo_pid = None;
        rt.secret = None;
        rt.firewall_signature = None;
    }
    // 手动停止 → 清 enabled（下次启动不自动恢复）。
    if let Ok(mut s) = config::try_load_settings(&state.workspace) {
        if s.enabled {
            s.enabled = false;
            let _ = config::save_settings(&state.workspace, &s);
        }
    }
    state.record_event("policy_stopped", "");
    Ok(())
}

/// 热重载核心：按姿态对齐防火墙 + mihomo PUT /configs 原地生效。未应用时 no-op。
async fn do_reload(state: Arc<AgentState>) -> Result<(), String> {
    if !crate::win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    state.invalidate_status_cache();
    let ws = state.workspace.clone();
    let (applied, secret, current_firewall_signature) = {
        let rt = state.rt.lock().unwrap();
        (
            rt.applied,
            rt.secret.clone().unwrap_or_default(),
            rt.firewall_signature.clone(),
        )
    };
    if !applied {
        return Ok(());
    }
    let settings = config::try_load_settings(&ws).map_err(e)?;
    let rules = config::try_load_rules(&ws).map_err(e)?;
    config::validate_combined(&settings, &rules).map_err(e)?;
    let dev = state.dev;
    let temp = state.temp_for_config();
    let state_for_reload = state.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        // kill-switch 仍按用户设定姿态挂（临时直连只改 mihomo MATCH，不摘围栏——DIRECT 经 mihomo
        // 拨号出物理由 R-mihomo 放行，例外进程仍被 Blackhole/IPv6 阻断，更安全）。
        let want = settings.killswitch_enabled && settings.default_route != Route::Direct;
        let up = firewall::status()
            .map(|f| f.rule_count > 0 || f.active)
            .unwrap_or(false);
        let bin = paths::resolve_mihomo_bin(dev).map_err(e)?;
        let desired_firewall_signature = if want {
            Some(firewall::configuration_signature(&settings, &bin))
        } else {
            None
        };
        let firewall_needs_sync =
            want && (!up || current_firewall_signature != desired_firewall_signature);
        if firewall_needs_sync {
            firewall::apply_base(&ws, &settings, &bin).map_err(e)?;
        }
        if let Err(err) = engine::write_config(&ws, &settings, &rules, &secret, &temp)
            .and_then(|_| engine::reload(&ws, &secret))
        {
            if want && !up {
                let _ = firewall::remove(&ws);
            }
            return Err(e(err));
        }
        if firewall_needs_sync {
            firewall::apply_tun(&ws).map_err(e)?;
        } else if !want && up {
            firewall::remove(&ws).map_err(e)?;
        }
        // mihomo 热载只影响新连接；旧连接必须主动关闭，才能保证改路、规则删除、临时直连
        // 开关及到期还原都在本次操作返回前真正生效。
        engine::reset_connections(&secret).map_err(e)?;
        state_for_reload.rt.lock().unwrap().firewall_signature = desired_firewall_signature;
        Ok(())
    })
    .await
    .map_err(|_| "reload 阻塞任务 panic".to_string())?
}

/// 主开关核心：存 enabled → 开则 apply、关则 stop（失败回滚 enabled）。
async fn do_set_enabled(state: Arc<AgentState>, enabled: bool) -> Result<(), String> {
    if !crate::win::is_windows() {
        return Err("net-policy 仅支持 Windows".into());
    }
    let ws = state.workspace.clone();
    let mut settings = config::try_load_settings(&ws).map_err(e)?;
    let previous = settings.enabled;
    settings.enabled = enabled;
    settings.validate().map_err(e)?;
    config::save_settings(&ws, &settings).map_err(e)?;
    state.invalidate_status_cache();

    if enabled {
        match do_apply(state.clone()).await {
            Ok(()) => Ok(()),
            Err(apply_err) => {
                // 回滚 enabled。
                settings.enabled = previous;
                let _ = config::save_settings(&ws, &settings);
                state.invalidate_status_cache();
                Err(apply_err)
            }
        }
    } else {
        let applied = { state.rt.lock().unwrap().applied };
        if applied {
            do_stop(state).await
        } else {
            Ok(())
        }
    }
}
