//! Windows WASAPI **独占 + event-driven** 输出——bit-perfect 的关键实现。
//!
//! 按源文件原始格式（采样率/位深/声道）申请 `IAudioClient` 独占流，绕过系统共享混音器，
//! PCM 原样直达 DAC。协商失败（设备被占用 / 不支持原始格式）由调用方（`sink::build_sink`）
//! 回退到共享模式 cpal——本模块只负责「能独占就独占，不能就报错」。
//!
//! 输出模型是 wasapi crate 的事件驱动**拉循环**（非回调）：起一条专用线程，循环
//! `get_available_space_in_frames → 从 rtrb 拉满该批 → write_to_device → wait_for_event`。
//! 拉循环内除一次 `Vec` 复用缓冲外**不分配**；样本只从 rtrb `pop`、乘定点音量。
//! 设备热插拔 / 等待事件超时 → 线程退出（引擎随后会感知 producer 满/欠载并自愈，
//! 真正的设备丢失错误经引擎 emit `music_error`）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use tracing::{info, warn};
use wasapi::{initialize_mta, Direction, SampleType, ShareMode, StreamMode, WaveFormat};

use super::super::types::{ActualFormat, AudioFormat};
use super::{AudioSink, FramesPlayed, SinkAlive, SinkHandle, VolumeQ16};

/// WASAPI 独占 sink。持有输出线程句柄与控制原子；drop 时通知线程退出并 join。
pub struct WasapiExclusiveSink {
    /// 实际生效格式（经 `actual_format()` 暴露，属 §5.4 trait 契约）。
    #[allow(dead_code)]
    actual: ActualFormat,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WasapiExclusiveSink {
    /// 尝试以独占 + event-driven 模式建立输出。成功返回 [`SinkHandle`]；任何协商失败返回 `Err`
    /// （调用方回退共享）。
    ///
    /// 设备初始化（COM / 申请独占 / 申请缓冲）放在**输出线程内**完成——wasapi 的 COM 对象
    /// 非 `Send`，必须与使用它的线程同源；故构造时通过 channel 把初始化结果（实际格式或错误）
    /// 回传，本函数据此决定返回 `Ok`/`Err`。
    pub fn try_build(
        fmt: AudioFormat,
        frames_played: FramesPlayed,
        volume: VolumeQ16,
        capacity: usize,
        alive: SinkAlive,
    ) -> Result<SinkHandle> {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity.max(4096));

        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(true));

        // 初始化结果回传通道：Ok(实际格式) 或 Err(协商失败原因)。
        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<ActualFormat, String>>();

        let stop_c = stop.clone();
        let paused_c = paused.clone();
        let alive_c = alive.clone();
        let thread = std::thread::Builder::new()
            .name("music-wasapi-out".into())
            .spawn(move || {
                output_thread(
                    fmt,
                    consumer,
                    frames_played,
                    volume,
                    stop_c,
                    paused_c,
                    alive_c,
                    init_tx,
                );
            })
            .map_err(|e| anyhow!("启动 WASAPI 输出线程失败: {e}"))?;

        // 等待线程把设备初始化结果回传。
        let actual = match init_rx.recv() {
            Ok(Ok(actual)) => actual,
            Ok(Err(msg)) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                return Err(anyhow!(msg));
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                return Err(anyhow!("WASAPI 输出线程异常退出"));
            }
        };

        Ok(SinkHandle {
            sink: Box::new(WasapiExclusiveSink {
                actual,
                stop,
                paused,
                thread: Some(thread),
            }),
            producer,
            format: actual,
            device_id: None, // 由 build_sink 统一填（与设备监听同源）
            alive,
        })
    }
}

impl AudioSink for WasapiExclusiveSink {
    fn actual_format(&self) -> ActualFormat {
        self.actual
    }

