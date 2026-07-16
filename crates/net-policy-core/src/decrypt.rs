//! L4 应用明文（TLS MITM）纯逻辑（抓包设计 §17–§18，Phase 4）。
//!
//! **只放机器无关的纯逻辑**：会话/CA/目标 DTO、参数与目标校验（§17.5）、**脱敏逻辑**（§17.6，
//! 默认凭据脱敏不可关，golden 可测）、状态机、读窗/artifact 校验。真正的 MITM 引擎（mitmdump
//! sidecar）、CA 生成/DPAPI 私钥保护/信任库安装、临时路由/Local Capture、明文落盘等**有副作用且
//! 高风险**的部分在 agent，且受 §18 Phase 4a 真机 spike 阻断。
//!
//! **4a spike 现状**（见 net-policy-capture-status.md / adr-2026-07-phase4-mitm-engine.md §6.2）：
//! 引擎冷启动已通（Defender 查杀由安装程序加排除解决）；真机已证明 **WinDivert 与 mihomo 共存 + 真实
//! 解密成立**，但 **方案 A（Local Capture）在 mihomo fake-ip 模式下上游路由不通**（WinDivert 抓到的是
//! fake-ip 目的地）。可用解密待 **方案 B**（mihomo 按域名送 loopback MITM）spike 定案。未通过前 agent
//! 不声明 `decrypt_v1`，对所有 Decrypt* 请求返回 `decrypt_unsupported`。
//!
//! 安全定位（§17.1）：L4 是主动终止 TLS 的高风险诊断能力，非 L3 的“更完整模式”。类型层强制：
//! 目标为**精确进程实例**（[`ProcessInstanceRef`] 带创建时间防 PID 复用）+ **必填域名 allowlist**
//! （最多 32）；默认 `capture_bodies=false`；默认脱敏档不可关核心凭据。

use crate::valid;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// agent 声明的 L4 能力标识（§17.8；仅在 CA/引擎/平台探测通过时经 `Hello.capabilities` 声明）。
pub const CAPABILITY_DECRYPT_V1: &str = "decrypt_v1";

/// 脱敏占位（§17.6）。
pub const REDACTED: &str = "[REDACTED]";

/// 域名 allowlist 上限（§17.5）。
pub const MAX_DOMAINS: usize = 32;

// 会话参数默认与范围（§17.5）。
pub const DEFAULT_MAX_SECS: u64 = 60;
pub const MAX_SECS_MIN: u64 = 10;
pub const MAX_SECS_MAX: u64 = 300;
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES_CAP: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;
pub const MAX_BODY_BYTES_CAP: u64 = 16 * 1024 * 1024;

/// `DecryptRead` 单次原始上限（复用 §10 的 512 KiB 分块）。
pub const DECRYPT_READ_MAX_LEN: u32 = 512 * 1024;

/// manifest schema 版本（§17.6）。
pub const DECRYPT_SCHEMA_VERSION: u32 = 1;

/// 精确进程实例（§17.5）：`created_at_100ns` 防 PID 复用；`path` agent 重新读取并 canonicalize。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessInstanceRef {
    pub pid: u32,
    /// Windows 进程创建时间（100ns ticks）。
    pub created_at_100ns: u64,
    pub path: String,
}

impl ProcessInstanceRef {
    pub fn validate(&self) -> Result<()> {
        if self.pid == 0 {
            bail!("进程实例 PID 非法");
        }
        valid::process_path(&self.path)
    }
}

/// 脱敏档（§17.6）。`Default` 不允许关闭核心凭据脱敏；`Raw` 才可保留（UI 须红标 + 更短时限）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RedactProfile {
    #[default]
    Default,
    Raw,
}

/// 解密目标（§17.5）：精确进程实例 + 必填域名 allowlist。**无 `All` target**。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecryptTarget {
    pub process: ProcessInstanceRef,
    /// 精确域名或受限 suffix（表达 `example.com` 及其子域）；必填，最多 [`MAX_DOMAINS`]。
    pub domains: Vec<String>,
}

