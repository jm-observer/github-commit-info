//! Recording command surface (remote-only client).
//!
//! Recognition runs on the GB10 orchestrator; this module only owns
//! mic capture plumbing and the start/stop/clear/state Tauri commands.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use cpal::traits::DeviceTrait;
use cpal::SampleFormat;
use tauri::State;
use tracing::info;

use crate::app_state::AppState;
use crate::modules::speech::lock_utils::write_lock;
use crate::shared::trace as tr;

#[derive(serde::Serialize, Clone)]
pub struct RecordingState {
    pub recording: bool,
}

#[tauri::command]
pub async fn speech_start_recording(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    info!(target: "speech", "[speech_start_recording]");
    let mut span = tr::CommandSpan::start(
        "speech_start_recording",
        serde_json::json!({"action": "start_recording"}),
    );
    let speech = state.speech.clone();
    if speech.recording.swap(true, Ordering::SeqCst) {
        return Err(span.fail("Already recording".to_string()));
    }

    // 局域网/外网活动路径若配了 ASR 地址，优先用它（与 g10 代理同节点同步切换），并写回
    // SpeechState.remote_url 让 history / base 推导保持一致；未配则回退用户在语音面板自存的地址。
    let resolved = state.net.resolve(&state.workspace).await;
    if !resolved.asr_url.is_empty() {
        *write_lock(&speech.remote_url) = resolved.asr_url.clone();
    }
    let Some(url) =
        crate::modules::speech::commands::remote::remote_url_from_state(&speech.remote_url)
    else {
        speech.recording.store(false, Ordering::SeqCst);
        return Err(span.fail("远程识别地址未配置(请在控制面板里设置)".to_string()));
    };
    info!(target: "speech", "[speech_start_recording] remote mode -> {url} (picked={:?})", resolved.picked);
    speech.stop_signal.store(false, Ordering::Relaxed);
    speech.init_status.store(1, Ordering::Relaxed);
    *write_lock(&speech.init_error) = String::new();

    // 通知前端录音已启动（状态栏指示灯用）。
    {
        use tauri::Emitter;
        let _ = app.emit(
            "speech_recording_state_changed",
            serde_json::json!({"recording": true}),
        );
    }

    let selected_device = Arc::clone(&speech.selected_device);
    let settings = Arc::clone(&speech.settings);
    let llm_settings = Arc::clone(&speech.llm_settings);
    let stop_signal = Arc::clone(&speech.stop_signal);
    let recording = Arc::clone(&speech.recording);
    let init_status = Arc::clone(&speech.init_status);
    let init_error = Arc::clone(&speech.init_error);
    let app2 = app.clone();

    tauri::async_runtime::spawn(async move {
        crate::modules::speech::commands::remote::run_remote_session(
            url,
            app2,
            selected_device,
            settings,
            llm_settings,
            stop_signal,
            recording,
            init_status,
            init_error,
        )
        .await;
    });
    Ok(())
}

/// Build a cpal input stream that pushes mono f32 frames into `tx`.
pub(crate) fn build_input_stream(
    device: &cpal::Device,
    tx: mpsc::Sender<Vec<f32>>,
    received_audio: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let supported = device
        .default_input_config()
        .map_err(|e| format!("No input config: {e}"))?;
    let config = supported.config();
    let sample_format = supported.sample_format();
    let channels = config.channels as usize;
    if channels == 0 {
        return Err("Device reports 0 channels".to_string());
    }

    info!(
        target: "speech",
        "[mic] format: {:?}, channels: {}, sample_rate: {}",
        sample_format, channels, config.sample_rate.0
    );

    let err_fn = |err| info!(target: "speech", "[mic] stream error: {:?}", err);

    let stream = match sample_format {
        SampleFormat::F32 => device
            .build_input_stream(
                &config,
                move |data: &[f32], _| {
                    if data.is_empty() {
                        return;
                    }
                    if !received_audio.swap(true, Ordering::Relaxed) {
                        info!(target: "speech", "[mic] first audio callback received");
                    }
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
                        .collect();
                    let _ = tx.send(mono);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("Build F32 stream: {e}"))?,

        SampleFormat::I16 => device
            .build_input_stream(
                &config,
                move |data: &[i16], _| {
                    if data.is_empty() {
                        return;
                    }
                    if !received_audio.swap(true, Ordering::Relaxed) {
                        info!(target: "speech", "[mic] first audio callback received");
                    }
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| {
                            frame
                                .iter()
                                .map(|&s| s as f32 / i16::MAX as f32)
                                .sum::<f32>()
                                / channels as f32
                        })
                        .collect();
                    let _ = tx.send(mono);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("Build I16 stream: {e}"))?,

        SampleFormat::U16 => device
            .build_input_stream(
                &config,
                move |data: &[u16], _| {
                    if data.is_empty() {
                        return;
                    }
                    if !received_audio.swap(true, Ordering::Relaxed) {
                        info!(target: "speech", "[mic] first audio callback received");
                    }
                    let mono: Vec<f32> = data
                        .chunks(channels)
                        .map(|frame| {
                            frame
                                .iter()
                                .map(|&s| (s as f32 - 32768.0) / 32768.0)
                                .sum::<f32>()
                                / channels as f32
                        })
                        .collect();
                    let _ = tx.send(mono);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("Build U16 stream: {e}"))?,

        other => return Err(format!("Unsupported sample format: {:?}", other)),
    };

    Ok(stream)
}

#[tauri::command]
pub fn speech_stop_recording(app: tauri::AppHandle, state: State<'_, AppState>) {
    info!(target: "speech", "[speech_stop_recording] signalling stop");
    state.speech.stop_signal.store(true, Ordering::Relaxed);
    use tauri::Emitter;
    let _ = app.emit(
        "speech_recording_state_changed",
        serde_json::json!({"recording": false}),
    );
}

#[tauri::command]
pub fn speech_clear_results(state: State<'_, AppState>) -> Result<(), String> {
    info!(target: "speech", "[speech_clear_results]");
    if state.speech.recording.load(Ordering::SeqCst) {
        return Err("Cannot clear while recording".to_string());
    }
    Ok(())
}

#[tauri::command]
pub fn speech_get_recording_state(state: State<'_, AppState>) -> Result<RecordingState, String> {
    let recording = state.speech.recording.load(Ordering::Relaxed);
    Ok(RecordingState { recording })
}
