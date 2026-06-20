#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod app_state;
mod modules;
mod shared;

use anyhow::{Context, Result};
use app_state::AppState;
use clap::{Parser, Subcommand};
use log::LevelFilter::Info;
use std::path::PathBuf;

const REPO_OWNER: &str = "jm-observer";
const REPO_NAME: &str = "toolkit";
const APP: &str = "zero-desktop";

#[derive(Parser, Debug, Clone)]
#[command(name = "zero-desktop", version)]
struct Cli {
    #[arg(long, env = "ZERO_DESKTOP_WORKSPACE", global = true)]
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
    /// Headless 跑 codeloop 三段（design → implementation → review），不开 Tauri 窗口。
    /// 设计见 `docs/codeloop-headless-smoke-runner-plan.md`。
    CodeloopSmoke {
        /// 仓库根（必须是 git 工作树根）。
        #[arg(long)]
        repo: PathBuf,
        /// 设计文档 / 目标的仓内相对路径。
        #[arg(long)]
        target: String,
        /// 必填：Claude 会话 id；传 `auto` 表示按 cwd + 最近活跃自动选（仍受劫持保护）。
        /// 不传直接拒绝，避免误选用户当前正在用的活会话。
        #[arg(long)]
        claude_session: Option<String>,
        /// Codex 会话 id；传 `auto` 自动选；与 `--new-codex-agent` 配合时可省。
        #[arg(long)]
        codex_session: Option<String>,
        /// 每段最大轮数。
        #[arg(long, default_value_t = 2)]
        max_rounds: u32,
        /// 全自动放行逐步确认门（无人值守必开）。
        #[arg(long)]
        auto_confirm: bool,
        /// 启动前新建 Codex 会话。
        #[arg(long)]
        new_codex_agent: bool,
        /// 解除"目标会话 transcript 近 5 分钟内仍在写入"的劫持保护。
        /// 不要在日常开发机上加这个；日常用 `--repo` 指向独立 clone。
        #[arg(long)]
        allow_hijack_current_session: bool,
        /// ASK_USER 子串→答案 的 JSON 映射文件；未提供且模型问就 abort。
        #[arg(long)]
        ask_user_answers: Option<PathBuf>,
        /// 恢复变体（v1 未实现，占位）。
        #[arg(long)]
        recover_after_stop: bool,
        /// 实现 worktree 内跑 `cargo test -p codeloop-core` + `cargo check -p zero-desktop`。
        #[arg(long)]
        verify: bool,
        /// 输出 JSONL 进度 + 终态对象到 stdout（人类摘要进 stderr）。
        #[arg(long)]
        json: bool,
        /// 全局 wall-clock 上限（如 `15m` / `1h`）。
        #[arg(long, default_value = "15m")]
        timeout: humantime::Duration,
        /// 多入口标记（codeloop-multi-entry-design.md §6.4 第 3 点）：
        /// `doc_review`（默认，兼容旧脚本） | `implement` | `review_seed`。
        #[arg(long, default_value = "doc_review")]
        entry_kind: String,
        /// 仅 `--entry-kind=review_seed` 且 `mode=implementation` 时可选：规格依据文档路径。
        #[arg(long)]
        design_doc: Option<String>,
        /// `--entry-kind=review_seed`：seed 文件路径。
        #[arg(long)]
        seed_review: Option<String>,
        /// `--entry-kind=review_seed`：从文件读 inline seed 文本（避免 shell 引号陷阱）。
        #[arg(long)]
        seed_review_inline_file: Option<PathBuf>,
    },
}

