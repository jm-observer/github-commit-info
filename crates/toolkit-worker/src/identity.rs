//! worker 身份与临时权限申请。
//!
//! **id 派生**：`w-<sm3(物理网卡 MAC 排序拼接 + 主机名)[..8]>`。要点是**稳定**：
//!
//! - 只取**物理网卡**：机器上常年挂着 VMware / Hyper-V vEthernet / VPN TUN 一堆虚拟网卡，
//!   它们重装即变，混进来 id 就不稳；
//! - **全部 MAC 排序后一起哈希**，而不是「取第一块」：插拔网线、连断 WiFi 都会改变枚举
//!   顺序，取第一块等于 id 随时会变；
//! - 混入主机名，避免同批机器网卡 MAC 高度相似时撞车；
//! - **算出来立刻写进 config.json，之后永远以配置为准**——所以派生只发生一次，
//!   后续换网卡也不会让 controller 上多出一台「新机器」。
//!
//! 拿不到任何物理 MAC（虚拟机/容器/权限受限）时退化为随机 uuid，同样落盘固化。
//!
//! **申请流程**（合并进 `run`，没有单独的子命令）：
//! `POST /api/internal/exec/access/request` → 每 10s `GET .../access/poll` → 批准后拿到
//! secret + 到期时间落盘 → 进主循环。凭据过期时同样走这条路续期，进程不退出。

use anyhow::{Context, Result};
use std::time::Duration;
use worker_core::proto::{ExecAccessPollResp, ExecAccessReq};

use crate::config::{StoredSecret, WorkerConfig};

/// 申请批准的轮询间隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// 取本机主机名（`COMPUTERNAME` / `HOSTNAME`；都没有则 `unknown-host`）。
pub fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown-host".to_string())
}

/// 本机 OS 标识，面板展示用。
pub fn os_name() -> &'static str {
    std::env::consts::OS
}

/// 判断网卡名是否是「虚拟网卡」——排除它们，见模块文档。名单按常见虚拟化/VPN 产品命名，
/// 宁可多排除（少一块物理网卡不影响稳定性，多一块虚拟网卡会破坏稳定性）。
fn is_virtual_nic(name: &str) -> bool {
    let n = name.to_lowercase();
    const NEEDLES: &[&str] = &[
        "vmware",
        "virtualbox",
        "vbox",
        "hyper-v",
        "vethernet",
        "loopback",
        "docker",
        "wsl",
        "tap",
        "tun",
        "wg",
        "awg",
        "wireguard",
        "zerotier",
        "tailscale",
        "bluetooth",
        "vpn",
        "meta", // Clash/TUN 常用适配器名
        "virtual",
    ];
    NEEDLES.iter().any(|k| n.contains(k))
}

/// 收集本机物理网卡的 MAC（去重 + 排序），拿不到则空。
fn physical_macs() -> Vec<String> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return vec![];
    };
    let mut macs: Vec<String> = ifaces
        .iter()
        .filter(|i| !i.is_loopback() && !is_virtual_nic(&i.name))
        .filter_map(|i| match mac_address::mac_address_by_name(&i.name) {
            Ok(Some(mac)) => Some(mac.to_string().replace(':', "").to_lowercase()),
            _ => None,
        })
        // 全零 MAC 是拿不到时的占位，没有区分度。
        .filter(|m| m.chars().any(|c| c != '0'))
        .collect();
    macs.sort();
    macs.dedup();
    macs
}

/// 从 MAC 集合 + 主机名派生稳定 id：`w-<sm3 前 8 字节 hex>`。
/// 纯函数，便于单测（不依赖真实网卡）。
pub fn derive_id_from(macs: &[String], hostname: &str) -> String {
    use sm3::{Digest, Sm3};
    let mut h = Sm3::new();
    for m in macs {
        h.update(m.as_bytes());
        h.update(b"|");
    }
    h.update(hostname.as_bytes());
    let out = h.finalize();
    let hex: String = out.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("w-{hex}")
}

/// 派生本机 id；拿不到任何物理 MAC 时退化为随机（同样会被上层固化进配置）。
pub fn derive_id() -> String {
    let macs = physical_macs();
    if macs.is_empty() {
        log::warn!("未找到可用的物理网卡 MAC,worker id 退化为随机值(已固化进配置,不影响后续稳定)");
        return format!("w-{}", uuid::Uuid::new_v4().simple());
    }
    derive_id_from(&macs, &hostname())
}

/// 确保配置里有 id 与 label；缺就补齐并回写。返回是否发生了改动。
pub fn ensure_identity(cfg: &mut WorkerConfig, label_override: Option<&str>) -> bool {
    let mut changed = false;
    if cfg.worker_id.trim().is_empty() {
        cfg.worker_id = derive_id();
        changed = true;
    }
    if let Some(l) = label_override {
        if !l.is_empty() && cfg.label != l {
            cfg.label = l.to_string();
            changed = true;
        }
    }
    if cfg.label.trim().is_empty() {
        cfg.label = hostname();
        changed = true;
    }
    changed
}