impl DecryptTarget {
    /// 规范化并校验目标。域名转小写、去尾点；非 ASCII（未 punycode 化）拒绝——缺可靠域名时无法
    /// 安全签发/校验证书（§17.5）。返回规范化后的域名列表（去重、保序）。
    pub fn normalized_domains(&self) -> Result<Vec<String>> {
        if self.domains.is_empty() {
            bail!("L4 必须指定至少一个域名 allowlist（不提供 All target）");
        }
        if self.domains.len() > MAX_DOMAINS {
            bail!(
                "域名 allowlist 超上限（{} > {MAX_DOMAINS}）",
                self.domains.len()
            );
        }
        let mut out: Vec<String> = Vec::new();
        for d in &self.domains {
            let norm = normalize_domain(d)?;
            if !out.contains(&norm) {
                out.push(norm);
            }
        }
        Ok(out)
    }

    /// 完整校验（进程实例 + 域名）。
    pub fn validate(&self) -> Result<()> {
        self.process.validate()?;
        self.normalized_domains()?;
        Ok(())
    }
}

/// 域名规范化：小写 + 去尾点；拒绝非 ASCII（要求调用方提供 punycode）与非法字符。
pub fn normalize_domain(d: &str) -> Result<String> {
    let t = d.trim().trim_end_matches('.').to_ascii_lowercase();
    if t.is_empty() {
        bail!("空域名");
    }
    if !t.is_ascii() {
        bail!("域名含非 ASCII 字符，请先做 IDNA/punycode 编码：{d:?}");
    }
    // 复用规则域名校验（label 白名单字符、长度）——同时挡注入。
    valid::domain(&t)?;
    Ok(t)
}

/// 解密会话选项（§17.5）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecryptOpts {
    pub max_secs: u64,
    pub max_total_bytes: u64,
    pub max_body_bytes: u64,
    /// 默认 false：只采集方法/URL/状态/头，不落正文。
    pub capture_bodies: bool,
    /// 默认 false：临时阻 UDP/443 逼 QUIC 回退 TCP（§17.7；改变应用行为，须单独确认）。
    pub force_tcp_for_quic: bool,
    pub redact_profile: RedactProfile,
}

impl Default for DecryptOpts {
    fn default() -> Self {
        Self {
            max_secs: DEFAULT_MAX_SECS,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            capture_bodies: false,
            force_tcp_for_quic: false,
            redact_profile: RedactProfile::Default,
        }
    }
}

impl DecryptOpts {
    pub fn validate(&self) -> Result<()> {
        if !(MAX_SECS_MIN..=MAX_SECS_MAX).contains(&self.max_secs) {
            bail!(
                "max_secs 非法：{}（{MAX_SECS_MIN}–{MAX_SECS_MAX}）",
                self.max_secs
            );
        }
        if self.max_total_bytes == 0 || self.max_total_bytes > MAX_TOTAL_BYTES_CAP {
            bail!(
                "max_total_bytes 非法：{}（1–{MAX_TOTAL_BYTES_CAP}）",
                self.max_total_bytes
            );
        }
        if self.max_body_bytes == 0 || self.max_body_bytes > MAX_BODY_BYTES_CAP {
            bail!(
                "max_body_bytes 非法：{}（1–{MAX_BODY_BYTES_CAP}）",
                self.max_body_bytes
            );
        }
        Ok(())
    }
}

// ── 脱敏逻辑（§17.6；§18.4 要求 golden tests）────────────────────────────────

/// 默认脱敏的敏感头（小写；核心凭据，`Default` 档不可关）。
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
];

/// URL query / form 中默认脱敏的键子串（小写；命中即整值替换）。
const SENSITIVE_QUERY_SUBSTRINGS: &[&str] = &[
    "token",
    "password",
    "passwd",
    "secret",
    "signature",
    "session",
    "apikey",
    "api_key",
    "access_key",
];

