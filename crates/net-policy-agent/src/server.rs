//! 命名管道 server：接受循环 + 版本握手 + 请求分发 + 事件订阅流（设计 §4/§7/§8.2）。

use crate::frame::{read_frame, write_frame};
use crate::ops::{self, OpError};
use crate::state::AgentState;
use crate::store::now_ms;
use crate::{connections, engine, paths, process_watch, ptree, repair, verify};
use anyhow::{Context, Result};
use net_policy_core::config::{self, WgConfig};
use net_policy_core::protocol::{
    ErrorKind, Event, Frame, ProtocolError, Request, Response, Version,
};
use std::sync::Arc;

/// 写配置类请求在长操作期间的互斥提示（评审点 7）。
fn busy_resp() -> Response {
    err_resp(
        ErrorKind::OperationConflict,
        "有网络策略操作正在进行，配置暂不可写，请稍候再试",
    )
}

fn err_resp(kind: ErrorKind, msg: impl Into<String>) -> Response {
    Response::Error {
        error: ProtocolError::new(kind, msg),
    }
}

/// 探测通过的能力列表（`Hello.capabilities`，§10）：pktmon 可用 → `capture_v1`。
/// L4 `decrypt_v1` 受 ADR 阻断（引擎/CA 未验证），此处不声明。
fn capture_capabilities() -> Vec<String> {
    let mut caps = Vec::new();
    if crate::capture::available() {
        caps.push(net_policy_core::capture::CAPABILITY_CAPTURE_V1.to_string());
    }
    caps
}

/// 分块读取 `done` 会话的 pcapng（§10：len ≤ 512 KiB，base64 分块，仅 done 会话）。
fn capture_read(state: &Arc<AgentState>, id: &str, offset: u64, len: u32) -> Response {
    use base64::Engine;
    use std::io::{Read, Seek, SeekFrom};
    let session = match state.capture.get(id) {
        Ok(s) => s,
        Err(e) => return Response::Error { error: e },
    };
    if !matches!(session.state, net_policy_core::capture::CaptureState::Done) {
        return err_resp(ErrorKind::CaptureNotFound, "仅 done 会话可下载");
    }
    let Some(path) = state.capture.store().pcapng_path(id) else {
        return err_resp(ErrorKind::Validation, "非法会话 id");
    };
    let file_len = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) => return err_resp(ErrorKind::Internal, format!("读 pcapng 失败：{e}")),
    };
    if let Err(e) = net_policy_core::capture::validate_read_window(offset, len, file_len) {
        return err_resp(ErrorKind::Validation, e.to_string());
    }
    let mut f = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => return err_resp(ErrorKind::Internal, format!("open pcapng：{e}")),
    };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return err_resp(ErrorKind::Internal, "seek 失败");
    }
    let to_read = std::cmp::min(len as u64, file_len - offset) as usize;
    let mut buf = vec![0u8; to_read];
    if f.read_exact(&mut buf).is_err() {
        return err_resp(ErrorKind::Internal, "读取 pcapng 分块失败");
    }
    let eof = offset + to_read as u64 >= file_len;
    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&buf);
    Response::CaptureChunk {
        id: id.to_string(),
        offset,
        data_base64,
        eof,
    }
}

fn op_result(r: Result<net_policy_core::types::NetPolicyStatus, OpError>) -> Response {
    match r {
        Ok(status) => Response::Status { status },
        Err(OpError::Conflict) => err_resp(
            ErrorKind::OperationConflict,
            "已有网络策略操作在进行中，请稍候再试",
        ),
        Err(OpError::Failed(m)) => err_resp(ErrorKind::Internal, m),
    }
}

