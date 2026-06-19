//! 全局应用配置，落盘在 `{workspace}/app.json`。
//!
//! **单服务 · 单 host 模型**：orchestrator 已并入 toolkit-server（ASR 走
//! `{g10_base}/api/asr/stream`），桌面端直连的后端只剩 toolkit-server 一个。所以用户只需填
//! 两个 **host**：局域网 IP、外网域名。各服务的协议/端口/路径是部署事实，烘进代码
//! （[`NetScheme`]）——`http(s)` 与端口按局域网/外网各自固定，ASR 由同一 host 派生 `ws(s)`。
//!
//! `mode`：`auto`（默认，探测局域网可达性自动选路）/ `lan` / `wan`（强制档，调试用）。
//! 解析带缓存：见 [`NetResolver`]。旧版 `app.json`（schema 1 单 `g10_base` / schema 2 双
//! `Endpoint`）会被平滑迁移成 host。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 当前 app.json schema 版本。
const CURRENT_SCHEMA: u32 = 3;
/// 自动探测的 health 请求超时（局域网内基本 10ms 返回；不可达则快速回退外网）。
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
/// 探测结果缓存时长；保存配置 / 网络变化时通过 [`NetResolver::invalidate`] 主动失效。
const PROBE_TTL: Duration = Duration::from_secs(30);

/// 默认外网 host（含端口，与既有 G10 反代一致）。
const DEFAULT_WAN_HOST: &str = "www.for-memory.cloud:28080";

/// 一种到达路径（局域网 / 外网）的协议与默认端口约定——**部署事实，非用户配置**。
/// `host` 不含端口时补 `default_port`；含端口则原样用。
struct NetScheme {
    /// g10 代理类服务（toolkit-server）的 HTTP 协议。
    http: &'static str,
    /// 语音 ASR WebSocket 协议（与 http 同 host/端口，仅协议升级）。
    ws: &'static str,
    /// host 未显式带端口时补的默认端口。
    default_port: u16,
}

/// 局域网：toolkit-server 原生端口 8788 + 明文 http/ws。
const LAN_SCHEME: NetScheme = NetScheme {
    http: "http",
    ws: "ws",
    default_port: 8788,
};
/// 外网：反向代理发布端口 28080 + TLS https/wss。
const WAN_SCHEME: NetScheme = NetScheme {
    http: "https",
    ws: "wss",
    default_port: 28080,
};

/// ASR WebSocket 在 toolkit-server 下的挂载路径（orchestrator 并入后）。
const ASR_PATH: &str = "/api/asr/stream";

/// 网络模式：自动探测 / 强制局域网 / 强制外网。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetMode {
    #[default]
    Auto,
    Lan,
    Wan,
}

fn default_schema() -> u32 {
    CURRENT_SCHEMA
}

fn default_wan_host() -> String {
    DEFAULT_WAN_HOST.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub mode: NetMode,
    /// 局域网 host（IP，可含 `:port`；不含端口则用 8788）。留空表示未配局域网。
    #[serde(default)]
    pub lan_host: String,
    /// 外网 host（域名，可含 `:port`；不含端口则用 28080）。
    #[serde(default = "default_wan_host")]
    pub wan_host: String,
    /// 可选 Bearer token（若 G10 server 启用了鉴权；内外网共用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub g10_token: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema: CURRENT_SCHEMA,
            mode: NetMode::Auto,
            lan_host: String::new(),
            wan_host: default_wan_host(),
            g10_token: None,
        }
    }
}

/// 把用户输入/旧配置规整成 `host[:port]`：剥掉 `scheme://` 前缀与路径；空 → 空。
/// 不在此补默认端口（补端口在派生时按 [`NetScheme`] 做，便于「裸 host + 默认端口」展示）。
fn normalize_host(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    // 容错：用户可能粘进整条 URL（http://x:8788/...）。
    let s = s.rsplit("://").next().unwrap_or(s);
    let s = s.split('/').next().unwrap_or(s);
    s.trim().trim_end_matches('.').to_string()
}