/// 该 header 名是否属核心敏感头（大小写不敏感）。
pub fn is_sensitive_header(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    SENSITIVE_HEADERS.contains(&n.as_str())
}

/// 该 query/form 键名是否应脱敏（大小写不敏感；命中子串或恰为 `key`/`sig`）。
pub fn is_sensitive_query_key(key: &str) -> bool {
    let k = key.trim().to_ascii_lowercase();
    if k == "key" || k == "sig" {
        return true;
    }
    SENSITIVE_QUERY_SUBSTRINGS.iter().any(|s| k.contains(s))
}

/// 按脱敏档处理一个头值：`Default` 对敏感头替换为 `[REDACTED]`；`Raw` 保留原值（§17.6）。
pub fn redact_header_value(name: &str, value: &str, profile: RedactProfile) -> String {
    match profile {
        RedactProfile::Default if is_sensitive_header(name) => REDACTED.to_string(),
        _ => value.to_string(),
    }
}

/// 脱敏 URL 的 query 串（只动 value，保留 key 与结构）。`Default` 档对敏感键整值替换；`Raw` 保留。
/// 输入是 `?` 之后的 query（不含 `?`）；无法解析的段原样保留。
pub fn redact_query(query: &str, profile: RedactProfile) -> String {
    if matches!(profile, RedactProfile::Raw) {
        return query.to_string();
    }
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _v)) if is_sensitive_query_key(k) => format!("{k}={REDACTED}"),
            _ => pair.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// 脱敏整个 URL 的 query 部分（保留 scheme/host/path）。
pub fn redact_url(url: &str, profile: RedactProfile) -> String {
    if matches!(profile, RedactProfile::Raw) {
        return url.to_string();
    }
    match url.split_once('?') {
        Some((base, q)) => format!("{base}?{}", redact_query(q, profile)),
        None => url.to_string(),
    }
}

// ── §17.6 http.jsonl 结构化明文索引（脱敏后落盘，一行一个事件）───────────────

/// 脱敏一组头（key 原样保留、value 按档脱敏；§17.6）。返回 `(key, value)` 保序。
pub fn redact_headers(
    headers: &[(String, String)],
    profile: RedactProfile,
) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| (k.clone(), redact_header_value(k, v, profile)))
        .collect()
}

/// 从头里取 `content-type`（小写键匹配，取第一个）。
fn content_type_of(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.clone())
}

/// 按选项把正文加工成可落盘文本（§17.6）：`capture_bodies=false` → None（只记大小）；
/// 否则 UTF-8 lossy 解码 + 截断到 `max_body_bytes`（超出置 `truncated=true`）；form-urlencoded
/// 正文额外走 query 脱敏。返回 `(body_text, truncated)`。
fn body_text(
    body: &[u8],
    content_type: Option<&str>,
    opts: &DecryptOpts,
) -> (Option<String>, bool) {
    if !opts.capture_bodies {
        return (None, false);
    }
    let cap = opts.max_body_bytes as usize;
    let truncated = body.len() > cap;
    let slice = &body[..body.len().min(cap)];
    let mut text = String::from_utf8_lossy(slice).into_owned();
    // form 正文里的敏感键（token/password…）按 query 脱敏（§17.6）。
    if content_type
        .map(|c| c.to_ascii_lowercase().contains("application/x-www-form-urlencoded"))
        .unwrap_or(false)
    {
        text = redact_query(&text, opts.redact_profile);
    }
    (Some(text), truncated)
}

/// 一条 http.jsonl 事件（§17.6：request / response / websocket）。**已脱敏**——序列化即落盘。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HttpEvent {
    Request {
        ts_ms: u64,
        host: String,
        method: String,
        /// 路径（query 已脱敏）。
        path: String,
        http_version: String,
        /// 脱敏后的头。
        headers: Vec<(String, String)>,
        body_size: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        body_truncated: bool,
    },
    Response {
        ts_ms: u64,
        host: String,
        status: u16,
        http_version: String,
        headers: Vec<(String, String)>,
        body_size: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        body_truncated: bool,
    },
    WsFrame {
        ts_ms: u64,
        host: String,
        /// `c2s`（client→server）/ `s2c`。
        direction: String,
        text: bool,
        size: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        body_truncated: bool,
    },
}

