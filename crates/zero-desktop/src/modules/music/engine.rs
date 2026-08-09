//! AudioEngine：专用 `std::thread` 上的 actor。**单一播放真值源**。
//!
//! 控制循环（非 tokio，实时负载）：
//! 1. 非阻塞收 `AudioCommand`（crossbeam）→ 改队列/状态/seek/切曲。
//! 2. 若在播放：从 [`Decoder`] 拉交织 f32 → 必要时 Rubato 重采样到 sink 实际采样率 →
//!    写 sink 的 rtrb producer（写满即让出，避免忙等）。
//! 3. 按 `frames_played` 原子量算播放位置，节流 emit `music_progress`。
//! 4. 曲终：**同采样率/声道的相邻曲走 gapless**——解码 EOF 时原地换下一首解码器、继续喂同一
//!    sink，按输出帧边界翻 index（无缝）；跨格式才重建 sink（采样率可能变，有短暂静默）。
//!
//! 事件经构造时注入的 `AppHandle` `emit`。设备/解码错误 → emit `music_error` 进 stopped 不崩。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use rubato::{FftFixedInOut, Resampler};
use serde_json::json;
use tauri::{AppHandle, Emitter};
use tracing::{info, warn};

use super::decode::Decoder;
use super::sink::{build_sink, FramesPlayed, SinkHandle, VolumeQ16};
use super::types::{ActualFormat, AudioFormat, OutputMode, PlayStatus, RepeatMode, Track};
use super::SharedPlayback;

/// 控制命令（UI → 引擎，全异步立即返回）。
pub enum AudioCommand {
    PlayQueue {
        tracks: Vec<String>,
        start: usize,
    },
    Pause,
    Resume,
    TogglePlay,
    Seek {
        secs: f64,
    },
    Next,
    Prev,
    SetVolume(f32),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    SetOutputMode(OutputMode),
    /// 系统默认输出设备已变（由 [`super::sink::device_watch`] 轮询发现），携带新默认设备 id。
    /// 引擎比对当前 sink 所在设备，不同则原位重建，把声音跟到新设备（如刚连上的蓝牙耳机）。
    OutputDeviceChanged(Option<String>),
    Stop,
    /// 请求完整状态快照（`music_get_state` 用）：引擎填好经回传 channel 送回。
    Snapshot(crossbeam_channel::Sender<super::types::PlaybackState>),
}

/// rtrb 容量（帧）。约 1 秒 @ 48k 的缓冲，吸收解码抖动并支持 seek 快速清空。
const SINK_BUFFER_FRAMES: usize = 48_000;
/// 新建输出流后先预填一小段，避免回调刚启动时连续欠载插静音造成爆裂声。
const PREFILL_FRAMES: usize = 12_000;
/// 进度事件节流间隔。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// 控制循环每轮无事可做时的让出睡眠。
const IDLE_SLEEP: Duration = Duration::from_millis(5);
/// 两次 sink 重建之间的最小间隔（设备持续不可用时的重试节流）。
const REBUILD_MIN_INTERVAL: Duration = Duration::from_millis(1500);

/// 当前正在播放的一首的运行时状态。
struct Current {
    decoder: Decoder,
    sink: SinkHandle,
    /// sink 实际格式（驱动是否重采样）。
    actual: ActualFormat,
    /// 源格式。
    src_fmt: AudioFormat,
    /// 重采样器（仅当 sink 采样率 ≠ 源采样率时存在）。
    resampler: Option<ResamplerState>,
    /// 已 push 到 sink 的「源帧」累计（用于 seek 后重置进度基准 + EOF 判定）。
    decoded_eof: bool,
    /// seek 后的进度基准（秒）；位置 = base + frames_played / actual_rate。
    position_base_secs: f64,
    /// 本曲起 frames_played 的基准（每次 seek/换曲重置 sink 计数对齐）。
    frames_base: u32,
    /// producer 的总容量（空时的可写 slots 数），用于判定缓冲是否已排空。
    producer_capacity: usize,
    /// 上次 producer 满时尚未写入的输出样本（已经过重采样/声道适配，按 actual.channels 交织）。
    pending_output: Vec<f32>,
    pending_pos: usize,
}

/// 一个已 gapless 预切、但音频尚未播到的「曲目切换边界」。
///
/// 在当前曲解码 EOF 时把解码器原地换成下一首并继续喂同一 sink；此时 sink 缓冲里还压着
/// 当前曲约 1 秒的尾巴，故新曲实际出声要等到输出帧计数到达 `boundary_frame`。届时才翻
/// `index`/位置基准、emit `music_track_changed`，让 UI 与听感对齐。
struct PendingSwitch {
    /// 新曲第一帧出声时的 `frames_played` 值（= 预切瞬间的输出帧 + 当时缓冲中的帧）。
    boundary_frame: u32,
    /// 新曲在队列中的下标。
    index: i64,
}

