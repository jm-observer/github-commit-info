//! 版本化 typed 管道线协议（设计文档 §4/§4.1）。
//!
//! **不是 REST**：单一内部客户端（Tauri GUI + 复用同库的 CLI），用类型明确的 enum 协议——类型即
//! 契约，无 HTTP 动词 / CORS / 端口发现之累。
//!
//! 帧格式：`4 字节小端长度前缀 + UTF-8 JSON`（[`encode`]/[`decode`]，纯函数，无 tokio；异步收发在
//! client/agent 用这两个函数拼）。envelope 显式带 `kind` 标签与字段名，**不把 Rust enum 的默认
//! serde 表示当永久线协议**。
//!
//! 版本协商：连接建立后客户端先发 `Request::Hello(version)`；**major 不同 → agent 拒绝所有业务
//! 请求（回 `VersionIncompatible`），只允许握手**；minor 不同按 capability 兼容（响应加字段兼容、
//! 改删字段升 major）。

use crate::config::{NetPolicySettings, ProcessRef, Rule, RuleSet, WgConfig};
use crate::operation::{ApplyProgress, OperationInfo, OperationResult};
use crate::types::{
    BlockedEntry, ConnectionsSnapshot, DomainAssoc, LifecycleEvent, NetPolicyStatus,
    ProcessCandidate, ProcessNode, RepairResult, RequestLogEntry, RouteEntry, TempDirectStatus,
    VerifyReport,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// 控制面命名管道名（agent server 建同名管道并挂 DACL；client/CLI 连它）。
pub const PIPE_NAME: &str = r"\\.\pipe\net-policy-agent";

/// 协议主版本：改/删字段必须 +1。
pub const PROTOCOL_MAJOR: u16 = 1;
/// 协议次版本：仅加字段/加请求时 +1（向后兼容）。minor 1：`Repair` + `ResyncRequired`；
/// minor 2：请求记录 / 进程树 / 路由视图 / 临时直连 / 生命周期事件。
pub const PROTOCOL_MINOR: u16 = 2;

/// 单帧最大字节数（防超大帧拖垮提权 agent，设计 §3.1 DoS 防护）。
pub const MAX_FRAME_LEN: u32 = 8 * 1024 * 1024;

/// 协议版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

impl Version {
    pub const CURRENT: Version = Version {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };
    /// 客户端 major 与本端是否兼容（major 必须相等）。
    pub fn compatible_with(self, other: Version) -> bool {
        self.major == other.major
    }
}

/// 机器可读错误码（稳定枚举，供 GUI 分支处理与国际化，设计 §4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 守护未提权（改防火墙/建 TUN 需管理员）。
    NotElevated,
    /// mihomo 控制器不可达。
    MihomoUnreachable,
    /// 已有长操作在跑（apply/stop/reload/enabled 互斥）。
    OperationConflict,
    /// 指向海外但 WG 未配。
    WgMissing,
    /// 规则未找到（删除时）。
    RuleNotFound,
    /// 输入校验失败。
    Validation,
    /// 协议大版本不兼容。
    VersionIncompatible,
    /// 仅 Windows。
    Unsupported,
    /// 其它内部错误。
    Internal,
}

/// 错误响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ProtocolError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// 客户端 → agent 请求。覆盖今天全部 17 个操作面 + 握手/订阅/当前操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 版本握手（连接建立后第一条）。
    Hello {
        version: Version,
    },
    GetStatus,
    GetSettings,
    SaveSettings {
        settings: NetPolicySettings,
    },
    /// 解析 wg-quick 文本（纯函数，不落盘）。
    ParseWgConf {
        content: String,
    },
    ListRules,
    /// upsert 一条规则（按 (kind,value) 覆盖）。
    SaveRule {
        rule: Rule,
    },
    /// 按 (kind,value) 删除（route 忽略）。规则无稳定 ID。
    DeleteRule {
        rule: Rule,
    },
    ListProcessCandidates,
    GetConnections,
    Blocked,
    ClearBlocked,
    DnsMap,
    Verify,
    /// 应用策略（独立作业，handler 等终态回 Status）。
    Apply,
    /// 停止（优雅拆 + 撤防火墙 + 清 enabled）。
    Stop,
    /// 主开关。
    SetEnabled {
        enabled: bool,
    },
    /// 热重载。
    Reload,
    /// 在线修复防火墙残留（分级，不等价于 stop）：`force` 时无快照也强设 NotConfigured。
    Repair {
        force: bool,
    },
    /// 订阅长操作事件流（此后本连接只收 `Event` 帧）。
    SubscribeEvents,
    /// 取当前/最近一次长操作（断线重连对齐用）。
    GetCurrentOperation,

    // ── minor 2：记录 / 观测 / 临时直连 / 路由 ──────────────────────────────────
    /// 查历史进程请求记录（最近 `limit` 条，倒序）。
    GetRequests {
        limit: u32,
    },
    /// 取当前进程树。
    GetProcessTree,
    /// 取生效路由列表（含优先级/来源/是否可删）。
    GetRoutes,
    /// 开启临时直连（限时）：`duration_secs` 后自动还原；`except` 进程强制 Blackhole 不走直连。
    SetTempDirect {
        duration_secs: u64,
        except: Vec<ProcessRef>,
    },
    /// 提前解除临时直连。
    ClearTempDirect,
    /// 取临时直连状态（剩余时间等）。
    GetTempDirect,
    /// 查生命周期事件（启停 / 策略应用结束 / 临时直连开关，最近 `limit` 条倒序）。
    GetEvents {
        limit: u32,
    },
    /// 清空请求记录（隐私）。
    ClearRequests,
    /// 清空生命周期事件。
    ClearEvents,
}