impl HttpEvent {
    /// 构造脱敏后的请求事件（§17.6）。`path` 含 query，query 会脱敏。
    // builder 参数即 flow 各字段，逐个显式传更清晰（不引 net-policy-mitm 的 FlowRequest 以免反向依赖）。
    #[allow(clippy::too_many_arguments)]
    pub fn request(
        ts_ms: u64,
        host: &str,
        method: &str,
        path: &str,
        http_version: &str,
        headers: &[(String, String)],
        body: &[u8],
        opts: &DecryptOpts,
    ) -> HttpEvent {
        let content_type = content_type_of(headers);
        let (body_s, truncated) = body_text(body, content_type.as_deref(), opts);
        HttpEvent::Request {
            ts_ms,
            host: host.to_string(),
            method: method.to_string(),
            path: redact_url(path, opts.redact_profile),
            http_version: http_version.to_string(),
            headers: redact_headers(headers, opts.redact_profile),
            body_size: body.len() as u64,
            content_type,
            body: body_s,
            body_truncated: truncated,
        }
    }

    /// 构造脱敏后的响应事件（§17.6）。
    #[allow(clippy::too_many_arguments)]
    pub fn response(
        ts_ms: u64,
        host: &str,
        status: u16,
        http_version: &str,
        headers: &[(String, String)],
        body: &[u8],
        opts: &DecryptOpts,
    ) -> HttpEvent {
        let content_type = content_type_of(headers);
        let (body_s, truncated) = body_text(body, content_type.as_deref(), opts);
        HttpEvent::Response {
            ts_ms,
            host: host.to_string(),
            status,
            http_version: http_version.to_string(),
            headers: redact_headers(headers, opts.redact_profile),
            body_size: body.len() as u64,
            content_type,
            body: body_s,
            body_truncated: truncated,
        }
    }

    /// 构造 WebSocket 帧事件（§17.6）。`client_to_server` 决定方向；文本帧在 `capture_bodies` 时留正文。
    pub fn ws_frame(
        ts_ms: u64,
        host: &str,
        client_to_server: bool,
        text: bool,
        payload: &[u8],
        opts: &DecryptOpts,
    ) -> HttpEvent {
        // 只对文本帧留正文（二进制不解码），并受 capture_bodies + 截断约束。
        let (body_s, truncated) = if text {
            body_text(payload, None, opts)
        } else {
            (None, false)
        };
        HttpEvent::WsFrame {
            ts_ms,
            host: host.to_string(),
            direction: if client_to_server { "c2s" } else { "s2c" }.to_string(),
            text,
            size: payload.len() as u64,
            body: body_s,
            body_truncated: truncated,
        }
    }

    /// 序列化为单行 JSON（http.jsonl 一行；保证无内嵌换行）。
    pub fn to_jsonl(&self) -> Result<String> {
        let s = serde_json::to_string(self)?;
        Ok(s.replace('\n', " "))
    }
}

/// 会话状态机（§17.5）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecryptState {
    CheckingCa,
    Preparing,
    Decrypting,
    Stopping,
    Finalizing,
    Done,
    Failed,
}

impl DecryptState {
    pub fn is_terminal(self) -> bool {
        matches!(self, DecryptState::Done | DecryptState::Failed)
    }
    pub fn can_stop(self) -> bool {
        matches!(
            self,
            DecryptState::Preparing | DecryptState::Decrypting | DecryptState::CheckingCa
        )
    }
    pub fn has_artifacts(self) -> bool {
        matches!(self, DecryptState::Done)
    }
}

