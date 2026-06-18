//! 全局应用配置，落盘在 `{workspace}/app.json`。
//!
//! **单节点双地址模型**：同一台 GB10 既可经局域网直连，也可经外网域名到达。配置里为
//! 每条路径各存一组地址（g10 代理 base + 语音 ASR 的 WebSocket 地址）。`mode` 决定运行时
//! 选哪条：
//! - `auto`（默认）：对局域网 g10 健康端点做一次短超时探测，通则走局域网、否则回退外网；
//! - `lan` / `wan`：强制档（调试用）。
//!
//! g10 代理类服务（cookie / TTS / 清洗 / LLM / english 替换）与语音 ASR 同属一个节点，
//! 自动切换时一起切，避免「代理走外网、语音仍连内网 IP」的错位。
//!
//! 解析带缓存：见 [`NetResolver`]。旧版 `app.json`（schema 1，仅单 `g10_base`）会被平滑迁移。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 当前 app.json schema 版本。
const CURRENT_SCHEMA: u32 = 2;
/// 自动探测的 health 请求超时（局域网内基本 10ms 返回；不可达则快速回退外网）。
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
/// 探测结果缓存时长；保存配置 / 网络变化时通过 [`NetResolver::invalidate`] 主动失效。
const PROBE_TTL: Duration = Duration::from_secs(30);

/// 一条到达路径的地址对。任一字段允许为空串，表示该路径未配置对应服务。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// g10 代理 base，如 `http://192.168.1.100:8788`（不含路径）。
    #[serde(default)]
    pub g10_base: String,
    /// 语音 ASR orchestrator 的 WebSocket 地址，如 `ws://192.168.1.100:8090/stream`。
    #[serde(default)]
    pub asr_url: String,
}

/// 网络模式：自动探测 / 强制局域网 / 强制外网。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetMode {
    Auto,
    Lan,
    Wan,
}

impl Default for NetMode {
    fn default() -> Self {
        NetMode::Auto
    }
}

fn default_schema() -> u32 {
    CURRENT_SCHEMA
}

fn default_wan() -> Endpoint {
    Endpoint {
        g10_base: "https://www.for-memory.cloud:28080".to_string(),
        asr_url: String::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub mode: NetMode,
    /// 局域网路径地址（在家直连，速度快）。
    #[serde(default)]
    pub lan: Endpoint,
    /// 外网路径地址（在外经域名到达）。
    #[serde(default = "default_wan")]
    pub wan: Endpoint,
    /// 可选 Bearer token（若 G10 server 启用了鉴权；内外网共用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub g10_token: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema: CURRENT_SCHEMA,
            mode: NetMode::Auto,
            lan: Endpoint::default(),
            wan: default_wan(),
            g10_token: None,
        }
    }
}

/// schema 1 旧结构（仅单 `g10_base` + token）。
#[derive(Deserialize)]
struct LegacyAppSettings {
    #[serde(default)]
    g10_base: String,
    #[serde(default)]
    g10_token: Option<String>,
}

impl AppSettings {
    /// 从旧版（schema 1）迁移：旧单地址挪到 `wan.g10_base`，mode 留 `auto`（局域网未配时
    /// auto 直接走外网，行为与旧版完全一致；用户补上局域网地址后即自动启用切换）。
    fn from_legacy(legacy: LegacyAppSettings) -> Self {
        let base = legacy.g10_base.trim().to_string();
        let wan = if base.is_empty() {
            default_wan()
        } else {
            Endpoint {
                g10_base: base,
                asr_url: String::new(),
            }
        };
        Self {
            schema: CURRENT_SCHEMA,
            mode: NetMode::Auto,
            lan: Endpoint::default(),
            wan,
            g10_token: legacy.g10_token,
        }
    }

    fn endpoint_for(&self, mode: NetMode) -> &Endpoint {
        match mode {
            NetMode::Lan => &self.lan,
            // Auto 仅作兜底（探测后会显式传 Lan/Wan）。
            NetMode::Wan | NetMode::Auto => &self.wan,
        }
    }

    /// 按选定路径构造运行时端点（trim 后）。
    fn resolved(&self, picked: NetMode) -> ResolvedEndpoint {
        let ep = self.endpoint_for(picked);
        ResolvedEndpoint {
            g10_base: ep.g10_base.trim().to_string(),
            asr_url: ep.asr_url.trim().to_string(),
            g10_token: self.g10_token.clone().filter(|s| !s.trim().is_empty()),
            picked,
        }
    }
}

