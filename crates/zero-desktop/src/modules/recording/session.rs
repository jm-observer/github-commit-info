//! 录制会话状态机：`idle → recording ⇄ paused → idle`。
//!
//! 进程内**同时只允许一场录制**（全局 [`SESSION`]）。理由不是实现方便，而是语义：
//! 热键是「按一下开、按一下关」，悬浮控制条也只有一条；允许并发会让「现在按停止是停哪一场」
//! 变成一个用户答不上来的问题。
//!
//! 线程模型：一条抓帧线程独占 GDI 资源与 ffmpeg 的 stdin，主线程只通过两个
//! [`AtomicBool`]（stop / paused）跟它通信。抓帧线程绝不回调主线程，避免热键回调
//! ↔ 抓帧线程之间出现锁序问题。
//!
//! **补帧策略**：喂给 ffmpeg 的是定帧率裸流，第 N 帧就代表第 N/fps 秒。抓屏偶尔卡一下
//! （切窗、GPU 忙）如果只是「少写几帧」，整段视频就会越录越快、和实际时长对不上。所以
//! 每轮按墙钟算出「此刻本应写到第几帧」，落后就用刚抓到的这帧补齐——静止画面重复帧
//! 对 x264 几乎不花码率，换来时间轴始终正确。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;

use super::output::RecordingMeta;
use super::{RecordingSettings, Rect};

/// 单轮最多补几帧。卡顿几秒后不该一口气灌上百帧把 ffmpeg 顶死，
/// 宁可这段时间轴稍微压缩一点。
const MAX_CATCHUP: u64 = 4;

/// 抓帧线程空转时的最小睡眠，别把一个核心烧在忙等上。
const IDLE_SLEEP: Duration = Duration::from_millis(4);

static SESSION: OnceLock<Mutex<Option<Active>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Active>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

/// 正在进行（或刚刚自行终止）的一场录制。
struct Active {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    /// 抓帧线程每轮回写的已录时长（不含暂停），供状态查询读取。
    elapsed_ms: Arc<AtomicU64>,
    /// 抓帧线程是否已经结束（ffmpeg 崩了会自行结束，不等停止指令）。
    finished: Arc<AtomicBool>,
    path: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    handle: std::thread::JoinHandle<Result<RecordingMeta, String>>,
}

/// 录制状态（给前端轮询用）。
#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatus {
    /// `idle` / `recording` / `paused`。
    pub state: &'static str,
    /// 已录时长（毫秒，不含暂停）。
    pub elapsed_ms: u64,
    /// 输出文件绝对路径（idle 时为空串）。
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl RecordingStatus {
    fn idle() -> Self {
        Self {
            state: "idle",
            elapsed_ms: 0,
            path: String::new(),
            width: 0,
            height: 0,
            fps: 0,
        }
    }
}

/// 当前状态。抓帧线程已自行结束（ffmpeg 挂了）时对外即 `idle`——
/// 悬浮控制条据此自动关掉，而不是停在一个永远不动的计时器上。
pub fn status() -> RecordingStatus {
    let guard = slot().lock().unwrap();
    match guard.as_ref() {
        Some(a) if !a.finished.load(Ordering::SeqCst) => RecordingStatus {
            state: if a.paused.load(Ordering::SeqCst) {
                "paused"
            } else {
                "recording"
            },
            elapsed_ms: a.elapsed_ms.load(Ordering::SeqCst),
            path: a.path.to_string_lossy().into_owned(),
            width: a.width,
            height: a.height,
            fps: a.fps,
        },
        _ => RecordingStatus::idle(),
    }
}

/// 是否有一场活着的录制（用于热键 toggle 判向）。
pub fn is_active() -> bool {
    let guard = slot().lock().unwrap();
    matches!(guard.as_ref(), Some(a) if !a.finished.load(Ordering::SeqCst))
}

/// 暂停 / 继续。返回切换后的状态串。
pub fn set_paused(paused: bool) -> Result<&'static str, String> {
    let guard = slot().lock().unwrap();
    match guard.as_ref() {
        Some(a) if !a.finished.load(Ordering::SeqCst) => {
            a.paused.store(paused, Ordering::SeqCst);
            Ok(if paused { "paused" } else { "recording" })
        }
        _ => Err("当前没有正在进行的录制".to_string()),
    }
}