/// CA 信任状态（§17.4）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaState {
    /// 未创建。
    Absent,
    /// 已创建且信任库指纹/私钥/有效期一致。
    Installed,
    /// 私钥缺失 / 指纹不符 / 过期——禁止启动会话，引导重建（§17.4）。
    Broken,
}

/// CA 状态 DTO（§17.4）。私钥永不出现；只给指纹/主题/有效期/owner/scope。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaStatus {
    pub state: CaState,
    pub thumbprint: Option<String>,
    pub subject: Option<String>,
    pub not_after_ms: Option<u64>,
    /// 安装到的用户 SID（`CurrentUser\Root`，§17.4）。
    pub owner_sid: Option<String>,
    /// 信任库 scope（固定 `current_user`；不写 `LocalMachine`）。
    pub store_scope: Option<String>,
}

/// 明文产物类型（§17.6；`DecryptRead` 用枚举，客户端不能传文件名）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecryptArtifact {
    /// `manifest.json`。
    Manifest,
    /// 结构化明文索引 `http.jsonl`。
    HttpJsonl,
    /// 引擎原生 flow（仅显式“保留原始明文”的 Raw 会话存在）。
    Flows,
}

impl DecryptArtifact {
    /// 固定文件名（服务端定位用；客户端只传枚举）。
    pub fn file_name(self) -> &'static str {
        match self {
            DecryptArtifact::Manifest => "manifest.json",
            DecryptArtifact::HttpJsonl => "http.jsonl",
            DecryptArtifact::Flows => "flows.mitm",
        }
    }
}

/// 每域名的处理计数（§17.9：不能只显示总“成功”）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainCounters {
    pub decrypted: u64,
    pub passthrough: u64,
    pub pinned: u64,
    pub quic: u64,
    pub failed: u64,
}

/// 解密会话 DTO（§17.8）：不暴露服务端绝对路径。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecryptSession {
    pub id: String,
    pub state: DecryptState,
    pub target: DecryptTarget,
    pub opts: DecryptOpts,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// 每域名计数（键为规范化域名）。
    #[serde(default)]
    pub per_domain: std::collections::BTreeMap<String, DomainCounters>,
    #[serde(default)]
    pub error: Option<crate::protocol::ProtocolError>,
}

