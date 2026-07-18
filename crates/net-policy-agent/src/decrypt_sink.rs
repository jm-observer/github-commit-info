//! L4 明文落盘 sink（抓包设计 §17.6）：把 `net-policy-mitm` 解密出的 flow 经 `net_policy_core::decrypt`
//! **脱敏**后写成结构化 `http.jsonl`（一行一个事件），并维护每域名计数与总字节配额。
//!
//! 分工：`net-policy-mitm` 只解密+解析+回调（不懂内容语义）；本 sink 是消费方——脱敏（默认档核心凭据
//! 不可关）+ 落盘 + 配额。真正把目标进程流量导进 MITM（mihomo 规则 / loopback 监听）是运行态编排，
//! 受 §18.1 方案 B 真机 spike 阻断，尚未接线。

use crate::store::now_ms;
use anyhow::{Context, Result};
use net_policy_core::decrypt::{DecryptOpts, DomainCounters, HttpEvent};
use net_policy_mitm::sink::{FlowRequest, FlowResponse, FlowSink, FlowWsFrame, WsDirection};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// 写 `http.jsonl` 的 [`FlowSink`]，脱敏 + 配额 + 每域名计数。
pub struct DecryptSink {
    opts: DecryptOpts,
    inner: Mutex<Inner>,
}

struct Inner {
    /// 追加写入的 http.jsonl。
    file: std::fs::File,
    /// 已写字节（含换行）；超 `opts.max_total_bytes` 后停止落盘（§17.5）。
    total_bytes: u64,
    /// 命中配额后置位：后续事件只计数不落盘。
    quota_hit: bool,
    /// 每域名计数（§17.9）。
    per_domain: BTreeMap<String, DomainCounters>,
}

impl DecryptSink {
    /// 在 `path` 创建（截断）http.jsonl 并返回 sink。
    pub fn create(path: &Path, opts: DecryptOpts) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("创建 http.jsonl 失败：{}", path.display()))?;
        Ok(Self {
            opts,
            inner: Mutex::new(Inner {
                file,
                total_bytes: 0,
                quota_hit: false,
                per_domain: BTreeMap::new(),
            }),
        })
    }

    /// 每域名计数快照（Phase 4b 写 DecryptSession.per_domain 用；当前仅测试引用）。
    #[allow(dead_code)]
    pub fn per_domain(&self) -> BTreeMap<String, DomainCounters> {
        self.inner.lock().unwrap().per_domain.clone()
    }

    /// 是否已命中总字节配额（Phase 4b manifest 用；当前仅测试引用）。
    #[allow(dead_code)]
    pub fn quota_hit(&self) -> bool {
        self.inner.lock().unwrap().quota_hit
    }

    /// 脱敏事件 → 落盘一行（配额内）+ 记一次「decrypted」到该域名。中毒锁不 panic 传播。
    fn emit(&self, host: &str, ev: &HttpEvent) {
        let line = match ev.to_jsonl() {
            Ok(l) => l,
            Err(e) => {
                log::warn!("http.jsonl 序列化失败：{e}");
                return;
            }
        };
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        inner
            .per_domain
            .entry(host.to_string())
            .or_default()
            .decrypted += 1;
        if inner.quota_hit {
            return;
        }
        let bytes = line.len() as u64 + 1; // +换行
        if inner.total_bytes + bytes > self.opts.max_total_bytes {
            inner.quota_hit = true;
            log::info!(
                "L4 http.jsonl 达总字节配额（{} bytes），停止落盘",
                self.opts.max_total_bytes
            );
            return;
        }
        if writeln!(inner.file, "{line}").is_ok() {
            inner.total_bytes += bytes;
        }
    }
}

impl FlowSink for DecryptSink {
    fn on_request(&self, req: &FlowRequest) {
        let ev = HttpEvent::request(
            now_ms(),
            req.domain,
            req.method,
            req.path,
            req.version,
            req.headers,
            req.body,
            &self.opts,
        );
        self.emit(req.domain, &ev);
    }

    fn on_response(&self, resp: &FlowResponse) {
        let ev = HttpEvent::response(
            now_ms(),
            resp.domain,
            resp.status,
            resp.version,
            resp.headers,
            resp.body,
            &self.opts,
        );
        self.emit(resp.domain, &ev);
    }

    fn on_ws_frame(&self, frame: &FlowWsFrame) {
        let ev = HttpEvent::ws_frame(
            now_ms(),
            frame.domain,
            frame.direction == WsDirection::ClientToServer,
            frame.is_text,
            frame.payload,
            &self.opts,
        );
        self.emit(frame.domain, &ev);
    }

    /// 未拦截透传（allowlist miss）：只记 per-domain passthrough，不落任何明文（§17.9）。
    fn on_passthrough(&self, domain: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .per_domain
                .entry(domain.to_string())
                .or_default()
                .passthrough += 1;
        }
    }

    /// 客户端拒绝伪造叶子证书（pinning/自带 CA/mTLS，§17.7）：记 per-domain pinned，诚实不宣称解密。
    fn on_client_cert_rejected(&self, domain: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .per_domain
                .entry(domain.to_string())
                .or_default()
                .pinned += 1;
        }
    }
}

