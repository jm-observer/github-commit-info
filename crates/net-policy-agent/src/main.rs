//! net-policy-agent：每用户特权网络代理（唯一副作用所有者，设计文档 §2/§3）。
//!
//! 子命令：
//! - `install`   注册登录触发的最高权限计划任务 + 复制 exe 到受保护目录（§5/§5.3）。
//! - `uninstall` 注销计划任务。
//! - `run`       前台运行：装配状态 → setup（接管/自动恢复 + 采集器）→ 启动管道 server。
//! - `repair-offline` 离线提权救援（§7 两级救援）。

mod capture;
mod connections;
mod decrypt_sink;
mod engine;
mod firewall;
mod frame;
mod install;
mod mitm_engine;
mod observe;
mod ops;
mod paths;
mod proc;
mod process_watch;
mod ptree;
mod repair;
mod security;
mod server;
mod service;
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
        /// L4 MITM 引擎（mitmproxy）预置 zip 源路径（可选，离线安装）。给了则安装时顺带部署引擎
        /// （SHA 校验 + Defender 放行）；不给则跳过引擎（可事后 `install-mitm-engine` 补）。
        #[arg(long)]
        mitm_zip: Option<String>,
    },
    /// 注销计划任务（须管理员）。
    Uninstall,
    /// 单独部署 L4 MITM 引擎（mitmproxy）到受保护目录（须管理员）。GUI 安装/设置流程调用：
    /// 下载或用 `--zip` 本地 zip → SHA-256 校验 → Defender 放行 → 解压到
    /// `%ProgramFiles%\net-policy\mitm\engine\<version>\`。不启动解密、不装 CA。
    InstallMitmEngine {
        /// 预置 zip 源路径（离线安装）；缺省则从官方站下载。
        #[arg(long)]
        zip: Option<String>,
    },
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
    /// 由 Windows SCM 拉起：以服务形态在 Session 0 常驻（开机自启、不依赖登录）。用户不直接调。
    RunService,
    /// L4 MITM 数据面 spike（方案 B 验证，抓包设计 §17.3/§18.1）：用 `net-policy-mitm` 起一个前台
    /// loopback 显式 MITM 代理，上游按域名链到 mihomo（解 fake-ip），对 `--domains` 白名单域名解密并把
    /// 脱敏明文写 `--out` http.jsonl。**不装 CA 到信任库、不改 mihomo**——用
    /// `curl --proxy http://<listen> --cacert <ca-dir>/ca.crt https://<域名>` 验证。仅诊断用。
    MitmSpike {
        #[arg(long, default_value = "127.0.0.1:18080")]
        listen: String,
        /// 上游代理（mihomo 混合端口）：`http://127.0.0.1:7890` 或 `socks5://...`。
        #[arg(long, default_value = "http://127.0.0.1:7890")]
        upstream: String,
        /// CA 目录（load_or_generate；ca.crt/ca.key）。
        #[arg(long)]
        ca_dir: String,
        /// 拦截解密的域名白名单（逗号分隔）；其余域名纯 TCP 隧道透传。
        #[arg(long)]
        domains: String,
        /// 脱敏明文索引落盘路径（http.jsonl）。
        #[arg(long)]
        out: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let service_mode = matches!(cli.cmd, Cmd::RunService);
    init_log(service_mode);
    install_panic_hook(service_mode);
    match cli.cmd {
        Cmd::Install {
            mihomo,
            wintun,
            mitm_zip,
        } => install::install(mihomo.as_deref(), wintun.as_deref(), mitm_zip.as_deref()),
        Cmd::Uninstall => install::uninstall(),
        Cmd::InstallMitmEngine { zip } => {
            let result = mitm_engine::deploy(zip.as_deref())?;
            println!("{result}");
            Ok(())
        }
        Cmd::RepairOffline { force, workspace } => {
            let ws = paths::workspace_dir(workspace.as_deref());
            repair::repair_offline(&ws, force)
        }
        Cmd::Run { dev, workspace } => run(dev, workspace),
        Cmd::RunService => service::run(),
        Cmd::MitmSpike {
            listen,
            upstream,
            ca_dir,
            domains,
            out,
        } => decrypt_sink::run_mitm_spike(&listen, &upstream, &ca_dir, &domains, &out),
    }
}

/// `run` 需要 tokio 运行时；install/uninstall/repair 是同步（PowerShell），不必进异步。
fn run(dev: bool, workspace: Option<String>) -> Result<()> {
    run_with_ready(dev, workspace, None)
}

/// 服务模式额外用 `ready` 把“命名管道已经创建”或启动失败原因回传给 SCM 主线程。
fn run_with_ready(
    dev: bool,
    workspace: Option<String>,
    ready: Option<std::sync::mpsc::SyncSender<Result<(), String>>>,
) -> Result<()> {
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
        server::serve(state, ready).await
    })
}

/// 日志走 custom-utils（AGENTS.md 约定）：dev 输出控制台；prod（`--features prod`）写
/// `{home}/log/net-policy-agent` 文件、stdout 保持干净（install/repair 的 JSON 不被污染）。
fn init_log(service_mode: bool) {
    if service_mode {
        let root = paths::service_workspace();
        let _ = custom_utils::logger::logger_feature_with_path(
            "net-policy-agent",
            "info,reqwest=warn",
            log::LevelFilter::Info,
            root.join("etc"),
            false,
            root.join("log"),
        )
        .build();
    } else {
        let _ = custom_utils::logger::logger_feature(
            "net-policy-agent",
            "info,reqwest=warn",
            log::LevelFilter::Info,
            false,
        )
        .build();
    }
}

/// panic 落文件：Rust panic 默认走 stderr，agent 后台/计划任务运行时无控制台会丢失，导致偶发崩溃
/// （如切姿态时的 auto_apply/reload）查无实据。此 hook 把 panic 的线程/位置/原因/backtrace **同步**
/// 写到 `{USERPROFILE}\log\net-policy-agent-panic.log`（绕过 flexi 缓冲，保证崩溃前落盘）；tokio 任务
/// 内的 panic 也会触发全局 hook，故 setup 采样器 / do_apply 的 spawn_blocking 崩溃同样能抓到。
fn install_panic_hook(service_mode: bool) {
    let log_dir = if service_mode {
        paths::service_workspace().join("log")
    } else {
        std::env::var("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join("log")
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let tname = std::thread::current()
            .name()
            .unwrap_or("unknown")
            .to_string();
        let text = format!(
            "\n===== AGENT PANIC =====\nthread: {tname}\n{info}\nbacktrace:\n{bt}\n=======================\n"
        );
        let _ = std::fs::create_dir_all(&log_dir);
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("net-policy-agent-panic.log"))
        {
            let _ = f.write_all(text.as_bytes());
            let _ = f.flush();
        }
        log::error!("agent panic: {info}");
        default_hook(info); // 保留默认行为（stderr，前台可见）
    }));
}
