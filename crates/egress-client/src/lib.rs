//! `egress-client` —— 出口代理「消费 HTTP 面」的薄客户端(F4)。
//!
//! 背景:`egress-pool` 的 `Pool`/`Session` 是**进程内**原语——只有跟 controller
//! (`toolkit-server`)同进程跑的代码才能拿到 `Arc<egress_pool::Registry>` 去构造它们。
//! **外部进程**(不同机器 / 不同语言生态的其它 Rust 项目)借不到这个 `Arc`,只能经
//! HTTP 走 `toolkit-server` 暴露的 `/api/web/egress/*` 消费面(见
//! `crates/toolkit-server/src/routes/egress.rs`)。本 crate 就是那层 HTTP 面的薄客户端,
//! 复用 [`egress_pool::EgressResponse`] 作为响应线类型(不重新定义一遍字段)。
//!
//! ## 两种借出口方式
//!
//! - [`EgressClient::fetch`] —— 匿名短租:单次代发,不占用 worker,IP 随机轮换。
//! - [`EgressClient::session`] —— 钉死长租:拿一个 [`RemoteSession`],其内所有请求
//!   经同一个 worker(同一出口 IP + 连续 cookie)。session 状态实际存活在 controller
//!   进程里(`toolkit-server` 的 `SessionStore`),本地的 [`RemoteSession`] 只持有一个
//!   不透明的 `session_handle` 字符串。
//!
//! ## 关于释放
//!
//! Rust 目前不支持 `async fn drop`,所以 [`RemoteSession`] **不会在 drop 时自动通知
//! controller 释放 session**——用完务必显式调用 [`RemoteSession::release`]。忘记调用
//! 也不会永久泄漏:controller 侧有 TTL reaper,session 空闲超时后会被自动回收
//! (见 toolkit-server 的 `egress_sessions` 模块,默认 5 分钟)。
//!
//! ## 用法
//!
//! ```no_run
//! # async fn _x() -> anyhow::Result<()> {
//! use egress_client::EgressClient;
//!
//! let client = EgressClient::new("http://127.0.0.1:8788", None);
//!
//! // 匿名短租
//! let resp = client.fetch("GET", "https://api.ipify.org", vec![], None).await?;
//! println!("ip={:?}", resp.body);
//!
//! // 钉死长租:同一 session 内多次请求走同一 worker + 连续 cookie
//! let session = client.session("douyin", Some("acc1")).await?;
//! let r1 = session.fetch("GET", "https://example.com", vec![], None).await?;
//! let r2 = session.fetch("GET", "https://example.com/next", vec![], None).await?;
//! println!("{} {}", r1.status, r2.status);
//! session.release().await?; // 显式释放;忘记调用也有 controller 侧 TTL 兜底
//! # Ok(()) }
//! ```

use anyhow::{bail, Context, Result};
pub use egress_pool::{EgressError, EgressResponse, WorkerStatus};
use serde::Serialize;
use serde_json::Value;

/// 出口代理 controller(`toolkit-server`)的薄 HTTP 客户端。`Clone` 廉价
/// (内部是 `reqwest::Client` 的 Arc 句柄 + 字符串)。
#[derive(Debug, Clone)]
pub struct EgressClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

/// `/fetch`、`/session/{handle}/fetch` 共用的请求体线格式,与
/// `toolkit-server::routes::egress::FetchReq` 对齐。
#[derive(Serialize)]
struct FetchBody<'a> {
    method: &'a str,
    url: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    headers: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

impl EgressClient {
    /// 用 controller base URL(如 `http://127.0.0.1:8788`,不带尾斜杠)+ 可选 bearer token 构造。
    ///
    /// `/api/web/egress/*` 目前不在 token 鉴权中间件后,带不带 token 都能通;这里仍接受
    /// `Option<String>` 并在有值时带上 `Authorization: Bearer`,为将来加鉴权保持前向兼容。
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        let http = reqwest::Client::new();
        Self::with_client(http, base_url, token)
    }

    /// 复用调用方已有的 `reqwest::Client`(自定义代理 / 超时 / TLS 配置时使用)。
    pub fn with_client(
        http: reqwest::Client,
        base_url: impl Into<String>,
        token: Option<String>,
    ) -> Self {
        let mut base = base_url.into();
        while base.ends_with('/') {
            base.pop();
        }
        Self { http, base, token }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/web/egress{}", self.base, path)
    }

    fn apply_auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// 发一个 POST 请求,非 2xx 时尝试解析 `{error}` 字段并 `bail!` 带出来。
    async fn post_json(&self, path: &str, body: &impl Serialize) -> Result<Value> {
        let rb = self.apply_auth(self.http.post(self.url(path))).json(body);
        let resp = rb
            .send()
            .await
            .with_context(|| format!("调 egress controller {path}"))?;
        let status = resp.status();
        let text = resp.text().await.context("读响应 body")?;
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| text.chars().take(200).collect());
            bail!("egress controller {path} 返回 {status}: {msg}");
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "解析 {path} 响应失败,前 200 字符: {}",
                text.chars().take(200).collect::<String>()
            )
        })
    }

    /// 发一个 GET 请求,非 2xx 时尝试解析 `{error}` 字段并 `bail!` 带出来。
    async fn get_json(&self, path: &str) -> Result<Value> {
        let rb = self.apply_auth(self.http.get(self.url(path)));
        let resp = rb
            .send()
            .await
            .with_context(|| format!("调 egress controller {path}"))?;
        let status = resp.status();
        let text = resp.text().await.context("读响应 body")?;
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| text.chars().take(200).collect());
            bail!("egress controller {path} 返回 {status}: {msg}");
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "解析 {path} 响应失败,前 200 字符: {}",
                text.chars().take(200).collect::<String>()
            )
        })
    }

    /// 列出 controller 上当前注册的 worker 快照(在线状态 / 出口 IP / 占用 type),
    /// 供外部诊断/测试程序做交叉核对。
    pub async fn workers(&self) -> Result<Vec<WorkerStatus>> {
        #[derive(serde::Deserialize)]
        struct Resp {
            workers: Vec<WorkerStatus>,
        }
        let v = self.get_json("/workers").await?;
        let parsed: Resp = serde_json::from_value(v).context("解析 workers 响应失败")?;
        Ok(parsed.workers)
    }

    /// 匿名短租:随手挑一个在线 worker 代发,不占用。
    pub async fn fetch(
        &self,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<String>,
    ) -> Result<EgressResponse> {
        let payload = FetchBody {
            method,
            url,
            headers,
            body,
        };
        let v = self.post_json("/fetch", &payload).await?;
        serde_json::from_value(v).context("解析 EgressResponse 失败")
    }

    /// 钉死长租:向 controller 申请一个 session,返回本地句柄 [`RemoteSession`]。
    /// `account=Some` 走具名身份(跨作用域复用 cookie + 出口);`account=None` 为临时钉定。
    pub async fn session(&self, typ: &str, account: Option<&str>) -> Result<RemoteSession> {
        #[derive(Serialize)]
        struct Req<'a> {
            typ: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            account: Option<&'a str>,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            session_handle: String,
            worker_id: String,
        }
        let v = self.post_json("/session", &Req { typ, account }).await?;
        let parsed: Resp = serde_json::from_value(v).context("解析 session 创建响应失败")?;
        Ok(RemoteSession {
            client: self.clone(),
            handle: parsed.session_handle,
            worker_id: parsed.worker_id,
        })
    }
}

