# 统一桌面壳设计

## 0. 背景

当前有三个桌面端相关项目：

| 项目 | 路径 | 定位 | 技术形态 |
|---|---|---|---|
| 英语学习桌面端 | `D:\git\english\desktop-app` | 英语听力音频播放、标注句子播放、环境配置 | Tauri 2 + React 18 + AntD |
| Toolkit Desktop | `crates/toolkit-desktop` | 抖音 / 同花顺登录态采集、Cookie 同步、G10 bridge | Tauri 2 + Rust workspace crate + 原生 HTML/JS UI |
| Streaming Speech | `D:\git\streaming-speech\src-tauri` | 麦克风录音、远程 ASR、翻译、托盘通知 | Tauri 2 + React 19 + Tailwind/lucide |

三者都属于“本机桌面伴随工具”：运行在 Windows 桌面，连接远端服务或本机资源，承载较多状态型能力。统一桌面壳的目标不是把代码简单拼接成一个大程序，而是提供一个统一入口、统一运行时规范、统一发布渠道，同时保留每个能力的模块边界和敏感状态隔离。

## 1. 目标与非目标

### 1.1 目标

- 一个桌面程序承载三个入口：英语听力、语音识别、Cookie 采集。
- 统一 Tauri 2 运行时、窗口、导航、日志、配置目录、自更新、打包发布。
- 后端按模块注册 command、state、后台任务和插件，避免互相污染。
- 前端按页面集成，用户在同一壳内切换工具。
- Cookie、录音、音频缓存等状态保持隔离，降低故障和安全风险。
- 第一阶段以最小可用迁移为主，不重写核心业务逻辑。

### 1.2 非目标

- 不把所有业务揉成一个无边界的 Rust/React 单体。
- 不在首版重做英语播放、ASR、Cookie 采集的业务能力。
- 不把 G10 server、ASR orchestrator 等远端服务塞进桌面程序。
- 不把敏感 Cookie 与普通学习/语音数据放在同一个存储命名空间。
- 不在首版统一所有 UI 视觉细节；先统一壳和路由，再逐步整理设计系统。

## 2. 总体判断

统一桌面壳是合理的，但必须采用“Host + Modules”模式。

推荐落点是在当前 Rust workspace 内演进 `toolkit-desktop`，或新增一个同级 crate，例如 `crates/zero-desktop`。原因：

- 当前 `toolkit-desktop` 已经接入 `custom-utils`、`prod` feature、updater、CLI/workspace 参数，最接近本仓工具规约。
- `english` 的 Tauri 后端很薄，适合迁移前端页面，不适合作为统一后端底座。
- `streaming-speech` 的录音、托盘、窗口模式和自动录音逻辑强绑定自身业务，直接作为总壳会让“语音识别”成为应用中心。

若追求低风险，建议新建 `crates/zero-desktop`；若追求少建工程，则改造 `crates/toolkit-desktop`。本文后续用 `zero-desktop` 指代统一壳，不强制最终 crate 名。

## 3. 架构概览

```
zero-desktop
├─ ui/                         # 统一前端应用
│  ├─ src/App.tsx              # ShellLayout + 路由
│  ├─ modules/
│  │  ├─ english/              # 英语听力页面
│  │  ├─ speech/               # 语音识别页面
│  │  └─ cookie/               # Cookie 采集页面
│  └─ shared/                  # 通用 UI、Tauri client、状态提示
└─ src/
   ├─ main.rs                  # CLI、logger、Tauri Builder
   ├─ app_state.rs             # 总状态容器
   ├─ modules/
   │  ├─ english/              # 英语模块后端，首版可很薄
   │  ├─ speech/               # 从 streaming-speech 迁入
   │  └─ cookie/               # 从 toolkit-desktop 迁入
   └─ shared/
      ├─ workspace.rs          # 统一工作目录
      ├─ settings.rs           # 全局设置
      ├─ update.rs             # updater 子命令
      └─ error.rs              # command 错误转换
```

运行时结构：

```
Windows Desktop
└─ zero-desktop.exe
   ├─ Tauri WebView 主窗口
   │  ├─ /english
   │  ├─ /speech
   │  └─ /cookie
   ├─ speech module
   │  ├─ microphone capture
   │  ├─ websocket to ASR orchestrator
   │  └─ tray / notification / clipboard
   ├─ cookie module
   │  ├─ Chrome profile per platform
   │  ├─ cookie watcher / uploader
   │  └─ local bridge 127.0.0.1:28788
   └─ english module
      ├─ HTTP API access
      ├─ audio cache
      └─ playback UI
```

## 4. 模块边界

### 4.1 English 模块

来源：`D:\git\english\desktop-app`

职责：

- 获取标注句子 / 全量句子列表。
- 缓存音频文件。
- 播放音频并展示标注、报错、播放状态。
- 管理环境配置和 `customer_id`。

迁移方式：

