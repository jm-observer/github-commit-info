# zero-desktop 音乐播放模块 设计文档

> 状态：设计稿（待评审）· 作者：fengqi · 日期：2026-06-16
> 关联：[zero-desktop 模块化架构](../crates/zero-desktop/)（english / speech / cookie 同款套路）

## 1. 目标与范围

在 zero-desktop 里集成**本地音乐播放**，**按最高音质标准（bit-perfect / hi-res）设计**：

- **曲库来源**：用户选定的本地文件夹，后端递归扫描音频文件。
- **形态**：独立模块页「音乐」（曲库浏览 / 播放列表）+ **常驻迷你播放器**（底栏，跨页不中断）。
- **架构基调**：**UI 只是控制面（control surface）**——选曲、按钮、进度展示；**解码、混音、输出全部在 Rust 后端**，由一个原生音频引擎统一负责。**不使用浏览器 `<audio>` 播放音频。**

非目标（本期不做）：在线流媒体 / 版权曲库、歌词滚动、均衡器/音效、跨设备同步。

## 2. 为什么"播放下沉到后端"是正确选择（音质论证）

> **解码阶段不丢音质；丢音质的是"输出阶段"——重采样 + 系统共享混音器。**
> 浏览器 `<audio>` 永远在 Windows 共享混音器后面，**做不到 bit-perfect**；只有原生后端能把文件 PCM 原样、按原始采样率、绕过混音器直送 DAC。

| 环节 | 浏览器 `<audio>` / Web Audio | **原生后端（本设计）** |
|---|---|---|
| 采样率 | 强制重采样到固定 48kHz；hi-res 96/192k 被降采样 | **跟随文件原始采样率**切换输出 |
| 系统混音器 | 必经 Windows 共享混音器 + 系统音效 | **WASAPI 独占绕过混音器**，PCM 直达 DAC |
| 位深 | 经系统混音降到 16/24bit | bit-perfect 原始位深 |
| 重采样质量 | 浏览器内置（非发烧级） | 必要时用 Rubato sinc（发烧级），原生速率时**零重采样** |
| 无缝切歌 | ❌ 有间隙 | ✅ gapless（预解码下一首） |
| 设备/独占控制 | ❌ 无 | ✅ 设备选择、独占/共享、event-driven |

无损格式（FLAC/WAV/ALAC）解码出的 PCM 逐位相同——音质差异**完全来自输出链路**，所以把播放放到后端是拿到原始音质的**前提**。

## 3. 后端音频引擎选型（Rust）

| crate | 角色 | 说明 |
|---|---|---|
| **`symphonia`** | 解码 + 解封装 | 纯 Rust，FLAC/MP3/AAC/ALAC/Ogg/Vorbis/WAV/MP4 全套，逐位精确、支持 gapless。解码核心。 |
| **`cpal`** | 跨平台音频输出 | Win=WASAPI / mac=CoreAudio / Linux=ALSA。默认共享模式，跨平台兜底。 |
| **`wasapi`** | Windows 独占模式 | bit-perfect 的关键：WASAPI **exclusive + event-driven**，cpal 对独占支持不完整时由它接管（CamillaDSP 作者维护）。 |
| **`rubato`** | 高质量重采样 | 仅当输出设备不支持文件原始采样率时启用（sinc，发烧级）。原生速率匹配时**不经过它**。 |
| **`rtrb`** / `ringbuf` | 无锁环形缓冲 | 解码线程 → 输出回调之间的实时安全 SPSC 队列。 |
| **`lofty`** | 标签/封面元数据 | title/artist/album/duration/封面，与播放链路解耦。 |

**输出后端抽象成 trait `AudioSink`**，两个实现：
- `WasapiExclusiveSink`（Windows，**默认首选**，bit-perfect）。
- `CpalSink`（跨平台兜底 / 非 Windows / 独占协商失败时回退）。

引擎按"**优先独占 + 跟随原始采样率**，失败逐级回退（独占→共享、原始速率→Rubato 重采样）"协商，并把实际生效的"采样率/位深/独占与否"通过事件回报 UI（让用户看到是不是真 bit-perfect）。

