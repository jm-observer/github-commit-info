//! net-policy 命名管道客户端（普通权限；Tauri 后端与 CLI 共用，设计文档 §3/§4）。
//!
//! 连接流程：打开 `\\.\pipe\net-policy-agent` → `Hello` 版本协商（major 不同即报
//! `VersionIncompatible`）→ 一问一答的请求/响应（单请求连接串行，§4.1）。事件订阅走**另一条**
//! 独立连接（[`subscribe_events`]），避免慢事件消费者阻塞普通请求。
//!
//! 仅 Windows：非 Windows 平台所有连接方法直接报错（net-policy 只承诺 Windows）。

pub mod frame;

use anyhow::{bail, Context, Result};
use net_policy_core::capture::{CaptureOpts, CaptureSession, CaptureTarget};
use net_policy_core::config::{NetPolicySettings, ProcessRef, Rule, RuleSet, WgConfig};
use net_policy_core::decrypt::{
    CaStatus, DecryptArtifact, DecryptOpts, DecryptSession, DecryptTarget,
};
use net_policy_core::egress::{EgressFallback, EgressStatus};
use net_policy_core::operation::OperationInfo;
use net_policy_core::protocol::{
    ErrorKind, Event, Frame, ProtocolError, Request, Response, Version,
};
use net_policy_core::types::{
    BlockedEntry, ConnectionsSnapshot, DomainAssoc, LifecycleEvent, NetPolicyStatus,
    ProcessCandidate, ProcessNode, ProxyNode, RepairResult, RequestLogEntry, RouteEntry,
    TempDirectStatus, VerifyReport,
};

/// 控制面命名管道名（agent server 端建同名管道并挂 DACL）。
pub use net_policy_core::protocol::PIPE_NAME;

/// 连接的抽象：任意可读可写的异步流（NamedPipeClient 满足）。
trait Conn: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Conn for T {}

/// 打开一条到 agent 的管道连接（Windows；忙时短重试）。
#[cfg(windows)]
async fn open_pipe(pipe: &str) -> Result<Box<dyn Conn>> {
    use tokio::net::windows::named_pipe::ClientOptions;
    use tokio::time::{sleep, Duration};
    // ERROR_PIPE_BUSY = 231：所有实例忙，标准做法是等一会重试。
    const ERROR_PIPE_BUSY: i32 = 231;
    for _ in 0..20 {
        match ClientOptions::new().open(pipe) {
            Ok(c) => return Ok(Box::new(c)),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(e).with_context(|| format!("打开管道 {pipe} 失败")),
        }
    }
    bail!("管道 {pipe} 持续繁忙，连接超时")
}

#[cfg(not(windows))]
async fn open_pipe(_pipe: &str) -> Result<Box<dyn Conn>> {
    bail!("net-policy 仅支持 Windows")
}

/// 请求/响应客户端（一条连接，串行一问一答）。
pub struct Client {
    conn: Box<dyn Conn>,
    next_id: u64,
    /// agent 端协商后的版本。
    pub server_version: Version,
    /// agent 声明的能力清单（minor 5，如 `capture_v1`；旧 agent 为空）。
    pub server_capabilities: Vec<String>,
}

impl Client {
    /// agent 是否声明了某能力（如 `net_policy_core::capture::CAPABILITY_CAPTURE_V1`）。
    pub fn has_capability(&self, cap: &str) -> bool {
        self.server_capabilities.iter().any(|c| c == cap)
    }
}

impl Client {
    /// 连接默认管道并完成版本握手。
    pub async fn connect() -> Result<Client> {
        Client::connect_to(PIPE_NAME).await
    }

    /// 连接指定管道并完成版本握手。
    pub async fn connect_to(pipe: &str) -> Result<Client> {
        let conn = open_pipe(pipe).await?;
        let mut c = Client {
            conn,
            next_id: 1,
            server_version: Version::CURRENT,
            server_capabilities: Vec::new(),
        };
        // 握手：先声明本端版本，取 agent 版本。major 不同 agent 会回 Error(VersionIncompatible)。
        let resp = c
            .request(Request::Hello {
                version: Version::CURRENT,
            })
            .await?;
        match resp {
            Response::Hello {
                version,
                capabilities,
            } => {
                c.server_version = version;
                c.server_capabilities = capabilities;
                Ok(c)
            }
            Response::Error { error } => Err(anyhow::anyhow!(
                "版本握手失败（{:?}）：{}",
                error.kind,
                error.message
            )),
            other => bail!("握手返回非预期响应：{other:?}"),
        }
    }