- 首版直接迁 React 组件和 service。
- 保留前端调用 `@tauri-apps/plugin-http`、`plugin-store`、`plugin-fs`。
- 音频缓存目录统一放到 `workspace/english/audio-cache` 或 Tauri app data 下的 `english` 命名空间。
- 后端只需要初始化相关 Tauri plugin，不必重写为 Rust API。

注意点：

- 当前 `EnvConfigService.getConfigSync()` 不包含异步读取的 `customerId`，迁移时**必须**在播放初始化前 `await` 完整配置（包括 customerId），否则首次播放会用空 customerId 调 API。**阶段 4 验收前置项**：english 模块的 `bootstrap` 入口需要返回 `Promise<EnvConfig>`，shell 路由进入 `/english/*` 前 await 它；若 customerId 缺失则导航到设置页而不是播放页。
- 当前前端日志使用 `console.log`，可以保留；Rust 后端日志仍走 `custom-utils`。
- 英语模块不应依赖 Cookie 模块或 Speech 模块。
- english 现有 `plugin-store` 的 KV 数据**仅作为 english 模块内部存储**保留，不参与全局设置；全局设置（窗口、上次模块、主题）走 `app.json`，由 shell 直接读写，不经 plugin-store。

### 4.2 Speech 模块

来源：`D:\git\streaming-speech\src-tauri`

职责：

- 枚举输入设备、选择麦克风。
- 启停录音。
- 将 PCM 推送到远端 ASR orchestrator。
- 接收识别、润色、翻译事件并推给前端。
- 处理自动复制、托盘提醒、通知、窗口注意力。

迁移方式：

- 将 `src-tauri/src/commands`、`settings.rs`、`llm_settings.rs`、`db`、`lock_utils.rs` 按模块迁入 `modules/speech`。
- 将 `AppState` 拆出为 `SpeechState`，由总 `AppState` 持有。
- command 全部增加前缀，避免与其他模块冲突。

命名建议：

| 当前 command | 统一壳 command |
|---|---|
| `start_recording` | `speech_start_recording` |
| `stop_recording` | `speech_stop_recording` |
| `get_recording_state` | `speech_get_recording_state` |
| `list_input_devices` | `speech_list_input_devices` |
| `get_settings` | `speech_get_settings` |
| `apply_settings` | `speech_apply_settings` |
| `fetch_remote_history` | `speech_fetch_remote_history` |

注意点：

- 录音和 Cookie 采集都可能有后台任务，`setup` 阶段要清晰启动各自任务。
- `streaming-speech` 当前有窗口简洁模式和大小控制逻辑，统一壳首版应先关闭或局部化该逻辑，避免它影响整个主窗口。
- 托盘图标属于全局资源，首版可先由 speech 模块注册，但后续应收敛到 shell 统一管理。

#### 4.2.1 Speech 在 shell 内的最小功能集（阶段 3 验收基线）

streaming-speech 原本是「应用即录音工具」，进入 shell 后必须裁掉所有把整个主窗口当成自己窗口的行为。**保留**：

- 输入设备枚举与选择。
- 启停录音、推送 PCM 到 ASR orchestrator、接收 segment/润色/翻译事件。
- 识别结果显示、复制到剪贴板（用户主动触发或开关控制）。
- 系统通知（来自 `tauri-plugin-notification`，shell 统一注册）。
- 语音历史 SQLite（`speech/speech_history.db`）。

**首版禁用 / 裁剪**：

- 主窗口尺寸/置顶/简洁模式切换（原 `compact_mode`、`resize_window` 类命令）——shell 主窗口只由用户拖拽控制，speech 模块不得调用 `app.get_webview_window().set_size/set_always_on_top`。
- 「自动录音」「窗口注意力闪烁」等强干扰行为，首版关闭，留作模块设置项后续重新打开。
- 托盘菜单项首版只保留「显示主窗口 / 退出」两个；speech 自己的「快速开始录音」托盘项不进首版，避免与 Cookie 模块未来的托盘需求互斥。

裁剪掉的逻辑不删代码，**移到 `modules/speech/legacy/` 下并 `#[allow(dead_code)]`**，等阶段 5 设计悬浮小窗时再启用。

### 4.3 Cookie 模块

来源：`crates/toolkit-desktop`

职责：

- 管理 G10 server base / token。
- 打开抖音、同花顺登录窗口。
- 采集 Cookie、检测登录态、同步到 G10。
- 提供本机 bridge 给 G10 web 读取 desktop 上下文。
- 解析当前抖音博主并写入 G10 博主库。

迁移方式：

- 将 `browser.rs`、`config.rs`、`db.rs`、`ths.rs`、`ths_watcher.rs`、`uploader.rs`、`bridge.rs`、`workspace.rs` 中与 cookie desktop 有关的内容迁入 `modules/cookie`。
- 将原 `AppCtx` 拆为 `CookieState`。
- command 全部增加前缀。

命名建议：

