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
    // rustls 0.23 要求进程级 CryptoProvider。依赖树里 aws-lc-rs 与 ring 两个 provider 同时
    // 被启用（reqwest 0.12 经 hyper-rustls 拉 aws-lc-rs，tokio-rustls 一路拉 ring），rustls
    // 无法自动选择 → 首次 TLS 握手时 panic。表现为外网档 wss 语音识别一连就崩：
    // `Could not automatically determine the process-level CryptoProvider`。
    // 必须在任何 TLS 使用之前装好；重复安装返回 Err，忽略即可。
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();

    let _ = custom_utils::logger::logger_feature(APP, "info,reqwest=warn", Info, false).build();

    init_trace();

    let workspace = resolve_workspace(&cli.workspace)?;
    log::info!("workspace = {}", workspace.display());

    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run_gui(workspace),
        Command::Update { force } => shared::update::run_update(REPO_OWNER, REPO_NAME, APP, force),
    }
}

fn resolve_workspace(arg: &Option<String>) -> Result<PathBuf> {
    // 统一走 custom-utils 生态约定：$HOME/.config/<app>（-w / ZERO_DESKTOP_WORKSPACE 仍优先）。
    custom_utils::args::workspace(arg, APP)
}

fn run_gui(workspace: PathBuf) -> Result<()> {
    shared::workspace::ensure_workspace(&workspace)?;

    let state = AppState::new(workspace).context("AppState::new")?;

    tauri::Builder::default()
        // 单实例必须是第一个插件（Tauri 要求）：第二个实例启动时不再自建窗口，
        // 而是把已有主窗拉到前台后自行退出。杜绝「两个进程互抢 Ctrl+Alt+A /
        // 同一个 workspace SQLite」，也杜绝旧进程被误当成已关闭而重复拉起。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
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
            use tauri::Manager;
            match event {
                // 窗口重新聚焦时失效活动端点探测缓存：用户可能刚切换了网络（回家/外出），
                // 下次请求据此重新探测局域网可达性、自动选路。
                tauri::WindowEvent::Focused(true) => {
                    let net = window.state::<AppState>().net.clone();
                    tauri::async_runtime::spawn(async move { net.invalidate().await });
                }
                // 关主窗 = 真退出进程。
                //
                // 不能依赖 Tauri 默认的「最后一个窗口关闭才退出」：截图 overlay 是
                // `skip_taskbar(true)` 的透明置顶窗（见 modules::screenshot::overlay），
                // 一旦它残留，关掉主窗后进程会因为「还有窗口」而继续存活，且任务栏上
                // 毫无痕迹 —— 用户以为关干净了，实际留下一个幽灵进程：它的全局热键仍
                // 注册着，它的 overlay 仍是一层拦截整个桌面输入的隐身玻璃。这正是
                // 「点哪都没反应」反复复发的根源。
                tauri::WindowEvent::CloseRequested { .. } if window.label() == "main" => {
                    let app = window.app_handle();
                    modules::screenshot::overlay::close_overlay(app);
                    // 录制中直接退进程会留下一个没写完 moov 的 mp4（播放器打不开）+ 一个
                    // 孤儿 ffmpeg。这里先正常收尾，宁可退出慢半秒。
                    if modules::recording::session::is_active() {
                        if let Err(e) = modules::recording::recording_stop(app.clone()) {
                            log::warn!(target: "recording", "退出前停止录制失败: {e}");
                        }
                    }
                    app.exit(0);
                }
                _ => {}
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
            modules::english::english_shadow_score,
            modules::english::english_shadow_stats,
            modules::english::english_shadow_stream_url,
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
            modules::speech::commands::samples::speech_fetch_segment_audio,
            modules::speech::commands::samples::speech_mark_sample,
            modules::speech::commands::samples::speech_list_samples,
            modules::speech::commands::samples::speech_export_samples,
            modules::speech::commands::samples::speech_export_homophone_candidates,
            modules::speech::commands::samples::speech_export_hotword_candidates,
            modules::speech::commands::samples::speech_scene_stats,
            modules::speech::commands::export::speech_copy_text_to_clipboard,
            modules::speech::commands::init::speech_get_init_status,
            modules::speech::commands::settings::speech_get_settings,
            modules::speech::commands::settings::speech_default_remote_url,
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
            // llm 模块（公共大模型层：配置 / 提示词 / 自测 / 对话总结）
            modules::llm::llm_get_config,
            modules::llm::llm_put_config,
            modules::llm::llm_list_prompts,
            modules::llm::llm_get_prompt,
            modules::llm::llm_put_prompt,
            modules::llm::llm_reset_prompt,
            modules::llm::llm_ping,
            modules::llm::llm_summarize,
            modules::llm::llm_list_sessions,
            modules::llm::llm_get_session,
            modules::llm::llm_create_chat,
            modules::llm::llm_chat_send,
            modules::llm::llm_rename_session,
            modules::llm::llm_delete_session,
            // egress 模块（出口代理 worker 列表，只读观测面）
            modules::egress::egress_list_workers,
            // exec 模块（远程节点：权限申请审批 + 在线节点/凭据观测）
            modules::exec::exec_list_requests,
            modules::exec::exec_list_workers,
            modules::exec::exec_list_creds,
            modules::exec::exec_approve_request,
            modules::exec::exec_reject_request,
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
            modules::screenshot::screenshot_overlay_ready,
            modules::screenshot::screenshot_get_settings,
            modules::screenshot::screenshot_save_settings,
            modules::screenshot::screenshot_list_history,
            modules::screenshot::screenshot_open_folder,
            modules::screenshot::screenshot_delete,
            modules::screenshot::screenshot_set_starred,
            modules::screenshot::screenshot_copy_to_clipboard,
            modules::screenshot::screenshot_reveal_in_folder,
            modules::screenshot::screenshot_save_as,
            // 录屏模块
            modules::recording::recording_get_settings,
            modules::recording::recording_save_settings,
            modules::recording::recording_detect_ffmpeg,
            modules::recording::recording_status,
            modules::recording::recording_start,
            modules::recording::recording_start_region,
            modules::recording::recording_set_paused,
            modules::recording::recording_stop,
            modules::recording::recording_discard,
            modules::recording::recording_list,
            modules::recording::recording_delete,
            modules::recording::recording_open_folder,
            modules::recording::recording_open_file,
            modules::recording::recording_reveal_in_folder,
            modules::recording::recording_save_as,
        ])
        .setup(move |app| {
            modules::english::setup(app.handle(), state.english.clone())
                .context("english::setup")?;
            modules::speech::setup(app.handle(), state.speech.clone()).context("speech::setup")?;
            modules::cookie::setup(app.handle(), state.cookie.clone()).context("cookie::setup")?;
            modules::music::setup(app.handle(), state.music.clone()).context("music::setup")?;
            modules::screenshot::setup(app.handle()).context("screenshot::setup")?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run tauri");
    Ok(())
}