/// `host[:port]` + scheme → `host:port`（无端口补默认）。空 host → 空。
fn host_with_port(host: &str, default_port: u16) -> String {
    let h = normalize_host(host);
    if h.is_empty() {
        return String::new();
    }
    if h.contains(':') {
        h
    } else {
        format!("{h}:{default_port}")
    }
}

impl AppSettings {
    fn host_for(&self, mode: NetMode) -> (&str, &NetScheme) {
        match mode {
            NetMode::Lan => (&self.lan_host, &LAN_SCHEME),
            // Auto 仅作兜底（探测后会显式传 Lan/Wan）。
            NetMode::Wan | NetMode::Auto => (&self.wan_host, &WAN_SCHEME),
        }
    }

    /// 按选定路径，从 host 派生运行时端点（g10_base + asr_url）。
    fn resolved(&self, picked: NetMode) -> ResolvedEndpoint {
        let (host, scheme) = self.host_for(picked);
        let hostport = host_with_port(host, scheme.default_port);
        let (g10_base, asr_url) = if hostport.is_empty() {
            (String::new(), String::new())
        } else {
            (
                format!("{}://{hostport}", scheme.http),
                format!("{}://{hostport}{ASR_PATH}", scheme.ws),
            )
        };
        ResolvedEndpoint {
            g10_base,
            asr_url,
            g10_token: self.g10_token.clone().filter(|s| !s.trim().is_empty()),
            picked,
        }
    }
}

/// 运行时选定的一条到达路径 + token，附带选中的模式（供 UI 展示）。
/// 各服务端点统一从这里派生，调用方不再直接读裸 host / `g10_base`。
#[derive(Debug, Clone)]
pub struct ResolvedEndpoint {
    pub g10_base: String,
    pub asr_url: String,
    pub g10_token: Option<String>,
    pub picked: NetMode,
}

impl ResolvedEndpoint {
    pub fn is_configured(&self) -> bool {
        !self.g10_base.trim().is_empty()
    }

    fn join(&self, suffix: &str) -> Option<String> {
        if !self.is_configured() {
            return None;
        }
        let base = self.g10_base.trim_end_matches('/');
        Some(format!("{base}{suffix}"))
    }

    pub fn cookie_endpoint(&self) -> Option<String> {
        self.join("/api/browser/cookie")
    }

    /// 音频清洗代理端点 `{g10_base}/api/web/audio/clean`。
    pub fn clean_endpoint(&self) -> Option<String> {
        self.join("/api/web/audio/clean")
    }

    /// TTS 代理端点 `{g10_base}/api/web/audio/tts`（toolkit-server 代理 → CosyVoice2）。
    pub fn tts_endpoint(&self) -> Option<String> {
        self.join("/api/web/audio/tts")
    }

    /// 音色库端点 `{g10_base}/api/web/audio/voices`。
    pub fn voices_endpoint(&self) -> Option<String> {
        self.join("/api/web/audio/voices")
    }

    /// 公共大模型层端点 `{g10_base}/api/web/llm{path}`（`path` 以 `/` 开头，如 `/config`）。
    pub fn llm_endpoint(&self, path: &str) -> Option<String> {
        self.join(&format!("/api/web/llm{path}"))
    }

    /// 句子整体替换端点 `{g10_base}/api/sentence/replace-audio`（english 后端）。
    pub fn replace_sentence_audio_endpoint(&self) -> Option<String> {
        self.join("/api/sentence/replace-audio")
    }

    /// 任意子路径端点（给少数 handler 内联拼接的 `/api/web/...` 用）。
    pub fn endpoint(&self, suffix: &str) -> Option<String> {
        self.join(suffix)
    }
}

pub fn app_settings_path(workspace: &Path) -> PathBuf {
    workspace.join("app.json")
}

/// 从旧配置的 URL/base 提取 `host[:port]`（剥 scheme + 路径）。供 schema 1/2 迁移。
fn host_from_base(base: &str) -> String {
    normalize_host(base)
}