/// L4 MITM 数据面 spike（方案 B 验证）：前台起一个 loopback 显式 MITM 代理，上游按域名链到 mihomo，
/// 对 `domains` 白名单域名解密并把脱敏明文写 `out`。**不装 CA、不改 mihomo**——诊断用（见 `main.rs` 子命令）。
/// 阻塞运行直到进程被杀。
pub fn run_mitm_spike(
    listen: &str,
    upstream: &str,
    ca_dir: &str,
    domains: &str,
    out: &str,
) -> Result<()> {
    use net_policy_mitm::cert::ca::CertAuthority;
    use net_policy_mitm::cert::site::CertCache;
    use net_policy_mitm::proxy::{run_proxy, ProxyRuntime};
    use net_policy_mitm::shutdown::ShutdownToken;
    use net_policy_mitm::upstream::Upstream;
    use std::sync::Arc;

    // rustls 0.23 需进程启动时装一次 CryptoProvider，否则首个 TLS 握手 panic。
    net_policy_mitm::install_crypto_provider();

    let (host, port_s) = listen.rsplit_once(':').context("--listen 需 host:port")?;
    let port: u16 = port_s.parse().context("--listen 端口非法")?;
    let ca_dir_p = std::path::PathBuf::from(ca_dir);
    let ca = CertAuthority::load_or_generate(&ca_dir_p).context("CA load_or_generate")?;
    println!("CA_CERT={}", ca_dir_p.join("ca.crt").display());
    let cert_cache = Arc::new(CertCache::new(ca));

    // 白名单域名（精确或子域后缀命中才 MITM，其余透传）。
    let allow: Vec<String> = domains
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let should_intercept: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(move |h: &str| {
        let h = h.to_ascii_lowercase();
        allow
            .iter()
            .any(|d| h == *d || h.ends_with(&format!(".{d}")))
    });

    // spike 用 capture_bodies=true 以便 http.jsonl 里能看到脱敏后的明文正文（更直观的证据）。
    let opts = DecryptOpts {
        capture_bodies: true,
        ..DecryptOpts::default()
    };
    let sink = Arc::new(DecryptSink::create(std::path::Path::new(out), opts)?);
    let runtime = ProxyRuntime {
        cert_cache,
        sink,
        should_intercept,
        expected_proxy_authorization: None,
    };
    let upstream = Arc::new(Upstream::parse(upstream).context("--upstream 解析失败")?);
    let shutdown = ShutdownToken::new();

    println!("MITM_LISTEN={host}:{port} UPSTREAM_OK OUT={out}");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(async move { run_proxy(host, port, upstream, runtime, shutdown).await })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("np-decrypt-sink-{}-{name}", std::process::id()))
    }

    #[test]
    fn writes_redacted_jsonl_and_counts() {
        let path = tmp("basic.jsonl");
        let _ = std::fs::remove_file(&path);
        let sink = DecryptSink::create(&path, DecryptOpts::default()).unwrap();
        sink.on_request(&FlowRequest {
            domain: "api.example.com",
            method: "POST",
            path: "/v1/chat?token=SEKRET",
            version: "HTTP/2",
            headers: &[("Cookie".into(), "sid=x".into())],
            body: b"{}",
        });
        sink.on_response(&FlowResponse {
            domain: "api.example.com",
            status: 200,
            version: "HTTP/2",
            headers: &[],
            body: b"ok",
        });
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "两行事件");
        // 脱敏：Cookie 与 token 不出现明文
        assert!(!content.contains("sid=x"));
        assert!(!content.contains("SEKRET"));
        assert!(content.contains("[REDACTED]"));
        assert!(lines[0].contains("\"kind\":\"request\""));
        assert!(lines[1].contains("\"kind\":\"response\""));
        // 每域名计数
        let pd = sink.per_domain();
        assert_eq!(pd.get("api.example.com").unwrap().decrypted, 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn passthrough_and_pinned_counters_no_plaintext() {
        let path = tmp("audit.jsonl");
        let _ = std::fs::remove_file(&path);
        let sink = DecryptSink::create(&path, DecryptOpts::default()).unwrap();
        // allowlist miss（透传）与 pinning 拒证：只记计数，绝不落明文行。
        sink.on_passthrough("cdn.example.com");
        sink.on_passthrough("cdn.example.com");
        sink.on_client_cert_rejected("pinned.example.com");
        let pd = sink.per_domain();
        assert_eq!(pd.get("cdn.example.com").unwrap().passthrough, 2);
        assert_eq!(pd.get("cdn.example.com").unwrap().decrypted, 0);
        assert_eq!(pd.get("pinned.example.com").unwrap().pinned, 1);
        assert_eq!(pd.get("pinned.example.com").unwrap().decrypted, 0);
        // http.jsonl 不因审计事件产生任何行。
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(content.is_empty(), "审计事件不落明文");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn total_bytes_quota_stops_writing() {
        let path = tmp("quota.jsonl");
        let _ = std::fs::remove_file(&path);
        // 极小配额 → 第一条就命中
        let opts = DecryptOpts {
            max_total_bytes: 10,
            ..Default::default()
        };
        let sink = DecryptSink::create(&path, opts).unwrap();
        for _ in 0..3 {
            sink.on_request(&FlowRequest {
                domain: "h",
                method: "GET",
                path: "/",
                version: "HTTP/1.1",
                headers: &[],
                body: b"",
            });
        }
        assert!(sink.quota_hit(), "应命中配额");
        // 计数仍累加，但落盘被截停
        assert_eq!(sink.per_domain().get("h").unwrap().decrypted, 3);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.lines().count() < 3, "落盘行数少于事件数");
        let _ = std::fs::remove_file(&path);
    }
}