## 4. 架构总览

```
┌─ UI (React) — 纯控制面 ───────────────────────────────────┐
│  MusicPage（曲库/列表）   MiniPlayer（底栏常驻）           │
│        │  invoke 命令              ▲  Tauri events          │
│        │  play/pause/seek/next…    │  state/progress/ended  │
└────────┼──────────────────────────┼───────────────────────┘
         ▼                          │
┌─ Rust: modules/music ─────────────┴───────────────────────┐
│  MusicState { tx: Sender<AudioCommand>, shared: Arc<...> } │
│  命令: pick_folder / scan / play_queue / pause / resume /  │
│        seek / next / prev / set_volume / stop              │
│                     │ AudioCommand                         │
│  ┌──────────────────▼─────────────────────────────────┐   │
│  │ AudioEngine（专用 std 线程，actor 模式）            │   │
│  │  队列管理 + 自动续播 + gapless 预解码               │   │
│  │  Symphonia 解码 ─► rtrb 环形缓冲 ─► AudioSink 回调   │   │
│  │  AudioSink = WasapiExclusive（首选）│ Cpal（回退）   │   │
│  └────────────────────────────────────────────────────┘   │
└───────────────────────────────────────────────────────────┘
```

**核心原则**：后端是**单一播放真值源**——队列、当前曲、播放位置、自动续播、gapless 全在后端；UI 只发命令 + 渲染事件。

## 5. 后端设计（`crates/zero-desktop/src/modules/music/`）

### 5.1 模块文件划分

```
modules/music/
  mod.rs        # Tauri 命令 + MusicState + setup（建引擎线程、注册事件）
  engine.rs     # AudioEngine：控制循环、队列、自动续播、gapless 调度
  decode.rs     # Symphonia 解码包装（open/seek/decode→f32 帧）
  sink/
    mod.rs      # AudioSink trait（start/write/stop/actual_format）
    wasapi.rs   # Windows 独占 bit-perfect 实现（cfg(windows)）
    cpal.rs     # 跨平台共享模式回退
  scan.rs       # 递归扫描 + lofty 元数据
  types.rs      # Track / PlaybackState / AudioFormat 等
```

### 5.2 状态、命令、事件

```rust
pub struct MusicState {
    tx: Sender<AudioCommand>,            // → 引擎线程
    shared: Arc<SharedPlayback>,         // 原子快照：position / playing / index
}

// 控制命令（UI → 引擎，全异步，立即返回）
enum AudioCommand {
    PlayQueue { tracks: Vec<String>, start: usize },
    Pause, Resume, TogglePlay,
    Seek { secs: f64 },
    Next, Prev,
    SetVolume(f32),                      // 软件音量；独占 bit-perfect 时可绕过/置 1.0
    SetRepeat(RepeatMode), SetShuffle(bool),
    Stop,
}

#[tauri::command] async fn music_pick_folder(app) -> Option<String>
#[tauri::command] async fn music_scan(dir: String) -> Result<Vec<Track>, String>
#[tauri::command] fn music_play_queue(state, paths: Vec<String>, start: usize)
#[tauri::command] fn music_pause(state) / music_resume / music_toggle / music_stop
#[tauri::command] fn music_seek(state, secs: f64)
#[tauri::command] fn music_next(state) / music_prev
#[tauri::command] fn music_set_volume(state, vol: f32)
#[tauri::command] fn music_set_repeat / music_set_shuffle
#[tauri::command] fn music_get_state(state) -> PlaybackState   // 首屏/自愈拉取
```

**事件（引擎 → UI，`app.emit`）：**
- `music_state_changed` → `{ status: playing|paused|stopped, index, track }`
- `music_progress` → `{ position_secs, duration_secs }`（节流 ~250ms）
- `music_format_changed` → `{ sample_rate, bits, exclusive, resampled }`（让 UI 显示是否真 bit-perfect）
- `music_track_changed` → 自动续播 / next 后的新曲
- `music_error` → 引擎侧错误（解码失败、设备丢失等）

