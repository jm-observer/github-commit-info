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

use crate::capture::{CaptureOpts, CaptureSession, CaptureTarget};
use crate::config::{NetPolicySettings, ProcessRef, Rule, RuleSet, WgConfig};
use crate::decrypt::{CaStatus, DecryptArtifact, DecryptOpts, DecryptSession, DecryptTarget};
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
/// minor 2：请求记录 / 进程树 / 路由视图 / 临时直连 / 生命周期事件；
/// minor 3：`ResetConnections`（切姿态后强制旧连接重连新出口）+ `GetMihomoLog`（运行日志查看）；
/// minor 4：规则新增 `domain-keyword`（渲染为 mihomo `DOMAIN-KEYWORD`）；
/// minor 5：抓包（Capture*，抓包设计 §10）+ `Hello.capabilities`（agent 探测通过才声明
/// `capture_v1`）；
/// minor 6：L4 应用明文（Decrypt*/DecryptCa*，抓包设计 §17.8）+ `decrypt_v1` 能力（CA/引擎/平台
/// 探测通过才声明）。纯追加请求/响应/错误码，旧客户端忽略未知字段。
pub const PROTOCOL_MINOR: u16 = 6;

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

    // ── minor 5：抓包稳定错误码（抓包设计 §10）──────────────────────────────
    /// agent 未声明抓包能力（pktmon 探测未通过 / 真实后端尚未实现）。
    CaptureUnsupported,
    /// pktmon 已有 capture/trace 在运行（机器级共享，绝不 stop 他人会话）。
    CaptureEngineBusy,
    /// pktmon 已存在其它过滤器（绝不 remove 他人过滤器）。
    CaptureFiltersBusy,
    /// 无法唯一定位 mihomo TUN 对应的 pktmon component。
    CaptureComponentNotFound,
    /// 定向目标解析出的端点为空（进程/域名当前无流量）。
    CaptureTargetEmpty,
    /// 去重后端点超过 pktmon 32 条过滤器上限（不静默退化全量）。
    CaptureFilterLimit,
    /// 抓包与 apply/reload/stop 等网络长操作冲突，需先停抓包。
    CaptureConflict,
    /// 会话 ID 不存在。
    CaptureNotFound,
    /// 会话处于运行态，Delete 拒绝（不隐式 Stop）。
    CaptureBusy,
    /// 存储配额 / 目标卷可用空间不足。
    CaptureStorageFull,
    /// ETL 转 pcapng 失败。
    CaptureConvertFailed,

    // ── minor 6：L4 应用明文稳定错误码（抓包设计 §17.8）──────────────────────
    /// agent 未声明 L4 能力（CA/引擎/平台探测未通过 / 真实后端未实现，受 ADR 阻断）。
    DecryptUnsupported,
    /// 专用调试 CA 未创建。
    DecryptCaMissing,
    /// CA 私钥缺失 / 指纹不符 / 过期。
    DecryptCaBroken,
    /// 目标进程实例失配（PID 复用 / 路径或创建时间变化 / 已退出）。
    DecryptTargetStale,
    /// MITM 引擎健康检查失败。
    DecryptEngineUnhealthy,
    /// 与 L3 抓包 / apply/reload/stop 等互斥操作冲突。
    DecryptConflict,
    /// 达到会话数 / 配额上限。
    DecryptLimitReached,
    /// 客户端拒绝伪造叶子证书（pinning / 自带 CA bundle）。
    DecryptClientRejectedCert,
    /// QUIC/HTTP3 不解密（默认旁路）。
    DecryptQuicNotSupported,
    /// finalize（脱敏索引 / 产物校验 / 原子提交）失败。
    DecryptFinalizeFailed,
}

