//! 抓包（Packet Capture）纯逻辑（抓包设计 net-policy-capture-design.md，Phase 2）。
//!
//! **只放机器无关的纯逻辑**：协议 DTO、参数校验（§9 边界）、定向过滤器预算（§5.2）、状态机、
//! session-id / 读窗校验、manifest schema。真正 shell 调 `pktmon`、探测 TUN component、etl2pcap、
//! 落盘配额等**有副作用**的部分在 `net-policy-agent::capture`（Phase 0 真机 spike 已过，见
//! docs/net-policy/net-policy-capture-validation-report.md；Phase 2a 全 TUN 后端已实现；定向抓包
//! Phase 2b 仍待 fake-ip 端点解析真机验证）。
//!
//! 设计约束落进类型：定向抓包用 [`CaptureEndpoint`]（§3.1 fake-ip 口径，`capture_ip` 必须是 TUN
//! 包面可匹配地址 + `source` 记录来源）；过滤器上限 32（§5.2，超限拒绝**不静默退化全量**）；
//! `CaptureRead` 单次原始上限 512 KiB（§10）；参数默认 128B 包头 / 120s / 128 MiB（§9）。

use crate::config::ProcessRef;
use crate::valid;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// manifest schema 版本（§9）。
pub const CAPTURE_SCHEMA_VERSION: u32 = 1;
/// pktmon 同时最多过滤器条数（§5.2；机器级硬上限）。
pub const CAPTURE_MAX_FILTERS: usize = 32;
/// `CaptureRead` 单次原始字节硬上限（§10；base64 后仍远低于 8 MiB 帧上限）。
pub const CAPTURE_READ_MAX_LEN: u32 = 512 * 1024;

/// 默认截断长度：仅保留每包前 128 字节包头（§9；用户显式选“完整包”才传 0）。
pub const DEFAULT_SNAP_LEN: u32 = 128;
/// 默认 circular 容量上限（MiB，§9）。
pub const DEFAULT_FILE_SIZE_MIB: u32 = 128;
/// 默认时间上限（秒，§9；agent 定时 stop）。
pub const DEFAULT_MAX_SECS: u64 = 120;

/// 允许范围（§9）：snap_len 为 0（完整包）或 [64, 65535]；file_size [16, 512] MiB；secs [10, 600]。
pub const SNAP_LEN_MIN: u32 = 64;
pub const SNAP_LEN_MAX: u32 = 65535;
pub const FILE_SIZE_MIB_MIN: u32 = 16;
pub const FILE_SIZE_MIB_MAX: u32 = 512;
pub const MAX_SECS_MIN: u64 = 10;
pub const MAX_SECS_MAX: u64 = 600;

/// agent 声明的抓包能力标识（§10；仅在 pktmon 探测通过时经 `Hello.capabilities` 声明）。
pub const CAPABILITY_CAPTURE_V1: &str = "capture_v1";

/// 抓包目标（§5.1）。`All`=全 TUN 短抓；其余为定向抓包，开始时解析为当前包面端点。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "target", content = "value", rename_all = "snake_case")]
pub enum CaptureTarget {
    /// 不加过滤器，只限定 TUN component。
    All,
    /// 按进程解析当前 `/connections` 端点（`ProcessPath` 优先、`ProcessName` 兜底）。
    Process(ProcessRef),
    /// 按域名解析（规范化 + 命中当前连接/Observatory 的已验证包面 IP）。
    Domain(String),
    /// 直接给 TUN 包面 IP / CIDR。
    Ip(String),
}

impl CaptureTarget {
    /// 是否为定向目标（需解析端点 + 加过滤器）；`All` 为否。
    pub fn is_directed(&self) -> bool {
        !matches!(self, CaptureTarget::All)
    }

    /// 校验目标输入（防注入 + 格式；进程/域名/IP 值最终会进 pktmon 命令参数）。
    pub fn validate(&self) -> Result<()> {
        match self {
            CaptureTarget::All => Ok(()),
            CaptureTarget::Process(p) => match p {
                ProcessRef::ProcessPath(v) => valid::process_path(v),
                ProcessRef::ProcessName(v) => valid::process_name(v),
            },
            CaptureTarget::Domain(d) => valid::domain(d),
            CaptureTarget::Ip(ip) => valid::ip_or_cidr(ip),
        }
    }

