# Codeloop 无头 Smoke Runner 方案

> 目标:给 Zero Desktop 的 codeloop 增加一条命令行 smoke 通路,让 E2E 测试不依赖点 Tauri UI。
>
> 主验收目标:`docs/codeloop-mini-feature-design.md`
>
> GUI runbook(人工,补充用):`docs/codeloop-mini-e2e-plan.md`
> 无头 runbook(本特性的用户文档):`docs/runbook-codeloop-smoke.md` *(步骤 11 创建)*

## 背景

codeloop 当前的运行时挂在 Tauri 命令 + UI 事件上。这让产品在 Zero Desktop 窗口里好用,但也
让自动化 E2E 没法从 shell 起:

- Tauri 命令只能在 WebView 里调。
- 应用没暴露 HTTP / DevTools 自动化端口。
- Windows 无障碍只能看到 WebView 容器,看不到页面控件。
- 跑中的 loop 通过 `AppHandle.emit(...)` 和 Tauri state(`AppState` 上的 `pending_confirm` /
  `pending_answer`)拿进度和确认。

正确的修法不是去自动化那个窗口,而是另开一条无头 CLI 路径,跑同一份 codeloop 引擎、同一份
数据库、同一套 Codex / Claude Code driver、同一套 worktree 重定位与 transcript 恢复逻辑。

## 目标

加一个 `zero-desktop codeloop-smoke` 子命令,跑通这条链路:

```text
设计评审 -> 代码实现 -> 代码评审 -> 验证 -> 总结
```

它在 `<workspace>/codeloop/state.db` 里写正常的记录,后续 Zero Desktop UI 能照样展示。

## ⚠️ 会话劫持风险与会话选择规则(v1 关键约束)

`agent-session` 里的 "session" 指的是 transcript 文件
(`~/.claude/projects/<编码 cwd>/<id>.jsonl` 或 codex 的对应文件),**不是运行中的进程**。
"最近活跃" = "transcript 文件最近一次被写入"。

如果默认按"最近活跃 + cwd 匹配 `--repo`"挑会话,会出现这种局面:**用户正在桌面端 / 终端
里用 Claude Code 跟开发者本人对话,smoke runner 直接拿到这个 session id 起一个新的
`claude --resume <id>` 子进程**。后果:

- smoke 自己的 prompts 被追加进用户正在用的对话历史(jsonl 一直在追加写);
- 两个 `claude` 进程并发 resume 同一 session,transcript 写入互相打架,UI 端"恢复 / 回放"
  会读到混乱状态;
- codeloop 的 step-confirm / ASK_USER 跟桌面 UI 抢同一会话的输入流。

虽然不是 OS 层"杀进程",语义上等同于**劫持掉用户当前的活会话**。在 toolkit 这种「自己仓
里开发自己」的场景里基本必然撞车。所以 v1 强制以下规则:

1. **`--claude-session` / `--codex-session` 没传时,默认拒绝**,preflight 直接报错并提示
   "传具体 id 或 `auto`"。`--new-codex-agent` 仍可免传 `--codex-session`(本来就要新建)。
2. 即使显式传了 session id 或 `auto`,preflight 还要再检查一次:
   该 session 的 `SessionSummary.updated_at`(transcript 最新事件时间戳)是否在最近
   **5 分钟**内 —— 是 → 视为"有人正在用",必须额外加 `--allow-hijack-current-session`
   才放行,否则退出 3 并打印 elapsed 秒数 / 上次时间戳。
3. `--claude-session` / `--codex-session` 接受特殊值 `auto`:把"按 cwd + `updated_at`
   倒序自动挑"显式打开,但仍受第 2 条的"近 5 分钟"保护。CI 里通常配合 `--new-codex-agent`
   + 一个干净 cwd 用,不在开发机跑。

会话清单来源:`agent_session::store::Store::list()`,按 cwd 匹配 `--repo` 过滤,按
`updated_at` 倒序。如果显式 id 在清单里不存在,退出 3。`auto` 选不到候选(cwd 下无会话)
同样退出 3。