/// 解析 app.json 文本，自动识别 schema 1/2/3 并迁移到 host 模型。
fn parse_app_settings(raw: &str) -> Result<AppSettings, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let schema = value.get("schema").and_then(|v| v.as_u64()).unwrap_or(1);
    let has_v3_keys = value.get("lan_host").is_some() || value.get("wan_host").is_some();
    if schema >= 3 || has_v3_keys {
        return serde_json::from_value(value);
    }

    // 旧版迁移：取 mode / token + 从 g10_base 抽 host。
    let mode = value
        .get("mode")
        .and_then(|m| serde_json::from_value::<NetMode>(m.clone()).ok())
        .unwrap_or_default();
    let g10_token = value
        .get("g10_token")
        .and_then(|t| t.as_str())
        .map(str::to_string);

    let (lan_host, wan_host) = if value.get("lan").is_some() || value.get("wan").is_some() {
        // schema 2：lan/wan 各有 g10_base。
        let pick = |key: &str| {
            value
                .get(key)
                .and_then(|e| e.get("g10_base"))
                .and_then(|b| b.as_str())
                .map(host_from_base)
                .unwrap_or_default()
        };
        (pick("lan"), pick("wan"))
    } else {
        // schema 1：单 g10_base → 当外网。
        let wan = value
            .get("g10_base")
            .and_then(|b| b.as_str())
            .map(host_from_base)
            .unwrap_or_default();
        (String::new(), wan)
    };
    let wan_host = if wan_host.is_empty() {
        default_wan_host()
    } else {
        wan_host
    };

    Ok(AppSettings {
        schema: CURRENT_SCHEMA,
        mode,
        lan_host,
        wan_host,
        g10_token,
    })
}

pub fn load_app_settings(workspace: &Path) -> AppSettings {
    let path = app_settings_path(workspace);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AppSettings::default(),
        Err(e) => {
            log::warn!("app.json read {} failed: {e}", path.display());
            return AppSettings::default();
        }
    };
    parse_app_settings(&raw).unwrap_or_else(|e| {
        log::warn!("app.json parse {} failed: {e}", path.display());
        AppSettings::default()
    })
}