| 当前 command | 统一壳 command |
|---|---|
| `cmd_get_settings` | `cookie_get_settings` |
| `cmd_save_settings` | `cookie_save_settings` |
| `cmd_open_login` | `cookie_open_douyin_login` |
| `cmd_close_login` | `cookie_close_douyin_login` |
| `cmd_open_ths_login` | `cookie_open_ths_login` |
| `cmd_close_ths_login` | `cookie_close_ths_login` |
| `cmd_ping_server` | `cookie_ping_server` |
| `cmd_inspect_cookies` | `cookie_inspect_cookies` |
| `cmd_track_current_creator` | `cookie_track_current_creator` |

注意点：

- Chrome profile 必须按平台隔离，例如 `workspace/cookie/login_profile/douyin`、`workspace/cookie/login_profile/ths`。
- Cookie 采集页面不应自动暴露敏感 cookie 原文给其他模块。
- bridge 端口 `28788` 需要在文档和设置中显式展示；后续可支持配置化。

## 5. 前端设计

### 5.1 路由结构

```
/
├─ /english/annotated
├─ /english/all
├─ /speech
├─ /cookie
└─ /settings
```

首屏直接进入上次使用的模块；没有历史记录时进入 `/english/annotated` 或总览页。

### 5.2 Shell Layout

统一壳提供：

- 左侧导航：英语听力、语音识别、Cookie 采集、设置。
- 顶部状态区：G10 连接状态、ASR 连接状态、录音状态、Cookie 状态。
- 模块内容区：每个模块独立渲染。
- 全局错误/通知中心：展示跨模块错误，不吞掉模块自己的详细状态。

### 5.3 UI 技术选择

统一到一个 React/Vite + **Tailwind/lucide** 前端，不混用 AntD。

理由：

- AntD 5 用 CSS-in-JS（`@emotion`），与 Tailwind 的 preflight/utility 同台会出现 `:where()` 优先级互踩、暗色主题切换需要两套机制，长期维护成本高。
- english 仓里实际重度依赖的 AntD 组件集中在 `Table / Form / Modal / message`，数量可控，手工替换为 Tailwind + headless 组件（如 `@radix-ui/react-dialog` + `@tanstack/react-table`）一次到位。
- speech 仓本就是 Tailwind/lucide，迁移零摩擦；shell 本身的导航 / 状态栏 / 设置页也直接 Tailwind。

迁移时 english 模块允许**临时**保留少量 AntD 组件，但需在阶段 4 收尾前全部替换，不进入稳定期。

## 6. 后端设计

### 6.1 main.rs 职责

`main.rs` 只做：

- `custom_utils::logger::logger_feature(...)` 初始化。
- CLI 解析：`run`、`update`、可选 `--workspace`。
- 初始化 workspace。
- 构造 `AppState`。
- 注册 Tauri plugin。
- 注册所有模块 command。
- 调用各模块 `setup`。

### 6.2 AppState 结构

```rust
pub struct AppState {
    pub workspace: PathBuf,
    pub speech: Arc<speech::SpeechState>,
    pub cookie: Arc<cookie::CookieState>,
    pub english: Arc<english::EnglishState>,
}
```

各模块只能直接访问自己的 state；跨模块能力通过显式函数调用或事件，不共享内部字段。

### 6.3 command 注册

Tauri 2 的 `tauri::generate_handler!` 是**过程宏**，只能在单个调用点静态展开 `#[tauri::command]` 函数名列表，**不能跨 crate 动态拼接 handler**。统一壳采取「集中注册 + 模块导出」模式：

- 每个模块在 `modules/<name>/mod.rs` 用 `pub use` 导出本模块所有 `#[tauri::command]` 函数（命名一律带模块前缀，例如 `cookie_open_login`、`speech_start_recording`）。
- `main.rs` 一次性 `tauri::generate_handler![cookie::cookie_open_login, cookie::cookie_close_login, speech::speech_start_recording, ...]` 注册全部。
- 每个模块只暴露一个 `setup` 入口：

  ```rust
  pub fn setup(app: &tauri::AppHandle, state: Arc<ModuleState>) -> anyhow::Result<()>;
  ```

  负责初始化后台任务、注册事件监听、注册托盘菜单项（若有）。`main.rs` 在 `Builder::setup` 闭包里依次调用 `cookie::setup` / `speech::setup` / `english::setup`，任一失败整壳启动失败（不静默降级）。

命令命名前缀是**强约束**，由 CI grep 校验：模块外文件不得出现裸 `#[tauri::command]` 且函数名不带模块前缀的情况。

### 6.4 Tauri plugin

统一壳需要的插件：

- `tauri-plugin-fs`
- `tauri-plugin-http`
- `tauri-plugin-store`
- `tauri-plugin-dialog`
- `tauri-plugin-shell`
- `tauri-plugin-clipboard-manager`
- `tauri-plugin-notification`
- `tauri-plugin-autostart` 可选

插件由壳统一注册，模块只声明依赖，不自行重复初始化。

## 7. 数据与目录

建议统一 workspace：