**v1 实现注意**:劫持保护**只查 `updated_at`**;尚未检测 `CLAUDE_PROJECT_DIR` 环境变量或
正在运行的 `claude` 子进程的 cwd。这意味着如果用户的活会话刚好静默超 5 分钟,但桌面端
窗口还开着,本检测漏过 —— 实践中影响不大(开发会话很少静默 5 分钟),后续可扩。

v1 非目标:交互式会话选择器;新建 Claude session
(`agent-session` 当前只暴露了 `create_codex_session`,见下方 Open Question 2)。

## 命令形态

两种典型调用(不留"是否默认开 `--auto-confirm`"这种模糊地带):

CI / 无人值守:

```powershell
cargo run -p zero-desktop -- codeloop-smoke `
  --repo D:\git\toolkit `
  --target docs/codeloop-mini-feature-design.md `
  --claude-session <id> `
  --max-rounds 2 `
  --new-codex-agent `
  --auto-confirm `
  --verify `
  --json
```

本地可观测(每个阶段之间人工 gate):

```powershell
cargo run -p zero-desktop -- codeloop-smoke `
  --repo D:\git\toolkit `
  --target docs/codeloop-mini-feature-design.md `
  --claude-session <id> `
  --max-rounds 2 `
  --new-codex-agent `
  --verify
```

参数:

| 参数 | 必填 | 默认 | 含义 |
|---|---:|---|---|
| `--repo <path>` | 是 | 无 | 仓库根。runner 用 `git rev-parse --show-toplevel` 校验是 git 顶层。 |
| `--target <path>` | 是 | 无 | 设计文档或仓内相对路径。在创建任何 DB 记录前必须存在。 |
| `--workspace <path>` | 否 | `%LOCALAPPDATA%\zero-desktop`(或 `ZERO_DESKTOP_WORKSPACE`) | DB / 日志的 workspace。 |
| `--claude-session <id>` | **是**(除非传 `auto`) | 无 | 要驱动的 Claude Code 会话 id。传 `auto` 等价于「按最近活跃 + cwd 匹配自动挑」,但仍受劫持保护。 |
| `--codex-session <id>` | 否(传 `--new-codex-agent` 即可免) | 无 | 要驱动的 Codex 会话 id。传 `auto` 同上语义。 |
| `--new-codex-agent` | 否 | false | 进入需要 Codex 的阶段前新建一个 Codex exec 会话。 |
| `--allow-hijack-current-session` | 否 | false | 解除"目标会话 transcript 近 5 分钟内仍有写入"的拦截。**不要在日常开发机上加这个**。 |
| `--max-rounds <n>` | 否 | 2 | 每个 codeloop 阶段的最大轮数。 |
| (worktree 强制开) | — | — | v1 不提供 `--no-worktree`:review 阶段必须从 implementation 的 worktree 续跑,关 worktree 后两段会共用主仓导致语义混淆。后续若放开,需要 review 路径支持「不带 worktree 续跑」(目前未实现)。 |
| `--auto-confirm` | 否 | false | 关闭逐步确认 gate。无人值守必开。不开时 runner 会在每个 gate 阻塞并打印本该展示的内容,只对接到人交互终端时有意义。 |
| `--ask-user-answers <path>` | 否 | 无 | JSON map `{ "问题子串": "答案", ... }`。模型出 `ASK_USER` 时按首个匹配子串挑答案。**没传这个,又不在交互终端里,`ASK_USER` 直接退出 2**。不允许静默自动回答。 |
| `--recover-after-stop` | 否 | false | 可选的 chaos 步,见下方"恢复 smoke 变体"。 |
| `--verify` | 否 | false | 实现 worktree 里跑后置验证命令。 |
| `--json` | 否 | false | stdout 输出 JSONL 进度 + 终态 summary 对象(此模式下纯文本走 stderr)。**实现细节**:此模式下 runner 完全跳过 `custom_utils::logger` 注册,driver / codeloop 的 `log::info!` 全部被丢弃,以保证 stdout 不被 logger 串字符污染。 |
| `--timeout <duration>` | 否 | `15m` | 全局墙钟预算。到期则 runner 立即:① 把活跃 loop 在 DB 里 `finalize` 成 `status=aborted, final_verdict=aborted_timeout, error=global_timeout`(幂等 update,只动 `status='running'` 行);② 发 `smoke_done` JSON 事件并退出 1。接受 `30s` / `5m` / `1h`。**实现细节**:DB 在 `tokio::time::timeout` 包裹之外打开,run_inner 跑在内部,超时分支据此能拿到 DB 句柄做兜底 finalize;loop_id 由 `CurrentLoopTracker`(LoopEvents 装饰)从 `progress` 事件捕获。 |