/// Rubato 定块重采样器 + 其 planar 工作缓冲。
struct ResamplerState {
    resampler: FftFixedInOut<f32>,
    channels: usize,
    /// 累积的源交织样本，凑够一个 input chunk 再喂。
    in_accum: Vec<f32>,
    /// planar 输入暂存（channels × frames）。
    in_planar: Vec<Vec<f32>>,
    /// planar 输出暂存。
    out_planar: Vec<Vec<f32>>,
    chunk_frames: usize,
}

impl ResamplerState {
    fn new(in_rate: u32, out_rate: u32, channels: usize) -> anyhow::Result<Self> {
        // chunk_frames：取约 1024 帧的输入块（实际值由 rubato 内部对齐）。
        let resampler =
            FftFixedInOut::<f32>::new(in_rate as usize, out_rate as usize, 1024, channels)
                .map_err(|e| anyhow::anyhow!("创建重采样器失败: {e}"))?;
        let chunk_frames = resampler.input_frames_next();
        Ok(Self {
            resampler,
            channels,
            in_accum: Vec::with_capacity(chunk_frames * channels * 2),
            in_planar: vec![Vec::new(); channels],
            out_planar: Vec::new(),
            chunk_frames,
        })
    }

    /// 喂入一批交织源样本，产出重采样后的交织样本（追加到 `out`）。
    fn process(&mut self, interleaved: &[f32], out: &mut Vec<f32>) -> anyhow::Result<()> {
        self.in_accum.extend_from_slice(interleaved);
        let frame_stride = self.channels;
        while self.in_accum.len() >= self.chunk_frames * frame_stride {
            // 拆 planar。
            for ch in 0..self.channels {
                self.in_planar[ch].clear();
                self.in_planar[ch].reserve(self.chunk_frames);
                for f in 0..self.chunk_frames {
                    self.in_planar[ch].push(self.in_accum[f * frame_stride + ch]);
                }
            }
            let processed = self
                .resampler
                .process(&self.in_planar, None)
                .map_err(|e| anyhow::anyhow!("重采样失败: {e}"))?;
            self.out_planar = processed;
            // 交织回 out。
            let out_frames = self.out_planar.first().map(|c| c.len()).unwrap_or(0);
            for f in 0..out_frames {
                for ch in 0..self.channels {
                    out.push(self.out_planar[ch][f]);
                }
            }
            self.in_accum.drain(0..self.chunk_frames * frame_stride);
        }
        Ok(())
    }
}

/// 引擎运行所需的全部句柄。
pub struct EngineContext {
    pub rx: Receiver<AudioCommand>,
    pub shared: Arc<SharedPlayback>,
    pub app: AppHandle,
    /// 封面落盘目录（懒补元数据时用）。
    pub covers_dir: std::path::PathBuf,
}

/// 引擎主循环（在专用线程上跑，永不返回直到进程退出）。
pub fn run(ctx: EngineContext) {
    let EngineContext {
        rx,
        shared,
        app,
        covers_dir,
    } = ctx;
    let mut engine = Engine {
        shared,
        app,
        covers_dir,
        queue: Vec::new(),
        index: -1,
        status: PlayStatus::Stopped,
        repeat: RepeatMode::Off,
        shuffle: false,
        shuffle_order: Vec::new(),
        volume: 1.0,
        output_mode: OutputMode::Auto,
        current: None,
        frames_played: Arc::new(AtomicU32::new(0)),
        volume_q16: Arc::new(AtomicU32::new(65536)),
        last_progress: Instant::now(),
        last_rebuild: Instant::now(),
        tracks_meta: Vec::new(),
        pending_switch: VecDeque::new(),
    };
    info!(target: "music", "音频引擎线程已启动");

    loop {
        // 1. 收命令（阻塞短超时，避免空转吃 CPU；播放时退化为非阻塞）。
        let timeout = if engine.status == PlayStatus::Playing {
            IDLE_SLEEP
        } else {
            Duration::from_millis(100)
        };
        match rx.recv_timeout(timeout) {
            Ok(cmd) => engine.handle_command(cmd),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                info!(target: "music", "命令通道断开，引擎退出");
                break;
            }
        }
        // 把累积的命令都处理掉（避免一轮只处理一条）。
        while let Ok(cmd) = rx.try_recv() {
            engine.handle_command(cmd);
        }

        // 2. 输出链路健康检查（暂停中也查：设备此刻丢了，恢复播放不该是静音）。
        if engine.status != PlayStatus::Stopped {
            engine.check_sink_alive();
        }

        // 3. 推进播放。
        if engine.status == PlayStatus::Playing {
            engine.pump();
            engine.maybe_gapless(); // 当前曲解码 EOF → 同格式则原地换下一首解码器，不重建 sink
            engine.check_track_boundary(); // 新曲音频到达输出 → 翻 index/位置/emit track_changed
            engine.maybe_emit_progress();
            engine.check_advance(); // 仅在未 gapless（decoded_eof 仍为真）时收尾/重建
        }
    }
}

