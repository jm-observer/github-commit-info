//! net-policy：救援 CLI（设计文档 §7 两级救援的**在线**层）。
//!
//! 复用 `net-policy-client` 打同一命名管道；GUI 挂掉时的安全出口（无需浏览器）。
//! - `status`  读真实机器状态（agent 以真实态为准对齐，不谎报）。
//! - `stop`    优雅停引擎 + 撤防火墙（= 拆策略）。
//! - `repair`  在线：连得上 agent 就 stop 回基线；**连不上则引导用离线提权救援**
//!   `net-policy-agent repair-offline`（agent 起不来/防火墙残留时的唯一出口）。
//!
//! 输出单行 JSON（脚本友好，与本仓 CLI 约定一致）。

use anyhow::Result;
use clap::{Parser, Subcommand};
use net_policy_client::Client;
use net_policy_core::capture::{CaptureOpts, CaptureTarget};
use net_policy_core::config::{ProcessRef, Route};
use net_policy_core::decrypt::{
    DecryptArtifact, DecryptOpts, DecryptTarget, ProcessInstanceRef, RedactProfile,
};

#[derive(Parser)]
#[command(name = "net-policy", about = "网络策略救援 CLI（在线，打 agent 管道）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 读当前真实状态。
    Status,
    /// 停止策略（优雅停 + 撤防火墙）。
    Stop,
    /// 在线修复防火墙残留（分级）；连不上引导离线提权救援。
    Repair {
        /// 无快照时也强设 NotConfigured（最后手段）。
        #[arg(long)]
        force: bool,
    },
    /// 查历史进程请求记录。
    Requests {
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// 查生命周期事件（启停 / 策略 / 临时直连）。
    Events {
        #[arg(long, default_value_t = 200)]
        limit: u32,
    },
    /// 查生效路由（含优先级/来源/实际出口）。
    Routes,
    /// 列出所有出口及其生命周期（与流量策略分离的两个维度）。
    Egresses,
    /// 启动出口（只改出口生命周期，不改导流规则）。
    EgressStart { id: String },
    /// 停止出口（从配置摘除；指向它的规则按 fallback 处理。不是「停止管控」）。
    EgressStop { id: String },
    /// 立即重连某个出口（重置其存量连接后重新探测）。
    EgressReconnect { id: String },
    /// 仅测试某个出口的连通性（不改生命周期，不改导流）。
    EgressProbe { id: String },
    /// 设置出口不可用时的行为：`block`（默认，fail-closed）或 `direct`。
    EgressFallback { id: String, fallback: String },
    /// 刷新代理订阅（只刷配置来源，不重连节点）。
    EgressRefreshSub {
        #[arg(default_value = "proxy")]
        id: String,
    },
    /// 切换代理订阅当前节点。
    EgressSelectNode {
        node: String,
        #[arg(default_value = "proxy")]
        id: String,
    },
    /// 查进程树。
    Tree,
    /// 临时直连状态。
    TempStatus,
    /// 开启临时直连（限时应急）。
    TempOn {
        /// 持续秒数。
        #[arg(long, default_value_t = 300)]
        secs: u64,
        /// 例外进程名（这些不走直连，被强制 Blackhole）；可多次。
        #[arg(long = "except")]
        except: Vec<String>,
    },
    /// 解除临时直连。
    TempOff,
    /// 清空请求记录（隐私）。
    ClearRequests,
    /// 清空生命周期事件。
    ClearEvents,

    /// 设置默认出口姿态（direct=直连观察 / wg=海外 / blackhole=阻断）并持久化。
    SetRoute {
        #[arg(value_parser = ["direct", "wg", "blackhole"])]
        route: String,
    },
    /// 应用当前策略（起 mihomo + TUN；直连观察姿态不阻断流量）。
    Apply,
    /// 开始全 TUN 抓包（返回会话；到时 agent 自动停）。
    CaptureStart {
        #[arg(long, default_value_t = 30)]
        secs: u64,
        #[arg(long, default_value_t = 128)]
        snap_len: u32,
        #[arg(long, default_value_t = 128)]
        file_size_mib: u32,
    },
    /// 列出抓包会话。
    CaptureList,
    /// 停止抓包会话。
    CaptureStop { id: String },
    /// 保存 done 会话 pcapng 到本地路径（分块 CaptureRead → 写文件）。
    CaptureSave { id: String, dest: String },

    // ── L4 应用明文（Decrypt*，需 agent decrypt_v1 解锁：NET_POLICY_DECRYPT_ENABLE=1）──
    /// 查 CA 信任状态。
    DecryptCaStatus,
    /// 生成专用调试 CA（agent 侧 + DPAPI 私钥保护；不装信任库）。
    DecryptCaCreate,
    /// 导出 CA 公钥证书 PEM 到文件（供 certutil 装 CurrentUser\Root）。
    DecryptCaExport { dest: String },
    /// GUI/手动装信任库后，把指纹 + owner SID 交 agent 复核。
    DecryptCaConfirm {
        thumbprint: String,
        owner_sid: String,
    },
    /// 移除本产品 CA（agent 侧文件 + 记录）。
    DecryptCaRemove,
    /// 开始解密会话（精确进程实例 + 必填域名 allowlist）。
    DecryptStart {
        #[arg(long)]
        pid: u32,
        /// 目标进程完整路径（PROCESS-PATH 匹配 + agent 重读校验）。
        #[arg(long)]
        path: String,
        /// allowlist 域名，逗号分隔（如 example.com,api.example.com）。
        #[arg(long)]
        domains: String,
        #[arg(long, default_value_t = 60)]
        secs: u64,
        /// 采集正文（默认只记方法/URL/状态/头）。
        #[arg(long)]
        bodies: bool,
        /// 逼 QUIC 回退 TCP（阻目标进程+域名 UDP/443）。
        #[arg(long = "force-tcp")]
        force_tcp: bool,
        /// Raw 原文模式（不脱敏；高敏感）。
        #[arg(long)]
        raw: bool,
    },
    /// 列出解密会话（含每域名计数）。
    DecryptList,
    /// 取单个会话当前态。
    DecryptGet { id: String },
    /// 停止解密会话。
    DecryptStop { id: String },
    /// 删除会话。
    DecryptDelete { id: String },
    /// 保存 done 会话产物到本地（artifact = http-jsonl | manifest）。
    DecryptSave {
        id: String,
        #[arg(value_parser = ["http-jsonl", "manifest"])]
        artifact: String,
        dest: String,
    },
}