/// 钉死到 controller 侧某个 session 的远程句柄。所有请求经同一台 worker 代发
/// (同一出口 IP + 连续 cookie)。
///
/// **不做 async Drop**(Rust 不支持):用完请显式调用 [`RemoteSession::release`];
/// 忘记调用有 controller 侧 TTL reaper 兜底(默认 5 分钟空闲后自动回收)。
pub struct RemoteSession {
    client: EgressClient,
    handle: String,
    worker_id: String,
}

impl RemoteSession {
    /// 当前钉定的 worker id(仅供观测/日志,不影响调用行为)。
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// 本地 session 句柄(诊断用;正常调用方不需要直接使用它)。
    pub fn handle(&self) -> &str {
        &self.handle
    }

    /// 经钉定 session 代发(同一 worker + 连续 cookie)。
    pub async fn fetch(
        &self,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<String>,
    ) -> Result<EgressResponse> {
        let payload = FetchBody {
            method,
            url,
            headers,
            body,
        };
        let path = format!("/session/{}/fetch", self.handle);
        let v = self.client.post_json(&path, &payload).await?;
        serde_json::from_value(v).context("解析 EgressResponse 失败")
    }

    /// 显式释放(消费 self):通知 controller 从 `SessionStore` 移除,立即触发 worker 占用释放。
    /// 幂等——即便 controller 侧已因 TTL 提前回收,这里仍返回成功。
    pub async fn release(self) -> Result<()> {
        let path = format!("/session/{}/release", self.handle);
        self.client.post_json(&path, &serde_json::json!({})).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_strips_trailing_slash() {
        let c = EgressClient::new("http://127.0.0.1:8788/", None);
        assert_eq!(
            c.url("/fetch"),
            "http://127.0.0.1:8788/api/web/egress/fetch"
        );

        let c = EgressClient::new("http://127.0.0.1:8788///", None);
        assert_eq!(
            c.url("/fetch"),
            "http://127.0.0.1:8788/api/web/egress/fetch"
        );
    }

    #[test]
    fn url_joins_workers_path() {
        let c = EgressClient::new("http://127.0.0.1:8788", None);
        assert_eq!(
            c.url("/workers"),
            "http://127.0.0.1:8788/api/web/egress/workers"
        );
    }

    #[test]
    fn url_joins_session_paths() {
        let c = EgressClient::new("http://host:8788", None);
        assert_eq!(
            c.url("/session/abc-123/fetch"),
            "http://host:8788/api/web/egress/session/abc-123/fetch"
        );
        assert_eq!(
            c.url("/session/abc-123/release"),
            "http://host:8788/api/web/egress/session/abc-123/release"
        );
    }

    #[test]
    fn fetch_body_serializes_optional_fields() {
        let body = FetchBody {
            method: "GET",
            url: "https://example.com",
            headers: vec![],
            body: None,
        };
        let v = serde_json::to_value(&body).unwrap();
        // headers 空 + body None 时应被 skip_serializing_if 省略,和 handler 的 #[serde(default)] 对齐。
        assert!(v.get("headers").is_none());
        assert!(v.get("body").is_none());
        assert_eq!(v["method"], "GET");
        assert_eq!(v["url"], "https://example.com");
    }

    #[test]
    fn remote_session_exposes_worker_id_and_handle() {
        let client = EgressClient::new("http://127.0.0.1:8788", None);
        let sess = RemoteSession {
            client,
            handle: "h1".to_string(),
            worker_id: "w1".to_string(),
        };
        assert_eq!(sess.worker_id(), "w1");
        assert_eq!(sess.handle(), "h1");
    }
}