```
{workspace}/
├─ app.json
├─ logs/
├─ english/
│  ├─ env-config.json
│  └─ audio-cache/
├─ speech/
│  └─ speech_history.db
└─ cookie/
   ├─ state.db
   ├─ settings.json
   └─ login_profile/
      ├─ douyin/
      └─ ths/
```

原则：

- 全局设置只放壳级配置，例如上次打开模块、窗口大小、主题。
- 模块业务状态放模块目录。
- Cookie、token、登录 profile 只属于 Cookie 模块。
- 语音历史和剪贴板/通知设置只属于 Speech 模块。
- 英语音频缓存可以清理，不应影响其他模块。
- **日志全局一个目录** `logs/`，由 shell 在 `main.rs` 用 `custom-utils` logger 单次初始化；模块通过 `tracing::info_span!(target: "speech", ...)` 等方式用 target 区分来源，**禁止模块自行调 `logger_feature`**（会冲突）。日志文件轮转策略由 shell 统一配。

## 8. 发布与更新

统一壳作为 workspace 中一个普通工具 crate：

- `prod = ["custom-utils/prod"]`。
- 提供 `update` 子命令。
- 发布构建使用 `--features prod`。
- Windows 桌面优先产出 NSIS 安装包。

`deploy-g10.ps1` 只负责 Linux/aarch64 CLI 工具部署；统一桌面壳主要面向 Windows，不一定需要加入 G10 部署列表。若未来需要 Linux 桌面版，再单独增加构建目标。

**updater 统一走 `custom-utils` updater**（与本仓其他 bin 一致，`REPO_OWNER`/`REPO_NAME` 指向 `jm-observer/toolkit`）。english 原仓使用的 Tauri 自带 updater + GitHub releases 流程在迁移时**移除**，避免一个壳里跑两套自更新。english 模块的 `plugin-store` 数据迁移路径：阶段 4 写一次性 migrator，把 `%APPDATA%/com.english.desktop/store.json` 读出后写入新 workspace 下的 `english/env-config.json`，旧文件保留一个版本周期作为回退。

## 8.1 trace-hub 集成

桌面壳同样接入 `custom-utils` 的 trace feature，与 toolkit-server 共享一套 trace-hub：

- 启动时若设了 `TRACE_HUB_ENDPOINT`，则初始化 trace exporter；**未设则完全无副作用**，与本仓约定一致。
- 每个模块 command 入口包一层 `SpanScope` 两阶段 span：anchor（入参摘要、用户操作来源）+ 完成 span（耗时 + 成败）。例如 `speech_start_recording` / `cookie_open_login` / `english_play_audio`。
- 长后台任务（录音 capture loop、cookie watcher、ths watcher）用独立顶层 span，子操作挂在其下；便于「为什么录音卡了 / cookie 怎么没同步 / 音频播不出来」跨 G10 与桌面端串联排查。
- W3C `traceparent` 透传：speech 推 PCM、cookie uploader 调 G10、english 拉句子列表，三处 HTTP 调用都需要在请求头注入当前 trace 上下文。

## 9. 迁移计划

### 阶段 0：确认壳形态

产出：

- 确认 crate 名：`zero-desktop` 或继续 `toolkit-desktop`。
- 确认前端技术：短期混用或直接统一 Tailwind。
- 确认是否保留现有 `toolkit-desktop` 独立二进制。

建议：新建 `crates/zero-desktop`，保留旧 `toolkit-desktop` 到新壳稳定。

### 阶段 1：搭建空壳

产出：

- Tauri 2 desktop crate。
- React/Vite 前端。
- ShellLayout、导航、空模块页面。
- logger、workspace、update 子命令。
- 基础 CI/build 能通过。

验收：

- `cargo clippy -- -D warnings`
- `cargo fmt --check`
- `cargo test`
- 前端 `npm run build`

### 阶段 2：迁入 Cookie 模块

产出：

- Cookie 采集页面替换原 `ui/index.html`。
- 原有抖音 / 同花顺登录、诊断、同步、bridge 功能可用。
- command 全部加 `cookie_` 前缀。

验收：

- 能打开抖音登录窗口。
- 能检测本地 Cookie 状态。
- 能 ping G10 server。
- 能向 G10 同步 Cookie。
- 旧 `toolkit-desktop` 可暂时保留作为回退。

### 阶段 3：迁入 Speech 模块

产出：

- 语音识别页面在统一壳内可用。
- 能选择输入设备、启停录音。
- 能连接 ASR orchestrator。
- 能接收 segment 更新、复制结果、通知。

验收：

- 启动录音后状态正确变化。
- 停止录音后后台 capture 线程退出。
- 主窗口不会被 speech 简洁模式强制改尺寸。
- 托盘/通知不会破坏 Cookie 和 English 模块。

### 阶段 4：迁入 English 模块

产出：

- 英语标注播放、全量播放、环境设置可用。
- 音频缓存迁移到统一 workspace 命名空间。
- 原 `english desktop-app` 可作为回退。

