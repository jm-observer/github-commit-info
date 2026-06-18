//! Remote ASR session.
//!
//! Recording streams mic PCM to the GB10 orchestrator over WebSocket.
//! The orchestrator URL is held in `SpeechState.remote_url` and edited
//! from the desktop UI (persisted as `remote.url` in SQLite).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, NaiveDateTime};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures_util::{SinkExt, StreamExt};
use tauri::Emitter;
use tauri::State;
use tokio::sync::mpsc as tok_mpsc;
use tokio_tungstenite::tungstenite::Message;

use std::sync::RwLock;

use tauri_plugin_clipboard_manager::ClipboardExt;
use tracing::{error, info, warn};

use crate::app_state::AppState;
use crate::modules::speech::commands::notify::bounce_tray_twice;
use crate::modules::speech::commands::recording::build_input_stream;
use crate::modules::speech::llm_settings::{AutoCopyMode, LlmSettings};
use crate::modules::speech::lock_utils::read_lock;
use crate::modules::speech::settings::VadSettings;

/// Target sample rate for the upstream PCM the orchestrator expects.
const SAMPLE_RATE: u32 = 16_000;

struct AutoCopyAccum {
    t_end: f64,
    text: String,
    ref_id: i64,
    prefix: String,
}

/// 自动粘贴（直接打字进焦点框）的极简状态。与自动复制的累加器相互独立：
/// 复制走「完整拼接文本进剪贴板」供手动 Ctrl+V 兜底，粘贴走「逐段增量直接输入」。
#[derive(Default)]
struct AutoPasteState {
    /// 已自动输入过的段落 id，保证同一段只输入一次（再次优化只更新剪贴板，不重复打字，也不覆盖）。
    typed_ids: std::collections::HashSet<i64>,
    /// 上一段自动输入的结束时刻，用于判定续接（决定英文模式是否补分隔空格）。
    last_t_end: Option<f64>,
}

/// 把一段识别结果直接打字进当前焦点输入框。同段去重、续接时英文补空格。
fn auto_paste_segment(
    ap: &mut AutoPasteState,
    text: &str,
    ref_id: i64,
    t_start: f64,
    t_end: f64,
    window: Duration,
    space_separator: bool,
) {
    if text.is_empty() {
        return;
    }
    // 同一段落只输入一次：再次优化不重复打字，也不做（已商定放弃的）覆盖。
    if !ap.typed_ids.insert(ref_id) {
        return;
    }
    let continues = ap
        .last_t_end
        .is_some_and(|prev_end| (t_start - prev_end) < window.as_secs_f64());
    ap.last_t_end = Some(t_end);
    let payload = if continues && space_separator {
        format!(" {text}")
    } else {
        text.to_string()
    };
    let typed = crate::modules::speech::paste_watch::type_text_to_foreground(&payload);
    info!(
        target: "speech",
        "[remote] auto paste ref={ref_id} typed={typed} chars={}",
        payload.chars().count()
    );
}

fn strip_overlap_prefix(head: &str, tail: &str) -> String {
    const MAX_OVERLAP_CHARS: usize = 200;
    const MIN_OVERLAP_CHARS: usize = 2;
    let head_chars: Vec<char> = head.chars().collect();
    let tail_chars: Vec<char> = tail.chars().collect();
    let max_k = head_chars
        .len()
        .min(tail_chars.len())
        .min(MAX_OVERLAP_CHARS);
    if max_k < MIN_OVERLAP_CHARS {
        return tail.to_string();
    }
    for k in (MIN_OVERLAP_CHARS..=max_k).rev() {
        if head_chars[head_chars.len() - k..] == tail_chars[..k] {
            return tail_chars[k..].iter().collect();
        }
    }
    tail.to_string()
}

fn join_dedup(head: &str, tail: &str) -> String {
    let rest = strip_overlap_prefix(head, tail);
    if rest.is_empty() {
        head.to_string()
    } else if rest.chars().count() == tail.chars().count() {
        format!("{} {}", head, tail)
    } else {
        format!("{}{}", head, rest)
    }
}