    /// 发一个请求，读回配对响应（校验 id）。
    pub async fn request(&mut self, req: Request) -> Result<Response> {
        let id = self.next_id;
        self.next_id += 1;
        frame::write_frame(&mut self.conn, &Frame::request(id, req)).await?;
        loop {
            match frame::read_frame(&mut self.conn).await? {
                Frame::Response { id: rid, resp } if rid == id => return Ok(*resp),
                Frame::Response { .. } => continue, // 非本请求的响应（不应发生），跳过
                Frame::Event { .. } => continue,    // 请求连接上不应有事件；忽略
                Frame::Request { .. } => bail!("agent 在请求连接上发来 Request 帧（协议错误）"),
            }
        }
    }

    /// 把 `Response::Error` 归一化为 anyhow 错误（保留 kind）。
    fn expect_ok(resp: Response) -> Result<Response> {
        if let Response::Error { error } = resp {
            return Err(protocol_err(error));
        }
        Ok(resp)
    }

    /// minor 能力门控（评审点 4）：调新功能前确认 agent 协议 minor 足够；否则明确报
    /// `version_incompatible` 而非发出旧 agent 无法反序列化的帧、被静默断连。
    fn require_minor(&self, min: u16) -> Result<()> {
        if self.server_version.minor < min {
            bail!(
                "[version_incompatible] agent 协议 minor={} 不支持该功能（需 >= {}），请升级 net-policy-agent",
                self.server_version.minor,
                min
            );
        }
        Ok(())
    }

    /// minor 门控后再发请求（新功能专用）。
    async fn request_v2(&mut self, req: Request) -> Result<Response> {
        self.require_minor(2)?;
        self.request(req).await
    }

    /// minor 3 门控后再发请求（`ResetConnections` / `GetMihomoLog`）。
    async fn request_v3(&mut self, req: Request) -> Result<Response> {
        self.require_minor(3)?;
        self.request(req).await
    }

    /// minor 5 门控后再发请求（抓包 `Capture*`）。
    async fn request_v5(&mut self, req: Request) -> Result<Response> {
        self.require_minor(5)?;
        self.request(req).await
    }

    /// minor 6 门控后再发请求（L4 `Decrypt*`/`DecryptCa*`）。
    async fn request_v6(&mut self, req: Request) -> Result<Response> {
        self.require_minor(6)?;
        self.request(req).await
    }

    async fn request_v7(&mut self, req: Request) -> Result<Response> {
        self.require_minor(7)?;
        self.request(req).await
    }

    /// minor 8 门控后再发请求（统一出口 `Egress*`）。
    async fn request_v8(&mut self, req: Request) -> Result<Response> {
        self.require_minor(8)?;
        self.request(req).await
    }

    // ── 出口生命周期（minor 8，出口设计 §8.8）───────────────────────────────
    //
    // 六个操作**语义互不重叠**：list 只读；probe 只测不改状态；start/stop 改生命周期；
    // reconnect 重建会话；set_fallback 改不可用时的行为。改导流策略不在这里（走
    // save_settings / save_rule）。全部回出口全量清单。

    async fn egress_call(&mut self, req: Request) -> Result<Vec<EgressStatus>> {
        match Client::expect_ok(self.request_v8(req).await?)? {
            Response::Egresses { egresses } => Ok(egresses),
            other => unexpected(other),
        }
    }

