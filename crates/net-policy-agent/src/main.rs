//! net-policy-agent：每用户特权网络代理（唯一副作用所有者，设计文档 §2/§3）。
//!
//! 子命令：
//! - `install`   注册登录触发的最高权限计划任务 + 复制 exe 到受保护目录（§5/§5.3）。
//! - `uninstall` 注销计划任务。
//! - `run`       前台运行：装配状态 → setup（接管/自动恢复 + 采集器）→ 启动管道 server。
//! - `repair-offline` 离线提权救援（§7 两级救援）。

mod connections;
mod engine;
mod firewall;
mod frame;
mod install;
mod observe;
mod ops;
mod paths;
mod proc;
mod process_watch;
mod ptree;
mod repair;
mod security;
mod server;
mod state;
mod store;
mod verify;
mod win;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "net-policy-agent", about = "每用户特权网络策略代理")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 注册登录自启的最高权限计划任务 + 原子化安装可执行资产（须管理员）。
    Install {
        /// mihomo 源路径（提权进程复制到 %ProgramFiles%\net-policy\；缺则要求受保护目录已有）。
        #[arg(long)]
        mihomo: Option<String>,
        /// wintun.dll 源路径（可选；gvisor 栈通常内置）。
        #[arg(long)]
        wintun: Option<String>,
    },
    /// 注销计划任务（须管理员）。
    Uninstall,
    /// 前台运行守护（装配 + 管道 server）。
    Run {
        /// 开发模式：允许 MIHOMO_BIN 覆盖、跳过安装目录严格校验（不得与已安装任务共存）。
        #[arg(long)]
        dev: bool,
        /// workspace 数据目录（默认 %LOCALAPPDATA%\net-policy）。
        #[arg(short = 'w', long)]
        workspace: Option<String>,
    },
    /// 离线提权救援：只清本产品防火墙规则并按快照恢复 Profile（agent 连不上时用）。
    RepairOffline {
        /// 无快照时也强设 DefaultOutboundAction=NotConfigured（最后手段）。
        #[arg(long)]
        force: bool,
        #[arg(short = 'w', long)]
        workspace: Option<String>,
    },
}

fn main() -> Result<()> {
    init_log();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Install { mihomo, wintun } => install::install(mihomo.as_deref(), wintun.as_deref()),
        Cmd::Uninstall => install::uninstall(),
        Cmd::RepairOffline { force, workspace } => {
            let ws = paths::workspace_dir(workspace.as_deref());
            repair::repair_offline(&ws, force)
        }
        Cmd::Run { dev, workspace } => run(dev, workspace),
    }
}

/// `run` 需要 tokio 运行时；install/uninstall/repair 是同步（PowerShell），不必进异步。
fn run(dev: bool, workspace: Option<String>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let ws = paths::workspace_dir(workspace.as_deref());
        log::info!("workspace = {}", ws.display());
        if !win::is_windows() {
            anyhow::bail!("net-policy-agent 仅支持 Windows");
        }
        if !win::is_elevated() {
            log::warn!("未以管理员运行：apply/停止等改防火墙/建 TUN 的操作会被拒（仅可读状态）");
        }
        if let Some(w) = server::preflight_single_instance() {
            log::warn!("单实例预检：{w}");
        }
        let state = Arc::new(state::AgentState::new(ws, dev));
        state::setup(state.clone());
        // 记录关闭时间（Ctrl-C / 任务停止的优雅信号；best-effort）。
        {
            let st = state.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    st.record_event("agent_stop", "signal");
                    std::process::exit(0);
                }
            });
        }
        server::serve(state).await
    })
}

/// 日志走 custom-utils（AGENTS.md 约定）：dev 输出控制台；prod（`--features prod`）写
/// `{home}/log/net-policy-agent` 文件、stdout 保持干净（install/repair 的 JSON 不被污染）。
fn init_log() {
    let _ = custom_utils::logger::logger_feature(
        "net-policy-agent",
        "info,reqwest=warn",
        log::LevelFilter::Info,
        false,
    )
    .build();
}
