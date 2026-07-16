#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod app_state;
mod net_policy;

use anyhow::{Context, Result};
use app_state::AppState;
use clap::{Parser, Subcommand};
use log::LevelFilter::Info;
use std::path::PathBuf;

const REPO_OWNER: &str = "jm-observer";
const REPO_NAME: &str = "toolkit";
const APP: &str = "net-policy-gui";

#[derive(Parser, Debug, Clone)]
#[command(name = "net-policy-gui", version)]
struct Cli {
    #[arg(long, env = "NET_POLICY_GUI_WORKSPACE", global = true)]
    workspace: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// 启动图形界面（默认）。
    Run,
    /// 自更新。
    Update {
        #[arg(short, long)]
        force: bool,
    },
    /// 预览 net-policy 从 workspace 配置生成的产物（不执行）：mihomo 配置 / 防火墙脚本。
    NetPolicyGen {
        /// config | firewall
        #[arg(long, default_value = "config")]
        what: String,
    },
}

/// 启用 trace-hub 全链路追踪——仅当设置了环境变量 `TRACE_HUB_ENDPOINT` 时生效；
/// 未设则完全无副作用。
fn init_trace() {
    if let Ok(endpoint) = std::env::var("TRACE_HUB_ENDPOINT") {
        custom_utils::trace::init(custom_utils::trace::TraceConfig::new(
            endpoint,
            "net-policy-gui",
        ));
        log::info!("trace enabled → trace-hub");
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let _ = custom_utils::logger::logger_feature(APP, "info,reqwest=warn", Info, false).build();
    init_trace();

    let workspace = resolve_workspace(&cli.workspace)?;
    log::info!("workspace = {}", workspace.display());

    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run_gui(workspace),
        Command::Update { force } => run_update(force),
        Command::NetPolicyGen { what } => {
            let (cfg, fw) = net_policy::gen_artifacts(&workspace)?;
            match what.as_str() {
                "firewall" => print!("{fw}"),
                _ => print!("{cfg}"),
            }
            Ok(())
        }
    }
}

fn resolve_workspace(arg: &Option<String>) -> Result<PathBuf> {
    // 统一走 custom-utils 生态约定：$HOME/.config/<app>（-w / NET_POLICY_GUI_WORKSPACE 仍优先）。
    custom_utils::args::workspace(arg, APP)
}

fn ensure_workspace(workspace: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(workspace).context("create workspace dir")?;
    Ok(())
}

/// 自更新：拉 GitHub release 替换本地二进制。照抄 zero-desktop 的 `shared::update::run_update`。
fn run_update(force: bool) -> Result<()> {
    use custom_utils::updater::UpdateConfig;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio rt")?;
    let outcome = rt.block_on(async {
        UpdateConfig::new(REPO_OWNER, REPO_NAME, env!("CARGO_PKG_VERSION"))
            .bin_name(APP)
            .force(force)
            .execute()
            .await
    })?;
    log::info!("update outcome: {outcome:?}");
    Ok(())
}

fn run_gui(workspace: PathBuf) -> Result<()> {
    ensure_workspace(&workspace)?;

    let state = AppState::new(workspace).context("AppState::new")?;

    tauri::Builder::default()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            // net-policy 模块
            net_policy::net_policy_get_status,
            net_policy::net_policy_connections,
            net_policy::net_policy_get_settings,
            net_policy::net_policy_save_settings,
            net_policy::net_policy_parse_wg_conf,
            net_policy::net_policy_list_rules,
            net_policy::net_policy_save_rule,
            net_policy::net_policy_delete_rule,
            net_policy::net_policy_list_process_candidates,
            net_policy::net_policy_apply,
            net_policy::net_policy_emergency_stop,
            net_policy::net_policy_set_enabled,
            net_policy::net_policy_reload,
            net_policy::net_policy_blocked,
            net_policy::net_policy_clear_blocked,
            net_policy::net_policy_dns_map,
            net_policy::net_policy_verify,
            net_policy::net_policy_requests,
            net_policy::net_policy_events,
            net_policy::net_policy_routes,
            net_policy::net_policy_process_tree,
            net_policy::net_policy_temp_status,
            net_policy::net_policy_temp_direct_on,
            net_policy::net_policy_temp_direct_off,
            net_policy::net_policy_clear_requests,
            net_policy::net_policy_clear_events,
            net_policy::net_policy_reset_connections,
            net_policy::net_policy_get_mihomo_log,
            net_policy::net_policy_capture_start,
            net_policy::net_policy_capture_stop,
            net_policy::net_policy_capture_get,
            net_policy::net_policy_capture_list,
            net_policy::net_policy_capture_delete,
            net_policy::net_policy_capture_read,
        ])
        .setup(move |app| {
            net_policy::setup(app.handle(), state.net_policy.clone())
                .context("net_policy::setup")?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run tauri");
    Ok(())
}