/// 分发单个业务请求（Hello / SubscribeEvents 在连接循环里单独处理）。
async fn dispatch(state: Arc<AgentState>, req: Request) -> Response {
    let ws = state.workspace.clone();
    match req {
        Request::Hello { .. } | Request::SubscribeEvents => {
            // 由连接循环处理，不应到这里。
            err_resp(ErrorKind::Internal, "协议帧处理错误")
        }

        Request::GetStatus => Response::Status {
            status: state.status_cached().await,
        },

        Request::GetSettings => match config::try_load_settings(&ws) {
            Ok(settings) => Response::Settings { settings },
            Err(e) => err_resp(ErrorKind::Internal, format!("{e:#}")),
        },

        Request::SaveSettings { settings } => {
            if state.op_in_flight() {
                return busy_resp();
            }
            if let Err(e) = settings.validate() {
                return err_resp(ErrorKind::Validation, format!("{e:#}"));
            }
            match config::save_settings(&ws, &settings) {
                Ok(()) => Response::Ok,
                Err(e) => err_resp(ErrorKind::Internal, format!("{e:#}")),
            }
        }

        Request::ParseWgConf { content } => match WgConfig::from_wg_quick(&content) {
            Ok(wg) => Response::Wg { wg },
            Err(e) => err_resp(ErrorKind::Validation, format!("{e:#}")),
        },

        Request::ListRules => match config::try_load_rules(&ws) {
            Ok(rules) => Response::Rules { rules },
            Err(e) => err_resp(ErrorKind::Internal, format!("{e:#}")),
        },

        Request::SaveRule { rule } => {
            if state.op_in_flight() {
                return busy_resp();
            }
            if let Err(e) = rule.validate() {
                return err_resp(ErrorKind::Validation, format!("{e:#}"));
            }
            let mut rs = match config::try_load_rules(&ws) {
                Ok(r) => r,
                Err(e) => return err_resp(ErrorKind::Internal, format!("{e:#}")),
            };
            let already_covered = rs.rules.iter().any(|r| rule.covered_by_same_route(r));
            rs.rules
                .retain(|r| !r.same_target(&rule) && !r.covered_by_same_route(&rule));
            if !already_covered {
                rs.rules.push(rule);
            }
            match config::save_rules(&ws, &rs) {
                Ok(()) => Response::Rules { rules: rs },
                Err(e) => err_resp(ErrorKind::Internal, format!("{e:#}")),
            }
        }

        Request::DeleteRule { rule } => {
            if state.op_in_flight() {
                return busy_resp();
            }
            let mut rs = match config::try_load_rules(&ws) {
                Ok(r) => r,
                Err(e) => return err_resp(ErrorKind::Internal, format!("{e:#}")),
            };
            let before = rs.rules.len();
            rs.rules.retain(|r| !r.same_target(&rule));
            if rs.rules.len() == before {
                return err_resp(ErrorKind::RuleNotFound, "未找到该规则（可能已被删除）");
            }
            match config::save_rules(&ws, &rs) {
                Ok(()) => Response::Rules { rules: rs },
                Err(e) => err_resp(ErrorKind::Internal, format!("{e:#}")),
            }
        }

        Request::ListProcessCandidates => {
            if !crate::win::is_windows() {
                return err_resp(ErrorKind::Unsupported, "net-policy 仅支持 Windows");
            }
            match tokio::task::spawn_blocking(process_watch::list_candidates).await {
                Ok(Ok(processes)) => Response::Processes { processes },
                Ok(Err(e)) => err_resp(ErrorKind::Internal, format!("{e:#}")),
                Err(_) => err_resp(ErrorKind::Internal, "进程枚举任务异常"),
            }
        }

        Request::GetConnections => {
            let secret = {
                let rt = state.rt.lock().unwrap();
                rt.secret.clone().unwrap_or_default()
            };
            let snapshot = connections::fetch(&secret).await;
            state.obs.ingest_connections(&snapshot.connections);
            Response::Connections { snapshot }
        }

        Request::Blocked => Response::Blocked {
            entries: state.obs.blocked_snapshot(),
        },
        Request::ClearBlocked => {
            state.obs.clear_blocked();
            Response::Ok
        }
        Request::DnsMap => Response::DnsMap {
            entries: state.obs.dns_snapshot(),
        },

        Request::Verify => {
            if !crate::win::is_windows() {
                return err_resp(ErrorKind::Unsupported, "net-policy 仅支持 Windows");
            }
            let secret = {
                let rt = state.rt.lock().unwrap();
                rt.secret.clone().unwrap_or_default()
            };
            let default_route = match config::try_load_settings(&ws) {
                Ok(s) => s.default_route,
                Err(e) => return err_resp(ErrorKind::Internal, format!("{e:#}")),
            };
            match tokio::task::spawn_blocking(move || verify::run(&secret, default_route)).await {
                Ok(Ok(report)) => Response::Verify { report },
                Ok(Err(e)) => err_resp(ErrorKind::Internal, format!("{e:#}")),
                Err(_) => err_resp(ErrorKind::Internal, "验证任务异常"),
            }
        }

        Request::Repair { force } => {
            if !crate::win::is_windows() {
                return err_resp(ErrorKind::Unsupported, "net-policy 仅支持 Windows");
            }
            if state.op_in_flight() {
                return err_resp(ErrorKind::OperationConflict, "有操作进行中，请稍候再修复");
            }
            let ws2 = ws.clone();
            match tokio::task::spawn_blocking(move || repair::graded_repair(&ws2, force)).await {
                Ok(Ok(result)) => Response::Repair { result },
                Ok(Err(e)) => err_resp(ErrorKind::Internal, format!("{e:#}")),
                Err(_) => err_resp(ErrorKind::Internal, "修复任务异常"),
            }
        }

        Request::Apply => op_result(ops::run_apply_operation(state.clone()).await),
        Request::Stop => op_result(ops::run_stop_operation(state.clone()).await),
        Request::Reload => op_result(ops::run_reload_operation(state.clone()).await),
        Request::SetEnabled { enabled } => {
            op_result(ops::run_set_enabled_operation(state.clone(), enabled).await)
        }

        Request::GetCurrentOperation => Response::Operation {
            operation: state.current_operation(),
        },

        // ── minor 2：记录 / 观测 / 临时直连 / 路由 ──────────────────────────────
        // limit 硬上限 1000（评审点 3：防单帧超 8MiB 上限导致编码失败断管道）。
        Request::GetRequests { limit } => match state.store.recent_requests(limit.min(1000)) {
            Ok(entries) => Response::Requests { entries },
            Err(e) => err_resp(ErrorKind::Internal, format!("读请求记录失败：{e:#}")),
        },

        Request::GetEvents { limit } => match state.store.recent_events(limit.min(1000)) {
            Ok(entries) => Response::Events { entries },
            Err(e) => err_resp(ErrorKind::Internal, format!("读事件失败：{e:#}")),
        },

        Request::ClearRequests => match state.store.clear_requests() {
            Ok(_) => {
                state.record_event("requests_cleared", "");
                Response::Ok
            }
            Err(e) => err_resp(ErrorKind::Internal, format!("清空请求记录失败：{e:#}")),
        },

        Request::ClearEvents => match state.store.clear_events() {
            Ok(_) => Response::Ok,
            Err(e) => err_resp(ErrorKind::Internal, format!("清空事件失败：{e:#}")),
        },

        Request::GetProcessTree => {
            if !crate::win::is_windows() {
                return err_resp(ErrorKind::Unsupported, "net-policy 仅支持 Windows");
            }
            match tokio::task::spawn_blocking(ptree::process_tree).await {
                Ok(Ok(roots)) => Response::ProcessTree { roots },
                Ok(Err(e)) => err_resp(ErrorKind::Internal, format!("{e:#}")),
                Err(_) => err_resp(ErrorKind::Internal, "进程树任务异常"),
            }
        }

        Request::GetRoutes => {
            let settings = match config::try_load_settings(&ws) {
                Ok(s) => s,
                Err(e) => return err_resp(ErrorKind::Internal, format!("{e:#}")),
            };
            let rules = match config::try_load_rules(&ws) {
                Ok(r) => r,
                Err(e) => return err_resp(ErrorKind::Internal, format!("{e:#}")),
            };
            let temp = state.temp_for_config();
            Response::Routes {
                routes: net_policy_core::routes::effective_routes(&settings, &rules, &temp),
            }
        }

        Request::GetTempDirect => Response::TempDirect {
            status: state.temp_status(),
        },

        Request::SetTempDirect {
            duration_secs,
            except,
        } => temp_result(ops::run_set_temp_direct(state.clone(), duration_secs, except).await),

        Request::ClearTempDirect => temp_result(ops::run_clear_temp_direct(state.clone()).await),

        // ── minor 3：连接重置 / 运行日志 ────────────────────────────────────
        Request::ResetConnections => {
            let secret = {
                let rt = state.rt.lock().unwrap();
                rt.secret.clone().unwrap_or_default()
            };
            match tokio::task::spawn_blocking(move || engine::reset_connections(&secret)).await {
                Ok(Ok(())) => Response::Ok,
                Ok(Err(e)) => err_resp(ErrorKind::MihomoUnreachable, format!("{e:#}")),
                Err(_) => err_resp(ErrorKind::Internal, "重置连接任务异常"),
            }
        }

        Request::GetMihomoLog { lines } => {
            // limit 硬上限 1000（同 GetRequests：防单帧超 8MiB 上限）。
            let n = lines.min(1000) as usize;
            let path = paths::mihomo_home(&ws).join(engine::MIHOMO_LOG_FILE);
            match tokio::task::spawn_blocking(move || read_log_tail(&path, n)).await {
                Ok(Ok(entries)) => Response::MihomoLog { lines: entries },
                Ok(Err(e)) => err_resp(ErrorKind::Internal, format!("读运行日志失败：{e:#}")),
                Err(_) => err_resp(ErrorKind::Internal, "读日志任务异常"),
            }
        }

        // ── 抓包（抓包设计 §10；Phase 2a 全 TUN，pktmon 后端真机 spike 已过）─────────
        // pktmon→pcapng 管道见 docs/net-policy/net-policy-capture-validation-report.md。定向抓包
        // （Process/Domain/Ip）仍待 Phase 2b（fake-ip 端点解析），manager 对定向 target 返回
        // `capture_target_empty`。全 TUN 需 mihomo TUN 已起栈，否则 `capture_component_not_found`。
        Request::CaptureStart { target, opts } => {
            // 定向 target（进程/域名/IP）：先取当前连接快照解析包面端点（§5.1）。All 传空。
            let endpoints = if target.is_directed() {
                let secret = {
                    let rt = state.rt.lock().unwrap();
                    rt.secret.clone().unwrap_or_default()
                };
                let snap = connections::fetch(&secret).await;
                match crate::capture::resolve_endpoints(&target, &snap.connections) {
                    Ok(eps) => eps,
                    Err(kind) => {
                        return err_resp(
                            kind,
                            "定向目标未解析到包面端点：请先让目标产生流量，或改用全 TUN 短抓",
                        )
                    }
                }
            } else {
                Vec::new()
            };
            let st = state.clone();
            let res = tokio::task::spawn_blocking(move || {
                let rand16: [u8; 16] = rand::random();
                st.capture
                    .start(target, opts, endpoints, rand16, now_ms(), String::new())
            })
            .await;
            match res {
                Ok(Ok(session)) => {
                    // 时间上限：spawn 定时器到时自动 stop（§9 时间上限由 agent 定时 stop）。
                    let st2 = state.clone();
                    let id = session.id.clone();
                    let secs = session.opts.max_secs;
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                        let _ = tokio::task::spawn_blocking(move || {
                            st2.capture.stop(
                                &id,
                                net_policy_core::capture::CaptureStopReason::Timeout,
                                now_ms(),
                            )
                        })
                        .await;
                    });
                    Response::CaptureSession { session }
                }
                Ok(Err(e)) => Response::Error { error: e },
                Err(_) => err_resp(ErrorKind::Internal, "抓包任务异常"),
            }
        }
        Request::CaptureStop { id } => {
            let st = state.clone();
            match tokio::task::spawn_blocking(move || {
                st.capture.stop(
                    &id,
                    net_policy_core::capture::CaptureStopReason::User,
                    now_ms(),
                )
            })
            .await
            {
                Ok(Ok(session)) => Response::CaptureSession { session },
                Ok(Err(e)) => Response::Error { error: e },
                Err(_) => err_resp(ErrorKind::Internal, "停止抓包异常"),
            }
        }
        Request::CaptureGet { id } => match state.capture.get(&id) {
            Ok(session) => Response::CaptureSession { session },
            Err(e) => Response::Error { error: e },
        },
        Request::CaptureList => Response::CaptureSessions {
            sessions: state.capture.list(),
        },
        Request::CaptureDelete { id } => match state.capture.delete(&id) {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error { error: e },
        },
        Request::CaptureRead { id, offset, len } => capture_read(&state, &id, offset, len),

        // ── L4 应用明文（抓包设计 §17–§18）───────────────────────────────────
        // 协议层已就绪（core::decrypt 纯逻辑 + 脱敏 + 参数/目标校验），但真实 MITM 引擎受
        // adr-2026-07-phase4-mitm-engine.md 阻断：mitmproxy PyInstaller 二进制被 Windows Defender
        // 查杀，Phase 4a 冷启动待用户就 AV 处置决策；CA 生命周期 / DPAPI 私钥 / 数据面同样未验证。
        // 未通过前不实现真实解密，也不声明 `decrypt_v1`——对所有 Decrypt* 请求返回 `decrypt_unsupported`。
        Request::DecryptCaStatus
        | Request::DecryptCaCreate
        | Request::DecryptCaConfirmInstalled { .. }
        | Request::DecryptCaRemove
        | Request::DecryptStart { .. }
        | Request::DecryptStop { .. }
        | Request::DecryptGet { .. }
        | Request::DecryptList
        | Request::DecryptDelete { .. }
        | Request::DecryptRead { .. } => err_resp(
            ErrorKind::DecryptUnsupported,
            "L4 应用明文尚未启用（引擎/CA 待 Phase 4a 真机验证与 AV 决策）",
        ),
    }
}