## 输出契约

纯文本(默认,走 stdout):

```text
[preflight] repo=D:\git\toolkit target=docs/codeloop-mini-feature-design.md
[sessions] claude=<id> codex=<id>
[design] loop_id=6 status=done verdict=pass rounds=1
[implementation] loop_id=7 status=done worktree=D:\git\toolkit-codeloop-smoke-...
[review] loop_id=8 status=done verdict=pass rounds=1
[verify] cargo test -p codeloop-core: pass
[verify] cargo check -p zero-desktop: pass
SMOKE PASS design=6 implementation=7 review=8 worktree=D:\git\...
```

纯文本,验证失败:

```text
[verify] cargo check -p zero-desktop: fail (tail: %TEMP%\codeloop-smoke-1234\verify-check.log)
SMOKE FAIL stage=verify reason=cargo_check_failed exit=4
```

`--json`(**stdout 一行一个 JSON 事件**;纯文本 summary、logger、错误摘要走 stderr):

```json
{"event":"preflight_done","repo":"D:\\git\\toolkit","target":"docs/codeloop-mini-feature-design.md"}
{"event":"sessions_resolved","claude":"<id>","codex":"<id>"}
{"event":"stage_starting","stage":"design"}
{"event":"progress","loop_id":6,"phase":"starting","mode":"design"}
{"event":"progress","loop_id":6,"phase":"reviewed","round":1,"verdict":"pass"}
{"event":"progress","loop_id":6,"phase":"done","final_verdict":"pass","total_rounds":1}
{"event":"stage_done","stage":"design","loop_id":6,"status":"done","verdict":"pass","rounds":1}
{"event":"stage_starting","stage":"implementation"}
{"event":"progress","loop_id":7,"phase":"starting","mode":"implementation"}
{"event":"progress","loop_id":7,"phase":"implementing","round":0}
{"event":"stage_done","stage":"implementation","loop_id":7,"status":"done","worktree_path":"D:\\git\\..."}
{"event":"stage_failed","stage":"review","loop_id":8,"reason":"max_rounds","verdict":"max_rounds"}
{"event":"error","scope":"preflight","message":"target does not exist: docs/foo.md"}
{"event":"verify_done","cmd":"cargo test -p codeloop-core","status":"pass"}
{"event":"verify_done","cmd":"cargo check -p zero-desktop","status":"fail","log_tail_path":"%TEMP%\\..."}
{"event":"smoke_done","status":"pass","exit_code":0,"design_loop_id":6,"implementation_loop_id":7,"review_loop_id":8}
```

事件契约:

- `stage_starting`(无 `loop_id`):阶段调用即将开始。loop_id 此时尚未生成,首条 `progress`
  事件随即带出。**不要**用 `stage_started`(早期方案中曾用此名但没有 loop_id,会误导消费方
  以为 loop 已建)。
- `progress`:run_codeloop 内每次推进 `phase` / `round` / `verdict` 发,**总是带 `loop_id`**。
  **DB 行一就绪 run_codeloop 立刻发一次 `phase=starting`**,关掉了「DB 行已建、首条业务
  progress 未发」之间的窗口 —— headless smoke 的超时分支据此能在该窗口内也兜底 finalize。