    pub async fn egress_list(&mut self) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressList).await
    }

    /// 启动出口（渲染进配置 + 立即探测）。**不改变任何导流规则。**
    pub async fn egress_start(&mut self, id: String) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressStart { id }).await
    }

    /// 停止出口（从配置摘除；指向它的规则按 fallback 处理，默认阻断）。
    /// **与「停止管控」（[`Client::stop`]）是两件事。**
    pub async fn egress_stop(&mut self, id: String) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressStop { id }).await
    }

    pub async fn egress_reconnect(&mut self, id: String) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressReconnect { id }).await
    }

    /// 仅测试连接：探测一次，不改生命周期也不改导流。
    pub async fn egress_probe(&mut self, id: String) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressProbe { id }).await
    }

    pub async fn egress_set_fallback(
        &mut self,
        id: String,
        fallback: EgressFallback,
    ) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressSetFallback { id, fallback })
            .await
    }

    /// 刷新代理订阅（只刷配置来源，不重连节点、不打断当前可用连接）。
    pub async fn egress_refresh_subscription(&mut self, id: String) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressRefreshSubscription { id })
            .await
    }

    /// 切换代理订阅当前节点（影响此后新建立的连接）。
    pub async fn egress_select_node(
        &mut self,
        id: String,
        node: String,
    ) -> Result<Vec<EgressStatus>> {
        self.egress_call(Request::EgressSelectNode { id, node })
            .await
    }

    // ── 逐操作 typed 便捷方法（对应今天前端 NetPolicyAPI 的 17 个 invoke） ─────────────

    pub async fn status(&mut self) -> Result<NetPolicyStatus> {
        match Client::expect_ok(self.request(Request::GetStatus).await?)? {
            Response::Status { status } => Ok(status),
            other => unexpected(other),
        }
    }

    pub async fn get_settings(&mut self) -> Result<NetPolicySettings> {
        match Client::expect_ok(self.request(Request::GetSettings).await?)? {
            Response::Settings { settings } => Ok(*settings),
            other => unexpected(other),
        }
    }

    pub async fn save_settings(&mut self, settings: NetPolicySettings) -> Result<()> {
        Client::expect_ok(
            self.request(Request::SaveSettings {
                settings: Box::new(settings),
            })
            .await?,
        )?;
        Ok(())
    }

    pub async fn parse_wg_conf(&mut self, content: String) -> Result<WgConfig> {
        match Client::expect_ok(self.request(Request::ParseWgConf { content }).await?)? {
            Response::Wg { wg } => Ok(wg),
            other => unexpected(other),
        }
    }

    pub async fn list_rules(&mut self) -> Result<RuleSet> {
        match Client::expect_ok(self.request(Request::ListRules).await?)? {
            Response::Rules { rules } => Ok(rules),
            other => unexpected(other),
        }
    }

    pub async fn save_rule(&mut self, rule: Rule) -> Result<RuleSet> {
        match Client::expect_ok(self.request(Request::SaveRule { rule }).await?)? {
            Response::Rules { rules } => Ok(rules),
            other => unexpected(other),
        }
    }

    pub async fn delete_rule(&mut self, rule: Rule) -> Result<RuleSet> {
        match Client::expect_ok(self.request(Request::DeleteRule { rule }).await?)? {
            Response::Rules { rules } => Ok(rules),
            other => unexpected(other),
        }
    }

    pub async fn list_process_candidates(&mut self) -> Result<Vec<ProcessCandidate>> {
        match Client::expect_ok(self.request(Request::ListProcessCandidates).await?)? {
            Response::Processes { processes } => Ok(processes),
            other => unexpected(other),
        }
    }

    pub async fn connections(&mut self) -> Result<ConnectionsSnapshot> {
        match Client::expect_ok(self.request(Request::GetConnections).await?)? {
            Response::Connections { snapshot } => Ok(snapshot),
            other => unexpected(other),
        }
    }

    pub async fn proxy_nodes(&mut self) -> Result<Vec<ProxyNode>> {
        match Client::expect_ok(self.request_v7(Request::GetProxyNodes).await?)? {
            Response::ProxyNodes { nodes } => Ok(nodes),
            other => unexpected(other),
        }
    }

    pub async fn test_proxy_node(&mut self, name: String) -> Result<ProxyNode> {
        match Client::expect_ok(self.request_v7(Request::TestProxyNode { name }).await?)? {
            Response::ProxyNode { node } => Ok(node),
            other => unexpected(other),
        }
    }

    pub async fn blocked(&mut self) -> Result<Vec<BlockedEntry>> {
        match Client::expect_ok(self.request(Request::Blocked).await?)? {
            Response::Blocked { entries } => Ok(entries),
            other => unexpected(other),
        }
    }

    pub async fn clear_blocked(&mut self) -> Result<()> {
        Client::expect_ok(self.request(Request::ClearBlocked).await?)?;
        Ok(())
    }

    pub async fn dns_map(&mut self) -> Result<Vec<DomainAssoc>> {
        match Client::expect_ok(self.request(Request::DnsMap).await?)? {
            Response::DnsMap { entries } => Ok(entries),
            other => unexpected(other),
        }
    }

    pub async fn verify(&mut self) -> Result<VerifyReport> {
        match Client::expect_ok(self.request(Request::Verify).await?)? {
            Response::Verify { report } => Ok(report),
            other => unexpected(other),
        }
    }

    pub async fn apply(&mut self) -> Result<NetPolicyStatus> {
        match Client::expect_ok(self.request(Request::Apply).await?)? {
            Response::Status { status } => Ok(status),
            other => unexpected(other),
        }
    }

    pub async fn stop(&mut self) -> Result<NetPolicyStatus> {
        match Client::expect_ok(self.request(Request::Stop).await?)? {
            Response::Status { status } => Ok(status),
            other => unexpected(other),
        }
    }

    pub async fn set_enabled(&mut self, enabled: bool) -> Result<NetPolicyStatus> {
        match Client::expect_ok(self.request(Request::SetEnabled { enabled }).await?)? {
            Response::Status { status } => Ok(status),
            other => unexpected(other),
        }
    }

    pub async fn reload(&mut self) -> Result<NetPolicyStatus> {
        match Client::expect_ok(self.request(Request::Reload).await?)? {
            Response::Status { status } => Ok(status),
            other => unexpected(other),
        }
    }

    pub async fn current_operation(&mut self) -> Result<Option<OperationInfo>> {
        match Client::expect_ok(self.request(Request::GetCurrentOperation).await?)? {
            Response::Operation { operation } => Ok(operation),
            other => unexpected(other),
        }
    }

    /// 在线修复防火墙残留（分级；`force` 无快照也强设 NotConfigured）。不等价于 stop。
    pub async fn repair(&mut self, force: bool) -> Result<RepairResult> {
        match Client::expect_ok(self.request(Request::Repair { force }).await?)? {
            Response::Repair { result } => Ok(result),
            other => unexpected(other),
        }
    }

    // ── minor 2：记录 / 观测 / 临时直连 / 路由（均经 request_v2 做 minor 门控） ──────────

    /// 历史进程请求记录（最近 limit 条，倒序）。
    pub async fn requests(&mut self, limit: u32) -> Result<Vec<RequestLogEntry>> {
        match Client::expect_ok(self.request_v2(Request::GetRequests { limit }).await?)? {
            Response::Requests { entries } => Ok(entries),
            other => unexpected(other),
        }
    }

    /// 生命周期事件（最近 limit 条，倒序）。
    pub async fn events(&mut self, limit: u32) -> Result<Vec<LifecycleEvent>> {
        match Client::expect_ok(self.request_v2(Request::GetEvents { limit }).await?)? {
            Response::Events { entries } => Ok(entries),
            other => unexpected(other),
        }
    }

    /// 清空请求记录（隐私）。
    pub async fn clear_requests(&mut self) -> Result<()> {
        Client::expect_ok(self.request_v2(Request::ClearRequests).await?)?;
        Ok(())
    }

    /// 清空生命周期事件。
    pub async fn clear_events(&mut self) -> Result<()> {
        Client::expect_ok(self.request_v2(Request::ClearEvents).await?)?;
        Ok(())
    }

    /// 进程树。
    pub async fn process_tree(&mut self) -> Result<Vec<ProcessNode>> {
        match Client::expect_ok(self.request_v2(Request::GetProcessTree).await?)? {
            Response::ProcessTree { roots } => Ok(roots),
            other => unexpected(other),
        }
    }

    /// 生效路由（含优先级/来源/可删）。
    pub async fn routes(&mut self) -> Result<Vec<RouteEntry>> {
        match Client::expect_ok(self.request_v2(Request::GetRoutes).await?)? {
            Response::Routes { routes } => Ok(routes),
            other => unexpected(other),
        }
    }

    /// 临时直连状态。
    pub async fn temp_direct(&mut self) -> Result<TempDirectStatus> {
        match Client::expect_ok(self.request_v2(Request::GetTempDirect).await?)? {
            Response::TempDirect { status } => Ok(status),
            other => unexpected(other),
        }
    }

    /// 开启临时直连（限时）。
    pub async fn set_temp_direct(
        &mut self,
        duration_secs: u64,
        except: Vec<ProcessRef>,
    ) -> Result<TempDirectStatus> {
        let req = Request::SetTempDirect {
            duration_secs,
            except,
        };
        match Client::expect_ok(self.request_v2(req).await?)? {
            Response::TempDirect { status } => Ok(status),
            other => unexpected(other),
        }
    }

    /// 解除临时直连。
    pub async fn clear_temp_direct(&mut self) -> Result<TempDirectStatus> {
        match Client::expect_ok(self.request_v2(Request::ClearTempDirect).await?)? {
            Response::TempDirect { status } => Ok(status),
            other => unexpected(other),
        }
    }

    // ── minor 3：连接重置 / 运行日志 ─────────────────────────────────────────

    /// 关闭 mihomo 所有活跃连接，逼流量用新出口重连（切姿态 reload 后调用；best-effort）。
    pub async fn reset_connections(&mut self) -> Result<()> {
        Client::expect_ok(self.request_v3(Request::ResetConnections).await?)?;
        Ok(())
    }

    /// mihomo 运行日志（最近 `lines` 行；agent 侧硬上限 1000）。
    pub async fn mihomo_log(&mut self, lines: u32) -> Result<Vec<String>> {
        match Client::expect_ok(self.request_v3(Request::GetMihomoLog { lines }).await?)? {
            Response::MihomoLog { lines } => Ok(lines),
            other => unexpected(other),
        }
    }

    // ── minor 5：抓包（Capture*，抓包设计 §10）─────────────────────────────────

    /// 开始抓包（`All` 全 TUN，或定向 Process/Domain/Ip）。返回 `running` 会话。
    pub async fn capture_start(
        &mut self,
        target: CaptureTarget,
        opts: CaptureOpts,
    ) -> Result<CaptureSession> {
        match Client::expect_ok(
            self.request_v5(Request::CaptureStart { target, opts })
                .await?,
        )? {
            Response::CaptureSession { session } => Ok(session),
            other => unexpected(other),
        }
    }

    /// 停止抓包（幂等）。
    pub async fn capture_stop(&mut self, id: String) -> Result<CaptureSession> {
        match Client::expect_ok(self.request_v5(Request::CaptureStop { id }).await?)? {
            Response::CaptureSession { session } => Ok(session),
            other => unexpected(other),
        }
    }

    /// 取单个会话当前态。
    pub async fn capture_get(&mut self, id: String) -> Result<CaptureSession> {
        match Client::expect_ok(self.request_v5(Request::CaptureGet { id }).await?)? {
            Response::CaptureSession { session } => Ok(session),
            other => unexpected(other),
        }
    }

    /// 列出所有会话。
    pub async fn capture_list(&mut self) -> Result<Vec<CaptureSession>> {
        match Client::expect_ok(self.request_v5(Request::CaptureList).await?)? {
            Response::CaptureSessions { sessions } => Ok(sessions),
            other => unexpected(other),
        }
    }

    /// 删除会话（运行态返回 `capture_busy`）。
    pub async fn capture_delete(&mut self, id: String) -> Result<()> {
        Client::expect_ok(self.request_v5(Request::CaptureDelete { id }).await?)?;
        Ok(())
    }

    /// 分块读取 `done` 会话 pcapng，返回 `(offset, data_base64, eof)`。
    pub async fn capture_read(
        &mut self,
        id: String,
        offset: u64,
        len: u32,
    ) -> Result<(u64, String, bool)> {
        let req = Request::CaptureRead { id, offset, len };
        match Client::expect_ok(self.request_v5(req).await?)? {
            Response::CaptureChunk {
                offset,
                data_base64,
                eof,
                ..
            } => Ok((offset, data_base64, eof)),
            other => unexpected(other),
        }
    }

    // ── minor 6：L4 应用明文（Decrypt*/DecryptCa*，抓包设计 §17.8）───────────────

    async fn decrypt_ca(&mut self, req: Request) -> Result<CaStatus> {
        match Client::expect_ok(self.request_v6(req).await?)? {
            Response::DecryptCa { status } => Ok(status),
            other => unexpected(other),
        }
    }
    /// CA 信任状态。
    pub async fn decrypt_ca_status(&mut self) -> Result<CaStatus> {
        self.decrypt_ca(Request::DecryptCaStatus).await
    }
    /// 生成专用调试 CA（不装信任库；GUI 在用户上下文安装公钥后再 confirm）。
    pub async fn decrypt_ca_create(&mut self) -> Result<CaStatus> {
        self.decrypt_ca(Request::DecryptCaCreate).await
    }
    /// GUI 安装公钥后把指纹 + owner SID 交 agent 复核。
    pub async fn decrypt_ca_confirm(
        &mut self,
        thumbprint: String,
        owner_sid: String,
    ) -> Result<CaStatus> {
        self.decrypt_ca(Request::DecryptCaConfirmInstalled {
            thumbprint,
            owner_sid,
        })
        .await
    }
    /// 移除本产品 CA（文件 + 安装记录）。
    pub async fn decrypt_ca_remove(&mut self) -> Result<CaStatus> {
        self.decrypt_ca(Request::DecryptCaRemove).await
    }
    /// 导出 CA 公钥证书 PEM（供 GUI 在用户上下文装 `CurrentUser\Root`；私钥永不出管道）。
    pub async fn decrypt_ca_export_public(&mut self) -> Result<String> {
        match Client::expect_ok(self.request_v6(Request::DecryptCaExportPublic).await?)? {
            Response::DecryptCaPublic { cert_pem } => Ok(cert_pem),
            other => unexpected(other),
        }
    }

    async fn decrypt_session(&mut self, req: Request) -> Result<DecryptSession> {
        match Client::expect_ok(self.request_v6(req).await?)? {
            Response::DecryptSession { session } => Ok(session),
            other => unexpected(other),
        }
    }
    /// 开始解密会话（精确进程实例 + 必填域名 allowlist）。
    pub async fn decrypt_start(
        &mut self,
        target: DecryptTarget,
        opts: DecryptOpts,
    ) -> Result<DecryptSession> {
        self.decrypt_session(Request::DecryptStart { target, opts })
            .await
    }
    pub async fn decrypt_stop(&mut self, id: String) -> Result<DecryptSession> {
        self.decrypt_session(Request::DecryptStop { id }).await
    }
    pub async fn decrypt_get(&mut self, id: String) -> Result<DecryptSession> {
        self.decrypt_session(Request::DecryptGet { id }).await
    }
    pub async fn decrypt_list(&mut self) -> Result<Vec<DecryptSession>> {
        match Client::expect_ok(self.request_v6(Request::DecryptList).await?)? {
            Response::DecryptSessions { sessions } => Ok(sessions),
            other => unexpected(other),
        }
    }
    pub async fn decrypt_delete(&mut self, id: String) -> Result<()> {
        Client::expect_ok(self.request_v6(Request::DecryptDelete { id }).await?)?;
        Ok(())
    }
    /// 分块读产物（manifest / http.jsonl）。返回 `(offset, data_base64, eof)`。
    pub async fn decrypt_read(
        &mut self,
        id: String,
        artifact: DecryptArtifact,
        offset: u64,
        len: u32,
    ) -> Result<(u64, String, bool)> {
        let req = Request::DecryptRead {
            id,
            artifact,
            offset,
            len,
        };
        match Client::expect_ok(self.request_v6(req).await?)? {
            Response::DecryptChunk {
                offset,
                data_base64,
                eof,
                ..
            } => Ok((offset, data_base64, eof)),
            other => unexpected(other),
        }
    }
}