    /// 稳定的短描述（manifest / UI 摘要用；不含敏感值）。
    pub fn kind_str(&self) -> &'static str {
        match self {
            CaptureTarget::All => "all",
            CaptureTarget::Process(_) => "process",
            CaptureTarget::Domain(_) => "domain",
            CaptureTarget::Ip(_) => "ip",
        }
    }
}

/// 抓包选项（§10 / §9）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureOpts {
    /// 每包保留字节数；`0`=完整包（不主动截断，高敏感）。
    pub snap_len: u32,
    /// pktmon circular 容量上限（MiB）。
    pub file_size_mib: u32,
    /// 时间上限（秒）；agent 到时 stop。
    pub max_secs: u64,
}

impl Default for CaptureOpts {
    fn default() -> Self {
        Self {
            snap_len: DEFAULT_SNAP_LEN,
            file_size_mib: DEFAULT_FILE_SIZE_MIB,
            max_secs: DEFAULT_MAX_SECS,
        }
    }
}

impl CaptureOpts {
    /// 校验参数落在 §9 允许范围。agent 不信任 GUI，apply 前必再校验。
    pub fn validate(&self) -> Result<()> {
        if self.snap_len != 0 && !(SNAP_LEN_MIN..=SNAP_LEN_MAX).contains(&self.snap_len) {
            bail!(
                "snap_len 非法：{}（应为 0 完整包，或 {SNAP_LEN_MIN}–{SNAP_LEN_MAX}）",
                self.snap_len
            );
        }
        if !(FILE_SIZE_MIB_MIN..=FILE_SIZE_MIB_MAX).contains(&self.file_size_mib) {
            bail!(
                "file_size_mib 非法：{}（应为 {FILE_SIZE_MIB_MIN}–{FILE_SIZE_MIB_MAX} MiB）",
                self.file_size_mib
            );
        }
        if !(MAX_SECS_MIN..=MAX_SECS_MAX).contains(&self.max_secs) {
            bail!(
                "max_secs 非法：{}（应为 {MAX_SECS_MIN}–{MAX_SECS_MAX} 秒）",
                self.max_secs
            );
        }
        Ok(())
    }

    /// 是否为“完整包”模式（高敏感，UI 须二次强调）。
    pub fn is_full_packet(&self) -> bool {
        self.snap_len == 0
    }

    /// 目标卷至少需要的可用空间（§9：`2 * file_size_mib + 128 MiB`，覆盖 ETL 与 pcapng 共存）。
    pub fn min_free_bytes(&self) -> u64 {
        (self.file_size_mib as u64 * 2 + 128) * 1024 * 1024
    }
}

/// 传输层协议（pktmon 过滤器 `-t`；抓包只区分 TCP/UDP）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum CaptureProtocol {
    Tcp,
    Udp,
}

impl CaptureProtocol {
    /// pktmon `filter add -t` 取值。
    pub fn pktmon_flag(self) -> &'static str {
        match self {
            CaptureProtocol::Tcp => "TCP",
            CaptureProtocol::Udp => "UDP",
        }
    }

    /// 从 mihomo `/connections.metadata.network`（tcp/udp）解析。
    pub fn from_network(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tcp" => Some(CaptureProtocol::Tcp),
            "udp" => Some(CaptureProtocol::Udp),
            _ => None,
        }
    }
}

/// 端点来源（§3.1）：记录 `capture_ip` 从哪来，manifest 保留以便回溯口径。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EndpointSource {
    /// 来自 mihomo `/connections`。
    Connection,
    /// 来自 fake-ip 反查映射。
    FakeIpMap,
    /// 用户直接输入（`Ip` target）。
    UserInput,
}