- `stage_done` / `stage_failed`:终态,带 `loop_id` + 完整 verdict / status。
- `smoke_done`:全局收尾;超时分支(`reason: "global_timeout"`)也由此事件报告,并带
  `aborted_loop_id`(被强制收尾为 `aborted_timeout` 的 loop id,无活跃 loop 时为 null)。
- `error`:preflight / 配置 / runner 自检失败,**不**用于 LLM 判定。

所有 JSON 事件均在 stdout;ConsoleLoopEvents、Output 的 emit_json、超时分支都遵守此约定。
`--json` 模式下 logger 不注册以保证 stdout 纯 JSONL。

## Tracing

runner 启动时设一个根 `SpanScope`(`codeloop_smoke_run` span,带 `repo` / `target` /
`mode_chain`)。每个阶段调用通过 `traceparent` 继承下来。`TRACE_HUB_ENDPOINT` 没设就是
no-op(和仓里其它地方一致)。CI 跑完后是这个东西用来排查问题。

## 架构

### 1. 把运行时从 Tauri 里拆出来

当前代码集中在 `crates/zero-desktop/src/modules/codeloop/mod.rs`。`LoopCtx` 直接埋了
`AppHandle`(`mod.rs:170`)和 Tauri 形态的 `pending_confirm` / `pending` `oneshot` map。
引擎本身(`drive(...)` / `confirm_gate(...)` / `send_and_resolve(...)`)其它部分对
`agent_session::driver` 是纯逻辑。

引入两个 trait:

```rust
// crates/zero-desktop/src/modules/codeloop/runtime.rs
pub trait LoopEvents: Send + Sync + 'static {
    fn progress(&self, loop_id: i64, value: serde_json::Value);
}

#[async_trait::async_trait]
pub trait ConfirmPolicy: Send + Sync + 'static {
    /// 跨 agent 发送前的逐步确认 gate。seq / title / content 仅作诊断。
    async fn confirm(&self, req: ConfirmRequest) -> Gate;
    /// ASK_USER:返回 Some(答案)喂回同一会话,返回 None 中止 loop
    /// (按 AbortedTimeout 等价处理)。
    async fn answer_user(&self, req: AskUserRequest) -> Option<String>;
}
```

实现:

| Trait | UI 实现 | 无头实现 |
|---|---|---|
| `LoopEvents` | `TauriLoopEvents { app: AppHandle }` → `AppHandle.emit(EV_PROGRESS, ...)`(当前行为) | `ConsoleLoopEvents { json: bool, sink: Box<dyn Write + Send> }` → 文本或 JSONL 一行一个进度 |
| `ConfirmPolicy` | `UiConfirmPolicy { app_state: Arc<AppState> }` → 保留当前 `pending_confirm` / `pending` `oneshot` map,现有 `codeloop_confirm` / `codeloop_answer` Tauri 命令照常驱动 | `AutoConfirmPolicy { ask_answers: Option<HashMap<String,String>> }` → confirm gate 自动放行;`answer_user` 按子串查表,查不到返回 `None` → loop 中止 |

`UiConfirmPolicy` 持有 AppState 是为了读写全局 pending map。**不要**把这个 state 搬进 trait
本身;UI 命令处理器期望它在原位。

### 2. 把 LoopCtx 改成依赖 trait

`LoopCtx` 现在直接埋了 `AppHandle` 和两个 `Arc<Mutex<Option<...>>>`。重构后:

```rust
struct LoopCtx {
    events: Arc<dyn LoopEvents>,
    confirm: Arc<dyn ConfirmPolicy>,
    store: agent_session::store::Store,
    db: Arc<db::Db>,
    loop_id: i64,
    claude: SessionRef,
    codex: SessionRef,
    target: TargetSpec,
    mode: ReviewMode,
    max_rounds: u32,
    wait_for_claude_idle: bool,
    step_confirm: Arc<AtomicBool>,
    use_worktree: bool,
    resume_from_worktree: bool,
    established_codex: bool,
    established_claude: bool,
    progress: Arc<Mutex<serde_json::Value>>,
    seq: Arc<AtomicI64>,
}
```