fn print_json(v: serde_json::Value) {
    println!("{v}");
}

#[tokio::main]
async fn main() -> Result<()> {
    // 日志走 custom-utils（AGENTS.md）；stdout 仅一行紧凑 JSON。
    let _ =
        custom_utils::logger::logger_feature("net-policy", "warn", log::LevelFilter::Warn, false)
            .build();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Status => match Client::connect().await {
            Ok(mut c) => match c.status().await {
                Ok(s) => print_json(serde_json::to_value(s)?),
                Err(e) => print_json(
                    serde_json::json!({"error": format!("{e:#}"), "error_kind": "request_failed"}),
                ),
            },
            Err(e) => print_json(serde_json::json!({
                "error": format!("连不上 net-policy-agent：{e:#}"),
                "error_kind": "agent_unreachable",
                "hint": "确认 net-policy-agent 已安装并在运行（net-policy-agent install / run）"
            })),
        },
        Cmd::Stop => match Client::connect().await {
            Ok(mut c) => match c.stop().await {
                Ok(s) => print_json(serde_json::json!({"result": "stopped", "status": s})),
                Err(e) => print_json(
                    serde_json::json!({"error": format!("{e:#}"), "error_kind": "request_failed"}),
                ),
            },
            Err(e) => print_json(serde_json::json!({
                "error": format!("连不上 net-policy-agent：{e:#}"),
                "error_kind": "agent_unreachable",
                "hint": "若防火墙残留导致断网，请以管理员运行：net-policy-agent repair-offline"
            })),
        },
        // 在线修复：走 agent 的分级修复（不等价于 stop，评审点 5）；连不上引导离线提权救援。
        Cmd::Repair { force } => match Client::connect().await {
            Ok(mut c) => match c.repair(force).await {
                Ok(r) => print_json(serde_json::json!({"result": "repaired_online", "repair": r})),
                Err(e) => print_json(serde_json::json!({
                    "error": format!("{e:#}"),
                    "error_kind": "request_failed",
                    "hint": "在线修复失败，可尝试以管理员运行：net-policy-agent repair-offline"
                })),
            },
            Err(e) => print_json(serde_json::json!({
                "result": "offline_repair_required",
                "message": format!("连不上 agent（{e:#}）——这正是最需要离线救援的场景"),
                "action": "请以管理员运行：net-policy-agent repair-offline（无快照且仍断网时加 --force）"
            })),
        },
        Cmd::Requests { limit } => simple(|mut c| async move { c.requests(limit).await }).await?,
        Cmd::Events { limit } => simple(|mut c| async move { c.events(limit).await }).await?,
        Cmd::Routes => simple(|mut c| async move { c.routes().await }).await?,
        Cmd::Egresses => simple(|mut c| async move { c.egress_list().await }).await?,
        Cmd::EgressStart { id } => simple(|mut c| async move { c.egress_start(id).await }).await?,
        Cmd::EgressStop { id } => simple(|mut c| async move { c.egress_stop(id).await }).await?,
        Cmd::EgressReconnect { id } => {
            simple(|mut c| async move { c.egress_reconnect(id).await }).await?
        }
        Cmd::EgressProbe { id } => simple(|mut c| async move { c.egress_probe(id).await }).await?,
        Cmd::EgressFallback { id, fallback } => {
            let fb = match fallback.as_str() {
                "block" => net_policy_core::egress::EgressFallback::Block,
                "direct" => net_policy_core::egress::EgressFallback::Direct,
                other => anyhow::bail!("未知 fallback：{other}（可选 block / direct）"),
            };
            simple(|mut c| async move { c.egress_set_fallback(id, fb).await }).await?
        }
        Cmd::EgressRefreshSub { id } => {
            simple(|mut c| async move { c.egress_refresh_subscription(id).await }).await?
        }
        Cmd::EgressSelectNode { node, id } => {
            simple(|mut c| async move { c.egress_select_node(id, node).await }).await?
        }
        Cmd::Tree => simple(|mut c| async move { c.process_tree().await }).await?,
        Cmd::TempStatus => simple(|mut c| async move { c.temp_direct().await }).await?,
        Cmd::TempOn { secs, except } => {
            let except: Vec<ProcessRef> = except.into_iter().map(ProcessRef::ProcessName).collect();
            simple(|mut c| async move { c.set_temp_direct(secs, except).await }).await?
        }
        Cmd::TempOff => simple(|mut c| async move { c.clear_temp_direct().await }).await?,
        Cmd::ClearRequests => {
            simple(|mut c| async move { c.clear_requests().await.map(|_| "cleared") }).await?
        }
        Cmd::ClearEvents => {
            simple(|mut c| async move { c.clear_events().await.map(|_| "cleared") }).await?
        }

        Cmd::SetRoute { route } => {
            let route = match route.as_str() {
                "direct" => Route::Direct,
                "wg" => Route::Wg,
                _ => Route::Blackhole,
            };
            simple(|mut c| async move {
                let mut s = c.get_settings().await?;
                s.default_route = route;
                c.save_settings(s).await?;
                anyhow::Ok(serde_json::json!({"result": "route_saved", "route": route}))
            })
            .await?
        }
        Cmd::Apply => {
            simple(|mut c| async move {
                c.apply()
                    .await
                    .map(|s| serde_json::json!({"result": "applied", "status": s}))
            })
            .await?
        }
        Cmd::CaptureStart {
            secs,
            snap_len,
            file_size_mib,
        } => {
            let opts = CaptureOpts {
                snap_len,
                file_size_mib,
                max_secs: secs,
            };
            simple(|mut c| async move { c.capture_start(CaptureTarget::All, opts).await }).await?
        }
        Cmd::CaptureList => simple(|mut c| async move { c.capture_list().await }).await?,
        Cmd::CaptureStop { id } => simple(|mut c| async move { c.capture_stop(id).await }).await?,
        Cmd::CaptureSave { id, dest } => {
            simple(|c| async move { capture_save(c, id, dest).await }).await?
        }

        // ── L4 应用明文 ──
        Cmd::DecryptCaStatus => simple(|mut c| async move { c.decrypt_ca_status().await }).await?,
        Cmd::DecryptCaCreate => simple(|mut c| async move { c.decrypt_ca_create().await }).await?,
        Cmd::DecryptCaExport { dest } => {
            simple(|mut c| async move {
                let pem = c.decrypt_ca_export_public().await?;
                std::fs::write(&dest, pem.as_bytes())?;
                anyhow::Ok(
                    serde_json::json!({"result": "exported", "dest": dest, "bytes": pem.len()}),
                )
            })
            .await?
        }
        Cmd::DecryptCaConfirm {
            thumbprint,
            owner_sid,
        } => {
            simple(|mut c| async move { c.decrypt_ca_confirm(thumbprint, owner_sid).await }).await?
        }
        Cmd::DecryptCaRemove => simple(|mut c| async move { c.decrypt_ca_remove().await }).await?,
        Cmd::DecryptStart {
            pid,
            path,
            domains,
            secs,
            bodies,
            force_tcp,
            raw,
        } => {
            let domains: Vec<String> = domains
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            // created_at_100ns=0：agent 按 pid 重读进程创建时间/路径（防 PID 复用，§17.5）。
            let target = DecryptTarget {
                process: ProcessInstanceRef {
                    pid,
                    created_at_100ns: 0,
                    path,
                },
                domains,
            };
            let opts = DecryptOpts {
                max_secs: secs,
                capture_bodies: bodies,
                force_tcp_for_quic: force_tcp,
                redact_profile: if raw {
                    RedactProfile::Raw
                } else {
                    RedactProfile::Default
                },
                ..DecryptOpts::default()
            };
            simple(|mut c| async move { c.decrypt_start(target, opts).await }).await?
        }
        Cmd::DecryptList => simple(|mut c| async move { c.decrypt_list().await }).await?,
        Cmd::DecryptGet { id } => simple(|mut c| async move { c.decrypt_get(id).await }).await?,
        Cmd::DecryptStop { id } => simple(|mut c| async move { c.decrypt_stop(id).await }).await?,
        Cmd::DecryptDelete { id } => {
            simple(|mut c| async move { c.decrypt_delete(id).await.map(|_| "deleted") }).await?
        }
        Cmd::DecryptSave { id, artifact, dest } => {
            let art = if artifact == "manifest" {
                DecryptArtifact::Manifest
            } else {
                DecryptArtifact::HttpJsonl
            };
            simple(|c| async move { decrypt_save(c, id, art, dest).await }).await?
        }
    }
    Ok(())
}