/// 提交一次申请。controller 返回 429 时说明待审批队列满了，属于可重试的软失败。
pub async fn submit_request(
    client: &reqwest::Client,
    controller: &str,
    cfg: &WorkerConfig,
) -> Result<()> {
    let body = ExecAccessReq {
        worker_id: cfg.worker_id.clone(),
        label: cfg.label.clone(),
        hostname: hostname(),
        os: os_name().to_string(),
    };
    let resp = client
        .post(format!("{controller}/api/internal/exec/access/request"))
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("提交权限申请失败(网络)")?;
    let status = resp.status().as_u16();
    match status {
        200 => Ok(()),
        429 => anyhow::bail!("controller 待审批队列已满,稍后重试"),
        other => anyhow::bail!("提交权限申请失败: HTTP {other}"),
    }
}

/// 轮询一次申请结果。
pub async fn poll_request(
    client: &reqwest::Client,
    controller: &str,
    worker_id: &str,
) -> Result<ExecAccessPollResp> {
    let resp = client
        .get(format!(
            "{controller}/api/internal/exec/access/poll?worker_id={worker_id}"
        ))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("查询申请结果失败(网络)")?;
    resp.json::<ExecAccessPollResp>()
        .await
        .context("解析申请结果失败")
}

/// 申请 → 等待批准的完整流程：阻塞到拿到凭据为止（Ctrl+C 可退出）。
///
/// 被拒 / 申请丢失时会重新提交，不退出——对方那台机器上跑的是个常驻进程，
/// 因为一次拒绝就退出会让「稍后再批准」变成「还得让人重新去开一次」。
pub async fn acquire_credential(
    client: &reqwest::Client,
    controller: &str,
    cfg: &WorkerConfig,
) -> Result<StoredSecret> {
    submit_request(client, controller, cfg).await.ok();
    log::info!(
        "已提交远程执行权限申请(worker_id={} label={});等待在 zero-desktop 面板批准…",
        cfg.worker_id,
        cfg.label
    );

    let mut announced = false;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        match poll_request(client, controller, &cfg.worker_id).await {
            Ok(r) => match r.state.as_str() {
                "approved" => {
                    let (Some(secret), Some(expires_at)) = (r.secret, r.expires_at) else {
                        log::warn!("controller 返回 approved 但没带 secret,重新申请");
                        let _ = submit_request(client, controller, cfg).await;
                        continue;
                    };
                    return Ok(StoredSecret {
                        secret,
                        expires_at: Some(expires_at),
                    });
                }
                "rejected" => {
                    log::warn!("申请被拒绝;{}s 后重新申请", POLL_INTERVAL.as_secs());
                    let _ = submit_request(client, controller, cfg).await;
                }
                // 申请被 TTL 清理 / controller 换库了 → 重新提交。
                "unknown" | "already_claimed" => {
                    let _ = submit_request(client, controller, cfg).await;
                }
                _ => {
                    if !announced {
                        log::info!("等待批准中…(每 {}s 查询一次)", POLL_INTERVAL.as_secs());
                        announced = true;
                    }
                }
            },
            Err(e) => log::warn!("查询申请结果失败: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_order_independent() {
        let a = derive_id_from(&["aabb".into(), "ccdd".into()], "PC-1");
        let b = derive_id_from(&["aabb".into(), "ccdd".into()], "PC-1");
        assert_eq!(a, b);
        assert!(a.starts_with("w-"));
        assert_eq!(a.len(), 2 + 16);
        // 主机名不同 → id 不同。
        assert_ne!(a, derive_id_from(&["aabb".into(), "ccdd".into()], "PC-2"));
        // MAC 集合不同 → id 不同。
        assert_ne!(a, derive_id_from(&["aabb".into()], "PC-1"));
    }

    #[test]
    fn virtual_nics_are_excluded() {
        for n in [
            "VMware Network Adapter VMnet1",
            "vEthernet (Default Switch)",
            "vEthernet (WSL (Hyper-V firewall))",
            "awg-client-52341",
            "Meta",
            "docker0",
            "tun0",
        ] {
            assert!(is_virtual_nic(n), "{n} 应被判为虚拟网卡");
        }
        for n in ["以太网", "WLAN", "eth0", "enp3s0", "Ethernet 2"] {
            assert!(!is_virtual_nic(n), "{n} 不应被判为虚拟网卡");
        }
    }

    #[test]
    fn ensure_identity_fills_and_is_idempotent() {
        let mut cfg = WorkerConfig::default();
        assert!(ensure_identity(&mut cfg, None));
        let id = cfg.worker_id.clone();
        assert!(!id.is_empty());
        assert!(!cfg.label.is_empty());
        // 再来一次不该改动。
        assert!(!ensure_identity(&mut cfg, None));
        assert_eq!(cfg.worker_id, id);
        // 显式 label 覆盖。
        assert!(ensure_identity(&mut cfg, Some("老王的机器")));
        assert_eq!(cfg.label, "老王的机器");
        assert_eq!(cfg.worker_id, id, "label 覆盖不应动 id");
    }
}