> 这与 speech 模块的事件模式（`speech_recording_state_changed` + 轮询自愈）一致；UI 既听事件、也可周期 `music_get_state` 兜底竞态。

### 5.3 AudioEngine（actor 模式，专用线程）

- **不进 tokio**：音频是实时负载，引擎跑在独立 `std::thread`，命令经 `crossbeam-channel` 进入。
- **控制循环**：`select` { 收命令 → 改状态/seek/切曲；解码定时推帧 → rtrb }。
- **解码 → 输出**：解码线程把 Symphonia 帧（f32 交织）写 `rtrb` 环形缓冲；`AudioSink` 的实时回调只从缓冲拉数据（回调内**零分配、零锁**）。
- **跟随采样率**：切到不同采样率的曲子时，重建 `AudioSink`（按新曲原始速率开输出流）；设备不支持该速率才用 Rubato 重采样并置 `resampled=true`。
- **gapless**：当前曲将尽时预解码下一首，无缝衔接。
- **自动续播**：曲终→按 repeat/shuffle 推进队列，emit `music_track_changed`。
- **进度**：输出回调累计已消费帧数 → `SharedPlayback` 原子量；控制循环节流 emit `music_progress`。

### 5.4 AudioSink trait

```rust
trait AudioSink {
    fn start(&mut self, fmt: AudioFormat) -> Result<()>;   // 协商独占/速率/位深
    fn actual_format(&self) -> AudioFormat;                // 实际生效（回报 UI）
    fn write(&mut self, frames: &[f32]) -> Result<()>;     // 或回调拉模型
    fn pause(&mut self); fn resume(&mut self); fn stop(&mut self);
}
```

- `wasapi.rs`：`IAudioClient` 独占 + event-driven，按文件原始 `WAVEFORMATEXTENSIBLE` 申请；失败回退共享。
- `cpal.rs`：`build_output_stream`，挑最接近的 supported config；非 Windows 默认走它。

### 5.5 元数据扫描（`scan.rs`）

- 递归扫 `mp3/flac/wav/m4a/aac/ogg/opus`，`lofty` 取 title/artist/album/duration/封面。
- 大目录：异步 + 可分批 emit 进度（MVP 可先一次性返回）。
- 封面：内嵌图落临时文件，前端用 `convertFileSrc` 显示**图片**（图片仍走 asset 协议，只有**音频**不走浏览器）。

### 5.6 持久化与目录

- 选中文件夹路径、音量、repeat/shuffle：`tauri-plugin-store`（与 english 同套路）。
- 音乐文件留用户文件夹，**不进 workspace、不建 SQLite**。
- **封面临时文件**落 workspace（如 `music/covers/`），需在 `ensure_workspace` 加子目录 + asset scope 放行。

### 5.7 后端接线清单

1. `Cargo.toml` → 加 `symphonia`(含所需 codec features) / `cpal` / `wasapi`(cfg windows) / `rubato` / `rtrb` / `lofty` / `crossbeam-channel`
2. `src/modules/mod.rs` → `pub mod music;`
3. `src/app_state.rs` → `AppState` 加 `pub music: Arc<MusicState>`，`AppState::new` 建引擎线程
4. `src/main.rs` → `invoke_handler!` 注册全部命令；`.setup()` 给引擎注入 `AppHandle`（emit 用）+ 读 store 已选目录授权封面 scope
5. `src/shared/workspace.rs` → `ensure_workspace` 加 `music/covers`

## 6. 前端设计（`crates/zero-desktop/ui/src/modules/music/`）

UI **不持有任何音频对象**，纯命令 + 事件。

### 6.1 `PlayerContext.tsx`（全局，无 `<audio>`）

- 挂在 `App.tsx` 的 `<ShellLayout>` 外层；启动 `listen` 三个事件（state/progress/track_changed）→ React state；首屏 `music_get_state` 拉初值。
- 暴露 `play(paths, start)/pause/resume/toggle/seek/next/prev/setVolume` —— 全是 `invoke`，无本地播放逻辑。
- 切路由不卸载 → 播放与状态订阅不中断（播放本就在后端，UI 卸载也不停）。