/// 错误响应体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

    // ── minor 3：连接重置 / 运行日志 ────────────────────────────────────────
    /// 关闭 mihomo 所有活跃连接（`DELETE /connections`），逼流量用新出口重连。
    /// best-effort：agent 侧失败只回 `Error`，调用方决定是否阻塞提示。
    ResetConnections,
    /// 取 mihomo 运行日志（stdout/stderr 落 `mihomo.log`）最近 `lines` 行；
    /// 引擎未跑过（日志文件不存在）返回空列表，不是错误。
    GetMihomoLog {
        lines: u32,
    },

    // ── minor 5：抓包（抓包设计 §10）────────────────────────────────────────
    /// 开始抓包（单会话；参数与目标在开始前冻结）。
    CaptureStart {
        target: CaptureTarget,
        opts: CaptureOpts,
    },
    /// 停止指定会话（仅 `running` 有效；其余幂等返回当前态）。
    CaptureStop {
        id: String,
    },
    /// 取单个会话状态。
    CaptureGet {
        id: String,
    },
    /// 列出全部会话。
    CaptureList,
    /// 删除会话（运行态返回 `capture_busy`，不隐式 Stop）。
    CaptureDelete {
        id: String,
    },
    /// 分块读取 `done` 会话的 pcapng（`len` 原始上限 512 KiB）。
    CaptureRead {
        id: String,
        offset: u64,
        len: u32,
    },

    // ── minor 6：L4 应用明文（抓包设计 §17.8）────────────────────────────────
    /// 查 CA 信任状态。
    DecryptCaStatus,
    /// 生成专用调试 CA（只返回公钥指纹/句柄；私钥永不出管道）。
    DecryptCaCreate,
    /// GUI 在当前用户上下文安装公钥后，把指纹 + SID 交 agent 复核（与 Create 分开，§17.8）。
    DecryptCaConfirmInstalled {
        thumbprint: String,
        owner_sid: String,
    },
    /// 按 thumbprint 精确删除本产品 CA。
    DecryptCaRemove,
    /// 开始明文会话（精确进程实例 + 必填域名 allowlist）。
    DecryptStart {
        target: DecryptTarget,
        opts: DecryptOpts,
    },
    DecryptStop {
        id: String,
    },
    DecryptGet {
        id: String,
    },
    DecryptList,
    DecryptDelete {
        id: String,
    },
    /// 分块读取产物（artifact 用枚举，客户端不能传文件名；`len` 原始上限 512 KiB）。
    DecryptRead {
        id: String,
        artifact: DecryptArtifact,
        offset: u64,
        len: u32,
    },
}

/// agent → 客户端响应。逐 Request 定型（设计 §4：响应与今天前端各命令返回一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "snake_case")]
pub enum Response {
    /// 握手回应（回 agent 自身版本 + 能力清单）。`capabilities` 为 minor 5 新增字段，
    /// 旧客户端 `#[serde(default)]` 忽略；agent 仅在 pktmon 探测通过时声明 `capture_v1`。
    Hello {
        version: Version,
        #[serde(default)]
        capabilities: Vec<String>,
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
    /// mihomo 运行日志（最近 N 行）。
    MihomoLog {
        lines: Vec<String>,
    },

    // ── minor 5：抓包（抓包设计 §10）────────────────────────────────────────
    /// 单会话（CaptureStart/Stop/Get/Delete 返回）。
    CaptureSession {
        session: CaptureSession,
    },
    /// 会话列表（CaptureList 返回）。
    CaptureSessions {
        sessions: Vec<CaptureSession>,
    },
    /// 一块 pcapng 数据（CaptureRead 返回；`data_base64` 是本块原始字节的 base64，
    /// `eof` 表示已到文件末尾）。
    CaptureChunk {
        id: String,
        offset: u64,
        data_base64: String,
        eof: bool,
    },

    // ── minor 6：L4 应用明文（抓包设计 §17.8）────────────────────────────────
    /// CA 状态（DecryptCaStatus/Create/ConfirmInstalled/Remove 返回）。
    DecryptCa {
        status: CaStatus,
    },
    /// 单个明文会话（DecryptStart/Stop/Get/Delete 返回）。
    DecryptSession {
        session: DecryptSession,
    },
    /// 明文会话列表（DecryptList 返回）。
    DecryptSessions {
        sessions: Vec<DecryptSession>,
    },
    /// 一块产物数据（DecryptRead 返回）。
    DecryptChunk {
        id: String,
        artifact: DecryptArtifact,
        offset: u64,
        data_base64: String,
        eof: bool,
    },
    /// 无载荷成功（SaveSettings / ClearBlocked / SubscribeEvents 确认 / ResetConnections）。
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
