//! mihomo `/connections` 快照代理（返回 `net_policy_core::types::ConnectionsSnapshot`）。
//!
//! 失败语义：mihomo 未跑 / secret 缺失 / 控制器不可达 → 返回**空快照**（非错误），UI 平滑降级。

use net_policy_core::mihomo::CONTROLLER;
use net_policy_core::types::{Connection, ConnectionsSnapshot};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct RawConnections {
    #[serde(default)]
    connections: Vec<RawConnection>,
}

#[derive(Debug, Deserialize)]
struct RawConnection {
    #[serde(default)]
    id: String,
    #[serde(default)]
    chains: Vec<String>,
    #[serde(default)]
    rule: String,
    #[serde(default)]
    metadata: RawMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct RawMetadata {
    #[serde(default)]
    host: String,
    #[serde(rename = "destinationIP", default)]
    destination_ip: String,
    #[serde(rename = "destinationPort", default)]
    destination_port: String,
    #[serde(default)]
    process: String,
    #[serde(rename = "processPath", default)]
    process_path: String,
    #[serde(default)]
    network: String,
}

/// 上限：明细只回前 N 条（聚合计数仍基于全量），防超大快照。
const MAX_DETAIL: usize = 200;

fn classify(chains: &[String]) -> &'static str {
    if chains.iter().any(|c| c.eq_ignore_ascii_case("wg-out")) {
        "wg-out"
    } else if chains
        .iter()
        .any(|c| c.eq_ignore_ascii_case("subscription-out"))
    {
        "subscription-out"
    } else if chains.iter().any(|c| c.eq_ignore_ascii_case("DIRECT")) {
        "DIRECT"
    } else {
        "other"
    }
}

fn is_mihomo_process(name: &str, path: &str) -> bool {
    let process = name.trim().trim_matches('"');
    if process.eq_ignore_ascii_case("mihomo") || process.eq_ignore_ascii_case("mihomo.exe") {
        return true;
    }
    path.rsplit(['\\', '/']).next().is_some_and(|file| {
        file.eq_ignore_ascii_case("mihomo.exe") || file.eq_ignore_ascii_case("mihomo")
    })
}

/// 拉取活跃连接快照。失败一律返回空快照，不报错。
pub async fn fetch(secret: &str) -> ConnectionsSnapshot {
    fetch_inner(secret)
        .await
        .unwrap_or_else(|_| ConnectionsSnapshot::empty())
}

async fn fetch_inner(secret: &str) -> anyhow::Result<ConnectionsSnapshot> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let mut req = client.get(format!("http://{CONTROLLER}/connections"));
    if !secret.is_empty() {
        req = req.bearer_auth(secret);
    }
    let resp = req.send().await?.error_for_status()?;
    let raw: RawConnections = resp.json().await?;

    let mut snap = ConnectionsSnapshot::empty();
    snap.available = true;
    for rc in raw.connections {
        if is_mihomo_process(&rc.metadata.process, &rc.metadata.process_path) {
            continue;
        }
        let outbound = classify(&rc.chains);
        match outbound {
            "wg-out" => snap.wg_count += 1,
            "subscription-out" => snap.proxy_count += 1,
            "DIRECT" => snap.direct_count += 1,
            _ => snap.other_count += 1,
        }
        let process = if rc.metadata.process.is_empty() {
            "(unknown)".to_string()
        } else {
            rc.metadata.process.clone()
        };
        *snap.by_process.entry(process.clone()).or_insert(0) += 1;
        if snap.connections.len() < MAX_DETAIL {
            snap.connections.push(Connection {
                id: rc.id,
                chains: rc.chains,
                outbound: outbound.to_string(),
                host: rc.metadata.host,
                destination_ip: rc.metadata.destination_ip,
                destination_port: rc.metadata.destination_port,
                process,
                process_path: rc.metadata.process_path,
                rule: rc.rule,
                network: rc.metadata.network,
            });
        }
    }
    snap.total = snap.wg_count + snap.proxy_count + snap.direct_count + snap.other_count;
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::{classify, is_mihomo_process};

    #[test]
    fn classify_subscription_out_separately() {
        assert_eq!(
            classify(&["node-a".into(), "subscription-out".into()]),
            "subscription-out"
        );
    }

    #[test]
    fn mihomo_self_connections_are_filtered() {
        assert!(is_mihomo_process("mihomo", ""));
        assert!(is_mihomo_process(
            "",
            r"C:\Program Files\net-policy\mihomo.exe"
        ));
        assert!(!is_mihomo_process(
            "chrome.exe",
            r"C:\Program Files\Google\Chrome\chrome.exe"
        ));
    }
}