### 6.2 `MusicPage.tsx`（侧栏「音乐」）

- 顶部：当前文件夹 + 「选择文件夹」「重新扫描」+ **实时格式徽标**（来自 `music_format_changed`：`FLAC 96kHz/24bit · 独占 · 无重采样`）。
- 主体：曲库列表（标题/歌手/时长/封面），搜索过滤，点击 → `music_play_queue(列表, index)`。
- 复用 `chat-summary` / `english` 的 Tailwind 卡片风格。

### 6.3 `MiniPlayer.tsx`（ShellLayout 底栏常驻）

- 封面 + 标题/歌手 + 播/暂停/上下首 + 进度条（拖动 → `music_seek`）+ 音量 + repeat/shuffle。
- 全从 `usePlayer()` 取；任何页面可控。

### 6.4 前端接线清单

1. `App.tsx` → 加 `<Route path="music">`；外层包 `<MusicPlayerProvider>`
2. `shared/ShellLayout.tsx` → `navItems` 加 `{ to:'/music', icon: Music, label:'音乐' }`（lucide `Music`）；底部渲染 `<MiniPlayer/>`
3. 新增 `ui/src/modules/music/`：`MusicPage.tsx` / `PlayerContext.tsx` / `MiniPlayer.tsx` / `api/tauri-client.ts`

## 7. 端到端路径

```
选文件夹 → music_pick_folder → music_scan(lofty) → Track[]
  → 点歌 → invoke music_play_queue(paths, i)
  → 引擎线程: Symphonia 解码 → rtrb → WasapiExclusiveSink(原始速率/位深, 绕混音器) → DAC
  → emit music_state_changed / music_progress / music_format_changed
  → MiniPlayer + MusicPage 渲染（含 bit-perfect 徽标）
```

## 8. 分期落地

- **一期 · 原生引擎打通**：`AudioEngine` + Symphonia 解码 + `CpalSink`（共享模式，跨平台）+ 队列/续播/seek/音量 + 全套命令事件；前端 Context + MusicPage + MiniPlayer。**已是后端解码、无浏览器音频**，但暂用共享模式。
- **二期 · bit-perfect**：`WasapiExclusiveSink`（独占 + event-driven + 跟随原始采样率）+ Rubato 回退 + 格式徽标。Windows 拿到真 bit-perfect。
- **三期 · 体验增强**：gapless 预解码、按歌手/专辑分组、播放列表持久化、ReplayGain/音量归一（可选）。

## 9. 风险与注意

- **实时回调纪律**：`AudioSink` 回调内**禁止分配/锁/系统调用**，只读 `rtrb`；违反会爆音/卡顿。
- **独占模式协商失败**：设备被占用 / 不支持原始格式 → 必须优雅回退共享并如实回报 UI（别假装 bit-perfect）。
- **采样率切换开销**：跨采样率切曲要重建输出流，有短暂静默；gapless 仅在同格式相邻曲生效。
- **seek 与缓冲一致性**：seek 要清空 rtrb + 重置 Symphonia 位置 + 重算进度基准，避免回跳。
- **设备热插拔**：输出设备消失要捕获并 emit `music_error`，引擎进 stopped 而非崩溃。
- **跨平台**：独占是 Windows 专属优化；macOS/Linux 走 `CpalSink`（CoreAudio/ALSA 已是较短路径），按平台 `cfg` 分流。

---

### 参考

- [symphonia (crates.io)](https://crates.io/crates/symphonia) — 解码/解封装，支持 gapless
- [CPAL — Rust audio library (lib.rs)](https://lib.rs/crates/cpal) · [RustAudio/cpal (GitHub)](https://github.com/RustAudio/cpal)
- [rodio decoder docs](https://docs.rs/rodio/latest/rodio/decoder/) — 高层封装参考（本设计不直接用，自管引擎）
- [WASAPI Exclusive vs Shared Mode 说明](https://aurisplayer.com/blog/wasapi-exclusive-guide.html)
