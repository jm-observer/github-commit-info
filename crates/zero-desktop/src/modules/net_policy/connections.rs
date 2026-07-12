//! mihomo `/connections` 快照代理（P0-1，设计 §3.1/§5）。
//!
//! 通过运行中 mihomo 的 external-controller（`engine::CONTROLLER`，loopback）拉取**当前活跃连接**
//! 快照，带 controller secret 做 Bearer 鉴权（复用 apply 时落进 runtime 的 secret）。
//!
//! 用途：驱动全景图的「双分支聚合」（DIRECT / wg-out，按 `chains` 归类）与「当前活跃连接」活数据，
//! **非累计命中**——mihomo `/connections` 本就是瞬时快照（设计 §3.1 ⚠️）。
//!
//! 失败语义（设计要求）：mihomo 未跑 / secret 缺失 / 控制器不可达 → 返回**空快照**（非错误），
//! 让 UI 平滑降级为「灰节点 / 0 连接」，不打断 3s 快轮询。

use super::engine::CONTROLLER;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// 单条活跃连接（取 UI 驱动全景图所需的最小集）。
#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    /// mihomo 连接 ID（uuid）。观察器按它对「命中次数」去重——同一持续连接在 3s 轮询里
    /// 会反复出现，没有它每轮 +1，count 会虚高成"被观测到的轮数"。
    pub id: String,
    /// 出口链（mihomo `chains`，如 `["wg-out"]` / `["DIRECT"]`）。第 0 个是最终出口。
    pub chains: Vec<String>,
    /// 命中的出口（`chains` 末项归一化：含 `wg-out` → `wg-out`，否则 `DIRECT`）。
    pub outbound: String,
    /// 目标主机名（fake-ip 场景下为真实域名）。
    pub host: String,
    /// 目标 IP。
    pub destination_ip: String,
    /// 目标端口。
    pub destination_port: String,
    /// 发起进程名（mihomo `find-process-mode: always` 时可得）。
    pub process: String,
    /// 命中的 mihomo 规则（如 `MATCH` / `IP-CIDR`）。
    pub rule: String,
    /// 网络类型（tcp/udp）。
    pub network: String,
}

/// 活跃连接快照 + 按出口聚合（设计 §3.1：双分支计数 = 当前连接按 `chains` 聚合，准确可做）。
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionsSnapshot {
    /// mihomo 控制器是否可达并成功取到快照。false 时 `connections` 为空、计数全 0。
    pub available: bool,
    /// 活跃连接总数。
    pub total: usize,
    /// 走 wg-out 海外隧道的活跃连接数。
    pub wg_count: usize,
    /// 走 DIRECT 本地直连的活跃连接数。
    pub direct_count: usize,
    /// 其它出口（非 wg-out / 非 DIRECT，理论上不应出现）的活跃连接数。
    pub other_count: usize,
    /// 按发起进程聚合的连接数（进程名 → 计数），便于 UI 显示「本机应用」节点。
    pub by_process: BTreeMap<String, usize>,
    /// 连接明细（截断到合理上限，避免大快照拖垮 IPC）。
    pub connections: Vec<Connection>,
}

impl ConnectionsSnapshot {
    /// 空快照（非 Windows 平台直接返回，命令层用）。
    pub fn empty_snapshot() -> Self {
        Self::empty()
    }

    /// 空快照（mihomo 未跑 / secret 缺失 / 控制器不可达时返回）。
    fn empty() -> Self {
        Self {
            available: false,
            total: 0,
            wg_count: 0,
            direct_count: 0,
            other_count: 0,
            by_process: BTreeMap::new(),
            connections: Vec::new(),
        }
    }
}

/// mihomo `/connections` 原始响应（仅解析需要的字段，其余忽略）。
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
    #[serde(default)]
    network: String,
}

/// 上限：明细只回前 N 条（聚合计数仍基于全量），防超大快照。
const MAX_DETAIL: usize = 200;

/// 把 `chains` 归一化到出口分类：含 `wg-out` → `wg-out`；含 `DIRECT` → `DIRECT`；否则 `other`。
fn classify(chains: &[String]) -> &'static str {
    if chains.iter().any(|c| c.eq_ignore_ascii_case("wg-out")) {
        "wg-out"
    } else if chains.iter().any(|c| c.eq_ignore_ascii_case("DIRECT")) {
        "DIRECT"
    } else {
        "other"
    }
}

/// 拉取活跃连接快照。`secret` 为 controller 鉴权口令（空则不带头）。
/// 失败（控制器不可达 / 鉴权失败 / 解析失败）一律返回空快照，不报错（设计要求）。
pub async fn fetch(secret: &str) -> ConnectionsSnapshot {
    match fetch_inner(secret).await {
        Ok(snap) => snap,
        Err(_) => ConnectionsSnapshot::empty(),
    }
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
        let outbound = classify(&rc.chains);
        match outbound {
            "wg-out" => snap.wg_count += 1,
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
                rule: rc.rule,
                network: rc.metadata.network,
            });
        }
    }
    snap.total = snap.wg_count + snap.direct_count + snap.other_count;
    Ok(snap)
}