/// 定向抓包的一个包面端点（§3.1）。`capture_ip` 必须是**目标 TUN component 上实际可匹配的地址**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureEndpoint {
    pub capture_ip: String,
    pub port: u16,
    pub network: CaptureProtocol,
    pub source: EndpointSource,
}

impl CaptureEndpoint {
    /// 校验 `capture_ip` 为合法 IP/CIDR（防注入 + 防抓空文件的最低门槛）。
    pub fn validate(&self) -> Result<()> {
        valid::ip_or_cidr(&self.capture_ip)
    }

    /// 去重键：pktmon 过滤器不区分源/目的，一条 `(ip, port, proto)` 对应一条过滤器（§5.2）。
    fn dedup_key(&self) -> (String, u16, CaptureProtocol) {
        (
            self.capture_ip.to_ascii_lowercase(),
            self.port,
            self.network,
        )
    }
}

/// 一条 pktmon 命名过滤器（§4 命令形态：`filter add <name> -i <ip> -p <port> -t <TCP|UDP>`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureFilter {
    pub name: String,
    pub capture_ip: String,
    pub port: u16,
    pub network: CaptureProtocol,
}

/// 过滤器预算错误（§5.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterPlanError {
    /// 去重后端点数仍超过 pktmon 32 条上限；**不静默退化全量**，回带去重后数量供 UI 提示。
    TooMany { deduped: usize },
}

/// 把解析出的端点去重、稳定排序后生成 pktmon 命名过滤器（§5.2）。
///
/// - 去重键 = `(capture_ip 小写, port, protocol)`——pktmon 不区分源/目的，同键只需一条过滤器。
/// - 超过 [`CAPTURE_MAX_FILTERS`] 返回 [`FilterPlanError::TooMany`]（含去重后数量），调用方转
///   `capture_filter_limit`，不自动退化为全量抓。
/// - 过滤器命名 `np-cap-<序号>`，确定性（同输入同输出，便于测试与幂等清理）。
pub fn plan_filters(endpoints: &[CaptureEndpoint]) -> Result<Vec<CaptureFilter>, FilterPlanError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut uniq: Vec<&CaptureEndpoint> = Vec::new();
    for e in endpoints {
        if seen.insert(e.dedup_key()) {
            uniq.push(e);
        }
    }
    // 稳定排序：按 (ip, port, proto)，与去重键一致，产出确定。
    uniq.sort_by_key(|a| a.dedup_key());
    if uniq.len() > CAPTURE_MAX_FILTERS {
        return Err(FilterPlanError::TooMany {
            deduped: uniq.len(),
        });
    }
    Ok(uniq
        .into_iter()
        .enumerate()
        .map(|(i, e)| CaptureFilter {
            name: format!("np-cap-{i}"),
            capture_ip: e.capture_ip.clone(),
            port: e.port,
            network: e.network,
        })
        .collect())
}

/// 会话生命周期状态（§6）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    Preparing,
    Running,
    Stopping,
    Converting,
    Done,
    Failed,
    Orphaned,
}

impl CaptureState {
    /// 终态（不再迁移）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            CaptureState::Done | CaptureState::Failed | CaptureState::Orphaned
        )
    }

    /// 是否有可下载的最终文件（仅 `done`，§6）。
    pub fn has_download(self) -> bool {
        matches!(self, CaptureState::Done)
    }

    /// 是否可执行 Stop（仅 `running`；对 stopping/converting/done 幂等返回当前态，§6）。
    pub fn can_stop(self) -> bool {
        matches!(self, CaptureState::Running)
    }

    /// Delete 是否应返回 `capture_busy`（运行态不得隐式 Stop，§6）。
    pub fn delete_is_busy(self) -> bool {
        matches!(self, CaptureState::Running)
    }
}

/// 停止原因（§10）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStopReason {
    User,
    Timeout,
    AgentRestart,
    Error,
}