/// 停止录制：置停止位 → 等抓帧线程收尾（关 stdin、等 ffmpeg 封装完成）→ 写 sidecar。
/// 返回落盘路径。
pub fn stop() -> Result<PathBuf, String> {
    let active = slot().lock().unwrap().take();
    let Some(active) = active else {
        return Err("当前没有正在进行的录制".to_string());
    };
    active.stop.store(true, Ordering::SeqCst);
    // 一并解除暂停：暂停态下的线程在等待循环里，靠这一下让它立刻看到停止位。
    active.paused.store(false, Ordering::SeqCst);

    let path = active.path.clone();
    match active.handle.join() {
        Ok(Ok(meta)) => {
            super::output::write_meta(&path, &meta);
            Ok(path)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("录制线程异常退出".to_string()),
    }
}

/// 开始录制指定矩形。已有录制在进行时直接报错（由调用方决定是先停还是拒绝）。
///
/// 宽高会被向下对齐到偶数：`yuv420p` 的色度是 2×2 抽样，奇数边长 libx264 直接拒绝编码。
#[cfg(windows)]
pub fn start(
    rect: Rect,
    settings: &RecordingSettings,
    out_path: PathBuf,
) -> Result<RecordingStatus, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut guard = slot().lock().unwrap();
    if matches!(guard.as_ref(), Some(a) if !a.finished.load(Ordering::SeqCst)) {
        return Err("已有录制正在进行".to_string());
    }
    // 上一场已自行结束的残留，这里顺手清掉（它的线程早已 join 不到人管）。
    *guard = None;

    let rect = Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width & !1,
        height: rect.height & !1,
    };
    if rect.width < 16 || rect.height < 16 {
        return Err("录制区域太小（至少 16×16）".to_string());
    }

    let exe = super::ffmpeg::resolve(&settings.ffmpeg_path).map_err(|e| e.to_string())?;
    let fps = settings.fps.max(1);
    let args = super::ffmpeg::encode_args(
        rect.width as u32,
        rect.height as u32,
        fps,
        settings.crf,
        &out_path,
    );

    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let elapsed_ms = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let t_stop = stop.clone();
    let t_paused = paused.clone();
    let t_elapsed = elapsed_ms.clone();
    let t_finished = finished.clone();
    let capture_cursor = settings.capture_cursor;

    let handle = std::thread::Builder::new()
        .name("recording-capture".into())
        .spawn(move || -> Result<RecordingMeta, String> {
            // 收尾必做：无论从哪条路径退出，都要标记 finished，否则状态会永远停在 recording。
            struct FinishGuard(Arc<AtomicBool>);
            impl Drop for FinishGuard {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _finish = FinishGuard(t_finished);

            // GDI 资源必须在本线程建、本线程销毁。
            let mut grabber = match super::capture::Grabber::new(rect, capture_cursor) {
                Ok(g) => g,
                Err(e) => {
                    let msg = format!("初始化抓屏失败: {e}");
                    let _ = ready_tx.send(Err(msg.clone()));
                    return Err(msg);
                }
            };

            let mut cmd = Command::new(&exe);
            cmd.args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            super::ffmpeg::hide_console(&mut cmd);
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("启动 ffmpeg 失败: {e}");
                    let _ = ready_tx.send(Err(msg.clone()));
                    return Err(msg);
                }
            };

            // ffmpeg 的报错在 stderr，且管道不读会写满阻塞它自己。单开一条线程收着，
            // 出错时才有话可说（`-loglevel error` 下正常录制几乎不产生输出）。
            let stderr = child.stderr.take();
            let stderr_thread = stderr.map(|mut s| {
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut buf = String::new();
                    let _ = s.read_to_string(&mut buf);
                    buf
                })
            });

            let mut stdin = match child.stdin.take() {
                Some(s) => s,
                None => {
                    let msg = "无法取得 ffmpeg 标准输入".to_string();
                    let _ = ready_tx.send(Err(msg.clone()));
                    let _ = child.kill();
                    return Err(msg);
                }
            };

            let _ = ready_tx.send(Ok(()));

            let frame_bytes = grabber.frame_bytes();
            let mut buf = vec![0u8; frame_bytes];

            let clock_start = std::time::Instant::now();
            let mut paused_total = Duration::ZERO;
            let mut pause_started: Option<std::time::Instant> = None;
            let mut frames: u64 = 0;
            let mut fail: Option<String> = None;

            loop {
                if t_stop.load(Ordering::SeqCst) {
                    break;
                }
                if t_paused.load(Ordering::SeqCst) {
                    if pause_started.is_none() {
                        pause_started = Some(std::time::Instant::now());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
                if let Some(t) = pause_started.take() {
                    paused_total += t.elapsed();
                }

                let elapsed = clock_start.elapsed().saturating_sub(paused_total);
                t_elapsed.store(elapsed.as_millis() as u64, Ordering::SeqCst);

                // 此刻「本应已写到」的帧数。
                let target = (elapsed.as_secs_f64() * fps as f64).floor() as u64 + 1;
                if frames >= target {
                    std::thread::sleep(IDLE_SLEEP);
                    continue;
                }

                if let Err(e) = grabber.grab_into(&mut buf) {
                    fail = Some(format!("抓帧失败: {e}"));
                    break;
                }
                let need = (target - frames).min(MAX_CATCHUP);
                let mut broken = false;
                for _ in 0..need {
                    if let Err(e) = stdin.write_all(&buf) {
                        // 多半是 ffmpeg 自己退了（磁盘满 / 参数不支持），真实原因在 stderr。
                        fail = Some(format!("写入 ffmpeg 失败: {e}"));
                        broken = true;
                        break;
                    }
                    frames += 1;
                }
                if broken {
                    break;
                }
            }

            if let Some(t) = pause_started.take() {
                paused_total += t.elapsed();
            }
            let duration_ms = clock_start
                .elapsed()
                .saturating_sub(paused_total)
                .as_millis() as u64;

            // 关 stdin = 告诉 ffmpeg「没有更多帧了」，它据此写完 moov 正常退出。
            // 这一步不能省，否则文件缺尾、播放器打不开。
            drop(stdin);
            let status = child.wait();
            let stderr_text = stderr_thread
                .and_then(|t| t.join().ok())
                .unwrap_or_default();

            if let Some(msg) = fail {
                let detail = stderr_text.trim();
                return Err(if detail.is_empty() {
                    msg
                } else {
                    format!("{msg}；ffmpeg: {detail}")
                });
            }
            match status {
                Ok(s) if s.success() => Ok(RecordingMeta {
                    duration_ms,
                    width: rect.width as u32,
                    height: rect.height as u32,
                    fps,
                    frames,
                }),
                Ok(s) => Err(format!(
                    "ffmpeg 退出码 {}；{}",
                    s.code().unwrap_or(-1),
                    stderr_text.trim()
                )),
                Err(e) => Err(format!("等待 ffmpeg 结束失败: {e}")),
            }
        })
        .map_err(|e| format!("启动录制线程失败: {e}"))?;

    // 等线程走完「建资源 + 起 ffmpeg」，失败要当场返回给用户，而不是让悬浮条先弹出来
    // 再莫名其妙消失。
    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = handle.join();
            return Err(e);
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            return Err("录制启动超时".to_string());
        }
    }

    let status = RecordingStatus {
        state: "recording",
        elapsed_ms: 0,
        path: out_path.to_string_lossy().into_owned(),
        width: rect.width as u32,
        height: rect.height as u32,
        fps,
    };
    *guard = Some(Active {
        stop,
        paused,
        elapsed_ms,
        finished,
        path: out_path,
        width: rect.width as u32,
        height: rect.height as u32,
        fps,
        handle,
    });
    Ok(status)
}

#[cfg(not(windows))]
pub fn start(
    _rect: Rect,
    _settings: &RecordingSettings,
    _out_path: PathBuf,
) -> Result<RecordingStatus, String> {
    Err("录屏功能仅支持 Windows".to_string())
}