/// 事件订阅流（独立连接：发一条 `SubscribeEvents` 后本连接只收 `Event` 帧）。
pub struct EventStream {
    conn: Box<dyn Conn>,
}

/// 打开事件订阅连接（先 Hello 再 SubscribeEvents）。
pub async fn subscribe_events() -> Result<EventStream> {
    subscribe_events_to(PIPE_NAME).await
}

pub async fn subscribe_events_to(pipe: &str) -> Result<EventStream> {
    let mut conn = open_pipe(pipe).await?;
    // 事件连接也先握手，再订阅——**两个响应都严格校验**（评审点 9：否则版本不兼容时仍返回
    // 看似成功的 EventStream，随后才以 EOF 表现）。
    frame::write_frame(
        &mut conn,
        &Frame::request(
            1,
            Request::Hello {
                version: Version::CURRENT,
            },
        ),
    )
    .await?;
    match read_response(&mut conn, 1).await? {
        Response::Hello { .. } => {}
        Response::Error { error } => return Err(protocol_err(error)),
        other => bail!("事件连接握手返回非预期响应：{other:?}"),
    }
    frame::write_frame(&mut conn, &Frame::request(2, Request::SubscribeEvents)).await?;
    match read_response(&mut conn, 2).await? {
        Response::Ok => {}
        Response::Error { error } => return Err(protocol_err(error)),
        other => bail!("订阅确认返回非预期响应：{other:?}"),
    }
    Ok(EventStream { conn })
}