/// 运行时选定的一条到达路径 + token，附带选中的模式（供 UI 展示）。
/// 各服务端点统一从这里派生，调用方不再直接读裸 `g10_base`。
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

/// 解析 app.json 文本，自动识别 schema 1 / 2 并迁移。
fn parse_app_settings(raw: &str) -> Result<AppSettings, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let schema = value.get("schema").and_then(|v| v.as_u64()).unwrap_or(1);
    let has_v2_keys =
        value.get("lan").is_some() || value.get("wan").is_some() || value.get("mode").is_some();
    if schema >= 2 || has_v2_keys {
        serde_json::from_value(value)
    } else {
        let legacy: LegacyAppSettings = serde_json::from_value(value)?;
        Ok(AppSettings::from_legacy(legacy))
    }
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
        // 局域网地址未配 → 无需探测，直接外网。
        if settings.lan.g10_base.trim().is_empty() {
            return settings.resolved(NetMode::Wan);
        }
        let mut guard = self.cache.lock().await;
        if let Some(entry) = guard.as_ref() {
            if entry.at.elapsed() < PROBE_TTL {
                return entry.resolved.clone();
            }
        }
        let lan_ok = health_ok(&settings.lan.g10_base).await;
        let resolved = settings.resolved(if lan_ok { NetMode::Lan } else { NetMode::Wan });
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
    fn legacy_schema1_migrates_to_wan_base() {
        let raw = r#"{"g10_base":"http://192.168.1.50:8788","g10_token":"abc"}"#;
        let s = parse_app_settings(raw).unwrap();
        assert_eq!(s.schema, CURRENT_SCHEMA);
        assert_eq!(s.mode, NetMode::Auto);
        assert_eq!(s.wan.g10_base, "http://192.168.1.50:8788");
        assert!(s.lan.g10_base.is_empty());
        assert_eq!(s.g10_token.as_deref(), Some("abc"));
    }

    #[test]
    fn legacy_empty_base_falls_back_to_default_wan() {
        let raw = r#"{"g10_token":null}"#;
        let s = parse_app_settings(raw).unwrap();
        assert_eq!(s.wan.g10_base, "https://www.for-memory.cloud:28080");
    }

    #[test]
    fn v2_roundtrip_parses() {
        let raw = r#"{
            "schema":2,"mode":"lan",
            "lan":{"g10_base":"http://10.0.0.2:8788","asr_url":"ws://10.0.0.2:8090/stream"},
            "wan":{"g10_base":"https://x.cloud:28080","asr_url":"wss://x.cloud:28090/stream"},
            "g10_token":"tok"
        }"#;
        let s = parse_app_settings(raw).unwrap();
        assert_eq!(s.mode, NetMode::Lan);
        assert_eq!(s.lan.asr_url, "ws://10.0.0.2:8090/stream");
        assert_eq!(s.wan.g10_base, "https://x.cloud:28080");
    }

    #[test]
    fn resolved_lan_builds_endpoints() {
        let s = AppSettings {
            schema: 2,
            mode: NetMode::Lan,
            lan: Endpoint {
                g10_base: "http://10.0.0.2:8788/".to_string(),
                asr_url: "ws://10.0.0.2:8090/stream".to_string(),
            },
            wan: default_wan(),
            g10_token: Some("tok".to_string()),
        };
        let r = s.resolved(NetMode::Lan);
        assert!(r.is_configured());
        assert_eq!(
            r.tts_endpoint().as_deref(),
            Some("http://10.0.0.2:8788/api/web/audio/tts")
        );
        assert_eq!(
            r.llm_endpoint("/config").as_deref(),
            Some("http://10.0.0.2:8788/api/web/llm/config")
        );
        assert_eq!(r.asr_url, "ws://10.0.0.2:8090/stream");
        assert_eq!(r.g10_token.as_deref(), Some("tok"));
    }

    #[test]
    fn resolved_empty_base_not_configured() {
        let s = AppSettings {
            schema: 2,
            mode: NetMode::Lan,
            lan: Endpoint::default(),
            wan: default_wan(),
            g10_token: None,
        };
        let r = s.resolved(NetMode::Lan);
        assert!(!r.is_configured());
        assert_eq!(r.tts_endpoint(), None);
    }

    #[test]
    fn blank_token_filtered_out() {
        let s = AppSettings {
            g10_token: Some("   ".to_string()),
            ..AppSettings::default()
        };
        assert_eq!(s.resolved(NetMode::Wan).g10_token, None);
    }
}