/// 读日志文件最近 `n` 行；文件不存在（引擎没跑过）返回空列表，不是错误。
fn read_log_tail(path: &std::path::Path, n: usize) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let all: Vec<&str> = content.lines().collect();
    let start = all.len().saturating_sub(n);
    Ok(all[start..].iter().map(|s| s.to_string()).collect())
}

fn temp_result(r: Result<net_policy_core::types::TempDirectStatus, OpError>) -> Response {
    match r {
        Ok(status) => Response::TempDirect { status },
        Err(OpError::Conflict) => {
            err_resp(ErrorKind::OperationConflict, "有操作进行中，请稍候再试")
        }
        Err(OpError::Failed(m)) => err_resp(ErrorKind::Internal, m),
    }
}

/// 单连接处理：先握手（major 不同即拒绝所有业务请求），再一问一答；`SubscribeEvents` 转事件流。
async fn handle_connection<S>(state: Arc<AgentState>, mut pipe: S) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut hello_done = false;
    loop {
        let frame = match read_frame(&mut pipe).await {
            Ok(f) => f,
            Err(_) => return Ok(()), // 断线/EOF
        };
        let (id, req) = match frame {
            Frame::Request { id, req } => (id, req),
            _ => continue, // 请求连接上不应收到 Response/Event
        };

        match req {
            Request::Hello { version } => {
                let resp = if Version::CURRENT.compatible_with(version) {
                    hello_done = true;
                    Response::Hello {
                        version: Version::CURRENT,
                        // pktmon 探测通过才声明 `capture_v1`（§10）；L4 `decrypt_v1` 仍受 ADR 阻断不声明。
                        capabilities: capture_capabilities(),
                    }
                } else {
                    err_resp(
                        ErrorKind::VersionIncompatible,
                        format!(
                            "协议大版本不兼容：client major={}，agent major={}，请升级",
                            version.major,
                            Version::CURRENT.major
                        ),
                    )
                };
                write_frame(&mut pipe, &Frame::response(id, resp)).await?;
            }

            _ if !hello_done => {
                // 未握手先拒绝所有业务请求（§5：major 不同只允许握手）。
                let resp = err_resp(ErrorKind::VersionIncompatible, "请先完成版本握手（Hello）");
                write_frame(&mut pipe, &Frame::response(id, resp)).await?;
            }

            Request::SubscribeEvents => {
                // 确认订阅，然后本连接转为只推事件（另一条连接跑请求，§4.1）。
                write_frame(&mut pipe, &Frame::response(id, Response::Ok)).await?;
                let mut rx = state.subscribe_events();
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            if write_frame(&mut pipe, &Frame::event(ev)).await.is_err() {
                                return Ok(()); // 订阅端断线
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // 订阅端落后、可能已丢事件（含终态 OperationFinished）——发一帧
                            // ResyncRequired 让 GUI 以真实态对齐，并**关闭订阅**（评审点 8）。
                            let _ =
                                write_frame(&mut pipe, &Frame::event(Event::ResyncRequired)).await;
                            return Ok(());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
            }

            other => {
                let resp = dispatch(state.clone(), other).await;
                write_frame(&mut pipe, &Frame::response(id, resp)).await?;
            }
        }
    }
}

