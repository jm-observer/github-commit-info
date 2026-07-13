//! net-policy：救援 CLI（设计文档 §7 两级救援的**在线**层）。
//!
//! 复用 `net-policy-client` 打同一命名管道；GUI 挂掉时的安全出口（无需浏览器）。
//! - `status`  读真实机器状态（agent 以真实态为准对齐，不谎报）。
//! - `stop`    优雅停引擎 + 撤防火墙（= 拆策略）。
//! - `repair`  在线：连得上 agent 就 stop 回基线；**连不上则引导用离线提权救援**
//!            `net-policy-agent repair-offline`（agent 起不来/防火墙残留时的唯一出口）。
//!
//! 输出单行 JSON（脚本友好，与本仓 CLI 约定一致）。

use anyhow::Result;
use clap::{Parser, Subcommand};
use net_policy_client::Client;
use net_policy_core::config::ProcessRef;

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
    /// 查生效路由（含优先级/来源）。
    Routes,
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
    }
    Ok(())
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