fn next_clipboard_text(
    acc: &mut Option<AutoCopyAccum>,
    text: &str,
    ref_id: i64,
    t_start: f64,
    t_end: f64,
    window: Duration,
) -> String {
    let window_secs = window.as_secs_f64();
    let (prefix, merged) = match acc.as_ref() {
        Some(prev) if prev.ref_id == ref_id => {
            if prev.prefix.is_empty() {
                (String::new(), text.to_string())
            } else {
                (prev.prefix.clone(), join_dedup(&prev.prefix, text))
            }
        }
        Some(prev) if (t_start - prev.t_end) < window_secs && !prev.text.is_empty() => {
            (prev.text.clone(), join_dedup(&prev.text, text))
        }
        _ => (String::new(), text.to_string()),
    };
    *acc = Some(AutoCopyAccum {
        t_end,
        text: merged.clone(),
        ref_id,
        prefix,
    });
    merged
}

fn add_seconds_to_wall(wall: &str, secs: f64) -> String {
    if secs.is_nan() || secs <= 0.0 {
        return wall.to_string();
    }
    let Ok(dt) = NaiveDateTime::parse_from_str(wall, "%Y-%m-%d %H:%M:%S") else {
        return wall.to_string();
    };
    let added = dt + chrono::Duration::seconds(secs.round() as i64);
    added.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Returns the configured remote orchestrator URL from state, if non-empty.
pub(crate) fn remote_url_from_state(remote_url: &RwLock<String>) -> Option<String> {
    let v = read_lock(remote_url).clone();
    if v.trim().is_empty() {
        None
    } else {
        Some(v)
    }
}

pub(crate) fn remote_http_base_from_state(remote_url: &RwLock<String>) -> Option<String> {
    let ws = remote_url_from_state(remote_url)?;
    let (scheme, rest) = if let Some(r) = ws.strip_prefix("wss://") {
        ("https://", r)
    } else if let Some(r) = ws.strip_prefix("ws://") {
        ("http://", r)
    } else {
        return None;
    };
    let host = rest.split_once('/').map(|(h, _)| h).unwrap_or(rest);
    Some(format!("{scheme}{host}"))
}

/// Fetch recent transcribed segments from the orchestrator's `/api/history`.
#[tauri::command]
pub async fn speech_fetch_remote_history(
    limit: u32,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    use crate::shared::trace as tr;
    use custom_utils::trace::{self, TraceContext};

    let mut span = tr::CommandSpan::start(
        "speech_fetch_remote_history",
        serde_json::json!({"limit": limit}),
    );
    let speech = state.speech.clone();
    let Some(base) = remote_http_base_from_state(&speech.remote_url) else {
        return Err(span.fail("远程识别地址未配置".to_string()));
    };
    let lim = limit.clamp(1, 200);
    let url = format!("{base}/api/history?limit={lim}");

    // A.4 traceparent 注入：HTTP GET 不走 WebSocket，注入 traceparent 接入 trace。
    let fetch_ctx = trace::enabled().then(TraceContext::root);
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(ctx) = &fetch_ctx {
        req = req.header("traceparent", ctx.to_traceparent());
    }
    let resp = req.send().await.map_err(|e| span.fail(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(span.fail(format!("history api status {}", resp.status())));
    }
    let body: Vec<serde_json::Value> = resp.json().await.map_err(|e| span.fail(e.to_string()))?;
    Ok(body)
}

/// Minimal stateful linear resampler (mono).
struct LinResampler {
    step: f64,
    pos: f64,
    last: f32,
    have_last: bool,
}

impl LinResampler {
    fn new(in_rate: f64, out_rate: f64) -> Self {
        Self {
            step: in_rate / out_rate,
            pos: 0.0,
            last: 0.0,
            have_last: false,
        }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        let mut buf: Vec<f32> = Vec::with_capacity(input.len() + 1);
        if self.have_last {
            buf.push(self.last);
        }
        buf.extend_from_slice(input);
        let mut out = Vec::with_capacity(((buf.len() as f64) / self.step) as usize + 1);
        while (self.pos as usize) + 1 < buf.len() {
            let i = self.pos as usize;
            let frac = self.pos - i as f64;
            let s = buf[i] as f64 * (1.0 - frac) + buf[i + 1] as f64 * frac;
            out.push(s as f32);
            self.pos += self.step;
        }
        self.last = *buf.last().unwrap();
        self.have_last = true;
        self.pos -= (buf.len() - 1) as f64;
        if self.pos < 0.0 {
            self.pos = 0.0;
        }
        out
    }
}

fn now_rfc3339() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[derive(Default, Clone)]
struct SegState {
    raw: String,
    opt: Option<String>,
    eng: Option<String>,
    sec: Option<String>,
    sec_kind: Option<String>,
    t0: f64,
    t1: f64,
    /// 权威墙上时钟,优先用 orchestrator 下发的 wall_start/wall_end;缺省回退本地推算。
    wall_start: String,
    wall_end: String,
    speaker: Option<String>,
    flashed: bool,
}

fn emit_state(app: &tauri::AppHandle, id: i64, s: &SegState) {
    let optimize_status = if s.opt.is_some() {
        "success"
    } else {
        "running"
    };
    let translate_status = if s.eng.is_some() {
        "success"
    } else {
        "running"
    };
    info!(
        target: "speech",
        "[remote][emit] id={id} raw={:?} opt={:?} eng={:?} sec={:?} t=[{:.2},{:.2}]",
        s.raw, s.opt, s.eng, s.sec, s.t0, s.t1
    );
    let _ = app.emit(
        "segment_updated",
        serde_json::json!({
            "id": id,
            "segment_id": id,
            "revision": id,
            "start_sec": s.t0,
            "end_sec": s.t1,
            "wall_start": s.wall_start,
            "wall_end": s.wall_end,
            "text_raw": s.raw,
            "optimize_status": optimize_status,
            "translate_status": translate_status,
            "text_optimized": s.opt,
            "text_english": s.eng,
            "text_secondary": s.sec,
            "secondary_kind": s.sec_kind,
            "speaker": s.speaker,
            "created_at": s.wall_start,
        }),
    );
}

/// 确保段的墙上时钟非空:orchestrator 未下发 wall 时(旧版/兜底)用本地时刻 + 音频时长推算。
fn ensure_wall_fallback(s: &mut SegState) {
    if s.wall_start.is_empty() {
        s.wall_start = now_rfc3339();
    }
    if s.wall_end.is_empty() {
        s.wall_end = add_seconds_to_wall(&s.wall_start, s.t1 - s.t0);
    }
}

fn spawn_capture(
    device_name: Option<String>,
    stop: Arc<AtomicBool>,
) -> Result<tok_mpsc::UnboundedReceiver<Vec<u8>>, String> {
    let (pcm_tx, pcm_rx) = tok_mpsc::unbounded_channel::<Vec<u8>>();

    std::thread::spawn(move || {
        let host = cpal::default_host();
        let device = match device_name {
            Some(name) => host
                .input_devices()
                .ok()
                .and_then(|mut it| it.find(|d| d.name().ok().as_deref() == Some(name.as_str()))),
            None => host.default_input_device(),
        };
        let Some(device) = device else {
            error!(target: "speech", "[remote] no input device");
            return;
        };
        let Ok(supported) = device.default_input_config() else {
            error!(target: "speech", "[remote] no input config");
            return;
        };
        let mic_rate = supported.sample_rate().0 as i32;
        let mut resampler = if mic_rate != SAMPLE_RATE as i32 {
            Some(LinResampler::new(mic_rate as f64, SAMPLE_RATE as f64))
        } else {
            None
        };

        let (tx, rx) = std_mpsc::channel::<Vec<f32>>();
        let received = Arc::new(AtomicBool::new(false));
        let stream = match build_input_stream(&device, tx, Arc::clone(&received)) {
            Ok(s) => s,
            Err(e) => {
                error!(target: "speech", "[remote] build stream: {e}");
                return;
            }
        };
        if let Err(e) = stream.play() {
            error!(target: "speech", "[remote] stream play: {e}");
            return;
        }
        info!(target: "speech", "[remote] capture started (mic {mic_rate} Hz -> {SAMPLE_RATE})");

        while !stop.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(frame) => {
                    let pcm16k: Vec<f32> = match resampler {
                        Some(ref mut r) => r.process(&frame),
                        None => frame,
                    };
                    let mut bytes = Vec::with_capacity(pcm16k.len() * 2);
                    for s in pcm16k {
                        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                    if pcm_tx.send(bytes).is_err() {
                        break;
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(stream);
        info!(target: "speech", "[remote] capture stopped");
    });

    Ok(pcm_rx)
}

#[derive(PartialEq)]
enum Outcome {
    Stopped,
    Disconnected,
}

const MAX_CONN_FAILS: u32 = 4;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_remote_session(
    url: String,
    app: tauri::AppHandle,
    selected_device: Arc<RwLock<Option<String>>>,
    settings: Arc<RwLock<VadSettings>>,
    llm_settings: Arc<RwLock<LlmSettings>>,
    stop_signal: Arc<AtomicBool>,
    recording: Arc<AtomicBool>,
    init_status: Arc<AtomicU8>,
    init_error: Arc<RwLock<String>>,
) {
    let device_name = read_lock(&selected_device).clone();
    let language = {
        let s = read_lock(&settings);
        if s.asr_language.is_empty() {
            "auto".to_string()
        } else {
            s.asr_language.clone()
        }
    };
    let want_secondary = read_lock(&llm_settings).want_secondary;
    let merge_window_ms = read_lock(&llm_settings).merge_window_ms;
    let stop = stop_signal;

    let mut pcm_rx = match spawn_capture(device_name, Arc::clone(&stop)) {
        Ok(rx) => rx,
        Err(e) => {
            error!(target: "speech", "[remote] capture init failed: {e}");
            *init_error.write().unwrap() = format!("麦克风初始化失败: {e}");
            init_status.store(2, Ordering::Relaxed);
            recording.store(false, Ordering::SeqCst);
            return;
        }
    };

    let hello = serde_json::json!({
        "type": "hello", "protocol": "1", "sample_rate": 16000,
        "format": "pcm_s16le", "language": language,
        "want_optimize": true, "want_translate": true,
        "want_secondary": want_secondary,
        "merge_window_ms": merge_window_ms,
    })
    .to_string();

    let mut fails: u32 = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => {
                fails = 0;
                info!(target: "speech", "[remote] connected {url}");
                let outcome =
                    run_one_connection(ws, &hello, &mut pcm_rx, &app, &llm_settings, &stop).await;
                if outcome == Outcome::Stopped || stop.load(Ordering::Relaxed) {
                    break;
                }
                warn!(target: "speech", "[remote] disconnected mid-session; reconnecting...");
            }
            Err(e) => {
                fails += 1;
                error!(target: "speech", "[remote] connect {url} failed ({fails}/{MAX_CONN_FAILS}): {e}");
                if fails >= MAX_CONN_FAILS {
                    *init_error.write().unwrap() = format!("无法连接识别服务 {url}: {e}");
                    init_status.store(2, Ordering::Relaxed);
                    break;
                }
                let backoff = Duration::from_secs(1u64 << fails.min(3));
                tokio::time::sleep(backoff).await;
            }
        }
    }

    stop.store(true, Ordering::Relaxed);
    recording.store(false, Ordering::SeqCst);
    info!(target: "speech", "[remote] session ended");
}

async fn run_one_connection(
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    hello: &str,
    pcm_rx: &mut tok_mpsc::UnboundedReceiver<Vec<u8>>,
    app: &tauri::AppHandle,
    llm_settings: &Arc<RwLock<LlmSettings>>,
    stop: &Arc<AtomicBool>,
) -> Outcome {
    let (mut wr, mut rd) = ws.split();
    if wr.send(Message::Text(hello.to_string())).await.is_err() {
        return Outcome::Disconnected;
    }

    let app_r = app.clone();
    let llm_settings_r = Arc::clone(llm_settings);
    let mut reader = tokio::spawn(async move {
        let mut segs: HashMap<i64, SegState> = HashMap::new();
        let mut copy_acc: Option<AutoCopyAccum> = None;
        let mut paste_state = AutoPasteState::default();
        while let Some(Ok(msg)) = rd.next().await {
            let Message::Text(t) = msg else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
                continue;
            };
            match v.get("type").and_then(|x| x.as_str()) {
                Some("ready") => info!(target: "speech", "[remote] session ready"),
                Some("segment") => {
                    let id = v.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
                    let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
                    let t0 = v.get("t_start").and_then(|x| x.as_f64());
                    let t1 = v.get("t_end").and_then(|x| x.as_f64());
                    info!(target: "speech", "[remote][segment] id={id} t=[{t0:?},{t1:?}] text={text:?}");
                    let st = segs.entry(id).or_default();
                    st.raw = text.to_string();
                    st.t0 = v.get("t_start").and_then(|x| x.as_f64()).unwrap_or(st.t0);
                    st.t1 = v.get("t_end").and_then(|x| x.as_f64()).unwrap_or(st.t1);
                    if let Some(sp) = v.get("speaker").and_then(|x| x.as_str()) {
                        st.speaker = Some(sp.to_string());
                    }
                    // 权威墙上时钟由 orchestrator 下发(会话锚点 + 音频偏移);缺省回退本地。
                    if let Some(w) = v.get("wall_start").and_then(|x| x.as_str()) {
                        st.wall_start = w.to_string();
                    }
                    if let Some(w) = v.get("wall_end").and_then(|x| x.as_str()) {
                        st.wall_end = w.to_string();
                    }
                    ensure_wall_fallback(st);
                    emit_state(&app_r, id, st);
                }
                Some("optimized") => {
                    let id = v.get("ref").and_then(|x| x.as_i64()).unwrap_or(0);
                    let text = v
                        .get("text")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    info!(target: "speech", "[remote][optimized] ref={id} text={text:?}");
                    let st = segs.entry(id).or_default();
                    ensure_wall_fallback(st);
                    st.opt = Some(text.clone());
                    emit_state(&app_r, id, st);
                    if !st.flashed && st.opt.is_some() && st.eng.is_some() {
                        st.flashed = true;
                        let play_beep = read_lock(&llm_settings_r).notify_sound;
                        bounce_tray_twice(&app_r, play_beep);
                    }
                    let (copy, window_ms) = {
                        let s = read_lock(&llm_settings_r);
                        (
                            matches!(s.auto_copy_mode, AutoCopyMode::OptimizedZh),
                            s.merge_window_ms,
                        )
                    };
                    if copy && !text.is_empty() {
                        // 用户上次粘贴后重新开始累加，避免重复粘贴已粘走的前一段。
                        if crate::modules::speech::paste_watch::take_paste_signal() {
                            copy_acc = None;
                        }
                        let merged = next_clipboard_text(
                            &mut copy_acc,
                            &text,
                            id,
                            st.t0,
                            st.t1,
                            Duration::from_millis(window_ms),
                        );
                        match app_r.clipboard().write_text(merged.clone()) {
                            Ok(_) => {
                                info!(target: "speech", "[remote] auto copy (优化中文) ref={id} chars={}", merged.chars().count())
                            }
                            Err(e) => {
                                error!(target: "speech", "[remote] clipboard 优化中文 failed: {e}")
                            }
                        }
                    }
                    // 自动粘贴（中文优化）：逐段直接打字进焦点框；中文不补分隔空格。
                    let (do_paste, paste_window_ms) = {
                        let s = read_lock(&llm_settings_r);
                        (
                            s.auto_paste && matches!(s.auto_copy_mode, AutoCopyMode::OptimizedZh),
                            s.merge_window_ms,
                        )
                    };
                    if do_paste {
                        auto_paste_segment(
                            &mut paste_state,
                            &text,
                            id,
                            st.t0,
                            st.t1,
                            Duration::from_millis(paste_window_ms),
                            false,
                        );
                    }
                }
                Some("translated") => {
                    let id = v.get("ref").and_then(|x| x.as_i64()).unwrap_or(0);
                    let text = v
                        .get("text")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    info!(target: "speech", "[remote][translated] ref={id} text={text:?}");
                    let st = segs.entry(id).or_default();
                    ensure_wall_fallback(st);
                    st.eng = Some(text.clone());
                    emit_state(&app_r, id, st);
                    if !st.flashed && st.opt.is_some() && st.eng.is_some() {
                        st.flashed = true;
                        let play_beep = read_lock(&llm_settings_r).notify_sound;
                        bounce_tray_twice(&app_r, play_beep);
                    }
                    let (copy, window_ms) = {
                        let s = read_lock(&llm_settings_r);
                        (
                            matches!(s.auto_copy_mode, AutoCopyMode::English),
                            s.merge_window_ms,
                        )
                    };
                    if copy && !text.is_empty() {
                        // 用户上次粘贴后重新开始累加，避免重复粘贴已粘走的前一段。
                        if crate::modules::speech::paste_watch::take_paste_signal() {
                            copy_acc = None;
                        }
                        let merged = next_clipboard_text(
                            &mut copy_acc,
                            &text,
                            id,
                            st.t0,
                            st.t1,
                            Duration::from_millis(window_ms),
                        );
                        match app_r.clipboard().write_text(merged.clone()) {
                            Ok(_) => {
                                info!(target: "speech", "[remote] auto copy (英文) ref={id} chars={}", merged.chars().count())
                            }
                            Err(e) => {
                                error!(target: "speech", "[remote] clipboard 英文 failed: {e}")
                            }
                        }
                    }
                    // 自动粘贴（英文翻译）：逐段直接打字进焦点框；续接段补一个分隔空格。
                    let (do_paste, paste_window_ms) = {
                        let s = read_lock(&llm_settings_r);
                        (
                            s.auto_paste && matches!(s.auto_copy_mode, AutoCopyMode::English),
                            s.merge_window_ms,
                        )
                    };
                    if do_paste {
                        auto_paste_segment(
                            &mut paste_state,
                            &text,
                            id,
                            st.t0,
                            st.t1,
                            Duration::from_millis(paste_window_ms),
                            true,
                        );
                    }
                }
                Some("secondary") => {
                    let id = v.get("ref").and_then(|x| x.as_i64()).unwrap_or(0);
                    let text = v
                        .get("text")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let kind = v.get("kind").and_then(|x| x.as_str()).map(str::to_string);
                    info!(target: "speech", "[remote][secondary] ref={id} kind={:?} text={text:?}", kind);
                    let st = segs.entry(id).or_default();
                    ensure_wall_fallback(st);
                    st.sec = Some(text);
                    st.sec_kind = kind;
                    emit_state(&app_r, id, st);
                }
                Some("error") => {
                    warn!(target: "speech", "[remote] server error: {}", v.get("message").and_then(|x| x.as_str()).unwrap_or(""));
                }
                Some("done") => {
                    info!(target: "speech", "[remote] server done");
                    break;
                }
                _ => {}
            }
        }
    });

    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = wr
                .send(Message::Text(r#"{"type":"stop"}"#.to_string()))
                .await;
            let _ = tokio::time::timeout(Duration::from_secs(20), &mut reader).await;
            return Outcome::Stopped;
        }
        if reader.is_finished() {
            return Outcome::Disconnected;
        }
        match tokio::time::timeout(Duration::from_millis(200), pcm_rx.recv()).await {
            Ok(Some(bytes)) => {
                if wr.send(Message::Binary(bytes)).await.is_err() {
                    reader.abort();
                    return Outcome::Disconnected;
                }
            }
            Ok(None) => {
                reader.abort();
                return Outcome::Disconnected;
            }
            Err(_) => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn first_call_writes_text_as_is() {
        let mut acc = None;
        let out = next_clipboard_text(&mut acc, "你好", 1, 0.0, 2.0, w(3000));
        assert_eq!(out, "你好");
        let a = acc.as_ref().unwrap();
        assert_eq!(a.text, "你好");
        assert_eq!(a.t_end, 2.0);
        assert_eq!(a.ref_id, 1);
    }

    #[test]
    fn merges_when_audio_gap_within_window() {
        let mut acc = None;
        next_clipboard_text(&mut acc, "你好", 1, 0.0, 2.0, w(3000));
        let out = next_clipboard_text(&mut acc, "世界", 2, 4.0, 6.0, w(3000));
        assert_eq!(out, "你好 世界");
        assert_eq!(acc.as_ref().unwrap().t_end, 6.0);
    }

    #[test]
    fn does_not_merge_when_audio_gap_exceeds_window() {
        let mut acc = None;
        next_clipboard_text(&mut acc, "A", 1, 0.0, 2.0, w(3000));
        let out = next_clipboard_text(&mut acc, "B", 2, 10.0, 11.0, w(3000));
        assert_eq!(out, "B");
    }

    #[test]
    fn zero_window_disables_merging() {
        let mut acc = None;
        next_clipboard_text(&mut acc, "A", 1, 0.0, 2.0, w(0));
        let out = next_clipboard_text(&mut acc, "B", 2, 2.0, 3.0, w(0));
        assert_eq!(out, "B");
    }

    #[test]
    fn chain_grows_across_many_segments() {
        let mut acc = None;
        next_clipboard_text(&mut acc, "一", 1, 0.0, 1.0, w(3000));
        next_clipboard_text(&mut acc, "二", 2, 1.5, 2.5, w(3000));
        let out = next_clipboard_text(&mut acc, "三", 3, 3.0, 4.0, w(3000));
        assert_eq!(out, "一 二 三");
    }

    #[test]
    fn dedup_no_overlap_falls_back_to_space_join() {
        assert_eq!(join_dedup("你好", "世界"), "你好 世界");
    }

    #[test]
    fn dedup_strips_repeated_tail_prefix() {
        let out = join_dedup("今天天气真好。", "今天天气真好。然后我们出门了。");
        assert_eq!(out, "今天天气真好。然后我们出门了。");
    }

    #[test]
    fn wall_end_adds_rounded_duration() {
        let out = add_seconds_to_wall("2026-05-27 15:42:46", 9.4);
        assert_eq!(out, "2026-05-27 15:42:55");
    }

    #[test]
    fn wall_end_zero_or_negative_returns_input() {
        assert_eq!(
            add_seconds_to_wall("2026-05-27 15:42:46", 0.0),
            "2026-05-27 15:42:46"
        );
    }
}