/// 会话 DTO（§10）：**不暴露服务端绝对路径**，只给 `file_name`、`bytes`、时间、结构化错误。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaptureSession {
    pub id: String,
    pub state: CaptureState,
    pub target: CaptureTarget,
    pub opts: CaptureOpts,
    /// 解析出的定向端点数（`All` 为 0）。
    pub endpoint_count: usize,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub stop_reason: Option<CaptureStopReason>,
    /// 仅 `done` 时给文件名（`capture.pcapng`），不给绝对路径。
    pub file_name: Option<String>,
    /// pcapng 字节数（`done` 时）。
    pub bytes: Option<u64>,
    /// 已知限制（IP/端口过滤不分源目的、LAN/未入 TUN 的 IPv6 盲区、快照非最终包数等）。
    #[serde(default)]
    pub known_limits: Vec<String>,
    /// 结构化错误（失败/orphaned 时；稳定错误码 + 脱敏摘要）。
    #[serde(default)]
    pub error: Option<crate::protocol::ProtocolError>,
}

/// 完整 manifest（§9，落 `manifest.json`）。**不得含 WG 私钥 / controller secret / 完整命令行 secret**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaptureManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub target: CaptureTarget,
    pub endpoints: Vec<CaptureEndpoint>,
    pub opts: CaptureOpts,
    pub filters: Vec<CaptureFilter>,
    /// TUN component 标识（探测所得，禁跨重启缓存）。
    pub tun_component: String,
    pub mihomo_version: String,
    /// 生成时的管道协议版本（major.minor 字符串，便于旧产物识别）。
    pub protocol: String,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub stop_reason: Option<CaptureStopReason>,
    pub etl_bytes: u64,
    pub pcapng_bytes: u64,
    pub convert_ok: bool,
    #[serde(default)]
    pub known_limits: Vec<String>,
}

impl CaptureManifest {
    /// 生成 DTO 视图（§10：抹掉端点明细与内部路径，只留计数/文件名/错误）。
    pub fn to_session(
        &self,
        state: CaptureState,
        error: Option<crate::protocol::ProtocolError>,
    ) -> CaptureSession {
        CaptureSession {
            id: self.session_id.clone(),
            state,
            target: self.target.clone(),
            opts: self.opts,
            endpoint_count: self.endpoints.len(),
            started_ms: self.started_ms,
            ended_ms: self.ended_ms,
            stop_reason: self.stop_reason,
            file_name: state.has_download().then(|| "capture.pcapng".to_string()),
            bytes: state.has_download().then_some(self.pcapng_bytes),
            known_limits: self.known_limits.clone(),
            error,
        }
    }
}

/// session id 前缀（agent 生成，客户端不能提供路径，§10）。
const SESSION_ID_PREFIX: &str = "cap-";
/// session id 十六进制体长度（16 字节 = 32 hex）。
const SESSION_ID_HEX_LEN: usize = 32;

