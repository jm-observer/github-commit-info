//! Music 模块：本地音乐**原生后端播放**（bit-perfect / hi-res）。
//!
//! 形态与 speech 模块同款套路：**专用线程 actor**（[`engine`]）+ `app.emit` 事件 +
//! 原子自愈状态（[`SharedPlayback`]）。UI 只是控制面——选曲/按钮/进度展示；解码、混音、
//! 输出全在 Rust（不使用浏览器 `<audio>`）。
//!
//! - 引擎线程在 `.setup()` 里启动（捕获 `AppHandle` 供 emit）；`MusicState::new` 只建 channel +
//!   共享原子。
//! - 命令经 crossbeam-channel 进引擎（非 tokio，音频是实时负载）。
//! - 事件：`music_state_changed` / `music_progress` / `music_format_changed` /
//!   `music_track_changed` / `music_error`。
//!
//! 设计权威文档：`docs/zero-desktop-music-design.md`（§3/§4/§5/§9）。

pub mod decode;
pub mod engine;
pub mod scan;
pub mod sink;
pub mod types;

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{Sender, TrySendError};
use tauri::State;
use tracing::info;

use crate::app_state::AppState;
use engine::AudioCommand;
use types::{OutputMode, PlaybackState, RepeatMode, Track};

/// 引擎与命令层共享的播放快照原子（自愈/快速读，不经引擎线程）。
pub struct SharedPlayback {
    /// 0=stopped 1=playing 2=paused。
    pub status: AtomicU8,
    /// 当前曲下标；-1 表无。
    pub index: AtomicI64,
    /// 当前播放位置（毫秒）。
    pub position_secs: AtomicU64,
}

impl Default for SharedPlayback {
    fn default() -> Self {
        Self {
            status: AtomicU8::new(0),
            index: AtomicI64::new(-1),
            position_secs: AtomicU64::new(0),
        }
    }
}

/// Music 模块状态：命令发送端 + 共享原子 + 封面目录。
pub struct MusicState {
    /// → 引擎线程。
    tx: Sender<AudioCommand>,
    /// 引擎线程接收端（在 `setup` 时取走交给引擎；用 Mutex<Option> 暂存）。
    rx_holder: std::sync::Mutex<Option<crossbeam_channel::Receiver<AudioCommand>>>,
    pub shared: Arc<SharedPlayback>,
    /// 封面落盘目录（workspace/music/covers）。
    covers_dir: PathBuf,
}

impl MusicState {
    /// 只建 channel + 共享原子（引擎线程在 `setup` 里启动，需 `AppHandle`）。
    pub fn new(workspace: &std::path::Path) -> Arc<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<AudioCommand>();
        let covers_dir = workspace.join("music").join("covers");
        Arc::new(Self {
            tx,
            rx_holder: std::sync::Mutex::new(Some(rx)),
            shared: Arc::new(SharedPlayback::default()),
            covers_dir,
        })
    }

    /// 发命令到引擎（unbounded channel，正常不会失败；失败仅当引擎线程已退出）。
    fn send(&self, cmd: AudioCommand) {
        match self.tx.try_send(cmd) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                tracing::error!(target: "music", "音频引擎线程已断开，命令丢弃");
            }
        }
    }
}

/// 在 `.setup()` 里启动引擎线程，注入 `AppHandle`（emit 用）。
pub fn setup(app: &tauri::AppHandle, state: Arc<MusicState>) -> Result<()> {
    let rx = state
        .rx_holder
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| anyhow::anyhow!("音乐引擎已初始化"))?;
    let shared = state.shared.clone();
    let app_handle = app.clone();
    let covers_dir = state.covers_dir.clone();
    std::fs::create_dir_all(&covers_dir).ok();

    std::thread::Builder::new()
        .name("music-engine".into())
        .spawn(move || {
            engine::run(engine::EngineContext {
                rx,
                shared,
                app: app_handle,
                covers_dir,
            });
        })?;
    info!(target: "music", "音乐引擎线程已派生");

    // 默认输出设备变更监听（换蓝牙耳机等）→ 引擎原位重建 sink 跟过去。
    sink::device_watch::spawn(state.tx.clone());
    Ok(())
}

// ─────────────────────────── Tauri 命令（前端冻结契约）───────────────────────────