/// agent → 客户端响应。逐 Request 定型（设计 §4：响应与今天前端各命令返回一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "snake_case")]
pub enum Response {
    /// 握手回应（回 agent 自身版本）。
    Hello {
        version: Version,
    },
    Status {
        status: NetPolicyStatus,
    },
    Settings {
        settings: NetPolicySettings,
    },
    Rules {
        rules: RuleSet,
    },
    Wg {
        wg: WgConfig,
    },
    Processes {
        processes: Vec<ProcessCandidate>,
    },
    Connections {
        snapshot: ConnectionsSnapshot,
    },
    Blocked {
        entries: Vec<BlockedEntry>,
    },
    DnsMap {
        entries: Vec<DomainAssoc>,
    },
    Verify {
        report: VerifyReport,
    },
    Repair {
        result: RepairResult,
    },
    Operation {
        operation: Option<OperationInfo>,
    },
    Requests {
        entries: Vec<RequestLogEntry>,
    },
    ProcessTree {
        roots: Vec<ProcessNode>,
    },
    Routes {
        routes: Vec<RouteEntry>,
    },
    TempDirect {
        status: TempDirectStatus,
    },
    Events {
        entries: Vec<LifecycleEvent>,
    },
    /// 无载荷成功（SaveSettings / ClearBlocked / SubscribeEvents 确认）。
    Ok,
    Error {
        error: ProtocolError,
    },
}

/// 长操作事件（订阅连接上流式推送）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    ApplyProgress {
        progress: ApplyProgress,
    },
    OperationFinished {
        result: OperationResult,
    },
    /// 订阅端落后（broadcast Lagged）：可能已丢事件，客户端应重新以 `GetCurrentOperation` +
    /// `GetStatus` 的真实态对齐；agent 发此帧后**关闭订阅连接**（设计评审点 8）。
    ResyncRequired,
}

/// 线上单帧（请求/响应配对用 `id`；事件无 id）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    Request { id: u64, req: Request },
    Response { id: u64, resp: Response },
    Event { ev: Event },
}

impl Frame {
    pub fn request(id: u64, req: Request) -> Frame {
        Frame::Request { id, req }
    }
    pub fn response(id: u64, resp: Response) -> Frame {
        Frame::Response { id, resp }
    }
    pub fn event(ev: Event) -> Frame {
        Frame::Event { ev }
    }
}

/// 把一帧编码为「4 字节小端长度前缀 + JSON」的完整字节串（纯函数）。
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(frame).context("serialize frame")?;
    let len = json.len();
    if len as u64 > MAX_FRAME_LEN as u64 {
        bail!("帧过大：{len} > {MAX_FRAME_LEN}");
    }
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

/// 从「长度前缀之后的」JSON 字节解码一帧（纯函数；调用方负责先读 4 字节长度、再读 len 字节）。
pub fn decode(payload: &[u8]) -> Result<Frame> {
    serde_json::from_slice(payload).context("deserialize frame")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_request_frame() {
        let f = Frame::request(7, Request::GetStatus);
        let bytes = encode(&f).unwrap();
        // 前 4 字节是长度。
        let len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len() - 4);
        let back = decode(&bytes[4..]).unwrap();
        match back {
            Frame::Request { id, req } => {
                assert_eq!(id, 7);
                assert!(matches!(req, Request::GetStatus));
            }
            _ => panic!("wrong frame"),
        }
    }

    #[test]
    fn roundtrip_error_response() {
        let f = Frame::response(
            1,
            Response::Error {
                error: ProtocolError::new(ErrorKind::OperationConflict, "busy"),
            },
        );
        let bytes = encode(&f).unwrap();
        let back = decode(&bytes[4..]).unwrap();
        if let Frame::Response {
            resp: Response::Error { error },
            ..
        } = back
        {
            assert_eq!(error.kind, ErrorKind::OperationConflict);
        } else {
            panic!("wrong");
        }
    }

    #[test]
    fn version_major_gate() {
        assert!(Version::CURRENT.compatible_with(Version {
            major: PROTOCOL_MAJOR,
            minor: 99,
        }));
        assert!(!Version::CURRENT.compatible_with(Version {
            major: PROTOCOL_MAJOR + 1,
            minor: 0,
        }));
    }

    #[test]
    fn hello_wire_shape_is_explicit() {
        // 显式 envelope：kind/op 标签必须在，不是裸 enum 表示。
        let f = Frame::request(
            1,
            Request::Hello {
                version: Version::CURRENT,
            },
        );
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"kind\":\"request\""));
        assert!(json.contains("\"op\":\"hello\""));
    }
}