/// 启动命名管道 server 接受循环（阻塞直到出错）。
#[cfg(windows)]
pub async fn serve(
    state: Arc<AgentState>,
    ready: Option<std::sync::mpsc::SyncSender<Result<(), String>>>,
) -> Result<()> {
    use net_policy_core::protocol::PIPE_NAME;
    use tokio::net::windows::named_pipe::ServerOptions;

    // fail-closed（评审点 2）：提权控制面的 ACL 构建失败 **必须拒绝启动**，不降级到默认安全。
    let attrs = crate::security::build_pipe_security();
    if attrs.is_null() {
        let message =
            "无法为控制面管道构建 DACL（取用户 SID / 安全描述符失败）——拒绝以不安全方式启动";
        if let Some(tx) = ready {
            let _ = tx.send(Err(message.to_string()));
        }
        anyhow::bail!(message);
    }

    // 第一个实例（first_pipe_instance 防同名管道被他人抢建）。
    // SAFETY: attrs 为 build_pipe_security 返回的有效 SECURITY_ATTRIBUTES 指针或 null。
    let first = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .create_with_security_attributes_raw(PIPE_NAME, attrs)
    };
    let mut server = match first {
        Ok(server) => server,
        Err(error) => {
            let message = format!("首次创建控制面管道 {PIPE_NAME} 失败：{error}");
            if let Some(tx) = ready {
                let _ = tx.send(Err(message.clone()));
            }
            anyhow::bail!(message);
        }
    };
    log::info!("net-policy-agent 管道 server 就绪：{PIPE_NAME}");
    if let Some(tx) = ready {
        let _ = tx.send(Ok(()));
    }

    // 退避：accept/重建管道的瞬态失败**不得打死守护**（评审点 3）——否则一次抖动就让 agent 退出，
    // 计划任务重启 3 次耗尽后彻底躺平，而 kill-switch 可能仍挂着（网络阻断却无人管理）。
    let backoff = std::time::Duration::from_millis(500);
    loop {
        if let Err(err) = server.connect().await {
            log::warn!(
                "管道 connect 失败，退避 {}ms 重试：{err}",
                backoff.as_millis()
            );
            tokio::time::sleep(backoff).await;
            continue; // connect 失败不消耗 server，直接重试
        }
        let connected = server;
        // 立刻建下一个实例，避免连接空窗被他人抢建同名管道；建失败则退避重试直到成功
        // （否则无 server 可 accept）。
        server = loop {
            // SAFETY: 同上。
            match unsafe {
                ServerOptions::new().create_with_security_attributes_raw(PIPE_NAME, attrs)
            } {
                Ok(s) => break s,
                Err(err) => {
                    log::warn!(
                        "重建管道实例失败，退避 {}ms 重试：{err}",
                        backoff.as_millis()
                    );
                    tokio::time::sleep(backoff).await;
                }
            }
        };
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(st, connected).await {
                log::debug!("连接处理结束：{e:#}");
            }
        });
    }
}

#[cfg(not(windows))]
pub async fn serve(
    _state: Arc<AgentState>,
    _ready: Option<std::sync::mpsc::SyncSender<Result<(), String>>>,
) -> Result<()> {
    anyhow::bail!("net-policy-agent 仅支持 Windows")
}

/// 单实例检测：9090 已有 controller 时不一定冲突（可能是本产品旧实例），但同名管道由
/// `first_pipe_instance(true)` 兜底——重复起 serve 会在建第一个实例时失败。此处仅提供预检提示。
pub fn preflight_single_instance() -> Option<String> {
    if engine::controller_present() {
        Some(
            "检测到 127.0.0.1:9090 已有 external-controller（可能是本产品旧实例或其它 clash/mihomo）"
                .to_string(),
        )
    } else {
        None
    }
}