`LoopCtx::report(...)` 调 `self.events.progress(self.loop_id, v)`。
`confirm_gate(...)` / `send_and_resolve(...)` 调 `self.confirm.confirm(...)` /
`self.confirm.answer_user(...)`,不再自己造 oneshot。

`UiConfirmPolicy` 内部继续造 oneshot、写 `app_state.pending_confirm`,现有
`codeloop_confirm` / `codeloop_answer` Tauri 命令一行不动。**DB 记录格式和事件 payload 不变。**

### 3. 共享引擎入口 + 显式依赖

```rust
pub struct RunLoopDeps {
    pub store: agent_session::store::Store,
    pub db: Arc<db::Db>,
    pub events: Arc<dyn LoopEvents>,
    pub confirm: Arc<dyn ConfirmPolicy>,
}

pub struct RunLoopInput {
    pub claude: SessionRef,
    pub codex: SessionRef,
    pub target: TargetSpec,
    pub mode: ReviewMode,
    pub max_rounds: u32,
    pub wait_for_claude_idle: bool,
    pub step_confirm: bool,
    pub use_worktree: bool,
    pub parent_loop_id: Option<i64>,
    pub resume_worktree_path: Option<String>,
    pub established: Established,
}

pub struct RunLoopResult {
    pub loop_id: i64,
    pub status: String,            // "done" | "aborted"
    pub final_verdict: Option<String>,
    pub total_rounds: i64,
    pub worktree_path: Option<String>,
}

pub async fn run_codeloop(deps: RunLoopDeps, input: RunLoopInput) -> Result<RunLoopResult>;
```

`run_codeloop` 创建 DB 记录、构造 `LoopCtx`、调 `drive(...)`、读回最终记录再返回。
`codeloop_start`(Tauri)和 `codeloop-smoke`(CLI)都走这里。引擎只有一份实现。

### 4. 无头 smoke 编排

在 `crates/zero-desktop/src/main.rs` 加一个 CLI 子命令:

```rust
Command::CodeloopSmoke {
    repo: String,
    target: String,
    claude_session: String,           // 必填或 "auto"
    codex_session: Option<String>,
    max_rounds: u32,
    auto_confirm: bool,
    new_codex_agent: bool,
    allow_hijack_current_session: bool,
    ask_user_answers: Option<PathBuf>,
    recover_after_stop: bool,
    verify: bool,
    json: bool,
    timeout: humantime::Duration,
    workspace: Option<PathBuf>,
}
```

编排流程:

0. **入口 fail-fast**(在打开 DB、解析 workspace 之前;**避免不支持的参数留下副作用**):
   - `--recover-after-stop` v1 未实现 → 退出 3。
   - `--auto-confirm` 没传 → 退出 3。
1. Preflight(`run()` 在 timeout 包裹**外**打开 workspace + DB,以便超时分支兜底):
   - 解析 workspace,打开 codeloop DB(Arc<Db> 同时传给 preflight 和超时兜底分支),
     打开 `agent_session::store::Store`。
   - 校验 `--repo` 是 git 顶层。
   - 校验 `--target` 存在。
   - **解析会话**:
     - 没传 `--claude-session` → 直接退出 3(避免误选活会话)。
     - `--claude-session` = 具体 id → 在 `Store::list()` 里 lookup;找不到退出 3。
     - `--claude-session` = `auto` → 按 cwd 过滤 + `updated_at` 倒序挑首个非 Unknown 状态
       的候选;选不到退出 3。
     - `--codex-session` 同上;若用 `--new-codex-agent` 则免传,在 `claude.cwd` 下用
       `driver::create_codex_session` 新建。
   - **劫持保护**(v1 实现,除非 `--allow-hijack-current-session`):
     - 选中的 Claude session 的 `SessionSummary.updated_at` 在最近 5 分钟内 → 拒绝,退出 3,
       打印 elapsed 秒数 / 上次时间戳。
     - `updated_at` 解析失败 → **fail-closed**(同样拒绝),因为无法判断安全状态时默认应该
       保护用户;要继续就显式加 `--allow-hijack-current-session`。
     - 用 `--new-codex-agent` 时 Codex 新建,跳过该检查;否则同样规则用在选中的 Codex
       session 上。
     - **未实现的检测**:`CLAUDE_PROJECT_DIR` 环境变量;正在跑的 `claude` 子进程 cwd
       匹配。开发会话很少静默 5 分钟,实践影响小。
   - 驱动健康探针目前不在 preflight 内显式跑(下游 `run_codeloop` 第一次 spawn 自然会暴露)。
   - `tokio::time::timeout(--timeout, run_inner(...))` 跑三段编排。超时分支拿外层
     Arc<Db> + `CurrentLoopTracker` 暴露的 loop_id,把活跃 loop `finalize` 为
     `aborted_timeout`,发 `smoke_done` 退出 1。