struct Engine {
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    covers_dir: std::path::PathBuf,
    queue: Vec<String>,
    index: i64,
    status: PlayStatus,
    repeat: RepeatMode,
    shuffle: bool,
    /// 随机播放序列：队列下标的一个随机排列，`shuffle=false` 时为空。
    ///
    /// **不是每次现掷随机数**——那样只能避开「紧邻重复」，一轮里同一首照样反复出现，
    /// 听感就是「随机 = 老听到那几首」。改成走一遍排列：一轮内每首恰好一次，走完
    /// （`repeat=all`）再重洗新一轮，且保证新一轮首曲不等于刚播完的那首。
    shuffle_order: Vec<i64>,
    volume: f32,
    /// 输出模式（独占优先 / 强制共享）。
    output_mode: OutputMode,
    current: Option<Current>,
    /// 与 sink 共享：sink 输出回调累加，引擎读出算位置。
    frames_played: FramesPlayed,
    /// 与 sink 共享的定点音量。
    volume_q16: VolumeQ16,
    last_progress: Instant,
    /// 上次重建 sink 的时刻（设备丢失重试节流用）。
    last_rebuild: Instant,
    /// queue 中各路径对应的元数据（扫描时不带；这里懒填，emit 给前端）。
    tracks_meta: Vec<Option<Track>>,
    /// gapless 预切但尚未播到的曲目切换边界（FIFO；通常至多 1 个）。
    pending_switch: VecDeque<PendingSwitch>,
}

impl Engine {
    fn handle_command(&mut self, cmd: AudioCommand) {
        match cmd {
            AudioCommand::PlayQueue { tracks, start } => self.play_queue(tracks, start),
            AudioCommand::Pause => self.set_paused(true),
            AudioCommand::Resume => self.set_paused(false),
            AudioCommand::TogglePlay => {
                let paused = self.status == PlayStatus::Paused;
                self.set_paused(!paused);
            }
            AudioCommand::Seek { secs } => self.seek(secs),
            AudioCommand::Next => self.advance(true),
            AudioCommand::Prev => self.prev(),
            AudioCommand::SetVolume(v) => self.set_volume(v),
            AudioCommand::SetRepeat(m) => {
                self.repeat = m;
                self.publish_state();
            }
            AudioCommand::SetShuffle(on) => {
                self.shuffle = on;
                if on {
                    // 当前曲置于新序列首位：开随机不该把正在听的这首挤掉，
                    // 下一首从序列第 2 个开始。
                    self.rebuild_shuffle_order(Some(self.index));
                } else {
                    self.shuffle_order.clear();
                }
                self.publish_state();
            }
            AudioCommand::SetOutputMode(m) => self.set_output_mode(m),
            AudioCommand::OutputDeviceChanged(id) => self.on_default_device_changed(id),
            AudioCommand::Stop => self.stop(),
            AudioCommand::Snapshot(reply) => {
                let snap = self.snapshot();
                let _ = reply.send(snap);
            }
        }
    }

    /// 加载并播放一个新队列，从 `start` 开始。
    fn play_queue(&mut self, tracks: Vec<String>, start: usize) {
        if tracks.is_empty() {
            self.stop();
            return;
        }
        self.tracks_meta = vec![None; tracks.len()];
        self.queue = tracks;
        let start = start.min(self.queue.len() - 1);
        self.index = start as i64;
        if self.shuffle {
            // 队列换了 → 旧序列的下标失效，按新队列重洗（起播那首置首）。
            self.rebuild_shuffle_order(Some(self.index));
        }
        self.start_current();
    }

    /// 用当前 index 的曲目建立解码器与 sink，开始播放。失败 → emit music_error 并尝试下一首。
    fn start_current(&mut self) {
        self.current = None;
        self.pending_switch.clear(); // 重建 sink → 作废所有 gapless 预切边界
        self.frames_played.store(0, Ordering::Relaxed);

        let path = match self.queue.get(self.index as usize) {
            Some(p) => p.clone(),
            None => {
                self.stop();
                return;
            }
        };

        let decoder = match Decoder::open(std::path::Path::new(&path)) {
            Ok(d) => d,
            Err(e) => {
                warn!(target: "music", "打开/解码失败 {path}: {e:#}");
                self.emit_error(format!("无法播放: {e}"));
                // 跳到下一首（避免卡死）。
                self.advance_after_error();
                return;
            }
        };
        let src_fmt = decoder.format();

        let sink = match build_sink(
            src_fmt,
            self.frames_played.clone(),
            self.volume_q16.clone(),
            SINK_BUFFER_FRAMES,
            self.output_mode,
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(target: "music", "建立音频输出失败: {e:#}");
                self.emit_error(format!("音频输出设备不可用: {e}"));
                self.status = PlayStatus::Stopped;
                self.publish_state();
                return;
            }
        };
        let actual = sink.format;

