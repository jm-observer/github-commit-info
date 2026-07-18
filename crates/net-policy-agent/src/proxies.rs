//! mihomo 代理订阅节点查询与测速。只允许读取当前激活 provider，避免 GUI 直接访问 controller。

use anyhow::{bail, Context, Result};
use net_policy_core::config::NetPolicySettings;
use net_policy_core::mihomo::CONTROLLER;
use net_policy_core::types::ProxyNode;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

const TEST_URL: &str = "https://www.gstatic.com/generate_204";

#[derive(Debug, Deserialize)]
struct ProvidersResponse {
    #[serde(default)]
    providers: BTreeMap<String, Provider>,
}

#[derive(Debug, Deserialize)]
struct Provider {
    #[serde(default)]
    proxies: Vec<RawNode>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawNode {
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    alive: bool,
    #[serde(default)]
    history: Vec<History>,
}

#[derive(Debug, Clone, Deserialize)]
struct History {
    #[serde(default)]
    delay: u32,
}

#[derive(Debug, Deserialize)]
struct DelayResponse {
    delay: u32,
}

fn active_provider_name(settings: &NetPolicySettings) -> Result<String> {
    let (slot, _) = settings
        .proxy_subscriptions
        .active_subscription()
        .context("尚未配置并激活代理订阅")?;
    Ok(format!("net-policy-sub-{}", slot + 1))
}

fn client(secret: &str) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if !secret.is_empty() {
        let value = format!("Bearer {secret}")
            .parse()
            .context("mihomo controller secret 非法")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(8))
        .build()
        .context("创建 mihomo controller 客户端失败")
}

fn node(raw: RawNode) -> ProxyNode {
    ProxyNode {
        name: raw.name,
        kind: raw.kind,
        alive: raw.alive,
        delay_ms: raw
            .history
            .iter()
            .rev()
            .find_map(|item| (item.delay > 0).then_some(item.delay)),
    }
}

async fn provider_nodes(secret: &str, settings: &NetPolicySettings) -> Result<Vec<ProxyNode>> {
    let provider_name = active_provider_name(settings)?;
    let response = client(secret)?
        .get(format!("http://{CONTROLLER}/providers/proxies"))
        .send()
        .await
        .context("读取 mihomo 代理订阅失败")?
        .error_for_status()
        .context("mihomo 未返回代理订阅节点")?
        .json::<ProvidersResponse>()
        .await
        .context("解析 mihomo 代理订阅响应失败")?;
    let provider = response
        .providers
        .get(&provider_name)
        .context("当前订阅尚未加载；请保存并应用代理设置后重试")?;
    Ok(provider.proxies.iter().cloned().map(node).collect())
}

pub async fn list_active(secret: &str, settings: &NetPolicySettings) -> Result<Vec<ProxyNode>> {
    provider_nodes(secret, settings).await
}

pub async fn test_active(
    secret: &str,
    settings: &NetPolicySettings,
    name: &str,
) -> Result<ProxyNode> {
    let existing = provider_nodes(secret, settings)
        .await?
        .into_iter()
        .find(|node| node.name == name)
        .context("节点不属于当前激活订阅")?;
    let mut url = reqwest::Url::parse(&format!("http://{CONTROLLER}/"))
        .context("构造 mihomo controller 地址失败")?;
    url.set_path(&format!("/proxies/{}/delay", existing.name));
    url.query_pairs_mut()
        .append_pair("url", TEST_URL)
        .append_pair("timeout", "5000");
    let response = client(secret)?
        .get(url)
        .send()
        .await
        .context("发送节点测速请求失败")?
        .error_for_status()
        .context("mihomo 节点测速失败")?
        .json::<DelayResponse>()
        .await
        .context("解析节点测速响应失败")?;
    if response.delay == 0 {
        bail!("节点测速超时或不可达")
    }
    Ok(ProxyNode {
        delay_ms: Some(response.delay),
        alive: true,
        ..existing
    })
}