2. 阶段 1 — 设计评审:
   - `mode=design`,`use_worktree=false`,`parent=None`。
   - pass:继续。非 pass 终态:报告并退出(见退出码)。
3. 阶段 2 — 实现:
   - `mode=implementation`,`parent=design_id`,`use_worktree=true`(v1 固定)。
   - 成功后断言:`use_worktree=true` 时 `worktree_path` 必须有值。
4. 阶段 3 — 代码评审:
   - `mode=implementation`,`parent=impl_id`,
     `resume_worktree_path=impl.worktree_path`。
   - 断言这条记录在 round 0 没有 `claude_implement`。
5. 可选验证(`--verify`)在 worktree 里跑:
   - `cargo test -p codeloop-core`
   - `cargo check -p zero-desktop`
   - stdout / stderr 落到 `<temp>/codeloop-smoke-<pid>/verify-*.log`;失败时只打路径,要看
     才显示最后 40 行。
6. 打 summary;设退出码。

### 5. 恢复 smoke 变体(范围有限)

`--recover-after-stop` **只覆盖进程内恢复路径**(就是记录被重新打开时
`codeloop_loop_messages` 从 Claude transcript 做的 `loop_messages` 回填)。**不覆盖**
跨进程冷启动恢复 —— 那条要靠 GUI runbook 里 kill Zero Desktop 来测。

行为:

1. 启动实现阶段。
2. 一旦 round-0 的"已进入实现阶段..."系统消息被记录,在 DB 里用 UI stop 命令同款的收尾
   逻辑把 loop 收成 `stopped`。
3. 等 Claude transcript 跑完并出现 `WORKTREE:`。
4. 调 `codeloop_loop_messages` 用的恢复 helper。
5. 断言:
   - 实现记录有一条恢复出来的 `claude_implement` 消息;
   - `worktree_path` 有值;
   - 该记录可以作为代码评审阶段的起点。

明确的限制:这是单元级恢复覆盖。跨进程恢复仍然要走 GUI runbook。

## 数据库断言

一次正常 smoke 跑完后 DB 里应有:

```text
design_id:
  parent_loop_id NULL
  mode design
  status done
  final_verdict pass
  含 codex_review

implementation_id:
  parent_loop_id = design_id
  mode implementation
  status done
  含 system round 0 "正在向 Claude 发送实现命令"
  含 claude_implement
  worktree_path 有值

review_id:
  parent_loop_id = implementation_id
  mode implementation
  status done
  final_verdict pass
  worktree_path 直接从父记录继承
  含 system round 0 关于从 worktree 恢复的内容
  round 0 不含 claude_implement
```

`mode` 字段提醒:codeloop 只有两种 `ReviewMode`(`design` / `implementation`,见
`codeloop-core/src/prompt.rs::ReviewMode`)。代码评审记录的 `mode` 也是 `implementation`;
靠 `resume_worktree_path IS NOT NULL` 或 `parent_loop_id` 指向另一条实现记录来区分。

