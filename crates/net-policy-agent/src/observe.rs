//! 可观测：被阻断尝试 feed（消费 mihomo `/logs`）+ 域名↔IP/进程 关联（累积 `/connections`）。

use anyhow::Result;
use futures_util::StreamExt;
use net_policy_core::mihomo::CONTROLLER;
use net_policy_core::types::{BlockedEntry, Connection, DomainAssoc};
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::Message;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

const BLOCKED_CAP: usize = 200;
const DNS_CAP: usize = 600;
const SEEN_IDS_CAP: usize = 4096;

#[derive(Clone, Default)]
struct DomainAgg {
    ips: BTreeSet<String>,
    processes: BTreeSet<String>,
    count: u64,
    last_ms: u64,
}

/// 模块级可观测状态（内部 Mutex，跨连接共享，挂在 AgentState）。
#[derive(Default)]
pub struct Observatory {
    blocked: Mutex<Vec<BlockedEntry>>,
    dns: Mutex<HashMap<String, DomainAgg>>,
    seen_ids: Mutex<(VecDeque<String>, HashSet<String>)>,
}

impl Observatory {
    fn key(e: &BlockedEntry) -> (String, String, String) {
        (e.network.clone(), e.host.clone(), e.dest_port.clone())
    }

    pub fn record_blocked(&self, e: BlockedEntry) {
        let mut v = self.blocked.lock().unwrap();
        let k = Self::key(&e);
        if let Some(found) = v.iter_mut().find(|x| Self::key(x) == k) {
            found.count += 1;
            found.last_ms = e.last_ms;
            if found.dest_ip.is_empty() && !e.dest_ip.is_empty() {
                found.dest_ip = e.dest_ip;
            }
            return;
        }
        v.push(e);
        if v.len() > BLOCKED_CAP {
            let n = v.len() - BLOCKED_CAP;
            v.drain(0..n);
        }
    }

    pub fn blocked_snapshot(&self) -> Vec<BlockedEntry> {
        let mut out = self.blocked.lock().unwrap().clone();
        out.sort_by_key(|entry| Reverse(entry.last_ms));
        out
    }

    pub fn clear_blocked(&self) {
        self.blocked.lock().unwrap().clear();
    }

    /// 从一次 `/connections` 快照累积「域名↔IP/进程」关联。count 按连接 ID 去重。
    pub fn ingest_connections(&self, conns: &[Connection]) {
        let ts = now_ms();
        let mut seen = self.seen_ids.lock().unwrap();
        let mut m = self.dns.lock().unwrap();
        for c in conns {
            if c.host.trim().is_empty() {
                continue;
            }
            let agg = m.entry(c.host.clone()).or_default();
            if !c.destination_ip.trim().is_empty() {
                agg.ips.insert(c.destination_ip.clone());
            }
            if !c.process.trim().is_empty() {
                agg.processes.insert(c.process.clone());
            }
            let is_new = c.id.is_empty() || !seen.1.contains(&c.id);
            if is_new {
                agg.count += 1;
                if !c.id.is_empty() {
                    seen.0.push_back(c.id.clone());
                    seen.1.insert(c.id.clone());
                    while seen.0.len() > SEEN_IDS_CAP {
                        if let Some(old) = seen.0.pop_front() {
                            seen.1.remove(&old);
                        }
                    }
                }
            }
            agg.last_ms = ts;
        }
        if m.len() > DNS_CAP {
            let mut by_age: Vec<(String, u64)> =
                m.iter().map(|(k, v)| (k.clone(), v.last_ms)).collect();
            by_age.sort_by_key(|(_, t)| *t);
            let remove = m.len() - DNS_CAP;
            for (k, _) in by_age.into_iter().take(remove) {
                m.remove(&k);
            }
        }
    }

    pub fn dns_snapshot(&self) -> Vec<DomainAssoc> {
        let m = self.dns.lock().unwrap();
        let mut out: Vec<DomainAssoc> = m
            .iter()
            .map(|(k, v)| DomainAssoc {
                domain: k.clone(),
                ips: v.ips.iter().cloned().collect(),
                processes: v.processes.iter().cloned().collect(),
                count: v.count,
                last_ms: v.last_ms,
            })
            .collect();
        out.sort_by_key(|entry| Reverse(entry.last_ms));
        out
    }
}

/// 解析 mihomo 一条 info 日志 payload。命中 `REJECT*` 出口的转成 `BlockedEntry`，否则 None。
pub fn parse_blocked(payload: &str) -> Option<BlockedEntry> {
    let network = payload
        .strip_prefix('[')?
        .split(']')
        .next()?
        .trim()
        .to_ascii_lowercase();
    let outbound = payload
        .rsplit_once(" using ")?
        .1
        .split_whitespace()
        .next()?
        .to_string();
    if !outbound.to_ascii_uppercase().contains("REJECT") {
        return None;
    }
    let remote = payload
        .split(" --> ")
        .nth(1)?
        .split(" match ")
        .next()?
        .trim();
    let (host, port) = remote.rsplit_once(':')?;
    let host = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let rule = payload
        .split(" match ")
        .nth(1)?
        .split(" using ")
        .next()?
        .trim()
        .to_string();
    let dest_ip = if host.parse::<std::net::IpAddr>().is_ok() {
        host.clone()
    } else {
        String::new()
    };
    Some(BlockedEntry {
        network,
        host,
        dest_ip,
        dest_port: port.trim().to_string(),
        rule,
        outbound,
        count: 1,
        last_ms: now_ms(),
    })
}

/// 连一次 mihomo `/logs` WebSocket，把被阻断的行喂进 `obs`。WS 关闭/出错即正常返回（调用方重连）。
pub async fn stream_logs(secret: &str, obs: &Observatory) -> Result<()> {
    let url = format!("ws://{CONTROLLER}/logs?level=info&token={secret}");
    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    let (_w, mut r) = ws.split();
    while let Some(msg) = r.next().await {
        let Message::Text(t) = msg? else { continue };
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            if let Some(p) = v.get("payload").and_then(|x| x.as_str()) {
                if let Some(e) = parse_blocked(p) {
                    obs.record_blocked(e);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reject_line() {
        let e = parse_blocked(
            "[TCP] 192.168.1.5:54321 --> ads.example.com:443 match DomainSuffix(example.com) using REJECT-DROP",
        )
        .expect("should parse");
        assert_eq!(e.network, "tcp");
        assert_eq!(e.host, "ads.example.com");
        assert_eq!(e.dest_port, "443");
        assert_eq!(e.outbound, "REJECT-DROP");
        assert!(e.dest_ip.is_empty());
    }

    #[test]
    fn skips_non_reject() {
        assert!(parse_blocked(
            "[TCP] 1.1.1.1:2 --> example.com:443 match DomainSuffix(x) using DIRECT"
        )
        .is_none());
    }
}