        // sink 采样率 ≠ 源采样率 → 建重采样器。
        let resampler = if actual.sample_rate != src_fmt.sample_rate {
            match ResamplerState::new(
                src_fmt.sample_rate,
                actual.sample_rate,
                src_fmt.channels as usize,
            ) {
                Ok(r) => Some(r),
                Err(e) => {
                    // 不能按设备率正确播放该曲 → 跳过，绝不退化为「源率直送 dev_rate 流」
                    // （那会导致播放加速 + 音高升高）。
                    warn!(target: "music", "重采样器创建失败，跳过该曲: {e:#}");
                    self.emit_error(format!("无法按设备采样率播放该曲（重采样器创建失败）: {e}"));
                    self.advance_after_error();
                    return;
                }
            }
        } else {
            None
        };

        self.apply_volume_atomic();

        // 刚建好、未写时 producer 的可写 slots = 总容量，作为「排空」判定基准。
        let producer_capacity = sink.producer.slots();

        self.current = Some(Current {
            decoder,
            sink,
            actual,
            src_fmt,
            resampler,
            decoded_eof: false,
            position_base_secs: 0.0,
            frames_base: 0,
            producer_capacity,
            pending_output: Vec::new(),
            pending_pos: 0,
        });

        self.prefill_current();
        if let Some(cur) = self.current.as_mut() {
            cur.sink.sink.resume();
        }
        self.status = PlayStatus::Playing;