runner 在每个阶段结束后查 DB 做对应断言。断言失败属于 runner 自己的 bug,不是产品判定 ——
对应退出码 2。

## 验证命令

mini 目标下,`--verify` 在实现 worktree 里跑:

```powershell
cargo test -p codeloop-core
cargo check -p zero-desktop
```

stdout / stderr 落到日志文件(`<temp>/codeloop-smoke-<pid>/verify-*.log`)。失败时 runner
在 stderr 打路径和最后 40 行;JSONL 的 `verify_done` 事件带 `log_tail_path`。不要把巨大日
志塞进最终 summary。

## 退出码

| 码 | 含义 | 例子 |
|---:|---|---|
| 0 | smoke 全程通过。 | 各阶段 `done/pass`,verify ok。 |
| 1 | 产品流程返回非 pass 判定 —— 流程跑通了,模型判了不行。 | Codex 给 `NEEDS_WORK` 且达到 max rounds;loop 收尾 `aborted_timeout`;ASK_USER 因没给答案被中止。CI 可以重试一次。 |
| 2 | runner 自检不变量违反或 bug。 | `use_worktree=true` 但实现记录没 `worktree_path`;DB 不变量失败;解析错误超阈值;评审记录 round 0 出现 `claude_implement`。**不要**重试,排查。 |
| 3 | preflight / 配置错误。 | 缺会话、`--repo` 不是 git 顶层、CLI 不存在、`--target` 文件不存在、**触发会话劫持保护**、未传 `--claude-session` 又没传 `auto`。 |
| 4 | `--verify` 的 cargo 命令失败。 | 产品流程本身通过了,但产出的代码编译 / 测试不过。 |

CI 经验值:0 = 绿,1 = 黄(模型判定,可以重试一次),2 / 3 / 4 = 红(别盲目重试)。

## 安全规则

- `--repo` 必须是 git 顶层(`git rev-parse --show-toplevel` 成功且匹配)。
- 用 `agent-session` 的 driver 默认配置(`approval_policy="never"`,workspace-write)。**不要**
  开 `danger-full-access`。
- **不要**清理主仓库里不相关的脏文件。
- **不要**自动删 worktree。后续可能加 `--cleanup`;当前 smoke 跑完把 worktree 留在盘上备查。
- `--target` 不存在 → 在创建任何 codeloop 记录前失败。
- **不要**在没有 `--allow-hijack-current-session` 的前提下绕过劫持保护。要在开发机上跑就
  自己起一个干净 cwd(单独 clone 一份)。

## 实现步骤

1. 加运行时 trait(`runtime.rs`):`LoopEvents` / `ConfirmPolicy` / `ConfirmRequest` /
   `AskUserRequest` / `Gate`。
2. 实现 `TauriLoopEvents` / `UiConfirmPolicy` 适配器,包住现有 `AppHandle.emit(...)` 和
   `AppState.pending_confirm` / `pending` —— UI 行为零变化。
3. 重构 `LoopCtx` 和 helpers(`confirm_gate` / `send_and_resolve` / `drive`),通过
   `Arc<dyn ...>` 依赖 trait,不再直接依赖 `AppHandle`。
4. 加 `RunLoopDeps` / `RunLoopInput` / `RunLoopResult` 和共享入口 `run_codeloop(...)`。
5. 重构 `codeloop_start(...)`(Tauri 命令)走 `run_codeloop(...)` + Tauri 适配器。
6. 实现 `ConsoleLoopEvents`(文本和 JSONL)和 `AutoConfirmPolicy`(可选答案表)。
7. 加 `Command::CodeloopSmoke { ... }` 变体和 `main.rs` 里的 CLI 解析。复用 `main()` 里
   不进 `run_gui(...)` 跑非 Tauri 子命令的分支(`NetPolicyGen` / `Update` 已有的模式)。