/// 启用 trace-hub 全链路追踪——仅当设置了环境变量 `TRACE_HUB_ENDPOINT` 时生效；
/// 未设则完全无副作用（record_* 全 no-op，不起后台任务）。
fn init_trace() {
    if let Ok(endpoint) = std::env::var("TRACE_HUB_ENDPOINT") {
        custom_utils::trace::init(custom_utils::trace::TraceConfig::new(
            endpoint,
            "zero-desktop",
        ));
        tracing::info!("trace enabled → trace-hub");
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // `codeloop-smoke --json` 模式下 stdout 是 JSONL 契约，不能让 logger 串字符进来。
    // 此模式下完全跳过 logger 注册（log crate 变 no-op，driver / codeloop 的 log::info!
    // 都被吃掉）；其它命令保持原行为。
    let suppress_logger = matches!(
        cli.command,
        Some(Command::CodeloopSmoke { json: true, .. })
    );
    if !suppress_logger {
        let _ =
            custom_utils::logger::logger_feature(APP, "info,reqwest=warn", Info, false).build();
    }

    init_trace();

    let workspace = resolve_workspace(&cli.workspace)?;
    if !suppress_logger {
        log::info!("workspace = {}", workspace.display());
    }

    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run_gui(workspace),
        Command::Update { force } => shared::update::run_update(REPO_OWNER, REPO_NAME, APP, force),
        Command::NetPolicyGen { what } => {
            let (cfg, fw) = modules::net_policy::gen_artifacts(&workspace)?;
            match what.as_str() {
                "firewall" => print!("{fw}"),
                _ => print!("{cfg}"),
            }
            Ok(())
        }
        Command::CodeloopSmoke {
            repo,
            target,
            claude_session,
            codex_session,
            max_rounds,
            auto_confirm,
            new_codex_agent,
            allow_hijack_current_session,
            ask_user_answers,
            recover_after_stop,
            verify,
            json,
            timeout,
            entry_kind,
            design_doc,
            seed_review,
            seed_review_inline_file,
        } => {
            let parsed_entry_kind = match entry_kind.as_str() {
                "doc_review" => codeloop_core::prompt::EntryKind::DocReview,
                "implement" => codeloop_core::prompt::EntryKind::Implement,
                "review_seed" => codeloop_core::prompt::EntryKind::ReviewSeed,
                other => {
                    eprintln!(
                        "[fatal] --entry-kind 取值非法：{other}（仅支持 doc_review / implement / review_seed）"
                    );
                    std::process::exit(modules::codeloop::smoke::EXIT_PREFLIGHT);
                }
            };
            let args = modules::codeloop::smoke::SmokeArgs {
                repo,
                target,
                workspace,
                claude_session,
                codex_session,
                max_rounds,
                auto_confirm,
                new_codex_agent,
                allow_hijack_current_session,
                ask_user_answers,
                recover_after_stop,
                verify,
                json,
                timeout: timeout.into(),
                entry_kind: parsed_entry_kind,
                design_doc,
                seed_review,
                seed_review_inline_file,
            };
            std::process::exit(modules::codeloop::smoke::run(args));
        }
    }
}

fn resolve_workspace(arg: &Option<String>) -> Result<PathBuf> {
    let path = match arg {
        Some(p) => PathBuf::from(p),
        None => dirs::data_local_dir()
            .context("cannot determine data_local_dir")?
            .join("zero-desktop"),
    };
    Ok(path)
}