/// 从连接读一帧，要求是配对 id 的 Response（事件连接握手阶段用）。
async fn read_response<C: tokio::io::AsyncRead + Unpin>(
    conn: &mut C,
    want_id: u64,
) -> Result<Response> {
    loop {
        match frame::read_frame(conn).await? {
            Frame::Response { id, resp } if id == want_id => return Ok(*resp),
            Frame::Response { .. } => continue,
            other => bail!("握手阶段收到非 Response 帧：{other:?}"),
        }
    }
}

impl EventStream {
    /// 取下一个事件；连接关闭返回 `None`。
    pub async fn next(&mut self) -> Option<Result<Event>> {
        match frame::read_frame(&mut self.conn).await {
            Ok(Frame::Event { ev }) => Some(Ok(ev)),
            Ok(_) => Some(Err(anyhow::anyhow!("事件连接上收到非 Event 帧"))),
            Err(_) => None, // EOF / 断线
        }
    }
}

fn unexpected<T>(resp: Response) -> Result<T> {
    bail!("agent 返回非预期响应：{resp:?}")
}

/// 把 `ProtocolError` 转成 anyhow 错误（附 kind 便于上层判断）。
fn protocol_err(e: ProtocolError) -> anyhow::Error {
    anyhow::anyhow!("[{}] {}", kind_str(e.kind), e.message)
}