    fn pause(&mut self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    fn resume(&mut self) {
        self.paused.store(false, Ordering::Relaxed);
    }
}

impl Drop for WasapiExclusiveSink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 选用的存储位深：独占模式按源位深申请（16/24/32 整数）。lofty/symphonia 报的源位深可能是
/// 16/24/32；其它值钳到 16。我们统一把解码出的 f32 量化回该位深整数 PCM 写设备（bit-perfect 前提是
/// 设备接受该精确格式；不接受则协商失败回退共享）。
fn choose_store_bits(src_bits: u32) -> usize {
    match src_bits {
        b if b >= 32 => 32,
        b if b >= 24 => 24,
        _ => 16,
    }
}

/// 线程退出即宣告输出链路已死（无论是正常 stop、设备丢失还是初始化失败）。引擎每轮读
/// `alive`，为 false 且仍在播放 → 原位重建 sink。放成 guard 是因为本函数有多个早 return。
struct AliveGuard(SinkAlive);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// 输出线程主体：初始化独占设备（失败回传 Err），然后事件驱动拉循环写 PCM。
#[allow(clippy::too_many_arguments)]
fn output_thread(
    fmt: AudioFormat,
    mut consumer: rtrb::Consumer<f32>,
    frames_played: FramesPlayed,
    volume: VolumeQ16,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    alive: SinkAlive,
    init_tx: std::sync::mpsc::Sender<Result<ActualFormat, String>>,
) {
    let _alive_guard = AliveGuard(alive);

    // 在本线程初始化 COM（MTA）。
    if initialize_mta().is_err() {
        let _ = init_tx.send(Err("WASAPI: 初始化 COM(MTA) 失败".into()));
        return;
    }

    let channels = fmt.channels.max(1) as usize;
    let store_bits = choose_store_bits(fmt.bits);

    let built = match build_exclusive_client(fmt, store_bits, channels) {
        Ok(v) => v,
        Err(e) => {
            let _ = init_tx.send(Err(format!("WASAPI 独占协商失败: {e}")));
            return;
        }
    };
    let ExclusiveClient {
        audio_client,
        render_client,
        event,
        block_align,
    } = built;

    let actual = ActualFormat {
        sample_rate: fmt.sample_rate,
        bits: store_bits as u32,
        channels: channels as u16, // 独占按源声道输出 → 与缓冲一致，引擎无需声道适配
        exclusive: true,
        resampled: false,
    };
    if init_tx.send(Ok(actual)).is_err() {
        return;
    }

    if audio_client.start_stream().is_err() {
        warn!(target: "music", "WASAPI start_stream 失败");
        return;
    }
    info!(
        target: "music",
        "WASAPI 独占输出已启动: {}Hz/{}bit/{}ch",
        fmt.sample_rate, store_bits, channels
    );

    // 复用的字节缓冲（线程内单次分配，循环复用——非实时回调，但仍避免每帧分配）。
    let mut byte_buf: Vec<u8> = Vec::new();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let avail = match audio_client.get_available_space_in_frames() {
            Ok(n) => n as usize,
            Err(e) => {
                warn!(target: "music", "WASAPI 获取可用缓冲失败（设备可能丢失）: {e}");
                break;
            }
        };

        let needed_bytes = avail * block_align;
        if byte_buf.len() < needed_bytes {
            byte_buf.resize(needed_bytes, 0);
        }

        let is_paused = paused.load(Ordering::Relaxed);
        let vol = volume.load(Ordering::Relaxed) as f32 / 65536.0;

        // 填充该批：暂停时写静音（保持流活跃，不推进进度）；否则从 rtrb 拉。
        let mut produced_frames = 0usize;
        let buf = &mut byte_buf[..needed_bytes];
        let mut off = 0usize;
        let bytes_per_sample = store_bits / 8;
        for _frame in 0..avail {
            // 整帧原子取：暂停、或缓冲不足一整帧 → 该帧写静音，避免在一帧的多声道中途欠载
            // 导致后续样本错位（声道永久串位/失真）。
            let have_frame = !is_paused && consumer.slots() >= channels;
            for _ch in 0..channels {
                let sample = if have_frame {
                    consumer.pop().map(|s| s * vol).unwrap_or(0.0)
                } else {
                    0.0
                };
                write_sample(&mut buf[off..off + bytes_per_sample], sample, store_bits);
                off += bytes_per_sample;
            }
            if have_frame {
                produced_frames += 1;
            }
        }

        if render_client
            .write_to_device(avail, &buf[..needed_bytes], None)
            .is_err()
        {
            warn!(target: "music", "WASAPI 写设备失败（设备可能丢失）");
            break;
        }

        if !is_paused && produced_frames > 0 {
            frames_played.fetch_add(produced_frames as u32, Ordering::Relaxed);
        }

        // 等待设备就绪事件；超时（设备停滞）即退出循环。
        if event.wait_for_event(2000).is_err() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            warn!(target: "music", "WASAPI 等待事件超时");
            break;
        }
    }

    let _ = audio_client.stop_stream();
}