fn run_gui(workspace: PathBuf) -> Result<()> {
    shared::workspace::ensure_workspace(&workspace)?;

    let state = AppState::new(workspace).context("AppState::new")?;

    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(state.clone())
        .on_window_event(|window, event| {
            // 窗口重新聚焦时失效活动端点探测缓存：用户可能刚切换了网络（回家/外出），
            // 下次请求据此重新探测局域网可达性、自动选路。
            if let tauri::WindowEvent::Focused(true) = event {
                use tauri::Manager;
                let net = window.state::<AppState>().net.clone();
                tauri::async_runtime::spawn(async move { net.invalidate().await });
            }
        })
        .invoke_handler(tauri::generate_handler![
            // 通用
            shared::console::open_url,
            // English 模块
            modules::english::english_ping,
            modules::english::english_get_g10_base,
            modules::english::english_get_audio_cache_dir,
            modules::english::english_tts_voices,
            modules::english::english_tts_preview,
            modules::english::english_replace_sentence_audio,
            // Speech 模块
            modules::speech::commands::device::speech_list_input_devices,
            modules::speech::commands::device::speech_set_input_device,
            modules::speech::commands::device::speech_get_selected_device,
            modules::speech::commands::recording::speech_start_recording,
            modules::speech::commands::recording::speech_stop_recording,
            modules::speech::commands::recording::speech_clear_results,
            modules::speech::commands::recording::speech_get_recording_state,
            modules::speech::commands::remote::speech_fetch_remote_history,
            modules::speech::commands::clean::speech_clean_recording,
            modules::speech::commands::clean::speech_pick_audio_file,
            modules::speech::commands::clean::speech_open_in_folder,
            modules::speech::commands::samples::speech_mark_sample,
            modules::speech::commands::samples::speech_list_samples,
            modules::speech::commands::samples::speech_export_samples,
            modules::speech::commands::export::speech_copy_text_to_clipboard,
            modules::speech::commands::init::speech_get_init_status,
            modules::speech::commands::settings::speech_get_settings,
            modules::speech::commands::settings::speech_apply_settings,
            // Cookie 模块
            modules::cookie::cookie_workspace_path,
            modules::cookie::cookie_get_app_settings,
            modules::cookie::cookie_save_app_settings,
            modules::cookie::net_resolve_status,
            modules::cookie::net_reprobe,
            modules::cookie::cookie_open_douyin_login,
            modules::cookie::cookie_close_douyin_login,
            modules::cookie::cookie_open_ths_login,
            modules::cookie::cookie_close_ths_login,
            modules::cookie::cookie_ths_status,
            modules::cookie::cookie_track_current_creator,
            modules::cookie::cookie_login_expiry,
            modules::cookie::cookie_ping_server,
            modules::cookie::cookie_inspect_cookies,
            modules::cookie::cookie_server_cookie_status,
            modules::cookie::cookie_force_upload_now,
            modules::cookie::cookie_recent_uploads,
            // net-policy 模块
            modules::net_policy::net_policy_get_status,
            modules::net_policy::net_policy_connections,
            modules::net_policy::net_policy_get_settings,
            modules::net_policy::net_policy_save_settings,
            modules::net_policy::net_policy_parse_wg_conf,
            modules::net_policy::net_policy_list_rules,
            modules::net_policy::net_policy_save_rule,
            modules::net_policy::net_policy_delete_rule,
            modules::net_policy::net_policy_list_process_candidates,
            modules::net_policy::net_policy_apply,
            modules::net_policy::net_policy_emergency_stop,
            modules::net_policy::net_policy_verify,
            // llm 模块（公共大模型层：配置 / 提示词 / 自测 / 对话总结）
            modules::llm::llm_get_config,
            modules::llm::llm_put_config,
            modules::llm::llm_list_prompts,
            modules::llm::llm_get_prompt,
            modules::llm::llm_put_prompt,
            modules::llm::llm_reset_prompt,
            modules::llm::llm_ping,
            modules::llm::llm_summarize,
            // codeloop 模块（Codex⇄Claude 复核循环）
            modules::codeloop::codeloop_list_sessions,
            modules::codeloop::codeloop_new_codex_session,
            modules::codeloop::codeloop_session_messages,
            modules::codeloop::codeloop_send_one,
            modules::codeloop::codeloop_start,
            modules::codeloop::codeloop_status,
            modules::codeloop::codeloop_preflight,
            modules::codeloop::codeloop_answer,
            modules::codeloop::codeloop_confirm,
            modules::codeloop::codeloop_set_auto_confirm,
            modules::codeloop::codeloop_stop,
            modules::codeloop::codeloop_continue,
            modules::codeloop::codeloop_list_loops,
            modules::codeloop::codeloop_loop_messages,
            modules::codeloop::codeloop_delete_loop,
            modules::codeloop::codeloop_merge_worktree,
            // g10-deploy 模块（G10 服务部署面板：列表/连通性/版本对比/一键部署）
            modules::g10_deploy::g10_list_services,
            modules::g10_deploy::g10_save_services,
            modules::g10_deploy::g10_probe_service,
            modules::g10_deploy::g10_local_version,
            modules::g10_deploy::g10_deploying_services,
            modules::g10_deploy::g10_deploy,
            // music 模块（本地音乐原生后端播放 + WASAPI 独占 bit-perfect）
            modules::music::music_pick_folder,
            modules::music::music_scan,
            modules::music::music_play_queue,
            modules::music::music_pause,
            modules::music::music_resume,
            modules::music::music_toggle,
            modules::music::music_stop,
            modules::music::music_seek,
            modules::music::music_next,
            modules::music::music_prev,
            modules::music::music_set_volume,
            modules::music::music_set_repeat,
            modules::music::music_set_shuffle,
            modules::music::music_set_output_mode,
            modules::music::music_get_state,
            // screenshot 模块（全局热键截图 → 框选标注 → 剪贴板 + 落盘）
            modules::screenshot::screenshot_capture,
            modules::screenshot::screenshot_commit,
            modules::screenshot::screenshot_cancel,
            modules::screenshot::screenshot_get_settings,
            modules::screenshot::screenshot_save_settings,
            modules::screenshot::screenshot_list_history,
            modules::screenshot::screenshot_open_folder,
            modules::screenshot::screenshot_delete,
            modules::screenshot::screenshot_copy_to_clipboard,
        ])
        .setup(move |app| {
            modules::english::setup(app.handle(), state.english.clone())
                .context("english::setup")?;
            modules::speech::setup(app.handle(), state.speech.clone()).context("speech::setup")?;
            modules::cookie::setup(app.handle(), state.cookie.clone()).context("cookie::setup")?;
            modules::net_policy::setup(app.handle(), state.net_policy.clone())
                .context("net_policy::setup")?;
            modules::music::setup(app.handle(), state.music.clone()).context("music::setup")?;
            modules::screenshot::setup(app.handle()).context("screenshot::setup")?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run tauri");
    Ok(())
}
