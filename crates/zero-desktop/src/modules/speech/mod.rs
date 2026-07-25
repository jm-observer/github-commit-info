pub mod capture;
pub mod commands;
pub mod db;
pub mod legacy;
pub mod llm_settings;
pub mod lock_utils;
pub mod paste_watch;
pub mod scene_log;
pub mod settings;
pub mod voice_commands;

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex, RwLock};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use tracing::{error, info, warn};

use crate::modules::speech::llm_settings::LlmSettings;
use crate::modules::speech::lock_utils::read_lock;
use crate::modules::speech::settings::VadSettings;

/// Speech 模块状态，持有录音、DB、设置等所有运行时字段。
pub struct SpeechState {
    pub(crate) recording: Arc<AtomicBool>,
    pub(crate) stop_signal: Arc<AtomicBool>,
    pub(crate) db: Arc<Mutex<Option<db::SpeechDatabase>>>,
    pub(crate) init_status: Arc<AtomicU8>,
    pub(crate) init_error: Arc<RwLock<String>>,
    pub(crate) settings: Arc<RwLock<VadSettings>>,
    pub(crate) llm_settings: Arc<RwLock<LlmSettings>>,
    pub(crate) selected_device: Arc<RwLock<Option<String>>>,
    pub(crate) remote_url: Arc<RwLock<String>>,
    pub(crate) remote_url_presets: Arc<RwLock<Vec<String>>>,
    pub(crate) capture: Arc<capture::CaptureState>,
}

impl SpeechState {
    /// 创建并初始化 SpeechState，打开 DB 并加载持久化设置。
    /// db_path 应为 `{workspace}/speech/speech_history.db`。
    pub fn new(db_path: &Path) -> Result<Arc<Self>> {
        // Block on async DB init; Tauri's async runtime is available here
        // because we call this before `Builder::setup`.
        let db = tauri::async_runtime::block_on(db::SpeechDatabase::init(db_path))
            .with_context(|| format!("speech DB init failed at {}", db_path.display()))?;

        let vad_settings = tauri::async_runtime::block_on(settings::load_vad_settings_from_db(&db));
        let llm_settings_val =
            tauri::async_runtime::block_on(settings::load_llm_settings_from_db(&db));
        let (remote_url, remote_url_presets) =
            tauri::async_runtime::block_on(settings::load_remote_settings_from_db(&db));

        let state = Arc::new(SpeechState {
            recording: Arc::new(AtomicBool::new(false)),
            stop_signal: Arc::new(AtomicBool::new(false)),
            db: Arc::new(Mutex::new(Some(db))),
            init_status: Arc::new(AtomicU8::new(0)),
            init_error: Arc::new(RwLock::new(String::new())),
            settings: Arc::new(RwLock::new(vad_settings)),
            llm_settings: Arc::new(RwLock::new(llm_settings_val)),
            selected_device: Arc::new(RwLock::new(None)),
            remote_url: Arc::new(RwLock::new(remote_url)),
            remote_url_presets: Arc::new(RwLock::new(remote_url_presets)),
            capture: capture::CaptureState::new(),
        });

        // Remote-only client: report ready immediately.
        state
            .init_status
            .store(1, std::sync::atomic::Ordering::Relaxed);

        Ok(state)
    }
}

/// 初始化 Speech 模块：注册托盘（仅 Show + Quit）、完成 DB/状态准备。
pub fn setup(app: &tauri::AppHandle, state: Arc<SpeechState>) -> Result<()> {
    // 装全局 Ctrl+V 观察器：粘贴后重置自动复制的拼接累加器，避免「每段即时粘贴」时重复粘贴前一段。
    paste_watch::start_paste_watcher();

    // 语音纠错一键采集：建信号通道 + 起后台 worker（Ctrl+Alt+X 命中时读剪贴板、配对、落库）。
    let capture_rx = capture::init_capture_channel();
    tauri::async_runtime::spawn(capture::run_capture_worker(
        app.clone(),
        Arc::clone(&state.db),
        Arc::clone(&state.capture),
        Arc::clone(&state.llm_settings),
        capture_rx,
    ));

    // 场景记录：每次交付都落库（日常全量收集）。同样走 channel + worker——auto_copy 的记录
    // 时刻在键盘钩子回调里，那里不能碰数据库。开关值从已加载的设置同步一次（默认开）。
    scene_log::set_enabled(read_lock(&state.llm_settings).scene_log_enabled);
    if let Some(delivery_rx) = scene_log::init_scene_channel() {
        tauri::async_runtime::spawn(scene_log::run_scene_log_worker(
            app.clone(),
            Arc::clone(&state.db),
            delivery_rx,
        ));
    }

    // Register tray icon. Per §4.2.1 the menu only has "Show window" and "Quit".
    // The "quick start recording" item is in legacy/mod.rs.
    if let Some(icon) = app.default_window_icon().cloned() {
        info!(target: "speech", "[tray] creating tray icon");
        TrayIconBuilder::with_id("main")
            .icon(icon)
            .tooltip("Zero Desktop")
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    info!(target: "speech", "[tray] left click received, restoring window");
                    let app = tray.app_handle();
                    if let Some(window) = app.get_webview_window("main") {
                        if let Err(err) = window.show() {
                            error!(target: "speech", "show window from tray failed: {}", err);
                            return;
                        }
                        let _ = window.unminimize();
                        let _ = window.set_focus();
                        info!(target: "speech", "[tray] window focus requested");
                    } else {
                        warn!(target: "speech", "[tray] main window not found on tray click");
                    }
                }
            })
            .build(app)
            .context("failed to build tray icon")?;
        info!(target: "speech", "[tray] tray icon created");
    } else {
        warn!(target: "speech", "[setup] default window icon missing, tray icon not created");
    }
    Ok(())
}