/// 由 16 字节随机数格式化 session id（随机源在 agent，core 只做纯格式化，保持可测/无副作用）。
pub fn format_session_id(bytes: [u8; 16]) -> String {
    let mut s = String::with_capacity(SESSION_ID_PREFIX.len() + SESSION_ID_HEX_LEN);
    s.push_str(SESSION_ID_PREFIX);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 校验 session id 严格为 `cap-` + 32 位小写 hex。**防路径穿越**：客户端传来的 id 会用于定位
/// `captures/<id>/`，任何含分隔符 / `..` / 非 hex 字符的值一律拒收（§9/§13）。
pub fn is_valid_session_id(s: &str) -> bool {
    let Some(hex) = s.strip_prefix(SESSION_ID_PREFIX) else {
        return false;
    };
    hex.len() == SESSION_ID_HEX_LEN
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 校验 `CaptureRead` 的 offset/len（§10）：`len` ≤ 512 KiB；`offset` ≤ 文件长度。
pub fn validate_read_window(offset: u64, len: u32, file_len: u64) -> Result<()> {
    if len > CAPTURE_READ_MAX_LEN {
        bail!("读取长度超上限：{len} > {CAPTURE_READ_MAX_LEN}");
    }
    if offset > file_len {
        bail!("读取偏移越界：offset {offset} > 文件长度 {file_len}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opts_default_matches_spec() {
        let o = CaptureOpts::default();
        assert_eq!(o.snap_len, 128);
        assert_eq!(o.file_size_mib, 128);
        assert_eq!(o.max_secs, 120);
        o.validate().expect("默认参数应合法");
        assert!(!o.is_full_packet());
    }

    #[test]
    fn opts_validate_bounds() {
        // snap_len：0（完整包）合法，64–65535 合法，其间非法。
        assert!(CaptureOpts {
            snap_len: 0,
            ..Default::default()
        }
        .validate()
        .is_ok());
        assert!(CaptureOpts {
            snap_len: 63,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(CaptureOpts {
            snap_len: 65536,
            ..Default::default()
        }
        .validate()
        .is_err());
        // file_size_mib：[16,512]。
        assert!(CaptureOpts {
            file_size_mib: 15,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(CaptureOpts {
            file_size_mib: 513,
            ..Default::default()
        }
        .validate()
        .is_err());
        // max_secs：[10,600]。
        assert!(CaptureOpts {
            max_secs: 9,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(CaptureOpts {
            max_secs: 601,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn min_free_bytes_covers_etl_and_pcapng() {
        let o = CaptureOpts {
            file_size_mib: 128,
            ..Default::default()
        };
        // 2*128 + 128 = 384 MiB
        assert_eq!(o.min_free_bytes(), 384 * 1024 * 1024);
    }

    #[test]
    fn target_validate_rejects_injection() {
        assert!(CaptureTarget::All.validate().is_ok());
        assert!(CaptureTarget::Ip("198.18.0.5".into()).validate().is_ok());
        assert!(CaptureTarget::Ip("198.18.0.0/16".into()).validate().is_ok());
        assert!(CaptureTarget::Domain("example.com".into())
            .validate()
            .is_ok());
        // 注入尝试
        assert!(CaptureTarget::Ip("1.2.3.4; calc".into())
            .validate()
            .is_err());
        assert!(CaptureTarget::Domain("a`b".into()).validate().is_err());
    }

    #[test]
    fn plan_filters_dedups_and_names() {
        let mk = |ip: &str, port: u16, net: CaptureProtocol| CaptureEndpoint {
            capture_ip: ip.into(),
            port,
            network: net,
            source: EndpointSource::Connection,
        };
        let eps = vec![
            mk("198.18.0.9", 443, CaptureProtocol::Tcp),
            // 重复（大小写 IP 视同）——去重
            mk("198.18.0.9", 443, CaptureProtocol::Tcp),
            // 同 IP 不同端口——保留
            mk("198.18.0.9", 80, CaptureProtocol::Tcp),
            // 同 IP 同端口不同协议——保留
            mk("198.18.0.9", 443, CaptureProtocol::Udp),
        ];
        let filters = plan_filters(&eps).expect("应生成过滤器");
        assert_eq!(filters.len(), 3, "去重后 3 条");
        // 命名确定
        assert_eq!(filters[0].name, "np-cap-0");
        assert_eq!(filters[2].name, "np-cap-2");
        // 排序确定：(ip, port, proto) → 80/tcp, 443/tcp, 443/udp
        assert_eq!(filters[0].port, 80);
        assert_eq!(filters[1].port, 443);
        assert_eq!(filters[1].network, CaptureProtocol::Tcp);
        assert_eq!(filters[2].network, CaptureProtocol::Udp);
    }

    #[test]
    fn plan_filters_rejects_over_budget_without_degrading() {
        let eps: Vec<CaptureEndpoint> = (0..33)
            .map(|i| CaptureEndpoint {
                capture_ip: format!("198.18.0.{i}"),
                port: 443,
                network: CaptureProtocol::Tcp,
                source: EndpointSource::Connection,
            })
            .collect();
        match plan_filters(&eps) {
            Err(FilterPlanError::TooMany { deduped }) => assert_eq!(deduped, 33),
            other => panic!("超预算应拒绝，得到 {other:?}"),
        }
        // 恰好 32 条允许
        assert_eq!(plan_filters(&eps[..32]).unwrap().len(), 32);
    }

    #[test]
    fn session_id_format_and_validate() {
        let id = format_session_id([0xab; 16]);
        assert_eq!(id, format!("cap-{}", "ab".repeat(16)));
        assert!(is_valid_session_id(&id));
        // 防路径穿越 / 篡改
        assert!(!is_valid_session_id("cap-../etc"));
        assert!(!is_valid_session_id("cap-ABCDEF")); // 大写拒
        assert!(!is_valid_session_id("cap-"));
        assert!(!is_valid_session_id(&format!("cap-{}", "a".repeat(31)))); // 长度不符
        assert!(!is_valid_session_id("xxx-00000000000000000000000000000000"));
        assert!(!is_valid_session_id("cap-abc\\def")); // 含分隔符
    }

    #[test]
    fn read_window_bounds() {
        validate_read_window(0, CAPTURE_READ_MAX_LEN, 10_000_000).unwrap();
        // len 超 512 KiB 拒
        assert!(validate_read_window(0, CAPTURE_READ_MAX_LEN + 1, 10).is_err());
        // offset == file_len 合法（读到 EOF），> 越界拒
        validate_read_window(100, 10, 100).unwrap();
        assert!(validate_read_window(101, 10, 100).is_err());
    }

    #[test]
    fn state_helpers() {
        assert!(CaptureState::Running.can_stop());
        assert!(!CaptureState::Stopping.can_stop());
        assert!(CaptureState::Done.has_download());
        assert!(!CaptureState::Failed.has_download());
        assert!(CaptureState::Running.delete_is_busy());
        assert!(!CaptureState::Done.delete_is_busy());
        assert!(CaptureState::Orphaned.is_terminal());
        assert!(!CaptureState::Preparing.is_terminal());
    }

    #[test]
    fn manifest_to_session_hides_paths_and_endpoints() {
        let manifest = CaptureManifest {
            schema_version: CAPTURE_SCHEMA_VERSION,
            session_id: format_session_id([1; 16]),
            target: CaptureTarget::Ip("198.18.0.9".into()),
            endpoints: vec![CaptureEndpoint {
                capture_ip: "198.18.0.9".into(),
                port: 443,
                network: CaptureProtocol::Tcp,
                source: EndpointSource::UserInput,
            }],
            opts: CaptureOpts::default(),
            filters: vec![],
            tun_component: "42".into(),
            mihomo_version: "1.18".into(),
            protocol: "1.5".into(),
            started_ms: 1000,
            ended_ms: Some(2000),
            stop_reason: Some(CaptureStopReason::User),
            etl_bytes: 4096,
            pcapng_bytes: 2048,
            convert_ok: true,
            known_limits: vec!["IP/端口过滤不分源目的".into()],
        };
        let s = manifest.to_session(CaptureState::Done, None);
        assert_eq!(s.endpoint_count, 1);
        assert_eq!(s.file_name.as_deref(), Some("capture.pcapng"));
        assert_eq!(s.bytes, Some(2048));
        // DTO 不含端点明细字段（编译期即保证：CaptureSession 无 endpoints 字段）。
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("capture_ip"), "DTO 不应泄漏端点 IP");
        assert!(!json.contains("tun_component"));
    }

    #[test]
    fn non_done_session_has_no_download() {
        let manifest = CaptureManifest {
            schema_version: CAPTURE_SCHEMA_VERSION,
            session_id: format_session_id([2; 16]),
            target: CaptureTarget::All,
            endpoints: vec![],
            opts: CaptureOpts::default(),
            filters: vec![],
            tun_component: "1".into(),
            mihomo_version: "1.18".into(),
            protocol: "1.5".into(),
            started_ms: 0,
            ended_ms: None,
            stop_reason: None,
            etl_bytes: 0,
            pcapng_bytes: 0,
            convert_ok: false,
            known_limits: vec![],
        };
        let s = manifest.to_session(CaptureState::Running, None);
        assert!(s.file_name.is_none());
        assert!(s.bytes.is_none());
    }
}
