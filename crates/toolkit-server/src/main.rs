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

    /// remote-exec 第一期:管理 worker 的 exec 专用凭据（见 docs/remote-exec-design.md §4.2）。
    /// 遵守仓库 stdout 契约:每个动作只输出一行紧凑 JSON;业务失败输出
    /// `{error, error_kind}` 且退出码 0。
    ExecCred {
        #[command(subcommand)]
        action: ExecCredAction,
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

/// `exec-cred` 子命令：签发 / 吊销 / 列出 remote-exec worker 凭据。每个动作独立带
/// `--workspace`（与 `serve` 一致的解析优先级：显式参数 > env > 默认），因为这条命令
/// 通常是脱离 daemon 单独跑的一次性操作，不共享 `Serve` 的运行时状态。
#[derive(Subcommand, Debug)]
enum ExecCredAction {
    /// 签发（或重新签发）一个 worker 的凭据，明文 secret 只在此刻输出一次。
    Add {
        #[arg(long)]
        worker_id: String,
        #[arg(long, env = "TOOLKIT_WORKSPACE")]
        workspace: Option<PathBuf>,
    },
    /// 吊销一个 worker 的凭据（拒绝后续领取任务/回传结果，不等于杀掉正在跑的进程）。
    Revoke {
        #[arg(long)]
        worker_id: String,
        #[arg(long, env = "TOOLKIT_WORKSPACE")]
        workspace: Option<PathBuf>,
    },
    /// 列出全部凭据概览（不含 secret/hash）。
    List {
        #[arg(long, env = "TOOLKIT_WORKSPACE")]
        workspace: Option<PathBuf>,
    },
}

/// 打开（必要时创建）`<workspace>/toolkit.db` 连接池，供 `exec-cred` 独立于 `serve`
/// 单独调用。
fn open_exec_cred_pool(workspace: Option<PathBuf>) -> Result<toolkit_core::SqlitePool> {
    let ws = match workspace {
        Some(p) => p,
        None => workspace_dir()?,
    };
    std::fs::create_dir_all(&ws).with_context(|| format!("create workspace {}", ws.display()))?;
    let pool = toolkit_core::open_pool(&ws.join("toolkit.db"))?;
    toolkit_core::migrate(&pool)?;
    Ok(pool)
}

/// 单行紧凑 JSON 输出（仓库 stdout 契约）。
fn print_json(v: serde_json::Value) {
    println!("{v}");
}

/// 业务失败：`{error, error_kind}`，退出码仍是 0（仅进程级异常才非 0，见仓库约定）。
fn print_business_error(kind: &str, err: impl std::fmt::Display) {
    print_json(serde_json::json!({ "error": err.to_string(), "error_kind": kind }));
}

fn run_exec_cred(action: ExecCredAction) {
    match action {
        ExecCredAction::Add {
            worker_id,
            workspace,
        } => match open_exec_cred_pool(workspace) {
            Ok(pool) => match toolkit_core::exec_creds::issue(&pool, &worker_id) {
                Ok(secret) => print_json(serde_json::json!({
                    "worker_id": worker_id,
                    "secret": secret,
                })),
                Err(e) => print_business_error("db", e),
            },
            Err(e) => print_business_error("io", e),
        },
        ExecCredAction::Revoke {
            worker_id,
            workspace,
        } => match open_exec_cred_pool(workspace) {
            Ok(pool) => match toolkit_core::exec_creds::revoke(&pool, &worker_id) {
                Ok(existed) => print_json(serde_json::json!({
                    "worker_id": worker_id,
                    "revoked": existed,
                })),
                Err(e) => print_business_error("db", e),
            },
            Err(e) => print_business_error("io", e),
        },
        ExecCredAction::List { workspace } => match open_exec_cred_pool(workspace) {
            Ok(pool) => match toolkit_core::exec_creds::list(&pool) {
                Ok(creds) => print_json(serde_json::json!({ "creds": creds })),
                Err(e) => print_business_error("db", e),
            },
            Err(e) => print_business_error("io", e),
        },
    }
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
        Command::ExecCred { action } => {
            run_exec_cred(action);
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