/// 弹系统文件夹选择器，返回所选目录绝对路径（取消则 `None`）。
#[tauri::command]
pub async fn music_pick_folder(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |f| {
        let _ = tx.send(f);
    });
    match rx.await {
        Ok(Some(fp)) => fp
            .into_path()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// 递归扫描给定目录，返回曲目列表（lofty 元数据 + 封面落盘）。
#[tauri::command]
pub async fn music_scan(state: State<'_, AppState>, dir: String) -> Result<Vec<Track>, String> {
    let covers_dir = state.music.covers_dir.clone();
    let dir_path = PathBuf::from(dir);
    // 扫描可能耗时（大目录 + lofty 解析），放阻塞线程池。
    tauri::async_runtime::spawn_blocking(move || {
        scan::scan_dir(&dir_path, &covers_dir).map_err(|e| format!("扫描失败: {e}"))
    })
    .await
    .map_err(|e| format!("扫描任务异常: {e}"))?
}

/// 播放给定路径队列，从 `start` 开始。
#[tauri::command]
pub fn music_play_queue(state: State<'_, AppState>, paths: Vec<String>, start: usize) {
    state.music.send(AudioCommand::PlayQueue {
        tracks: paths,
        start,
    });
}

#[tauri::command]
pub fn music_pause(state: State<'_, AppState>) {
    state.music.send(AudioCommand::Pause);
}

#[tauri::command]
pub fn music_resume(state: State<'_, AppState>) {
    state.music.send(AudioCommand::Resume);
}

#[tauri::command]
pub fn music_toggle(state: State<'_, AppState>) {
    state.music.send(AudioCommand::TogglePlay);
}

#[tauri::command]
pub fn music_stop(state: State<'_, AppState>) {
    state.music.send(AudioCommand::Stop);
}

#[tauri::command]
pub fn music_seek(state: State<'_, AppState>, secs: f64) {
    state.music.send(AudioCommand::Seek { secs });
}

#[tauri::command]
pub fn music_next(state: State<'_, AppState>) {
    state.music.send(AudioCommand::Next);
}

#[tauri::command]
pub fn music_prev(state: State<'_, AppState>) {
    state.music.send(AudioCommand::Prev);
}

#[tauri::command]
pub fn music_set_volume(state: State<'_, AppState>, vol: f32) {
    state.music.send(AudioCommand::SetVolume(vol));
}

#[tauri::command]
pub fn music_set_repeat(state: State<'_, AppState>, mode: String) {
    state
        .music
        .send(AudioCommand::SetRepeat(RepeatMode::from_str(&mode)));
}

#[tauri::command]
pub fn music_set_shuffle(state: State<'_, AppState>, on: bool) {
    state.music.send(AudioCommand::SetShuffle(on));
}

/// 切换输出模式：`"auto"`=独占 bit-perfect 优先；`"shared"`=强制共享+重采样（兼容/音量可调）。
#[tauri::command]
pub fn music_set_output_mode(state: State<'_, AppState>, mode: String) {
    state
        .music
        .send(AudioCommand::SetOutputMode(OutputMode::from_str(&mode)));
}

/// 拉完整播放状态快照（首屏/自愈）。经引擎线程回传，保证一致性；引擎不可达时回退原子近似。
#[tauri::command]
pub fn music_get_state(state: State<'_, AppState>) -> PlaybackState {
    let (reply_tx, reply_rx) = crossbeam_channel::bounded::<PlaybackState>(1);
    state.music.send(AudioCommand::Snapshot(reply_tx));
    match reply_rx.recv_timeout(Duration::from_millis(500)) {
        Ok(snap) => snap,
        Err(_) => fallback_state(&state.music.shared),
    }
}

/// 引擎无响应时用共享原子拼一个近似快照（自愈兜底）。
fn fallback_state(shared: &SharedPlayback) -> PlaybackState {
    let status = match shared.status.load(Ordering::Relaxed) {
        1 => "playing",
        2 => "paused",
        _ => "stopped",
    };
    PlaybackState {
        status: status.to_string(),
        index: shared.index.load(Ordering::Relaxed),
        track: None,
        position_secs: shared.position_secs.load(Ordering::Relaxed) as f64 / 1000.0,
        duration_secs: 0.0,
        volume: 1.0,
        repeat: "off".to_string(),
        shuffle: false,
    }
}