验收：

- 能读取环境配置和 `customer_id`。
- 能拉取句子列表。
- 能缓存并播放音频。
- 切换模块时音频能停止或保持预期行为。

### 阶段 5：收敛体验

产出：

- 统一设置页。
- 统一状态栏。
- 统一错误展示和日志查看。
- 统一设计系统。
- 删除或归档旧独立桌面程序。

## 10. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| command 命名冲突 | 前端调用错模块，行为混乱 | 所有 command 强制模块前缀 |
| 后台任务互相影响 | 一个模块异常拖垮整个 app | 模块 state 隔离，后台任务记录清晰日志 |
| Cookie 泄露到其他模块 | 安全风险 | Cookie 模块独立目录，前端只展示摘要，诊断需用户主动点击 |
| Speech 窗口控制影响主壳 | 主窗口尺寸/置顶异常 | 简洁模式局部化或首版禁用 |
| 前端技术栈混杂 | 维护成本上升 | 允许短期混用，稳定后统一设计系统 |
| 打包体积变大 | 发布和启动变慢 | 不内置模型，ASR 继续远程 orchestrator |
| 旧项目路径依赖复杂 | 迁移容易漏文件 | 逐模块迁移，每阶段保留旧程序回退 |

## 11. 首版验收标准

首版不要求视觉完全统一，但必须满足：

- 一个桌面程序内能进入三类功能页面。
- 三类功能的核心路径可用。
- 日志不污染 stdout，prod 构建走文件日志。
- Cookie 登录态、语音历史、英语缓存分目录存储。
- `cargo clippy -- -D warnings`、`cargo fmt --check`、`cargo test` 全通过。
- 前端构建通过。
- Windows 安装包可生成。

## 12. 待决策问题

1. ~~统一壳 crate 名称~~：**已决** 新建 `crates/zero-desktop`，旧 `toolkit-desktop` 保留至阶段 5 归档。
2. ~~前端视觉体系~~：**已决** Tailwind/lucide 单选，english 的 AntD 组件分批替换（见 §5.3）。
3. 是否保留 `toolkit-desktop` 独立二进制作为 Cookie 专用回退？（暂保留，阶段 5 决定归档时机）
4. ~~Speech 的”简洁模式”~~：**已决** 首版禁用，移到 `legacy/` 下保留代码，阶段 5 评估悬浮小窗（见 §4.2.1）。
5. ~~Cookie bridge 端口~~：**已决** 固定 `28788`，不进设置项；如未来端口冲突再评估。
6. ~~english 远端 API base 与 cookie 模块 G10 base 的关系~~：**已决** 同一台 G10，合并为一项「G10 base」配置；状态栏一个指示灯；english 的 `apiBase` 从此处取，不再独立配置。

## 13. 推荐决策

- 新建 `crates/zero-desktop`，不要直接覆盖旧 `toolkit-desktop`。
- 首版前端允许混用，但 Shell 和新增页面使用 Tailwind/lucide。
- 迁移顺序：空壳 -> Cookie -> Speech -> English。
- 旧三个程序保留到统一壳通过完整验收后再归档。
- Cookie 和 Speech 后端优先按模块拆 state，再迁 UI；English 优先迁 UI。

## 14. 网络出口策略模块验证设计

### 14.0 背景与核心约束

用户当前在 Windows 下使用 WireGuard 连接海外 VPN。问题不是 WireGuard 隧道本身，而是默认海外出口导致国内服务访问不理想；同时用户要求未知请求必须继续走海外出口，只有明确配置的程序、域名或 IP 才允许走本地出口。