        self.emit_format_changed(actual);
        self.emit_track_changed();
        self.publish_state();
    }

    /// 把解码帧泵进 sink，直到 producer 接近满或解码 EOF。
    fn pump(&mut self) {
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        if cur.decoded_eof {
            return;
        }
        // 限制单轮泵入量，避免长时间占用循环。
        let mut iterations = 0;
        while iterations < 32 {
            iterations += 1;
            if !flush_pending_output(cur) {
                break;
            }
            // producer 剩余空间不足一批就停（让出给输出线程消费）。
            if cur.sink.producer.slots() < cur.actual.channels.max(1) as usize * 2048 {
                break;
            }
            match cur.decoder.next_frames() {
                Ok(Some(chunk)) => {
                    push_chunk(cur, &chunk.samples);
                    if !pending_output_empty(cur) {
                        break;
                    }
                }
                Ok(None) => {
                    // EOF：冲洗重采样器尾巴（若有），标记。
                    cur.decoded_eof = true;
                    break;
                }
                Err(e) => {
                    warn!(target: "music", "解码错误: {e:#}");
                    cur.decoded_eof = true;
                    break;
                }
            }
        }
    }

    fn prefill_current(&mut self) {
        let Some(target) = self.current.as_ref().map(|cur| {
            let capacity_frames = cur.producer_capacity / cur.actual.channels.max(1) as usize;
            PREFILL_FRAMES.min(capacity_frames / 2).max(1024)
        }) else {
            return;
        };

        for _ in 0..16 {
            let buffered = match self.current.as_ref() {
                Some(cur) => buffered_output_frames(cur),
                None => return,
            };
            if buffered >= target {
                break;
            }
            self.pump();
            if self
                .current
                .as_ref()
                .map(|cur| cur.decoded_eof)
                .unwrap_or(true)
            {
                break;
            }
        }
    }

    /// 当前播放位置（秒）。
    fn position_secs(&self) -> f64 {
        match &self.current {
            Some(cur) => {
                let frames = self.frames_played.load(Ordering::Relaxed);
                let played = frames.saturating_sub(cur.frames_base) as f64
                    / cur.actual.sample_rate.max(1) as f64;
                cur.position_base_secs + played
            }
            None => 0.0,
        }
    }

    fn duration_secs(&self) -> f64 {
        self.current_track().map(|t| t.duration_secs).unwrap_or(0.0)
    }

    fn maybe_emit_progress(&mut self) {
        if self.last_progress.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        self.last_progress = Instant::now();
        let position = self.position_secs();
        let duration = self.duration_secs();
        self.shared
            .position_secs
            .store((position * 1000.0) as u64, Ordering::Relaxed);
        let _ = self.app.emit(
            "music_progress",
            json!({ "position_secs": position, "duration_secs": duration }),
        );
    }

    /// 解码 EOF 且 sink 缓冲排空 → 当前曲放完，按 repeat/shuffle 推进。
    fn check_advance(&mut self) {
        let done = match &self.current {
            Some(cur) => cur.decoded_eof && cur.sink.producer.slots() >= cur.producer_capacity,
            None => false,
        };
        if !done {
            return;
        }
        // 额外宽限：缓冲排空后给输出线程一点时间播完，用位置≈时长近似。
        match self.repeat {
            RepeatMode::One => {
                // 单曲循环：重新开始当前曲。
                self.start_current();
            }
            _ => self.advance(false),
        }
    }

    /// 当前曲解码 EOF 时，尝试「同格式无缝接下一首」：原地把解码器换成下一首、继续喂同一
    /// sink（不重建输出流），并登记一个 [`PendingSwitch`] 边界。仅当下一首存在且**采样率与
    /// 声道与当前曲一致**（沿用同一 sink/重采样配置、bit-perfect 不破）时生效；否则不动，交给
    /// [`check_advance`] 走重建路径（跨格式有短暂静默，符合设计「gapless 仅同格式相邻曲」）。
    fn maybe_gapless(&mut self) {
        // 仅在当前曲刚解码 EOF（decoded_eof=true）时尝试；已 gapless 过的为 false。
        let (src_fmt, frames_now, buffered_frames) = match self.current.as_ref() {
            Some(cur) if cur.decoded_eof => {
                // producer 里是 sink 实际声道交织的输出帧 → 按 actual.channels 换算，与
                // frames_played（输出帧）同基准。
                let ch = cur.actual.channels.max(1) as usize;
                let buffered = cur
                    .producer_capacity
                    .saturating_sub(cur.sink.producer.slots())
                    / ch;
                (
                    cur.src_fmt,
                    self.frames_played.load(Ordering::Relaxed),
                    buffered as u32,
                )
            }
            _ => return,
        };

        let Some(next_idx) = self.gapless_next_index() else {
            return; // 队尾且非循环：不 gapless，交给 check_advance 收尾
        };
        let Some(path) = self.queue.get(next_idx as usize).cloned() else {
            return;
        };
        let next_dec = match Decoder::open(std::path::Path::new(&path)) {
            Ok(d) => d,
            Err(e) => {
                // 下一首打不开（如 .opus）：放弃 gapless，让 check_advance 走 advance
                // （其 start_current 会 emit music_error 并跳过坏文件）。
                warn!(target: "music", "gapless 预解码失败，回退重建: {e:#}");
                return;
            }
        };
        let nf = next_dec.format();
        if nf.sample_rate != src_fmt.sample_rate || nf.channels != src_fmt.channels {
            // 跨格式：不 gapless，交给 check_advance 重建 sink（按新采样率，保 bit-perfect）。
            return;
        }

        // 原地换解码器、清 EOF，继续喂同一 sink；登记边界（新曲尾随当前缓冲之后出声）。
        let boundary = frames_now.saturating_add(buffered_frames);
        if let Some(cur) = self.current.as_mut() {
            cur.decoder = next_dec;
            cur.decoded_eof = false;
            // 重采样器（若有）沿用：源格式相同 → 配置不变，accum 中当前曲尾样本继续出声不丢音。
        }
        self.pending_switch.push_back(PendingSwitch {
            boundary_frame: boundary,
            index: next_idx,
        });
        info!(target: "music", "gapless 预切 → #{next_idx}（boundary_frame={boundary}）");
    }

    /// gapless 的下一首下标：单曲循环 → 自身（无缝循环）；否则同 [`next_index`]。
    fn gapless_next_index(&self) -> Option<i64> {
        if self.repeat == RepeatMode::One {
            return Some(self.index);
        }
        self.next_index()
    }

    /// 输出帧计数越过某个预切边界 → 新曲已开始出声：翻 index、重置位置基准、emit track_changed。
    fn check_track_boundary(&mut self) {
        loop {
            let frames = self.frames_played.load(Ordering::Relaxed);
            let cross =
                matches!(self.pending_switch.front(), Some(sw) if frames >= sw.boundary_frame);
            if !cross {
                return;
            }
            let sw = self.pending_switch.pop_front().unwrap();
            self.index = sw.index;
            if let Some(cur) = self.current.as_mut() {
                cur.frames_base = sw.boundary_frame;
                cur.position_base_secs = 0.0;
            }
            self.emit_track_changed();
            self.publish_state();
        }
    }

    /// 推进到下一首。`manual=true` 表示用户点 next（忽略 repeat-one 语义按顺序走）。
    fn advance(&mut self, _manual: bool) {
        if self.queue.is_empty() {
            self.stop();
            return;
        }
        // 随机序列走完一轮 + 列表循环 → 重洗开新一轮（首曲避开刚播完的那首，
        // 否则一轮之交会紧接着重复同一首）。
        if self.shuffle && self.repeat == RepeatMode::All && self.next_index().is_none() {
            let just_played = self.index;
            self.rebuild_shuffle_order(None);
            if self.shuffle_order.len() > 1 && self.shuffle_order[0] == just_played {
                self.shuffle_order.swap(0, 1);
            }
            if let Some(&first) = self.shuffle_order.first() {
                self.index = first;
                self.start_current();
                return;
            }
        }
        let next = self.next_index();
        match next {
            Some(i) => {
                self.index = i;
                self.start_current();
            }
            None => {
                // 队尾且非循环：停。
                self.stop();
            }
        }
    }

    /// 错误后跳下一首（避免一首坏文件卡死整个队列）；到队尾则停。
    fn advance_after_error(&mut self) {
        match self.next_index() {
            Some(i) if i != self.index => {
                self.index = i;
                self.start_current();
            }
            _ => {
                self.status = PlayStatus::Stopped;
                self.publish_state();
            }
        }
    }

    /// 计算下一首下标（考虑 shuffle / repeat-all）。
    ///
    /// **纯查询**（`&self`）：随机序列走完时返回 `None`，续轮重洗由 [`advance`] 负责。
    /// 这样 gapless 的「预看下一首」不会因为一次预看就把序列洗掉。
    fn next_index(&self) -> Option<i64> {
        let len = self.queue.len() as i64;
        if len == 0 {
            return None;
        }
        if self.shuffle {
            return match self.shuffle_pos() {
                Some(p) if p + 1 < self.shuffle_order.len() => Some(self.shuffle_order[p + 1]),
                // 序列走完（或当前曲不在序列里）：交给 advance 决定重洗还是停。
                _ => None,
            };
        }
        let nxt = self.index + 1;
        if nxt < len {
            Some(nxt)
        } else if self.repeat == RepeatMode::All {
            Some(0)
        } else {
            None
        }
    }

    /// 当前曲在随机序列中的位置。序列为空 / 当前曲不在其中（队列刚变）→ `None`。
    fn shuffle_pos(&self) -> Option<usize> {
        self.shuffle_order.iter().position(|&i| i == self.index)
    }

    /// 洗出一个新的随机序列（Fisher-Yates）。`pin_first` 若给定且在队列内，
    /// 洗完把它换到首位——用于「开随机 / 换队列」时不打断正在听的这首。
    fn rebuild_shuffle_order(&mut self, pin_first: Option<i64>) {
        let len = self.queue.len();
        self.shuffle_order = (0..len as i64).collect();
        for i in (1..len).rev() {
            let j = (pseudo_rand() % (i as u64 + 1)) as usize;
            self.shuffle_order.swap(i, j);
        }
        if let Some(f) = pin_first {
            if let Some(p) = self.shuffle_order.iter().position(|&x| x == f) {
                self.shuffle_order.swap(0, p);
            }
        }
    }

    fn prev(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        // 若已播放 >3s，prev 回到本曲开头；否则上一首。
        if self.position_secs() > 3.0 {
            self.start_current();
            return;
        }
        // 随机播放：上一首 = 序列里的前一个（即刚才真正听过的那首），
        // 而不是队列下标 -1（那是「列表上一首」，随机模式下从没播过）。
        if self.shuffle {
            if let Some(p) = self.shuffle_pos() {
                let target = if p > 0 {
                    Some(self.shuffle_order[p - 1])
                } else if self.repeat == RepeatMode::All {
                    self.shuffle_order.last().copied()
                } else {
                    None
                };
                if let Some(i) = target {
                    self.index = i;
                }
                self.start_current();
                return;
            }
        }
        let len = self.queue.len() as i64;
        let prev = if self.index > 0 {
            self.index - 1
        } else if self.repeat == RepeatMode::All {
            len - 1
        } else {
            0
        };
        self.index = prev;
        self.start_current();
    }

    fn seek(&mut self, secs: f64) {
        // 处于 gapless 预切窗口（解码器已换到下一首、当前曲尾巴还在缓冲）：先回到当前曲的
        // 干净状态再 seek，避免 seek 到错误的下一首解码器。
        if !self.pending_switch.is_empty() {
            self.pending_switch.clear();
            self.start_current();
        }
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        // 清 sink 缓冲：drop 当前 producer 数据不可行（rtrb 无 clear），改为重建解码位置 +
        // 重置进度基准；缓冲里残留旧帧会被很快消费掉。为正确性，重建解码器位置后让
        // frames_base 对齐当前 frames_played，position_base 设为实际 seek 到的秒。
        match cur.decoder.seek(secs) {
            Ok(actual_secs) => {
                cur.position_base_secs = actual_secs;
                cur.frames_base = self.frames_played.load(Ordering::Relaxed);
                cur.decoded_eof = false;
                cur.pending_output.clear();
                cur.pending_pos = 0;
                if let Some(rs) = cur.resampler.as_mut() {
                    rs.in_accum.clear();
                }
                self.publish_state();
            }
            Err(e) => {
                warn!(target: "music", "seek 失败: {e:#}");
                self.emit_error(format!("跳转失败: {e}"));
            }
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if self.current.is_none() {
            return;
        }
        if paused && self.status == PlayStatus::Playing {
            if let Some(cur) = self.current.as_mut() {
                cur.sink.sink.pause();
            }
            self.status = PlayStatus::Paused;
            self.publish_state();
        } else if !paused && self.status == PlayStatus::Paused {
            if let Some(cur) = self.current.as_mut() {
                cur.sink.sink.resume();
            }
            self.status = PlayStatus::Playing;
            self.publish_state();
        }
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.apply_volume_atomic();
        self.publish_state();
    }

    /// 切换输出模式：存模式；若正在播放，原位重建 sink 使新模式即时生效（保留播放位置/暂停态）。
    fn set_output_mode(&mut self, mode: OutputMode) {
        if self.output_mode == mode {
            return;
        }
        self.output_mode = mode;
        self.rebuild_sink_in_place("输出模式切换");
    }

    /// 原位重建 sink：保留当前曲、播放位置与暂停态，只把输出链路换成按当前状态重新协商的新流。
    ///
    /// 实现即「从曲首 `start_current` + `seek` 回原位」——sink 与解码器在 [`Current`] 里同生共死，
    /// 单独换 sink 要重做重采样器/声道适配/进度基准，不如整条重建来得可靠（seek 成本远小于
    /// 一次设备协商）。重建失败（设备真没了）由 `start_current` 走 emit error + stopped。
    fn rebuild_sink_in_place(&mut self, reason: &str) {
        if self.current.is_none() {
            return;
        }
        let pos = self.position_secs();
        let was_paused = self.status == PlayStatus::Paused;
        info!(target: "music", "重建输出链路（{reason}），回到 {pos:.1}s");
        self.last_rebuild = Instant::now();
        self.start_current();
        if self.current.is_none() {
            return; // 重建失败：start_current 已 emit 错误并置 stopped
        }
        if pos > 0.0 {
            self.seek(pos);
        }
        if was_paused {
            self.set_paused(true);
        }
    }

    /// 系统默认输出设备变了：当前 sink 不在该设备上就重建跟过去。
    ///
    /// 比对而非无脑重建，是因为变更通知与当前流未必相关（例如刚重建过、已经在新设备上，
    /// 或者变的是采集端引发的连带通知）——无谓重建会造成一次可闻的断音。
    fn on_default_device_changed(&mut self, new_id: Option<String>) {
        let Some(cur) = self.current.as_ref() else {
            return; // 没在播：下次 start_current 自然用新默认设备
        };
        if cur.sink.device_id == new_id {
            return;
        }
        self.rebuild_sink_in_place("系统默认输出设备变更");
    }

    /// 输出链路已死（拔设备 / 驱动重启 / 独占被抢）→ 重建。带最小间隔节流，避免设备持续
    /// 不可用时死循环重建刷 CPU；被节流时下一轮循环仍会再判，不会漏。
    fn check_sink_alive(&mut self) {
        let dead = matches!(&self.current, Some(cur) if !cur.sink.alive.load(Ordering::Relaxed));
        if !dead || self.last_rebuild.elapsed() < REBUILD_MIN_INTERVAL {
            return;
        }
        warn!(target: "music", "输出链路已断开，尝试重建");
        self.rebuild_sink_in_place("输出设备丢失");
    }

    /// 写定点音量给 sink。独占 bit-perfect 时旁路（置 1.0），不污染原始 PCM。
    fn apply_volume_atomic(&self) {
        let effective = match &self.current {
            Some(cur) if cur.actual.exclusive => 1.0,
            _ => perceptual_gain(self.volume),
        };
        self.volume_q16
            .store((effective * 65536.0) as u32, Ordering::Relaxed);
    }

    fn stop(&mut self) {
        self.current = None;
        self.pending_switch.clear();
        self.status = PlayStatus::Stopped;
        self.index = -1;
        self.frames_played.store(0, Ordering::Relaxed);
        self.publish_state();
    }

    fn current_track(&self) -> Option<&Track> {
        if self.index < 0 {
            return None;
        }
        self.tracks_meta
            .get(self.index as usize)
            .and_then(|o| o.as_ref())
    }

    /// 懒解析当前曲元数据（用 lofty；扫描已做过，这里为 emit 给前端补全）。
    fn ensure_current_meta(&mut self) {
        if self.index < 0 {
            return;
        }
        let i = self.index as usize;
        if self
            .tracks_meta
            .get(i)
            .map(|o| o.is_some())
            .unwrap_or(false)
        {
            return;
        }
        let Some(path) = self.queue.get(i).cloned() else {
            return;
        };
        // 复用扫描的单文件读取：covers 落 workspace 的 covers 目录（构造时注入）。
        let meta = super::scan::read_track_pub(std::path::Path::new(&path), &self.covers_dir);
        if let Some(slot) = self.tracks_meta.get_mut(i) {
            *slot = Some(meta);
        }
    }

    fn emit_track_changed(&mut self) {
        self.ensure_current_meta();
        let track = self.current_track().cloned();
        let _ = self.app.emit(
            "music_track_changed",
            json!({ "index": self.index, "track": track }),
        );
    }

    fn emit_format_changed(&self, actual: ActualFormat) {
        let _ = self.app.emit(
            "music_format_changed",
            json!({
                "sample_rate": actual.sample_rate,
                "bits": actual.bits,
                "channels": actual.channels,
                "exclusive": actual.exclusive,
                "resampled": actual.resampled,
            }),
        );
    }

    fn emit_error(&self, message: String) {
        let _ = self.app.emit("music_error", json!({ "message": message }));
    }

    /// 写共享原子 + emit `music_state_changed`。
    fn publish_state(&mut self) {
        self.ensure_current_meta();
        let track = self.current_track().cloned();
        self.shared
            .status
            .store(status_code(self.status), Ordering::Relaxed);
        self.shared.index.store(self.index, Ordering::Relaxed);
        let _ = self.app.emit(
            "music_state_changed",
            json!({
                "status": self.status.as_str(),
                "index": self.index,
                "track": track,
            }),
        );
    }

    /// 构造完整 PlaybackState 快照（`music_get_state` 用）。
    pub fn snapshot(&mut self) -> super::types::PlaybackState {
        self.ensure_current_meta();
        let track = self.current_track().cloned();
        super::types::PlaybackState {
            status: self.status.as_str().to_string(),
            index: self.index,
            track,
            position_secs: self.position_secs(),
            duration_secs: self.duration_secs(),
            volume: self.volume,
            repeat: self.repeat.as_str().to_string(),
            shuffle: self.shuffle,
        }
    }
}