pub fn save_app_settings(workspace: &Path, s: &AppSettings) -> anyhow::Result<()> {
    use anyhow::Context;
    let path = app_settings_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut to_save = s.clone();
    to_save.schema = CURRENT_SCHEMA;
    let body = serde_json::to_string_pretty(&to_save)?;
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// 对 `{g10_base}/api/web/health` 发一次短超时 GET，2xx 即视为该路径可达。
pub async fn health_ok(g10_base: &str) -> bool {
    let base = g10_base.trim_end_matches('/');
    if base.is_empty() {
        return false;
    }
    let url = format!("{base}/api/web/health");
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(client.get(&url).send().await, Ok(resp) if resp.status().is_success())
}

struct CacheEntry {
    resolved: ResolvedEndpoint,
    at: Instant,
}

/// 带缓存的活动端点解析器（放进 `AppState`，全应用共享）。
///
/// `auto` 模式下对局域网地址做健康探测、结果缓存 [`PROBE_TTL`]；`lan`/`wan` 强制档不探测、
/// 不缓存。保存配置 / 窗口重新聚焦 / 连续请求失败时调 [`invalidate`](Self::invalidate)
/// 强制重探。
#[derive(Default)]
pub struct NetResolver {
    cache: Mutex<Option<CacheEntry>>,
}

impl NetResolver {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(None),
        }
    }

    /// 解析当前应使用的端点。非 auto 直接返回、不探测；auto 命中缓存直接返回，否则探测
    /// 局域网健康端点后选路并刷新缓存（持锁探测，并发调用共享同一次探测结果）。
    pub async fn resolve(&self, workspace: &Path) -> ResolvedEndpoint {
        let settings = load_app_settings(workspace);
        match settings.mode {
            NetMode::Lan => return settings.resolved(NetMode::Lan),
            NetMode::Wan => return settings.resolved(NetMode::Wan),
            NetMode::Auto => {}
        }
        // 局域网未配 → 无需探测，直接外网。
        let lan = settings.resolved(NetMode::Lan);
        if !lan.is_configured() {
            return settings.resolved(NetMode::Wan);
        }
        let mut guard = self.cache.lock().await;
        if let Some(entry) = guard.as_ref() {
            if entry.at.elapsed() < PROBE_TTL {
                return entry.resolved.clone();
            }
        }
        let lan_ok = health_ok(&lan.g10_base).await;
        let resolved = if lan_ok {
            lan
        } else {
            settings.resolved(NetMode::Wan)
        };
        *guard = Some(CacheEntry {
            resolved: resolved.clone(),
            at: Instant::now(),
        });
        resolved
    }

    /// 失效探测缓存（保存配置 / 网络切换 / 窗口聚焦时调用）。
    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_default_is_auto_wan_host() {
        let s = AppSettings::default();
        assert_eq!(s.mode, NetMode::Auto);
        assert_eq!(s.wan_host, DEFAULT_WAN_HOST);
        assert!(s.lan_host.is_empty());
    }

    #[test]
    fn lan_host_derives_http_and_ws() {
        let s = AppSettings {
            lan_host: "192.168.1.100".to_string(),
            ..AppSettings::default()
        };
        let r = s.resolved(NetMode::Lan);
        assert!(r.is_configured());
        assert_eq!(r.g10_base, "http://192.168.1.100:8788");
        assert_eq!(r.asr_url, "ws://192.168.1.100:8788/api/asr/stream");
        assert_eq!(
            r.tts_endpoint().as_deref(),
            Some("http://192.168.1.100:8788/api/web/audio/tts")
        );
    }

    #[test]
    fn wan_host_derives_https_and_wss() {
        let s = AppSettings::default();
        let r = s.resolved(NetMode::Wan);
        assert_eq!(r.g10_base, "https://www.for-memory.cloud:28080");
        assert_eq!(r.asr_url, "wss://www.for-memory.cloud:28080/api/asr/stream");
    }

    #[test]
    fn explicit_port_in_host_overrides_default() {
        let s = AppSettings {
            lan_host: "10.0.0.2:9000".to_string(),
            ..AppSettings::default()
        };
        assert_eq!(s.resolved(NetMode::Lan).g10_base, "http://10.0.0.2:9000");
    }

    #[test]
    fn empty_lan_host_not_configured() {
        let s = AppSettings::default();
        let r = s.resolved(NetMode::Lan);
        assert!(!r.is_configured());
        assert_eq!(r.tts_endpoint(), None);
        assert_eq!(r.asr_url, "");
    }

    #[test]
    fn migrate_schema1_single_base_to_wan_host() {
        let raw = r#"{"g10_base":"http://192.168.1.50:8788","g10_token":"abc"}"#;
        let s = parse_app_settings(raw).unwrap();
        assert_eq!(s.schema, CURRENT_SCHEMA);
        assert_eq!(s.wan_host, "192.168.1.50:8788");
        assert!(s.lan_host.is_empty());
        assert_eq!(s.g10_token.as_deref(), Some("abc"));
    }

    #[test]
    fn migrate_schema2_extracts_hosts() {
        let raw = r#"{
            "schema":2,"mode":"lan",
            "lan":{"g10_base":"http://10.0.0.2:8788","asr_url":"ws://10.0.0.2:8090/stream"},
            "wan":{"g10_base":"https://x.cloud:28080","asr_url":"wss://x.cloud:28090/stream"},
            "g10_token":"tok"
        }"#;
        let s = parse_app_settings(raw).unwrap();
        assert_eq!(s.mode, NetMode::Lan);
        assert_eq!(s.lan_host, "10.0.0.2:8788");
        assert_eq!(s.wan_host, "x.cloud:28080");
        assert_eq!(s.g10_token.as_deref(), Some("tok"));
    }

    #[test]
    fn migrate_empty_wan_falls_back_to_default() {
        let raw = r#"{"g10_token":null}"#;
        let s = parse_app_settings(raw).unwrap();
        assert_eq!(s.wan_host, DEFAULT_WAN_HOST);
    }

    #[test]
    fn blank_token_filtered_out() {
        let s = AppSettings {
            lan_host: "1.2.3.4".to_string(),
            g10_token: Some("   ".to_string()),
            ..AppSettings::default()
        };
        assert_eq!(s.resolved(NetMode::Lan).g10_token, None);
    }
}
