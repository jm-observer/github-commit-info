//! Remote ASR session.
//!
//! Recording streams mic PCM to the GB10 orchestrator over WebSocket.
//! The orchestrator URL is held in `SpeechState.remote_url` and edited
//! from the desktop UI (persisted as `remote.url` in SQLite).

use std::collections::{HashMap, HashSet};
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
    /// 已自动输入过的段落 id。用于区分首次输入和同段累计文本的增量输入。
    typed_ids: HashSet<i64>,
    /// 每个 ref 已经自动输入到的完整文本。合并链模式下，同一 ref 会反复回发累计文本；
    /// 只有新文本以旧文本为前缀时才输入后缀，改写旧内容则保守跳过，避免盲目覆盖。
    typed_text_by_id: HashMap<i64, String>,
    /// 上一段自动输入的结束时刻，用于判定续接（决定英文模式是否补分隔空格）。
    last_t_end: Option<f64>,
}

/// `next_auto_paste_text` 的输出动作。
///
/// - `Type`: 直接打字（首次入段 / 同段严格追加后缀）。
/// - `Retype`: 同段被改写且开启「改写回退重打」时，先发 N 个退格回到公共前缀，再补打新尾巴。
#[derive(Debug, PartialEq)]
enum AutoPasteAction {
    Type(String),
    Retype { backspaces: usize, text: String },
}

#[derive(Debug, Clone, Copy)]
struct AutoPasteOptions {
    window: Duration,
    space_separator: bool,
    rewrite_retype: bool,
}