fn buffered_output_frames(cur: &Current) -> usize {
    let ch = cur.actual.channels.max(1) as usize;
    cur.producer_capacity
        .saturating_sub(cur.sink.producer.slots())
        / ch
}

/// 把一批源交织样本（可能重采样后）push 到 sink producer。producer 满时保留剩余样本，
/// 下轮继续写，避免 FLAC/重采样大块输出被截断导致听感加速。
fn push_chunk(cur: &mut Current, samples: &[f32]) {
    let src_ch = cur.src_fmt.channels.max(1) as usize;
    let dst_ch = cur.actual.channels.max(1) as usize;

    // 1) 采样率：必要时重采样到 sink 实际率（重采样器按源声道工作）。
    let rate_adj: std::borrow::Cow<[f32]> = match cur.resampler.as_mut() {
        Some(rs) => {
            let mut resampled = Vec::new();
            if rs.process(samples, &mut resampled).is_err() {
                return;
            }
            std::borrow::Cow::Owned(resampled)
        }
        None => std::borrow::Cow::Borrowed(samples),
    };

    // 2) 声道：源声道 → sink 实际声道。设备声道与源不同（如单声道音轨 + 立体声设备）时
    //    必须适配，否则输出回调按设备声道消费按源声道交织的流 → 播放加速/音高错乱/串扰。
    if src_ch == dst_ch {
        push_or_buffer(cur, &rate_adj);
    } else {
        let mut remapped = Vec::with_capacity(rate_adj.len() / src_ch * dst_ch + dst_ch);
        remap_channels(&rate_adj, src_ch, dst_ch, &mut remapped);
        push_or_buffer(cur, &remapped);
    }
}

