//! 跨平台共享模式输出（cpal）。Win=WASAPI 共享 / mac=CoreAudio / Linux=ALSA。
//!
//! 作为 WASAPI 独占协商失败或非 Windows 平台的兜底。输出回调从 rtrb 拉交织 f32，
//! 回调内**零分配、零锁**：只 `pop` 样本、乘定点音量、累加已播帧原子量。
//!
//! cpal 共享模式优先按源采样率开流，让系统共享混音器处理必要的设备重采样；若驱动拒绝，
//! 再回退设备默认采样率，并由上层引擎（rubato）在写入前重采样。

use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, SupportedStreamConfig};
use tracing::{info, warn};

use super::super::types::{ActualFormat, AudioFormat, AudioKind};
use super::{AudioSink, FramesPlayed, SinkAlive, SinkHandle, VolumeQ16};

/// cpal 共享模式 sink，持有 `Stream`（drop 即停流）。
pub struct CpalSink {
    _stream: cpal::Stream,
    /// 实际生效格式（经 `actual_format()` 暴露，属 §5.4 trait 契约）。
    #[allow(dead_code)]
    actual: ActualFormat,
    paused: bool,
}

impl CpalSink {
    /// 建立共享输出流。`fmt` 为源格式；实际输出采样率取设备默认配置的采样率
    /// （若与源不同，`resampled=true`，引擎负责重采样到该率）。
    pub fn build(
        fmt: AudioFormat,
        frames_played: FramesPlayed,
        volume: VolumeQ16,
        capacity: usize,
        alive: SinkAlive,
    ) -> Result<SinkHandle> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("找不到默认音频输出设备"))?;

        let default_supported = device
            .default_output_config()
            .context("获取默认输出配置失败")?;
        let supported = choose_shared_config(&device, fmt, &default_supported);
        let sample_format = supported.sample_format();
        let mut default_config: cpal::StreamConfig = supported.config();
        let dev_channels = default_config.channels as usize;
        default_config.buffer_size = cpal::BufferSize::Default;

        // 共享模式优先按源采样率开流，让系统共享混音器负责必要的设备重采样。
        // 这样避免应用内重采样比例/驱动默认率不一致导致的加速播放；若驱动拒绝源率，再回退默认率。
        let mut source_config = default_config.clone();
        source_config.sample_rate = cpal::SampleRate(fmt.sample_rate);

        let channels_for_cb = dev_channels.max(1);

        let source_attempt = build_stream_with_ring(
            &device,
            &source_config,
            sample_format,
            frames_played.clone(),
            volume.clone(),
            capacity,
            channels_for_cb,
            alive.clone(),
        );
        let (stream, producer, actual_rate) = match source_attempt {
            Ok((stream, producer)) => (stream, producer, fmt.sample_rate),
            Err(source_err) => {
                warn!(
                    target: "music",
                    "cpal 共享源采样率 {}Hz 建流失败，回退设备默认率 {}Hz: {source_err:#}",
                    fmt.sample_rate,
                    default_config.sample_rate.0
                );
                let (stream, producer) = build_stream_with_ring(
                    &device,
                    &default_config,
                    sample_format,
                    frames_played,
                    volume,
                    capacity,
                    channels_for_cb,
                    alive.clone(),
                )
                .context("建立 cpal 输出流失败")?;
                (stream, producer, default_config.sample_rate.0)
            }
        };

        let resampled = actual_rate != fmt.sample_rate;
        info!(
            target: "music",
            "cpal 共享输出: 流率={actual_rate} 源率={} 通道={dev_channels} 格式={:?} 源类型={:?} 应用重采样={resampled}",
            fmt.sample_rate, sample_format, fmt.kind
        );

        let actual = ActualFormat {
            sample_rate: actual_rate,
            bits: bits_for_format(sample_format),
            channels: dev_channels as u16,
            exclusive: false,
            resampled,
        };

        Ok(SinkHandle {
            sink: Box::new(CpalSink {
                _stream: stream,
                actual,
                paused: true,
            }),
            producer,
            format: actual,
            device_id: None, // 由 build_sink 统一填（与设备监听同源）
            alive,
        })
    }
}

fn choose_shared_config(
    device: &cpal::Device,
    fmt: AudioFormat,
    default_supported: &SupportedStreamConfig,
) -> SupportedStreamConfig {
    if prefers_float_output(fmt) {
        if let Some(config) = find_f32_config(device, fmt.sample_rate) {
            info!(
                target: "music",
                "高位深/FLAC 共享输出优先使用 f32 源采样率配置: {}Hz/{}ch",
                config.sample_rate().0,
                config.channels()
            );
            return config;
        }
        if let Some(config) = find_f32_config(device, default_supported.sample_rate().0) {
            info!(
                target: "music",
                "高位深/FLAC 共享输出优先使用 f32 设备默认率配置: {}Hz/{}ch",
                config.sample_rate().0,
                config.channels()
            );
            return config;
        }
        warn!(
            target: "music",
            "高位深/FLAC 未找到可用 f32 共享输出配置，回退设备默认格式 {:?}",
            default_supported.sample_format()
        );
    }
    default_supported.clone()
}

