//! orchestrator 独立二进制入口:CLI(serve / install / update)。
//!
//! 业务逻辑全在 `lib.rs`(`router` / `init_ctx` / `serve` …),本文件只做 CLI 解析与分发。
//! toolkit-server 走 lib 同进程挂载,不经过这里。

use anyhow::Context;
use clap::{Parser, Subcommand};
use custom_utils::updater::{CliAction, DeployCommand};
use orchestrator::{init_trace, linux_service, serve, workspace_dir, DEFAULT_BIND};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "orchestrator", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 启动 daemon(默认子命令)。
    Serve {
        #[arg(long, env = "ORCH_BIND", default_value = DEFAULT_BIND)]
        bind: String,
        /// workspace 根目录(存 app.db);省略走 env / 默认 `~/.config/orchestrator`。
        #[arg(long, env = "ORCH_WORKSPACE")]
        workspace: Option<PathBuf>,
    },
    /// 安装为 systemd 用户级服务(rootless,~/.local/bin + ~/.config/orchestrator)。
    Install {
        #[arg(long, short = 'n', help = "只打印渲染后的 unit 不真正安装")]
        dry_run: bool,
        /// 显式 workspace 路径,覆盖 `~/.config/orchestrator` 默认。
        #[arg(long, short = 'w')]
        workspace: Option<String>,
        /// 监听地址,写进 unit 的 `Environment=ORCH_BIND=<addr>` 决定服务端口。
        #[arg(long, default_value = DEFAULT_BIND)]
        bind: String,
        /// 额外注入 unit 的环境变量,`KEY=VAL`,可重复(如 `-e VLLM_BASE=http://127.0.0.1:8085/v1`)。
        #[arg(long, short = 'e', value_name = "KEY=VAL")]
        env: Vec<String>,
    },
    /// 从 GitHub Release 自更新当前可执行文件。
    Update {
        #[arg(short, long, help = "即使版本未升级也强制更新")]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    // trace init 须在 tokio 运行时内(本函数是 #[tokio::main],OK)。
    init_trace();

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Serve {
        bind: DEFAULT_BIND.to_string(),
        workspace: None,
    });

    match command {
        Command::Serve { bind, workspace } => {
            let _watchdog = linux_service(DEFAULT_BIND).spawn_watchdog();
            let workspace = match workspace {
                Some(p) => p,
                None => workspace_dir()?,
            };
            serve(bind, workspace).await
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
                CliAction::Handled => tracing::info!("install ok"),
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
    }
}