fn kind_str(k: ErrorKind) -> &'static str {
    match k {
        ErrorKind::NotElevated => "not_elevated",
        ErrorKind::MihomoUnreachable => "mihomo_unreachable",
        ErrorKind::OperationConflict => "operation_conflict",
        ErrorKind::WgMissing => "wg_missing",
        ErrorKind::RuleNotFound => "rule_not_found",
        ErrorKind::Validation => "validation",
        ErrorKind::VersionIncompatible => "version_incompatible",
        ErrorKind::Unsupported => "unsupported",
        ErrorKind::Internal => "internal",
        ErrorKind::CaptureUnsupported => "capture_unsupported",
        ErrorKind::CaptureEngineBusy => "capture_engine_busy",
        ErrorKind::CaptureFiltersBusy => "capture_filters_busy",
        ErrorKind::CaptureComponentNotFound => "capture_component_not_found",
        ErrorKind::CaptureTargetEmpty => "capture_target_empty",
        ErrorKind::CaptureFilterLimit => "capture_filter_limit",
        ErrorKind::CaptureConflict => "capture_conflict",
        ErrorKind::CaptureNotFound => "capture_not_found",
        ErrorKind::CaptureBusy => "capture_busy",
        ErrorKind::CaptureStorageFull => "capture_storage_full",
        ErrorKind::CaptureConvertFailed => "capture_convert_failed",
        ErrorKind::DecryptUnsupported => "decrypt_unsupported",
        ErrorKind::DecryptCaMissing => "decrypt_ca_missing",
        ErrorKind::DecryptCaBroken => "decrypt_ca_broken",
        ErrorKind::DecryptTargetStale => "decrypt_target_stale",
        ErrorKind::DecryptEngineUnhealthy => "decrypt_engine_unhealthy",
        ErrorKind::DecryptConflict => "decrypt_conflict",
        ErrorKind::DecryptLimitReached => "decrypt_limit_reached",
        ErrorKind::DecryptClientRejectedCert => "decrypt_client_rejected_cert",
        ErrorKind::DecryptQuicNotSupported => "decrypt_quic_not_supported",
        ErrorKind::DecryptFinalizeFailed => "decrypt_finalize_failed",
        ErrorKind::EgressNotFound => "egress_not_found",
        ErrorKind::EgressUnconfigured => "egress_unconfigured",
        ErrorKind::EgressConflict => "egress_conflict",
        ErrorKind::EgressApplyFailed => "egress_apply_failed",
        ErrorKind::EgressProbeFailed => "egress_probe_failed",
        ErrorKind::EgressSubscriptionFailed => "egress_subscription_failed",
    }
}