fn prefers_float_output(fmt: AudioFormat) -> bool {
    fmt.kind == AudioKind::Flac || fmt.bits > 16
}

fn find_f32_config(device: &cpal::Device, sample_rate: u32) -> Option<SupportedStreamConfig> {
    let requested = SampleRate(sample_rate);
    device
        .supported_output_configs()
        .ok()?
        .filter(|range| range.sample_format() == SampleFormat::F32)
        .filter_map(|range| range.try_with_sample_rate(requested))
        .max_by_key(|config| config.channels())
}

#[allow(clippy::too_many_arguments)]
fn build_stream_with_ring(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: SampleFormat,
    frames_played: FramesPlayed,
    volume: VolumeQ16,
    capacity: usize,
    channels: usize,
    alive: SinkAlive,
) -> Result<(cpal::Stream, rtrb::Producer<f32>)> {
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity.max(channels * 1024));
    let stream = build_stream(
        device,
        config,
        sample_format,
        consumer,
        volume,
        frames_played,
        channels,
        alive,
    )?;
    Ok((stream, producer))
}

#[allow(clippy::too_many_arguments)]
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: SampleFormat,
    mut consumer: rtrb::Consumer<f32>,
    volume: VolumeQ16,
    frames_played: FramesPlayed,
    channels: usize,
    alive: SinkAlive,
) -> Result<cpal::Stream> {
    macro_rules! build_typed {
        ($t:ty, $convert:expr) => {{
            // 错误回调：设备丢失/驱动异常 → 置 alive=false，引擎下一轮据此重建 sink。
            let alive_cb = alive.clone();
            device.build_output_stream(
                config,
                move |out: &mut [$t], _: &cpal::OutputCallbackInfo| {
                    fill_callback::<$t>(
                        out,
                        &mut consumer,
                        &volume,
                        &frames_played,
                        channels,
                        $convert,
                    );
                },
                move |err| on_stream_error(err, &alive_cb),
                None,
            )?
        }};
    }

    match sample_format {
        SampleFormat::F32 => Ok(build_typed!(f32, |s: f32| s)),
        SampleFormat::I16 => Ok(build_typed!(i16, |s: f32| {
            (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
        })),
        SampleFormat::U16 => Ok(build_typed!(u16, |s: f32| {
            ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16
        })),
        other => Err(anyhow!("设备不支持的样本格式: {other:?}")),
    }
}

fn on_stream_error(err: cpal::StreamError, alive: &SinkAlive) {
    tracing::error!(target: "music", "cpal 输出流错误: {err}");
    alive.store(false, Ordering::Relaxed);
}

impl AudioSink for CpalSink {
    fn actual_format(&self) -> ActualFormat {
        self.actual
    }

    fn pause(&mut self) {
        if !self.paused {
            let _ = self._stream.pause();
            self.paused = true;
        }
    }

    fn resume(&mut self) {
        if self.paused {
            let _ = self._stream.play();
            self.paused = false;
        }
    }
}

/// 设备样本格式对应的位深（回报 UI）。
fn bits_for_format(fmt: SampleFormat) -> u32 {
    match fmt {
        SampleFormat::F32 => 32,
        SampleFormat::I16 | SampleFormat::U16 => 16,
        _ => 32,
    }
}

/// 实时回调主体：从 rtrb 拉交织 f32 → 乘音量 → 转目标样本类型写 `out`；欠载补静音。
/// **零分配、零锁**：仅 `pop`、原子读写、算术。
#[inline]
fn fill_callback<T>(
    out: &mut [T],
    consumer: &mut rtrb::Consumer<f32>,
    volume: &AtomicU32,
    frames_played: &AtomicU32,
    channels: usize,
    convert: impl Fn(f32) -> T,
) where
    T: Copy,
{
    // 定点音量：volume 存为 (vol * 65536) 的整数。
    let vol = volume.load(Ordering::Relaxed) as f32 / 65536.0;
    let ch = channels.max(1);
    let n = out.len();
    let mut i = 0usize;
    let mut produced_frames = 0usize;
    // 按帧原子消费：缓冲不足一整帧时该帧写静音，避免在一帧的多声道中途欠载导致 L/R 永久错位。
    while i + ch <= n {
        if consumer.slots() >= ch {
            for _ in 0..ch {
                let s = consumer.pop().unwrap_or(0.0);
                out[i] = convert(s * vol);
                i += 1;
            }
            produced_frames += 1;
        } else {
            for _ in 0..ch {
                out[i] = convert(0.0);
                i += 1;
            }
        }
    }
    // 尾部不足一帧的零头（通常 out 帧对齐，不会触发）补静音。
    while i < n {
        out[i] = convert(0.0);
        i += 1;
    }
    if produced_frames > 0 {
        frames_played.fetch_add(produced_frames as u32, Ordering::Relaxed);
    }
}