/// 把交织 f32 从 `src_ch` 声道重排到 `dst_ch` 声道，结果追加到 `out`。
/// 规则：单声道→多声道复制；多声道→单声道取平均；其余取前 min(src,dst) 声道、多出补 0。
fn remap_channels(input: &[f32], src_ch: usize, dst_ch: usize, out: &mut Vec<f32>) {
    if src_ch == dst_ch {
        out.extend_from_slice(input);
        return;
    }
    let frames = input.len() / src_ch.max(1);
    if src_ch == 1 {
        for &s in input {
            for _ in 0..dst_ch {
                out.push(s);
            }
        }
    } else if dst_ch == 1 {
        for f in 0..frames {
            let base = f * src_ch;
            let sum: f32 = input[base..base + src_ch].iter().sum();
            out.push(sum / src_ch as f32);
        }
    } else {
        for f in 0..frames {
            let base = f * src_ch;
            for c in 0..dst_ch {
                out.push(if c < src_ch { input[base + c] } else { 0.0 });
            }
        }
    }
}

fn push_or_buffer(cur: &mut Current, samples: &[f32]) {
    debug_assert!(pending_output_empty(cur));
    let mut idx = 0usize;
    while idx < samples.len() {
        if cur.sink.producer.push(samples[idx]).is_err() {
            cur.pending_output.extend_from_slice(&samples[idx..]);
            cur.pending_pos = 0;
            break;
        }
        idx += 1;
    }
}