/// 校验 `DecryptRead` 的 offset/len（同 §10 分块）。
pub fn validate_read_window(offset: u64, len: u32, file_len: u64) -> Result<()> {
    if len > DECRYPT_READ_MAX_LEN {
        bail!("读取长度超上限：{len} > {DECRYPT_READ_MAX_LEN}");
    }
    if offset > file_len {
        bail!("读取偏移越界：offset {offset} > 文件长度 {file_len}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc() -> ProcessInstanceRef {
        ProcessInstanceRef {
            pid: 1234,
            created_at_100ns: 133_000_000_000_000_000,
            path: r"C:\Program Files\App\app.exe".into(),
        }
    }

    #[test]
    fn opts_default_and_bounds() {
        let o = DecryptOpts::default();
        assert_eq!(o.max_secs, 60);
        assert!(!o.capture_bodies);
        assert!(matches!(o.redact_profile, RedactProfile::Default));
        o.validate().unwrap();
        assert!(DecryptOpts {
            max_secs: 9,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(DecryptOpts {
            max_secs: 301,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(DecryptOpts {
            max_total_bytes: MAX_TOTAL_BYTES_CAP + 1,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(DecryptOpts {
            max_body_bytes: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn target_requires_domains_and_dedups() {
        // 空域名拒
        let empty = DecryptTarget {
            process: proc(),
            domains: vec![],
        };
        assert!(empty.validate().is_err());
        // 规范化 + 去重 + 去尾点 + 小写
        let t = DecryptTarget {
            process: proc(),
            domains: vec![
                "Example.com".into(),
                "example.com.".into(),
                "api.example.com".into(),
            ],
        };
        let norm = t.normalized_domains().unwrap();
        assert_eq!(norm, vec!["example.com", "api.example.com"]);
    }

    #[test]
    fn target_rejects_too_many_and_non_ascii() {
        let many: Vec<String> = (0..33).map(|i| format!("h{i}.example.com")).collect();
        assert!(DecryptTarget {
            process: proc(),
            domains: many
        }
        .normalized_domains()
        .is_err());
        // 非 ASCII 未 punycode → 拒
        assert!(normalize_domain("例子.com").is_err());
        // 注入字符 → 拒
        assert!(normalize_domain("a;b.com").is_err());
    }

    #[test]
    fn process_instance_validation() {
        assert!(proc().validate().is_ok());
        assert!(ProcessInstanceRef { pid: 0, ..proc() }.validate().is_err());
    }

    #[test]
    fn header_redaction_default_and_raw() {
        // 大小写不敏感命中核心敏感头
        assert!(is_sensitive_header("Authorization"));
        assert!(is_sensitive_header("set-cookie"));
        assert!(is_sensitive_header("X-Api-Key"));
        assert!(!is_sensitive_header("Content-Type"));
        // Default 档替换
        assert_eq!(
            redact_header_value("Cookie", "sid=abc", RedactProfile::Default),
            REDACTED
        );
        assert_eq!(
            redact_header_value("Content-Type", "application/json", RedactProfile::Default),
            "application/json"
        );
        // Raw 档保留（即便敏感头）
        assert_eq!(
            redact_header_value("Authorization", "Bearer x", RedactProfile::Raw),
            "Bearer x"
        );
    }

    #[test]
    fn query_and_url_redaction() {
        assert!(is_sensitive_query_key("access_token"));
        assert!(is_sensitive_query_key("X-Signature"));
        assert!(is_sensitive_query_key("password"));
        assert!(is_sensitive_query_key("key"));
        assert!(!is_sensitive_query_key("page"));
        // query 只脱敏敏感 value，保留 key 与非敏感项
        assert_eq!(
            redact_query("page=2&token=SEKRET&q=hello", RedactProfile::Default),
            format!("page=2&token={REDACTED}&q=hello")
        );
        // 整 URL：保留 scheme/host/path，只动 query
        assert_eq!(
            redact_url("https://a.com/p?u=1&session=XYZ", RedactProfile::Default),
            format!("https://a.com/p?u=1&session={REDACTED}")
        );
        // 无 query 原样
        assert_eq!(
            redact_url("https://a.com/p", RedactProfile::Default),
            "https://a.com/p"
        );
        // Raw 档保留
        assert_eq!(
            redact_query("token=SEKRET", RedactProfile::Raw),
            "token=SEKRET"
        );
    }

    #[test]
    fn http_event_request_redacts_headers_and_query_no_body_by_default() {
        let headers = vec![
            ("Cookie".to_string(), "sid=secret".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let ev = HttpEvent::request(
            1000,
            "api.example.com",
            "POST",
            "/v1/chat?token=SEKRET&q=hi",
            "HTTP/2",
            &headers,
            br#"{"a":1}"#,
            &DecryptOpts::default(), // capture_bodies=false
        );
        match &ev {
            HttpEvent::Request {
                path,
                headers,
                body_size,
                body,
                content_type,
                ..
            } => {
                assert_eq!(path, &format!("/v1/chat?token={REDACTED}&q=hi"));
                // Cookie 脱敏，Content-Type 保留
                assert_eq!(headers[0], ("Cookie".into(), REDACTED.to_string()));
                assert_eq!(headers[1].1, "application/json");
                assert_eq!(*body_size, 7);
                assert!(body.is_none(), "默认 capture_bodies=false 不留正文");
                assert_eq!(content_type.as_deref(), Some("application/json"));
            }
            _ => panic!("应为 Request"),
        }
        // 单行 jsonl，无换行
        let line = ev.to_jsonl().unwrap();
        assert!(!line.contains('\n'));
        assert!(line.contains("\"kind\":\"request\""));
    }

    #[test]
    fn http_event_body_captured_and_truncated_and_form_redacted() {
        // capture_bodies=true + 小 max_body_bytes → 截断
        let opts = DecryptOpts {
            capture_bodies: true,
            max_body_bytes: 4,
            ..Default::default()
        };
        let ev = HttpEvent::response(
            1,
            "h",
            200,
            "HTTP/1.1",
            &[("Content-Type".into(), "text/plain".into())],
            b"0123456789",
            &opts,
        );
        if let HttpEvent::Response {
            body,
            body_truncated,
            body_size,
            ..
        } = &ev
        {
            assert_eq!(body.as_deref(), Some("0123"));
            assert!(*body_truncated);
            assert_eq!(*body_size, 10);
        } else {
            panic!()
        }
        // form 正文里的敏感键脱敏
        let opts2 = DecryptOpts {
            capture_bodies: true,
            max_body_bytes: 1024,
            ..Default::default()
        };
        let ev2 = HttpEvent::request(
            1,
            "h",
            "POST",
            "/login",
            "HTTP/1.1",
            &[(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            b"user=bob&password=hunter2",
            &opts2,
        );
        if let HttpEvent::Request { body, .. } = &ev2 {
            assert_eq!(body.as_deref(), Some(&format!("user=bob&password={REDACTED}")[..]));
        } else {
            panic!()
        }
    }

    #[test]
    fn http_event_ws_frame_direction_and_binary_no_body() {
        let opts = DecryptOpts {
            capture_bodies: true,
            ..Default::default()
        };
        let text = HttpEvent::ws_frame(1, "h", true, true, b"hello", &opts);
        if let HttpEvent::WsFrame {
            direction,
            text: is_text,
            body,
            size,
            ..
        } = &text
        {
            assert_eq!(direction, "c2s");
            assert!(*is_text);
            assert_eq!(body.as_deref(), Some("hello"));
            assert_eq!(*size, 5);
        } else {
            panic!()
        }
        // 二进制帧不留正文（即使 capture_bodies）
        let bin = HttpEvent::ws_frame(1, "h", false, false, &[0xff, 0x00], &opts);
        if let HttpEvent::WsFrame { direction, body, .. } = &bin {
            assert_eq!(direction, "s2c");
            assert!(body.is_none());
        } else {
            panic!()
        }
    }

    #[test]
    fn state_helpers() {
        assert!(DecryptState::Decrypting.can_stop());
        assert!(!DecryptState::Finalizing.can_stop());
        assert!(DecryptState::Done.has_artifacts());
        assert!(!DecryptState::Failed.has_artifacts());
        assert!(DecryptState::Failed.is_terminal());
        assert!(!DecryptState::Preparing.is_terminal());
    }

    #[test]
    fn artifact_file_names_fixed() {
        assert_eq!(DecryptArtifact::Manifest.file_name(), "manifest.json");
        assert_eq!(DecryptArtifact::HttpJsonl.file_name(), "http.jsonl");
        assert_eq!(DecryptArtifact::Flows.file_name(), "flows.mitm");
    }

    #[test]
    fn read_window_bounds() {
        validate_read_window(0, DECRYPT_READ_MAX_LEN, 100).unwrap();
        assert!(validate_read_window(0, DECRYPT_READ_MAX_LEN + 1, 100).is_err());
        assert!(validate_read_window(101, 10, 100).is_err());
    }

    #[test]
    fn session_dto_hides_no_secret() {
        let s = DecryptSession {
            id: "dec-abc".into(),
            state: DecryptState::Done,
            target: DecryptTarget {
                process: proc(),
                domains: vec!["example.com".into()],
            },
            opts: DecryptOpts::default(),
            started_ms: 1,
            ended_ms: Some(2),
            per_domain: Default::default(),
            error: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        // DTO 无路径/私钥字段（编译期保证；这里确认序列化不含明显 secret 键）。
        assert!(!json.contains("private"));
        assert!(!json.contains("confdir"));
    }
}