8. 实现 preflight:repo / target 校验、workspace 打开、根 span、必填会话 + `auto` 哨兵
   解析、**劫持保护**(`SessionSummary.updated_at` 5 分钟窗口 + `--allow-hijack-current-session`
   旁路)。`main.rs` 里 logger 注册推到 CLI parse 之后,且 `--json` 模式跳过注册以保证
   stdout 纯净。
9. 实现三阶段编排,带显式 DB 断言和退出码映射。
10. 实现 `--verify` 的 cargo runner + 日志文件捕获。
11. 加 `docs/runbook-codeloop-smoke.md`,放"命令形态"里的两种典型调用 + 一段排障。从
    `docs/codeloop-mini-e2e-plan.md` 反向链过来。
12. *(延后)* 实现 `--recover-after-stop`。先让 flag 能解析但打印"v1 未实现"并退出 3
    (**fail-fast 在 preflight / Codex 新建之前**,避免不支持的参数留下副作用);helper
    落地后再放开。

## 测试计划

静态 / 构建检查:

```powershell
cargo check --workspace --exclude toolkit-desktop
cargo check -p zero-desktop
# UI bundle 没动就不用跑:
npm --prefix crates/zero-desktop/ui run build
```

单测(最少):

- `runtime::ConsoleLoopEvents` JSONL 行格式(一个测)。
- `AutoConfirmPolicy::answer_user` 子串查表(命中 / 未命中各一个测)。
- 会话候选挑选:`auto` 模式按 `updated_at` 倒序 + cwd 过滤(用小 fake `Store` 或 fixture)。
- **劫持保护**:`updated_at` 在最近 5 分钟内 → 默认拒绝、带 `--allow-hijack-current-session`
  → 放行(各一个测)。
- **logger 抑制**:`--json` 模式下 stdout 必须是 100% 合法 JSONL,无任何 logger 串字符
  (跑一遍 preflight 失败用例,把 stdout 喂 `JSON.parse` 逐行验证)。

Smoke 跑(人工,对着 mini 目标):

```powershell
# 无人值守(CI 形态)
cargo run -p zero-desktop -- codeloop-smoke `
  --repo D:\git\toolkit `
  --target docs/codeloop-mini-feature-design.md `
  --claude-session <id> `
  --max-rounds 2 `
  --new-codex-agent `
  --auto-confirm `
  --verify `
  --json
```

```powershell
# 恢复变体(步骤 12 落地后)
cargo run -p zero-desktop -- codeloop-smoke `
  --repo D:\git\toolkit `
  --target docs/codeloop-mini-feature-design.md `
  --claude-session <id> `
  --max-rounds 2 `
  --new-codex-agent `
  --auto-confirm `
  --recover-after-stop
```

## 验收标准

特性完成的判据:

- `codeloop-smoke` 不起 Tauri 窗口就能跑。
- 它写正常的 codeloop DB 记录,后续 UI 能正常展示。
- 一次正常 mini 跑产生 design / implementation / review 三条记录。
- 实现记录拿到 `worktree_path`(worktree 模式开着时)。
- 评审记录从 `resume_worktree_path` 起,不会再跑一次 implementation(round 0 没有
  `claude_implement`)。
- 终态 summary 打出 loop id 和 worktree 路径。
- 退出码遵循 0 / 1 / 2 / 3 / 4 划分。
- **默认不会劫持当前活会话**:不传 `--claude-session` → 退出 3;命中近 5 分钟活跃 →
  退出 3。
- 现有 UI codeloop 仍然能构建、行为一致(Tauri 命令不变)。

## Open Questions

- **runner 也应该新建一个 Claude session 吗?** v1 复用已有 Claude session,因为
  `agent-session` 当前只暴露了 `create_codex_session`。新增 `create_claude_session` 是跨
  crate 的独立改动。
- **失败的 smoke 记录默认要不要保留?** v1 留 —— 有法医价值。`--cleanup` 后续连 worktree
  清理一起加。
- **`--cleanup` 怎么设计?** 等正常 smoke 稳定后再设。大概形态:
  `--cleanup=on-pass` / `--cleanup=always` / `--cleanup=never(默认)`。