/// 分块拉取 done 会话产物落地到 `dest`（DecryptRead → base64 解码 → 追加写）。
async fn decrypt_save(
    mut c: Client,
    id: String,
    artifact: DecryptArtifact,
    dest: String,
) -> anyhow::Result<serde_json::Value> {
    use base64::Engine;
    use std::io::Write;
    let mut f = std::fs::File::create(&dest)?;
    let mut offset = 0u64;
    let mut total = 0u64;
    loop {
        let (_off, data_b64, eof) = c
            .decrypt_read(id.clone(), artifact, offset, 512 * 1024)
            .await?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64.as_bytes())?;
        f.write_all(&bytes)?;
        offset += bytes.len() as u64;
        total += bytes.len() as u64;
        if eof || bytes.is_empty() {
            break;
        }
    }
    f.flush()?;
    Ok(serde_json::json!({"result": "saved", "dest": dest, "bytes": total}))
}

/// 分块拉取 done 会话 pcapng 落地到 `dest`（CaptureRead → base64 解码 → 追加写）。
async fn capture_save(
    mut c: Client,
    id: String,
    dest: String,
) -> anyhow::Result<serde_json::Value> {
    use base64::Engine;
    use std::io::Write;
    let mut f = std::fs::File::create(&dest)?;
    let mut offset = 0u64;
    let mut total = 0u64;
    loop {
        let (_off, data_b64, eof) = c.capture_read(id.clone(), offset, 512 * 1024).await?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data_b64.as_bytes())?;
        f.write_all(&bytes)?;
        offset += bytes.len() as u64;
        total += bytes.len() as u64;
        if eof || bytes.is_empty() {
            break;
        }
    }
    f.flush()?;
    Ok(serde_json::json!({"result": "saved", "dest": dest, "bytes": total}))
}

/// 连接 agent → 跑闭包 → 打印 JSON（连不上/请求失败都输出结构化错误）。
async fn simple<T, F, Fut>(f: F) -> Result<()>
where
    T: serde::Serialize,
    F: FnOnce(Client) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    match Client::connect().await {
        Ok(c) => match f(c).await {
            Ok(v) => print_json(serde_json::to_value(v)?),
            Err(e) => print_json(
                serde_json::json!({"error": format!("{e:#}"), "error_kind": "request_failed"}),
            ),
        },
        Err(e) => print_json(serde_json::json!({
            "error": format!("连不上 net-policy-agent：{e:#}"),
            "error_kind": "agent_unreachable"
        })),
    }
    Ok(())
}