fn common_prefix_char_count(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn next_auto_paste_text(
    ap: &mut AutoPasteState,
    text: &str,
    ref_id: i64,
    t_start: f64,
    t_end: f64,
    options: AutoPasteOptions,
) -> Option<AutoPasteAction> {
    if text.is_empty() {
        return None;
    }
    if ap.typed_ids.contains(&ref_id) {
        let prev = ap.typed_text_by_id.get(&ref_id)?.clone();
        if let Some(suffix) = text.strip_prefix(prev.as_str()) {
            if suffix.is_empty() {
                return None;
            }
            ap.typed_text_by_id.insert(ref_id, text.to_string());
            ap.last_t_end = Some(t_end);
            return Some(AutoPasteAction::Type(suffix.to_string()));
        }
        // 同 ref 改写：旧文本不再是新文本前缀（LLM 把已说的部分修订成别的）。
        // 关闭开关时保守跳过，留给剪贴板兜底；开启时按公共前缀长度回退再补打。
        if !options.rewrite_retype {
            return None;
        }
        let common = common_prefix_char_count(&prev, text);
        let backspaces = prev.chars().count().saturating_sub(common);
        let new_suffix: String = text.chars().skip(common).collect();
        if backspaces == 0 && new_suffix.is_empty() {
            return None;
        }
        ap.typed_text_by_id.insert(ref_id, text.to_string());
        ap.last_t_end = Some(t_end);
        return Some(AutoPasteAction::Retype {
            backspaces,
            text: new_suffix,
        });
    }

    ap.typed_ids.insert(ref_id);
    ap.typed_text_by_id.insert(ref_id, text.to_string());
    let continues = ap
        .last_t_end
        .is_some_and(|prev_end| (t_start - prev_end) < options.window.as_secs_f64());
    ap.last_t_end = Some(t_end);
    Some(AutoPasteAction::Type(
        if continues && options.space_separator {
            format!(" {text}")
        } else {
            text.to_string()
        },
    ))
}

/// 把一段识别结果直接打字进当前焦点输入框。
///
/// 新 ref 首次输入完整文本；合并链模式下同 ref 后续回发累计文本时，只输入严格后缀。
/// 若同 ref 后续结果改写了已输入部分：默认保守跳过（剪贴板有最新整段供 Ctrl+V 兜底）；
/// `rewrite_retype=true` 时则按公共前缀长度发退格回退、再补打新尾巴。
fn auto_paste_segment(
    ap: &mut AutoPasteState,
    text: &str,
    ref_id: i64,
    t_start: f64,
    t_end: f64,
    options: AutoPasteOptions,
) {
    let Some(action) = next_auto_paste_text(ap, text, ref_id, t_start, t_end, options) else {
        return;
    };
    match action {
        AutoPasteAction::Type(payload) => {
            let typed = crate::modules::speech::paste_watch::type_text_to_foreground(&payload);
            info!(
                target: "speech",
                "[remote] auto paste ref={ref_id} typed={typed} chars={}",
                payload.chars().count()
            );
        }
        AutoPasteAction::Retype { backspaces, text } => {
            let typed =
                crate::modules::speech::paste_watch::type_text_with_backspaces_to_foreground(
                    backspaces, &text,
                );
            info!(
                target: "speech",
                "[remote] auto paste retype ref={ref_id} typed={typed} backspaces={backspaces} chars={}",
                text.chars().count()
            );
        }
    }
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
    // 去掉末尾 `/stream` 得到 HTTP 基址，**保留中间路径前缀**：合并后 asr_url 形如
    // `ws://host:8788/api/asr/stream` → `http://host:8788/api/asr`，于是 `{base}/api/history`
    // 命中 nest 在 /api/asr 下的 orchestrator 路由；旧独立 `ws://host:8090/stream` →
    // `http://host:8090`，`{base}/api/history` 仍命中（向后兼容）。
    let base = match rest.strip_suffix("/stream") {
        Some(b) => b,
        None => rest.split_once('/').map(|(h, _)| h).unwrap_or(rest),
    };
    Some(format!("{scheme}{base}"))
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

/// 把 16k mono f32 帧转成上游要的 pcm_s16le 字节。
fn pcm_to_bytes(pcm16k: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pcm16k.len() * 2);
    for &s in pcm16k {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
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
                    if pcm_tx.send(pcm_to_bytes(&pcm16k)).is_err() {
                        drop(stream);
                        info!(target: "speech", "[remote] capture stopped (downstream gone)");
                        return;
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        // 停止时排空 cpal 刚推进来、主循环还没读走的尾帧,否则录音停止前最后
        // ~100ms 的音频(最后一两个字)会随 drop(stream) 丢掉,导致尾字缺失。
        while let Ok(frame) = rx.try_recv() {
            let pcm16k: Vec<f32> = match resampler {
                Some(ref mut r) => r.process(&frame),
                None => frame,
            };
            if pcm_tx.send(pcm_to_bytes(&pcm16k)).is_err() {
                break;
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
/// 连上后存活不到这个时长就断开 → 视为「假性成功」，按连接失败计入退避计数。
/// 防止上游(如 orchestrator→FunASR)持续踢人时客户端无退避狂重连刷屏。
const MIN_STABLE_SESSION: Duration = Duration::from_secs(5);

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
                info!(target: "speech", "[remote] connected {url}");
                let conn_start = std::time::Instant::now();
                let outcome =
                    run_one_connection(ws, &hello, &mut pcm_rx, &app, &llm_settings, &stop).await;
                if outcome == Outcome::Stopped || stop.load(Ordering::Relaxed) {
                    break;
                }
                let lifetime = conn_start.elapsed();
                if lifetime < MIN_STABLE_SESSION {
                    // 连上后秒断 = 上游(orchestrator→FunASR)在踢人,按失败计入退避;否则
                    // 长会话偶尔断网,清零计数走快速重连。
                    fails += 1;
                    warn!(
                        target: "speech",
                        "[remote] disconnected after {}ms ({fails}/{MAX_CONN_FAILS}); upstream likely failing",
                        lifetime.as_millis()
                    );
                    if fails >= MAX_CONN_FAILS {
                        *init_error.write().unwrap() = format!(
                            "识别服务连上后立即断开({url})——上游 ASR 通常不可达,检查 G10 上的 FunASR (:9100) 与 toolkit-server 日志"
                        );
                        init_status.store(2, Ordering::Relaxed);
                        break;
                    }
                    let backoff = Duration::from_secs(1u64 << fails.min(3));
                    tokio::time::sleep(backoff).await;
                } else {
                    fails = 0;
                    warn!(target: "speech", "[remote] disconnected mid-session; reconnecting...");
                }
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
                    // 本地语音命令分发：
                    //   Whole（整段就是命令，如用户停顿后说"发送"）→ 执行动作并跳过剪贴板/粘贴。
                    //   Tail（命令挂在正文末尾，如"你好，发送"）→ 把 `text` 改成分隔符前的
                    //     正文走原链路；命令仅在 auto_paste 模式下追加（剪贴板模式下提前
                    //     回车会误提交空内容）。
                    // 仅在 OptimizedZh 模式下生效（其它模式优化稿不进剪贴板，无需拦截）。
                    let voice_cmd_on = {
                        let s = read_lock(&llm_settings_r);
                        s.voice_commands_enabled
                            && matches!(s.auto_copy_mode, AutoCopyMode::OptimizedZh)
                    };
                    let mut pending_tail_cmd: Option<
                        crate::modules::speech::voice_commands::VoiceCommand,
                    > = None;
                    let mut text = text;
                    if voice_cmd_on {
                        match crate::modules::speech::voice_commands::match_command(&text) {
                            Some(crate::modules::speech::voice_commands::CommandMatch::Whole(
                                cmd,
                            )) => {
                                let acted =
                                    crate::modules::speech::voice_commands::dispatch(cmd, &text);
                                if acted {
                                    // 命中且真的派发了按键 → 重置剪贴板累加（避免下一段把
                                    // 已被视为"命令"的这段当成续接拼回去），continue 跳过
                                    // 本段后续的剪贴板写入 / 自动粘贴。
                                    copy_acc = None;
                                    continue;
                                }
                                // 未派发（前台是本进程等）→ 落回原文走默认链路。
                            }
                            Some(crate::modules::speech::voice_commands::CommandMatch::Tail {
                                prefix,
                                command,
                            }) => {
                                info!(
                                    target: "speech",
                                    "[voice_cmd] tail matched cmd={command:?} prefix_chars={} raw={:?}",
                                    prefix.chars().count(),
                                    text
                                );
                                text = prefix;
                                pending_tail_cmd = Some(command);
                            }
                            None => {}
                        }
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
                    let (do_paste, paste_window_ms, rewrite_retype) = {
                        let s = read_lock(&llm_settings_r);
                        (
                            s.auto_paste && matches!(s.auto_copy_mode, AutoCopyMode::OptimizedZh),
                            s.merge_window_ms,
                            s.auto_paste_rewrite_retype,
                        )
                    };
                    if do_paste {
                        auto_paste_segment(
                            &mut paste_state,
                            &text,
                            id,
                            st.t0,
                            st.t1,
                            AutoPasteOptions {
                                window: Duration::from_millis(paste_window_ms),
                                space_separator: false,
                                rewrite_retype,
                            },
                        );
                    }
                    // 尾部命令：正文已通过 auto_paste 打进焦点框（do_paste 为真才会发生），
                    // 此时再补一次回车。剪贴板模式（do_paste=false）下不补 —— 用户尚未粘贴，
                    // 提前回车 = 提交空内容。
                    if let Some(cmd) = pending_tail_cmd {
                        if do_paste {
                            let _ = crate::modules::speech::voice_commands::dispatch(cmd, "<tail>");
                            copy_acc = None;
                        }
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
                    let (do_paste, paste_window_ms, rewrite_retype) = {
                        let s = read_lock(&llm_settings_r);
                        (
                            s.auto_paste && matches!(s.auto_copy_mode, AutoCopyMode::English),
                            s.merge_window_ms,
                            s.auto_paste_rewrite_retype,
                        )
                    };
                    if do_paste {
                        auto_paste_segment(
                            &mut paste_state,
                            &text,
                            id,
                            st.t0,
                            st.t1,
                            AutoPasteOptions {
                                window: Duration::from_millis(paste_window_ms),
                                space_separator: true,
                                rewrite_retype,
                            },
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
            // 采集线程同样观察 `stop`:它会 flush 尾帧后 drop 掉发送端,关闭 pcm_rx。
            // 发 stop(让服务端做最终切分)之前,把还在 pcm_rx 里排队的尾部 PCM 全部转发出去,
            // 否则途中的最后 ~100-300ms 音频(最后一两个字)会被丢弃、从转写里缺失。
            while let Ok(Some(bytes)) =
                tokio::time::timeout(Duration::from_millis(500), pcm_rx.recv()).await
            {
                if wr.send(Message::Binary(bytes)).await.is_err() {
                    reader.abort();
                    return Outcome::Stopped;
                }
            }
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

    fn auto_options(ms: u64, space_separator: bool, rewrite_retype: bool) -> AutoPasteOptions {
        AutoPasteOptions {
            window: w(ms),
            space_separator,
            rewrite_retype,
        }
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
    fn autopaste_same_ref_cumulative_text_types_suffix() {
        let mut ap = AutoPasteState::default();
        let first = next_auto_paste_text(
            &mut ap,
            "第一句。",
            1,
            0.0,
            1.0,
            auto_options(3000, false, false),
        );
        assert_eq!(first, Some(AutoPasteAction::Type("第一句。".into())));

        let second = next_auto_paste_text(
            &mut ap,
            "第一句。第二句。",
            1,
            0.0,
            2.0,
            auto_options(3000, false, false),
        );
        assert_eq!(second, Some(AutoPasteAction::Type("第二句。".into())));
    }

    #[test]
    fn autopaste_same_ref_identical_text_does_not_repeat() {
        let mut ap = AutoPasteState::default();
        next_auto_paste_text(
            &mut ap,
            "Hello",
            1,
            0.0,
            1.0,
            auto_options(3000, true, false),
        );
        let out = next_auto_paste_text(
            &mut ap,
            "Hello",
            1,
            0.0,
            1.0,
            auto_options(3000, true, false),
        );
        assert_eq!(out, None);
    }

    #[test]
    fn autopaste_same_ref_rewrite_is_skipped_when_retype_off() {
        let mut ap = AutoPasteState::default();
        next_auto_paste_text(
            &mut ap,
            "第一句。",
            1,
            0.0,
            1.0,
            auto_options(3000, false, false),
        );
        let out = next_auto_paste_text(
            &mut ap,
            "第一句改写。第二句。",
            1,
            0.0,
            2.0,
            auto_options(3000, false, false),
        );
        assert_eq!(out, None);
    }

    #[test]
    fn autopaste_same_ref_rewrite_retypes_after_common_prefix() {
        // 旧：「G10里面的这个Z2的服务，它原先要调用这个闹钟的那个。」(20 字)
        // 新：「G10里面的这个Z2的服务，它原先要调用这个闹钟的，闹钟本来是要…」
        // 公共前缀截止「调用这个闹钟的」(18 字)，旧尾巴「那个。」(3 字) 应回退，
        // 新尾巴「，闹钟本来是要…」补打。
        let mut ap = AutoPasteState::default();
        let prev = "G10里面的这个Z2的服务，它原先要调用这个闹钟的那个。";
        let next = "G10里面的这个Z2的服务，它原先要调用这个闹钟的，闹钟本来是要调那个的。";
        next_auto_paste_text(&mut ap, prev, 1, 0.0, 1.0, auto_options(3000, false, true));
        let out = next_auto_paste_text(&mut ap, next, 1, 0.0, 2.0, auto_options(3000, false, true));
        let common = common_prefix_char_count(prev, next);
        let expect_back = prev.chars().count() - common;
        let expect_tail: String = next.chars().skip(common).collect();
        assert_eq!(
            out,
            Some(AutoPasteAction::Retype {
                backspaces: expect_back,
                text: expect_tail,
            })
        );
    }

    #[test]
    fn autopaste_same_ref_identical_text_no_retype_action() {
        let mut ap = AutoPasteState::default();
        next_auto_paste_text(
            &mut ap,
            "Hello",
            1,
            0.0,
            1.0,
            auto_options(3000, true, true),
        );
        let out = next_auto_paste_text(
            &mut ap,
            "Hello",
            1,
            0.0,
            1.0,
            auto_options(3000, true, true),
        );
        assert_eq!(out, None);
    }

    #[test]
    fn autopaste_new_english_ref_continuation_gets_separator() {
        let mut ap = AutoPasteState::default();
        next_auto_paste_text(
            &mut ap,
            "Hello.",
            1,
            0.0,
            1.0,
            auto_options(3000, true, false),
        );
        let out = next_auto_paste_text(
            &mut ap,
            "World.",
            2,
            1.5,
            2.0,
            auto_options(3000, true, false),
        );
        assert_eq!(out, Some(AutoPasteAction::Type(" World.".into())));
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
