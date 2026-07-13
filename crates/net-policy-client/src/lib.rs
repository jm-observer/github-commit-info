//! net-policy 命名管道客户端（普通权限；Tauri 后端与 CLI 共用，设计文档 §3/§4）。
//!
//! 连接流程：打开 `\\.\pipe\net-policy-agent` → `Hello` 版本协商（major 不同即报
//! `VersionIncompatible`）→ 一问一答的请求/响应（单请求连接串行，§4.1）。事件订阅走**另一条**
//! 独立连接（[`subscribe_events`]），避免慢事件消费者阻塞普通请求。
//!
//! 仅 Windows：非 Windows 平台所有连接方法直接报错（net-policy 只承诺 Windows）。

pub mod frame;

use anyhow::{bail, Context, Result};
use net_policy_core::config::{NetPolicySettings, ProcessRef, Rule, RuleSet, WgConfig};
use net_policy_core::operation::OperationInfo;
use net_policy_core::protocol::{
    ErrorKind, Event, Frame, ProtocolError, Request, Response, Version,
};
use net_policy_core::types::{
    BlockedEntry, ConnectionsSnapshot, DomainAssoc, LifecycleEvent, NetPolicyStatus,
    ProcessCandidate, ProcessNode, RepairResult, RequestLogEntry, RouteEntry, TempDirectStatus,
    VerifyReport,
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
        };
        // 握手：先声明本端版本，取 agent 版本。major 不同 agent 会回 Error(VersionIncompatible)。
        let resp = c
            .request(Request::Hello {
                version: Version::CURRENT,
            })
            .await?;
        match resp {
            Response::Hello { version } => {
                c.server_version = version;
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
                Frame::Response { id: rid, resp } if rid == id => return Ok(resp),
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

    // ── 逐操作 typed 便捷方法（对应今天前端 NetPolicyAPI 的 17 个 invoke） ─────────────

    pub async fn status(&mut self) -> Result<NetPolicyStatus> {
        match Client::expect_ok(self.request(Request::GetStatus).await?)? {
            Response::Status { status } => Ok(status),
            other => unexpected(other),
        }
    }

    pub async fn get_settings(&mut self) -> Result<NetPolicySettings> {
        match Client::expect_ok(self.request(Request::GetSettings).await?)? {
            Response::Settings { settings } => Ok(settings),
            other => unexpected(other),
        }
    }

    pub async fn save_settings(&mut self, settings: NetPolicySettings) -> Result<()> {
        Client::expect_ok(self.request(Request::SaveSettings { settings }).await?)?;
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
            Frame::Response { id, resp } if id == want_id => return Ok(resp),
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
    }
}