网络策略不再作为 `zero-desktop` 内嵌页面或后台模块；当前以独立的 `net-policy-gui`、`net-policy-agent`、`net-policy-cli` 组件交付。本节保留为历史验证与架构决策记录；**net-policy 已于 2026-07 拆为独立仓**
[jm-observer/net-policy](https://github.com/jm-observer/net-policy)，后续实现以该仓的 `docs/net-policy/` 为准。

不可妥协约束：

- **未知流量默认海外出口**：没有命中本地直连规则的 TCP/UDP/ICMP 流量必须走 WireGuard。
- **fail-closed**：策略服务异常、规则引擎退出、TUN 失效、WireGuard 断开时，不能静默回落到本地公网出口。
- **本地直连必须显式配置**：本地出口只允许用户手动配置的程序、进程组、域名、IP/CIDR 或内网保留地址。
- **Windows 优先**：首版只验证 Windows 10/11，不承诺 Linux/macOS 行为。
- **用户按程序管理**：用户不需要知道目标 IP；UI 需要允许选择 `.exe` 路径、进程名，并能展示该程序实际产生的连接摘要。

### 14.1 目标与非目标

目标：

- 在统一壳内提供“网络出口策略”页面：全局状态、WireGuard 状态、本地直连规则、泄漏检测结果。
- 支持按程序/进程路径配置本地直连，例如 `steam.exe`、`C:\Program Files\...\app.exe`。
- 支持进程组规则：用户选择主程序后，系统记录它常见子进程，并允许用户确认加入同一组。
- 支持域名和 IP/CIDR 直连白名单。
- 默认规则为 `MATCH -> WireGuard`，不是 `MATCH -> DIRECT`。
- 提供一键验证：当前公网 IP、DNS 出口、指定程序出口、WireGuard 断开后的泄漏测试。

非目标：

- 不重写 WireGuard 协议、握手或加密实现。
- 不在首版实现企业级 DLP、审计、防篡改。
- 不承诺绕过所有拥有管理员权限的本机程序；管理员程序可以修改路由、防火墙或卸载驱动。
- 不自动使用公共中国 IP 库做大规模国内分流；首版坚持“手动配置才直连”。

### 14.2 候选架构

#### 方案 A：官方 WireGuard + Windows 路由/防火墙管理

结构：

```
net-policy 独立应用
├─ WireGuard 官方客户端或 tunnel service
├─ Windows route table
├─ Windows Firewall 规则
└─ 可选 NRPT 域名解析策略
```

做法：

- WireGuard peer 配置默认海外出口。
- 对用户手动配置的 IP/CIDR 添加更具体的本地网关路由。
- 用 Windows 防火墙限制物理网卡出站，只放行 WireGuard 握手、本地直连白名单和局域网必要流量。
- 域名规则需要解析为 IP 后再下发路由；DNS 变化时刷新。

优点：

- 组件少，贴近 WireGuard 原生模型。
- 对 IP/CIDR 直连很可靠。
- 容易做 fail-closed 防火墙兜底。

缺点：

- WireGuard 原生不懂域名和进程。
- 进程级分流很难仅靠 route table 完成。
- 域名到 IP 的映射会受 CDN、DNS 污染、多 A/AAAA 记录影响。

验证结论预期：

- 适合作为底层 kill-switch 和 IP/CIDR 兜底。
- 不足以单独满足“按程序管理”的产品目标。

#### 方案 B：mihomo TUN + WireGuard 节点 + 规则管理

结构：

```
Windows traffic
└─ mihomo TUN
   ├─ DIRECT outbound     # 仅手动白名单命中
   └─ WireGuard outbound  # MATCH 默认出口
```

规则模型：

```
PROCESS-PATH,C:\Program Files\App\app.exe,DIRECT
PROCESS-NAME,steamwebhelper.exe,DIRECT
DOMAIN-SUFFIX,example.cn,DIRECT
IP-CIDR,203.0.113.0/24,DIRECT
GEOIP,private,DIRECT
MATCH,wg
```

优点：

- 天然支持域名、IP、进程路径、进程名等规则。
- UI 可以生成配置并热重载，管理体验较好。
- WireGuard 节点可作为规则引擎的默认 outbound，用户不需要维护复杂路由表。

缺点：

- TUN + WireGuard 组合必须验证是否存在握手流量路由环。
- fail-closed 不能只依赖 mihomo 自身；仍需 Windows 防火墙兜底。
- 子进程继承不是规则引擎天然语义，需要额外进程树观察和规则补全。

验证结论预期：

- 最可能满足首版产品目标。
- 需要把防泄漏能力外置到 Windows 防火墙/WFP 层。

#### 方案 C：自研 Windows WFP 服务 + WireGuard 官方客户端

结构：

```
zero-net-policy-service
├─ WFP callout/filter
├─ process tree observer
├─ WireGuard tunnel control
└─ net-policy-gui UI
```

优点：

- 可以最精确地做进程、子进程、目标地址、接口绑定和阻断。
- fail-closed 能力最强。
- 长期可控性最好。

缺点：

- 实现成本高，涉及 Windows Filtering Platform、服务权限、驱动/签名或复杂系统 API。
- 发布、升级、卸载、故障恢复成本明显高于桌面壳普通模块。
- 首版容易陷入系统网络栈细节，拖慢主项目。

验证结论预期：

- 不作为首版实现。
- 仅当方案 B 无法满足泄漏约束或进程策略稳定性时，再进入专项设计。

### 14.3 推荐验证路线

首选验证组合：

```
方案 B 负责策略表达和用户体验
方案 A 的 Windows 防火墙部分负责 fail-closed 兜底
方案 C 暂缓，仅记录必须下沉到 WFP 的证据
```

验证时不追求一次做完 UI，而是先证明四个命题：

1. `MATCH -> WireGuard` 时，未知流量公网 IP 为海外出口。
2. 手动配置程序或域名为 `DIRECT` 后，只有该规则命中的流量走本地出口。
3. 子进程可以被发现、展示，并通过自动补全规则纳入同一程序组。
4. mihomo/TUN/WireGuard 任一关键组件退出时，物理网卡不会泄漏未知流量。

### 14.4 进程与子进程规则模型

用户视角：

- 用户选择一个程序，例如 `D:\Apps\Foo\foo.exe`。
- 系统创建一个“程序组”：

  ```json
  {
    "id": "app-foo",
    "name": "Foo",
    "root_paths": ["D:\\Apps\\Foo\\foo.exe"],
    "known_children": [],
    "route": "direct"
  }
  ```

- 后台观察进程创建事件和连接事件，发现 `foo.exe -> helper.exe -> updater.exe` 后，在 UI 中提示“发现子进程”，用户确认后加入：

  ```json
  {
    "known_children": [
      { "kind": "process_path", "value": "D:\\Apps\\Foo\\helper.exe" },
      { "kind": "process_name", "value": "foo-updater.exe" }
    ]
  }
  ```

规则生成：

```
PROCESS-PATH,D:\Apps\Foo\foo.exe,DIRECT
PROCESS-PATH,D:\Apps\Foo\helper.exe,DIRECT
PROCESS-NAME,foo-updater.exe,DIRECT
```

边界：

- 子进程规则不是“继承父进程策略”的内核保证，而是通过观察后补全规则实现。
- 若程序动态释放随机路径二进制，需要 UI 明确提示风险，不能自动放大到整个目录。
- 对浏览器这类承载大量站点的程序，不建议整进程直连；应优先用域名规则，否则会让浏览器访问的未知站点也走本地。

### 14.5 DNS 策略

DNS 是防泄漏重点。验证阶段按以下原则：

- 未知域名解析默认走海外出口或规则引擎的远端解析能力。
- 手动直连域名可以使用本地 DNS，但解析结果只用于该规则。
- 禁止 Windows 多宿主 DNS 行为导致请求同时发往本地和海外 DNS。
- DNS 查询本身必须纳入泄漏测试：抓包或通过 DNS leak test 确认。

待验证问题：

- mihomo TUN 下 fake-ip / redir-host 哪种模式更适合 WireGuard 默认出口。
- 对本地直连域名，是否需要单独指定本地 DNS。
- WireGuard 官方客户端配置 `DNS = ...` 与 mihomo DNS 配置同时存在时是否冲突。

### 14.6 Windows 防泄漏策略

验证阶段需要设计一个最小 kill-switch：

允许物理网卡出站：

- WireGuard peer endpoint 的 UDP 握手目标。
- 用户手动配置为本地直连的目标 IP/CIDR。
- 局域网保留地址，可配置开关：`192.168.0.0/16`、`10.0.0.0/8`、`172.16.0.0/12`、`fd00::/8`。
- DHCP、NDP 等维持联网所需的基础流量。

阻断物理网卡出站：

- 未命中白名单的公网 TCP/UDP。
- 未命中白名单的 DNS。
- WireGuard/TUN/规则引擎异常时的所有未知公网流量。

验证要点：

- 关闭 mihomo 进程后，浏览器访问未知公网失败，而不是走本地公网。
- 关闭 WireGuard 后，未知公网失败；本地直连规则是否继续允许，需要产品上明确开关，默认建议失败。
- 修改默认路由后，防火墙仍能阻断未知物理出口。

### 14.7 独立应用形态（已取代 zero-desktop 集成）

独立 GUI 入口（不加入 zero-desktop Shell）：

```
/net-policy
```

独立应用侧栏包含：

- 网络策略

顶部状态栏新增：

- WG：已连接 / 未连接 / 异常
- 策略：运行中 / 降级 / 停止
- 泄漏防护：启用 / 未启用

页面布局：

- 概览：当前默认出口、公网 IP、DNS 出口、最近泄漏检测时间。
- 规则：程序组、域名、IP/CIDR、局域网访问开关。
- 进程发现：最近产生连接的进程、子进程建议、目标域名/IP 摘要。
- 验证：一键执行未知流量、直连规则、DNS、断线泄漏测试。
- 日志：规则生成、引擎状态、防火墙应用结果。

后端模块：

```
crates/net-policy-agent/ + crates/net-policy-gui/
├─ mod.rs
├─ state.rs
├─ config.rs
├─ engine.rs          # mihomo / wg / firewall 进程与配置编排
├─ firewall.rs        # Windows 防火墙或 WFP 兜底，验证期可先调用 netsh / PowerShell
├─ process_watch.rs   # 进程树和连接观察
└─ verify.rs          # 出口与泄漏测试
```

command 命名：

| command | 作用 |
|---|---|
| `net_policy_get_status` | 获取 WG、规则引擎、防泄漏状态 |
| `net_policy_list_rules` | 列出程序组、域名、IP/CIDR 规则 |
| `net_policy_save_rule` | 新增或更新规则 |
| `net_policy_delete_rule` | 删除规则 |
| `net_policy_list_process_candidates` | 列出最近连接进程和子进程建议 |
| `net_policy_apply` | 生成并应用规则 |
| `net_policy_verify` | 执行一组验证用例 |
| `net_policy_emergency_stop` | 停止策略并保持 fail-closed |

### 14.8 运行时目录

新增：

```
独立应用工作区：`%ProgramData%/net-policy`（服务）或用户配置目录（GUI/CLI）
├─ settings.json
├─ rules.json
├─ generated/
│  ├─ mihomo.yaml
│  └─ firewall.ps1
├─ logs/
└─ verify/
   └─ last-report.json
```

敏感信息：

- WireGuard private key 不应明文存入普通 `rules.json`。
- 验证阶段可引用用户已有 WireGuard 配置文件路径。
- 稳定版若需要托管密钥，优先使用 Windows Credential Manager 或 Tauri stronghold 类能力，另起设计。

### 14.9 验证用例

| 编号 | 用例 | 操作 | 通过标准 |
|---|---|---|---|
| VP-01 | 未知流量默认海外 | 不配置任何直连规则，访问公网 IP 检测服务 | 返回海外 WireGuard 出口 IP |
| VP-02 | 手动 IP 直连 | 添加一个测试 IP/CIDR 为 DIRECT | 只有该目标走本地出口，其他公网仍走 WG |
| VP-03 | 手动域名直连 | 添加 `example.cn` 类测试域名为 DIRECT | 该域名连接走本地，未配置域名走 WG |
| VP-04 | 程序直连 | 将一个测试 exe 配为 DIRECT | 该 exe 的连接走本地，其他程序走 WG |
| VP-05 | 子进程发现 | 测试 exe 启动 helper 子进程发起连接 | UI 能展示 helper，确认后 helper 走本地 |
| VP-06 | 浏览器风险提示 | 将浏览器加入 DIRECT | UI 明确提示整浏览器直连会影响其所有站点 |
| VP-07 | DNS 防泄漏 | 执行 DNS leak test 或抓包 | 未知域名 DNS 不发往本地 DNS |
| VP-08 | 规则引擎崩溃 | 强制结束 mihomo 进程 | 未知公网访问失败，不走本地 |
| VP-09 | WireGuard 断开 | 断开 WG | 未知公网访问失败，不走本地 |
| VP-10 | 路由被改动 | 手动调整默认路由到本地网关 | 防火墙仍阻断未知公网 |
| VP-11 | 重启恢复 | 重启 Windows 或 zero-desktop | 策略状态可恢复，恢复完成前不泄漏 |
| VP-12 | IPv6 | 启用本地 IPv6 网络 | 未配置 IPv6 不得绕过 WG 或本地泄漏 |

验证报告格式：

```json
{
  "started_at": "2026-06-12T00:00:00Z",
  "windows_version": "...",
  "wireguard": {
    "mode": "official-client",
    "endpoint": "redacted"
  },
  "engine": {
    "kind": "mihomo",
    "version": "..."
  },
  "cases": [
    {
      "id": "VP-01",
      "status": "passed",
      "observed_exit": "wg",
      "evidence": ["public_ip=...", "interface=..."]
    }
  ],
  "blocking_issues": []
}
```

### 14.10 验证任务拆分

给其他会话的建议任务：

1. 验证 WireGuard 官方客户端在 Windows 下 `/0`、`/1 + /1`、DNS、kill-switch 行为差异。
2. 验证 mihomo TUN + WireGuard 节点能否稳定实现 `MATCH -> wg`，并排除 WireGuard 握手路由环。
3. 验证 mihomo 的 `PROCESS-NAME` / `PROCESS-PATH` 在 Windows 下对普通程序、子进程、浏览器、多进程应用的匹配效果。
4. 验证 Windows 防火墙最小规则集能否在 mihomo 或 WireGuard 异常退出时 fail-closed。
5. 验证 DNS 行为：未知域名、本地直连域名、DoH/DoT 应用、浏览器内置 DNS 的处理差异。
6. 验证 IPv6 泄漏风险，并决定首版是完整支持 IPv6 还是默认阻断 IPv6。
7. 输出一份 `docs/net-policy/net-policy-validation-report.md`，给出是否采用“mihomo TUN + WG + Windows 防火墙兜底”的最终判断。

### 14.11 阶段决策门槛

只有当以下条件全部满足，才进入 `zero-desktop` 正式实现：

- VP-01、VP-07、VP-08、VP-09、VP-10、VP-12 通过。
- 程序规则至少能稳定覆盖普通 exe 和已确认子进程。
- 防火墙兜底不依赖 UI 常驻；即使 `zero-desktop` 退出，未知公网也不泄漏。
- 规则应用失败时 UI 能明确展示失败原因，并保持 fail-closed。
- 用户可以一键恢复网络，但恢复动作必须明确提示会关闭泄漏防护。

推荐阶段结论：

- 若方案 B + 防火墙兜底通过决策门槛，首版按此实现。
- 若方案 B 的进程规则可用但 fail-closed 不稳定，先实现 IP/域名/程序管理 UI，但把“一键启用”置为实验功能。
- 若方案 B 无法证明未知流量不泄漏，暂停主壳集成，单独设计 WFP 服务。