/// 已初始化好的独占客户端打包。
#[allow(dead_code)]
struct ExclusiveClient {
    audio_client: wasapi::AudioClient,
    render_client: wasapi::AudioRenderClient,
    event: wasapi::Handle,
    /// 每帧字节数 = channels * store_bits/8。
    block_align: usize,
}

/// 申请独占 + event-driven 客户端，按源格式精确协商。任何步骤失败返回 Err。
fn build_exclusive_client(
    fmt: AudioFormat,
    store_bits: usize,
    channels: usize,
) -> Result<ExclusiveClient> {
    let enumerator =
        wasapi::DeviceEnumerator::new().map_err(|e| anyhow!("枚举音频设备失败: {e}"))?;
    let device = enumerator
        .get_default_device(&Direction::Render)
        .map_err(|e| anyhow!("获取默认输出设备失败: {e}"))?;
    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|e| anyhow!("获取 IAudioClient 失败: {e}"))?;

    // 独占按源采样率/位深申请整数 PCM。
    let desired = WaveFormat::new(
        store_bits,
        store_bits,
        &SampleType::Int,
        fmt.sample_rate as usize,
        channels,
        None,
    );

    // 校验设备是否支持精确格式（不支持即返回 Err → 回退共享）。
    audio_client
        .is_supported(&desired, &ShareMode::Exclusive)
        .map_err(|e| anyhow!("设备不支持源格式（独占）: {e}"))?;

    let (_def_period, min_period) = audio_client
        .get_device_period()
        .map_err(|e| anyhow!("获取设备周期失败: {e}"))?;
    let period = audio_client
        .calculate_aligned_period_near(3 * min_period / 2, Some(128), &desired)
        .map_err(|e| anyhow!("计算对齐周期失败: {e}"))?;

    let mode = StreamMode::EventsExclusive { period_hns: period };
    audio_client
        .initialize_client(&desired, &Direction::Render, &mode)
        .map_err(|e| anyhow!("初始化独占客户端失败: {e}"))?;

    let event = audio_client
        .set_get_eventhandle()
        .map_err(|e| anyhow!("获取事件句柄失败: {e}"))?;
    let render_client = audio_client
        .get_audiorenderclient()
        .map_err(|e| anyhow!("获取渲染客户端失败: {e}"))?;

    let block_align = channels * (store_bits / 8);

    Ok(ExclusiveClient {
        audio_client,
        render_client,
        event,
        block_align,
    })
}

/// 把 [-1,1] 的 f32 样本量化为 `bits` 位小端整数 PCM 写入 `dst`（长度 = bits/8）。
#[inline]
fn write_sample(dst: &mut [u8], sample: f32, bits: usize) {
    let s = sample.clamp(-1.0, 1.0);
    match bits {
        16 => {
            let v = (s * i16::MAX as f32) as i16;
            dst.copy_from_slice(&v.to_le_bytes());
        }
        24 => {
            // 24-bit packed: 取 i32 高 24 位，写低 3 字节。
            let v = (s * 8_388_607.0) as i32; // 2^23 - 1
            let b = v.to_le_bytes();
            dst.copy_from_slice(&b[0..3]);
        }
        _ => {
            let v = (s as f64 * i32::MAX as f64) as i32;
            dst.copy_from_slice(&v.to_le_bytes());
        }
    }
}
