use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use custom_utils::updater::{CliAction, DeployCommand, LinuxService};
use log::LevelFilter::Info;
use std::path::PathBuf;
use toolkit_server::{run, workspace_dir, Config};

const REPO_OWNER: &str = "jm-observer";
const REPO_NAME: &str = "toolkit";
const APP: &str = "toolkit-server";
/// systemd watchdog 心跳间隔（秒）。axum + 后台 task 调度都很快，60s 给足喘息。
const WATCHDOG_SEC: u32 = 60;

/// 安装时写进 unit 的默认监听地址；可被 install 的 `--bind` 覆盖。
const DEFAULT_BIND: &str = "0.0.0.0:8788";

/// 安装/自更新统一描述。ExecStart 由 `{workspace}` 模板在 install 时实拼。
///
/// `bind` 决定写进生成 unit `[Service]` 段的 `Environment=TOOLKIT_BIND=<bind>`，
/// serve 子命令完全靠该 env（+ clap default）决定监听端口，故 exec_args 不再写死 `--bind`。
fn linux_service(bind: &str) -> LinuxService {
    LinuxService::new(APP, REPO_OWNER, REPO_NAME, env!("CARGO_PKG_VERSION"))
        .bin_name(APP)
        .description("toolkit-server: axum + toolkit-core/tasks daemon")
        .exec_args("serve --workspace {workspace}")
        .env("TOOLKIT_BIND", bind)
        .watchdog_sec(WATCHDOG_SEC)
        .restart_sec(5)
}

#[derive(Parser, Debug)]
#[command(name = "toolkit-server", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 启动 daemon（默认子命令）。workspace 默认 `$TOOLKIT_WORKSPACE` → `~/.config/toolkit-server`。
    Serve {
        #[arg(long, env = "TOOLKIT_BIND", default_value = "0.0.0.0:8788")]
        bind: String,
        /// workspace 根目录；省略走 env / 默认。
        #[arg(long, env = "TOOLKIT_WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Web 控制台静态目录；省略 = `<workspace>/web`。
        #[arg(long, env = "TOOLKIT_WEB_DIR")]
        web_dir: Option<PathBuf>,
    },
    /// 安装为 systemd 用户级服务（rootless，`~/.local/bin` + `~/.config/toolkit-server`）。
    Install {
        #[arg(long, short = 'n', help = "只打印渲染后的 unit 不真正安装")]
        dry_run: bool,
        /// 显式 workspace 路径，覆盖 `~/.config/toolkit-server` 默认。
        #[arg(long, short = 'w')]
        workspace: Option<String>,
        /// 监听地址，写进 unit 的 `Environment=TOOLKIT_BIND=<addr>` 决定服务端口。
        #[arg(long, default_value = DEFAULT_BIND)]
        bind: String,
        /// 额外注入 unit 的环境变量，`KEY=VAL`，可重复（如 `-e TTS_BASE_URL=http://127.0.0.1:8095`）。
        /// 追加在内置 `.env()` 之后，键冲突时此处的值生效（覆盖含 `--bind` 的 `TOOLKIT_BIND`）。
        #[arg(long, short = 'e', value_name = "KEY=VAL")]
        env: Vec<String>,
    },
    /// 从 GitHub Release 自更新当前可执行文件。
    Update {
        #[arg(short, long, help = "即使版本未升级也强制更新")]
        force: bool,
    },

    /// 内部：后台下载 worker。由 douyin 库 spawn（current_exe），勿手动调。
    #[command(hide = true)]
    DownloadWorker {
        #[arg(long)]
        task_dir: PathBuf,
        #[arg(long)]
        task_id: String,
    },
    /// 内部：后台 list-works worker。由 douyin 库 spawn，勿手动调。
    #[command(hide = true)]
    ListWorksWorker {
        #[arg(long)]
        task_dir: PathBuf,
        #[arg(long)]
        task_id: String,
    },
    /// 内部：后台「下载+ASR」process worker。由 douyin 库 spawn，勿手动调。
    #[command(hide = true)]
    ProcessWorker {
        #[arg(long)]
        task_dir: PathBuf,
        #[arg(long)]
        task_id: String,
    },
}

/// 启用 trace-hub 全链路追踪——仅当设置了环境变量 `TRACE_HUB_ENDPOINT` 时生效；
/// 未设则完全无副作用（record_* 全 no-op，不起后台任务）。
fn init_trace() {
    if let Ok(endpoint) = std::env::var("TRACE_HUB_ENDPOINT") {
        custom_utils::trace::init(custom_utils::trace::TraceConfig::new(
            endpoint,
            "toolkit-server",
        ));
        log::info!("trace enabled → trace-hub");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ =
        custom_utils::logger::logger_feature("toolkit-server", "info,reqwest=warn", Info, false)
            .build();

    init_trace();

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Serve {
        bind: "0.0.0.0:8788".to_string(),
        workspace: None,
        web_dir: None,
    });

    match command {
        Command::Serve {
            bind,
            workspace,
            web_dir,
        } => {
            let _watchdog = linux_service(DEFAULT_BIND).spawn_watchdog();
            let bind: std::net::SocketAddr = bind.parse().context("parse bind")?;
            let workspace = match workspace {
                Some(p) => p,
                None => workspace_dir()?,
            };
            let web_dir = web_dir.unwrap_or_else(|| workspace.join("web"));
            run(Config {
                bind,
                workspace,
                web_dir,
            })
            .await
        }
        Command::Install {
            dry_run,
            workspace,
            bind,
            env,
        } => {
            match linux_service(&bind)
                .dispatch(DeployCommand::Install {
                    dry_run,
                    workspace,
                    env,
                })
                .await
                .context("安装失败")?
            {
                CliAction::DryRun(unit) => println!("{unit}"),
                CliAction::Handled => log::info!("install ok"),
                _ => {}
            }
            Ok(())
        }
        Command::Update { force } => {
            linux_service(DEFAULT_BIND)
                .dispatch(DeployCommand::Update { force })
                .await
                .context("自更新失败")?;
            Ok(())
        }
        // 抖音长任务（list_works / download / process）由 douyin 库 spawn current_exe
        // + 隐藏 worker 子命令跑后台进程。toolkit-server 即 current_exe，故必须在此接住
        // 这三个子命令委托给 douyin 库 worker 入口，否则任务永远卡 queued。
        Command::DownloadWorker { task_dir, task_id } => {
            douyin::download::run_worker(&task_dir, &task_id).await
        }
        Command::ListWorksWorker { task_dir, task_id } => {
            douyin::list_works_task::run_worker(&task_dir, &task_id).await
        }
        Command::ProcessWorker { task_dir, task_id } => {
            douyin::process::run_worker(&task_dir, &task_id).await
        }
    }
}