fn flush_pending_output(cur: &mut Current) -> bool {
    while cur.pending_pos < cur.pending_output.len() {
        if cur
            .sink
            .producer
            .push(cur.pending_output[cur.pending_pos])
            .is_err()
        {
            return false;
        }
        cur.pending_pos += 1;
    }
    if !cur.pending_output.is_empty() {
        cur.pending_output.clear();
        cur.pending_pos = 0;
    }
    true
}

fn pending_output_empty(cur: &Current) -> bool {
    cur.pending_pos >= cur.pending_output.len()
}

fn status_code(s: PlayStatus) -> u8 {
    match s {
        PlayStatus::Stopped => 0,
        PlayStatus::Playing => 1,
        PlayStatus::Paused => 2,
    }
}

/// 滑块位置（0..=1）→ 实际线性增益。
///
/// 人耳对响度的感知接近对数：线性映射下滑块上半程几乎听不出变化，全部动态都挤在末尾一小段。
/// 三次方是音频软件常用的近似（v=0.5 → 约 −18dB，听感恰好「一半」），且两端精确：
/// v=0 是真静音、v=1 是原始电平（满音量时乘 1.0，不引入任何量化误差）。
///
/// 只作用于共享模式；独占 bit-perfect 下软件音量整体旁路，不走这里。
fn perceptual_gain(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    v * v * v
}

/// 轻量伪随机（shuffle 用，无需密码学强度）。基于时间种子的 xorshift。
fn pseudo_rand() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = Cell::new({
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15);
            t | 1
        });
    }
    STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x
    })
}
