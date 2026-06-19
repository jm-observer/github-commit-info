# Codeloop loop ⇄ attempt 两层模型 — 设计文档

> 状态：草案
> 范围：`crates/zero-desktop/src/modules/codeloop/*` + `crates/zero-desktop/ui/src/modules/codeloop/*` + `crates/agent-session/src/driver.rs`
> 关联：[codeloop-multi-entry-design.md](codeloop-multi-entry-design.md)、[codeloop-mini-feature-design.md](codeloop-mini-feature-design.md)、[runbook-codeloop-e2e.md](runbook-codeloop-e2e.md)

## 1. 背景

现行 codeloop 把「**一行 `loops` = 一次完整循环跑动**」绑死。承接 / 重试 / 续跑全部走「新建一条 loop + `parent_loop_id` 指回去」的路径：

- design pass → 进 implementation：[mod.rs:878](../crates/zero-desktop/src/modules/codeloop/mod.rs:878) `insert_loop` + `parent_loop_id`。
- 从已完成 worktree 继续复核：[mod.rs:1574](../crates/zero-desktop/src/modules/codeloop/mod.rs:1574) 又是 `insert_loop`。
- implementation 失败要重试：没有显式入口，只能再走「从 worktree 继续复核」或重建一条。
- `codeloop_stop`：把行直接标 `aborted` + `final_verdict='stopped_tracking'`（[mod.rs:1310](../crates/zero-desktop/src/modules/codeloop/mod.rs:1310)），**没有续跑入口**。

实测后果（2026-06-19 当天 state.db）：

| id | mode | use_worktree | parent | 结局 | 备注 |
|---|---|---|---|---|---|
| 10 | design | off | 8 | done/pass | 文档复核 30s 通过 |
| 12 | impl | on | 10 | failed | 1200s 超时，无 claude_implement、无 worktree_path |
| 13 | impl | on | 12 | aborted/stopped_tracking | 后续补录到 `claude_implement` 与 `WORKTREE: D:\git\toolkit.worktrees\codeloop-multi-entry`；但 worktree 被现有 home-only 校验拒绝，`worktree_path` 未持久化，无法直接进入 Codex 复核 |

问题面：

1. **记录爆炸**：一次"设计→实现→失败→重试"链路至少 3–4 行 loop，前端列表噪声大，相互关系全靠 `parent_loop_id` 自连勉强串。
2. **stop 即终结**：停了就续不上。只能再开一条，前次已建立的 worktree、session、消息流，复用难度高。
3. **承接被建模成"新事物"**：design 通过后开 implementation 在概念上是同一意图的下一步，DB 上却是两条独立记录。
4. **失败无重试位**：implementation 1200s 卡死 → failed 后想"换条件再来一次"只能重建，没法明确表达"这是同一次工作的第 N 次尝试"。

根因不是 bug，是数据模型本身不支持「一次工作有多次尝试」。本设计把"循环"和"跑动"分两层。

## 2. 目标 & 非目标

**目标**

- 一条 `loops` 记录代表「**针对某目标的一次工作意图**」（design / impl / 两者串联）；可以经历 N 次实际跑动而不裂成 N 行。
- 每次实际的 Codex/Claude CLI 跑动是一条 `loop_attempts`，状态独立、消息归属独立。
- `stop` 仅终止当前 attempt；用户可在同一条 loop 下点"再试"开新 attempt，自动复用已建立的 worktree / sessions。
- 列表默认按 loop 折叠展示（一条线），点开看 attempt 历史。
- 既有数据可一次性迁移：现存 `parent_loop_id` 链拍平成单 loop + 多 attempts；无 parent 的退化为单 attempt loop。

**非目标**

- 不重做循环引擎本身的算法（`drive()` / `send_and_resolve` / `confirm_gate`）——只动状态/边界的归属。
- 不动 codeloop-core 的 prompt 模板与 verdict 解析。
- 不动 toolkit-server 的 `cross_review` kind（那条线没用 attempt 概念，桌面端先落地）。
- 不引入跨 loop 的合并/分叉（也就是 attempt 仍属于唯一一个 loop，不支持把 attempt 重挂到别条 loop 下）。

## 3. 数据模型

### 3.1 表结构（新版）

```sql
-- 一次「工作意图」：稳定身份 + 目标定位 + 配置 + 聚合状态
CREATE TABLE IF NOT EXISTS loops (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,

  -- 目标 & 仓库定位（贯穿所有 attempts）
  repo_root TEXT NOT NULL,
  target_repo_rel TEXT NOT NULL,
  target_abs TEXT NOT NULL,
  target_label TEXT NOT NULL,

  -- 工作意图形态
  mode_plan TEXT NOT NULL,    -- 'design_only' | 'design_then_impl' | 'impl_only' | 'review_existing'
  max_rounds INTEGER NOT NULL,
  use_worktree INTEGER NOT NULL,
  step_confirm INTEGER NOT NULL,
  wait_for_idle INTEGER NOT NULL,

  -- 稳定身份：一旦确定就跟着 loop 走，attempts 共用
  worktree_path TEXT,         -- 实现首步解析到 WORKTREE: 后回填，之后所有 attempt 复用
  claude_session TEXT NOT NULL,
  codex_session TEXT NOT NULL,
  claude_cwd TEXT NOT NULL,
  codex_cwd TEXT NOT NULL,

  -- 聚合状态（由 current attempt 推导写回）
  current_attempt_id INTEGER, -- 指向 loop_attempts.id；NULL=还没起 attempt
  status TEXT NOT NULL,       -- 'idle' | 'running' | 'done' | 'aborted' | 'failed'
  final_verdict TEXT,         -- 同 attempts.final_verdict，但代表「整条意图」的最新判定
  error TEXT                  -- 最近一次失败的简短错误
);
CREATE INDEX IF NOT EXISTS loops_updated ON loops(updated_at DESC);

-- 一次实际跑动：一段从 spawn 到终态的 driver 生命周期
CREATE TABLE IF NOT EXISTS loop_attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  loop_id INTEGER NOT NULL REFERENCES loops(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,       -- 在 loop 内的尝试序号（从 1 起）

  kind TEXT NOT NULL,         -- 'design' | 'implementation' | 'resume_review'
  started_at TEXT NOT NULL,
  ended_at TEXT,

  status TEXT NOT NULL,       -- 'running' | 'done' | 'aborted' | 'failed'
  final_verdict TEXT,         -- pass / needs_work / aborted_timeout / aborted_by_user / aborted_parse / max_rounds / interrupted
  total_rounds INTEGER NOT NULL DEFAULT 0,
  error TEXT,

  -- 跑动级附带（可与 loop 层重复，便于审计每次尝试时的实际入参）
  rounds_planned INTEGER NOT NULL,
  worktree_path_at_start TEXT, -- 起跑时 loop.worktree_path 的快照（用于追溯）

  UNIQUE(loop_id, idx)
);
CREATE INDEX IF NOT EXISTS loop_attempts_loop ON loop_attempts(loop_id, idx);

-- 消息归到 attempt：UI 按 attempt 切片折叠
CREATE TABLE IF NOT EXISTS loop_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  attempt_id INTEGER NOT NULL REFERENCES loop_attempts(id) ON DELETE CASCADE,
  loop_id INTEGER NOT NULL,   -- 冗余列：按 loop 拉全量消息时省一次 join
  ts TEXT NOT NULL,
  round INTEGER NOT NULL,
  kind TEXT NOT NULL,
  verdict TEXT,
  content TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS loop_messages_attempt ON loop_messages(attempt_id, id);
CREATE INDEX IF NOT EXISTS loop_messages_loop ON loop_messages(loop_id, id);
```

### 3.2 字段语义要点

- **`loops.status` 聚合规则**：
  - 当 `current_attempt_id` 存在且其 attempt.status='running' → loop.status='running'。
  - attempt 终态：把其 final_verdict 提升到 loops.final_verdict；如 attempt 是 `done/pass` 且 mode_plan 的下一阶段已无（如 design_only 或 design_then_impl 的 impl attempt 通过）→ loops.status='done'，否则 loops.status='idle'（等用户决定下一步：再试 / 进入下一阶段 / 关）。
  - attempt failed/aborted → loops.status='aborted'/'failed'，但**允许再开 attempt 重置回 running**（这是关键区别于现状）。
- **`worktree_path` 的归属**：上提到 loop 层。任何 attempt 一旦解析到可信的 `WORKTREE: <abs>` 立刻写回 `loops.worktree_path`，后续 attempts 默认复用（避免 13 这种"已实现但续不上复核"）。
- **session 也上提到 loop 层**：同一条 loop 的所有 attempts 默认共用 Claude/Codex session（一致的对话历史），与现状一致——只是从 attempt 层提到了 loop 层，去掉重复。
- **删除 `parent_loop_id`**：blood-line 内化为 attempts，不再需要跨 loop 的 self-ref。

### 3.3 Worktree 信任边界

现有 `relocate_to_worktree()` 只允许 worktree 位于用户 home 下；这会误拒本机开发仓常用的 `D:\git\...`。id=13 的真实路径
`D:\git\toolkit.worktrees\codeloop-multi-entry` 存在且是同一仓库的 worktree，却被判为"越出用户目录"。

改为**可信根 + 同仓校验**两层：

1. worktree 路径必须存在，且 `find_repo_root()` 能识别为 git 工作树。
2. worktree 必须与原仓库同源：
   - 优先校验 `git rev-parse --git-common-dir` canonicalize 后一致；或
   - 校验 `git remote get-url origin` 一致（没有 common dir 时兜底）。
3. worktree 必须位于可信根之一：
   - 用户 home；
   - 当前 repo 根的同级派生目录，如 `D:\git\toolkit.worktrees\*`；
   - 配置项 `codeloop.trusted_worktree_roots`（默认可包含 `D:\git`，允许用户在设置页增删）。

因此 `D:\git\toolkit.worktrees\codeloop-multi-entry` 应通过校验并写入 `loops.worktree_path`。真正危险的是"任意路径 + 非同仓"；不是 home 之外本身。

### 3.4 现有数据迁移

一次性迁移脚本，桌面端启动时检测 schema 版本执行：

1. 建新表：`loops_v2` / `loop_attempts` / `loop_messages_v2`。
2. 把现有 `loops` 按 `parent_loop_id` 串成弱连通分量。每个分量内：
   - 选「最早创建」的那一行作为 loop seed，沿用它的 `target_*` / `repo_root` / sessions / config。
   - 分量内每行 loops 变成一条 attempt，按 `created_at` 升序分配 idx。
   - mode 映射成 attempt.kind：原 `mode='design'` → 'design'；`mode='implementation' && resume_from_worktree` → 'resume_review'；其余 → 'implementation'。
   - 把原 loops.{status,final_verdict,total_rounds,error,worktree_path} 拷到对应 attempt。worktree_path 同时上提到新 loop（取分量内最后一个非空值）。
   - mode_plan 推导：先把连续重复 kind 压缩为阶段序列（例如真实链 `design,design,implementation,implementation` 代表 design 重试 + implementation 重试），再映射：`['design']` → `design_only`，`['design','implementation']` → `design_then_impl`，`['implementation']` → `impl_only`，包含 `resume_review` → `review_existing`。
3. `loop_messages` 表的 loop_id 改写成新 loop_id；新增 attempt_id 字段，按 `old_loop_id -> new_attempt_id` 映射直接归属。不要按 ts 时间窗推断；旧消息已经精确挂在旧 loop 行上，时间窗只作为发现孤儿/异常数据时的兜底。
4. 把旧表重命名 `loops_v1_bak` / `loop_messages_v1_bak`（不删，方便回滚 1–2 个版本），打开新表。
5. 写 `meta.schema_version = 2`。

兜底：若分量内某行 attempts 全部失败、当前没有 running、用户也没明确要重试，loops.status 写 'aborted'，前端列表显示一个"重试"按钮。

## 4. 行为与 API

### 4.1 启动与生命周期

| 场景 | 现状 | 改后 |
|---|---|---|
| 新建 design 复核 | `insert_loop(mode=design)` | `create_loop(mode_plan=design_only OR design_then_impl)` + `start_attempt(kind=design)` |
| design pass → 进 impl | 用户点按钮 → `insert_loop(mode=impl, parent_loop_id)` | 同一 loop 上 `start_attempt(kind=implementation)`，沿用 worktree（若 use_worktree）/sessions |
| impl 失败重试 | 没有，只能再 insert 一条 | `start_attempt(kind=implementation, idx+1)`；UI 按钮"重试本次实现" |
| 从外部 worktree 继续复核 | `insert_loop(... resume_worktree_path=...)` | `create_loop(mode_plan=review_existing, worktree_path=...)` + `start_attempt(kind=resume_review)` |
| stop | loop 终态 `aborted/stopped_tracking`，无下文 | 当前 attempt 标 `aborted/aborted_by_user`，loop 落回 `idle`，UI 出现"再试" |
| 进程崩溃恢复 | open() 把 running 标 aborted | open() 把 running attempt 标 'interrupted'；loop 落 'aborted'，等用户决定 |

### 4.2 Tauri 命令变化

- `codeloop_start`：参数移除 `parent_loop_id` / `resume_worktree_path`；改为返回 `{ loop_id, attempt_id }`。新建 loop + 第一个 attempt。
- 新增 `codeloop_start_next_attempt(loop_id, kind, options?)`：在已有 loop 上开新 attempt。kind 取 'implementation' | 'resume_review' | 'design'（设计模式下重试用，少见）；options 可覆盖 max_rounds 等。
- `codeloop_stop(loop_id)`：改为停 loop 的 current_attempt；不再写 loop 级终态。
- `codeloop_get_detail(loop_id)`：返回 `{ loop, attempts: [{id, idx, kind, status, final_verdict, total_rounds, started_at, ended_at, error}] }`。messages 拉取改 `codeloop_get_attempt_messages(attempt_id)`，避免一次拉全部 attempts 的消息流。
- `codeloop_list_loops(limit)`：返回 loops 列表（不带 messages），每条带 `latest_attempt` 概要 + `attempt_count`。

### 4.3 内存状态

`CodeloopState.running` 当前是 `HashMap<loop_id, RunningLoop>`，改为 `HashMap<loop_id, RunningAttempt { attempt_id, handle, ... }>`：一个 loop 同一时刻最多一个 running attempt。`codeloop_stop` abort 这个 handle 并标 attempt。

### 4.4 driver 流式 + worktree 早回填（与本设计同期落地）

[driver.rs:284 `run_capture`](../crates/agent-session/src/driver.rs:284) 现在 `wait_with_output()` 一把梭，没法中途看到任何输出 —— 这就是 id=13 只能看见一句 system 消息的根因。改造点（独立小重构，但必须与本设计一起落，否则 attempt 级"看得见"就是空话）：

- 新增 `pub async fn send_with(session, prompt, opts: SendOpts { timeout, on_line })`；老 `send` 转包装传默认。
- `run_capture` 改成 `BufReader::new(child.stdout).lines()` 循环：累积到 String 的同时把每行交给 `on_line`。
- codeloop 调用方传一个 `on_line` 闭包：
  - 实时 `app.emit("codeloop://stream", { attempt_id, source: 'claude'|'codex', line })`。
  - 节流落 attempt 消息（kind=`claude_log`/`codex_log`, round=0），每 N 行或 K 秒一条快照（避免 DB 暴炸）。
  - **看到可信 `WORKTREE: <abs>` 立刻调 `db.set_loop_worktree(loop_id, path)`**；这样即便下一秒被 kill，worktree 也保住了，下次 attempt 能复用。可信规则见 §3.3，不再把 `D:\git` 这类开发根一概判为越界。
- timeout 拆分见 §5。

### 4.5 边界与约束

- 同一 loop 同一时刻**最多一个 running attempt**：`start_next_attempt` 若发现 current attempt 仍 running，直接 reject（让用户先 stop）。
- attempt.kind=`design` 只允许在 mode_plan 含 design 的 loop 上；`resume_review` 要求 loop.worktree_path 非空。校验在后端做，不靠前端守门。
- 删除 loop：级联删 attempts + messages（外键 ON DELETE CASCADE）；前端按 loop 删，不暴露按 attempt 删。
- stopped_tracking 且已补录到 `claude_implement + WORKTREE` 的 attempt，不应提示"重试实现"作为唯一动作；应优先提示"从 worktree 继续复核"。

## 5. Timeout 拆分（顺带做掉）

[driver.rs:31](../crates/agent-session/src/driver.rs:31) 单常量 `TURN_TIMEOUT=1200s` 拆为：

```rust
pub const TURN_TIMEOUT_DEFAULT: Duration = Duration::from_secs(1200);  // review / revise / wait_idle
pub const TURN_TIMEOUT_IMPL: Duration    = Duration::from_secs(3600);  // implementation 首步（worktree+多文件+编译验证）
```

调用方按 step 类型显式传：[mod.rs:379](../crates/zero-desktop/src/modules/codeloop/mod.rs:379) implementation 首步用 `TURN_TIMEOUT_IMPL`，其余维持默认。理由：worktree 模式下 Claude 子 agent 跑多文件实现 + cargo check 常态 30–60min，1200s 死得太干脆，loop 12 就是被这刀切的。

## 6. UI 改动

### 6.1 列表（LoopList）

- 一行 = 一条 loop，不再"承接自 #N"的二级行。
- 右侧状态：聚合自当前 attempt（running / done / idle/aborted）+ 一个 attempt 计数 chip（如 `尝试 2`）。
- aborted/failed 的 loop 列表项右侧出现快捷"再试"按钮。

### 6.2 详情（LoopDetail）

- 顶部信息条：目标、mode_plan、worktree_path、当前 status。
- attempts 切换器（tabs 或 segmented control）：默认选中最新；每个 tab 标 kind + 状态 + 用时。
- 消息流：仅显示当前选中 attempt 的消息。切换 attempt 触发 `codeloop_get_attempt_messages`。
- 操作按钮（按 loop 状态变化）：
  - running：`停止当前尝试`。
  - idle/aborted/failed 且 mode_plan 还有未完成阶段：`开始实现` / `重试实现` / `从 worktree 继续复核`（按 mode_plan 派生）。
  - done：`再次复核`（开新 resume_review attempt）。

### 6.3 实时 stream 面板（新）

详情页底部加一个可折叠的"实时输出"区，订阅 `codeloop://stream`：滚动显示 Claude/Codex 的 stdout 行（彩色区分），上限 200 行循环 buffer。这是解决"启没启动看不到"的最直接面。停 attempt 之前用户随时能确认子 agent 真的在跑而不是死了。

## 7. 落地顺序

按风险递增 + 价值递减拆三步：

1. **driver 流式 + timeout 拆分**（0.5–1d）：纯加法，对数据结构无影响。立刻能解决"启没启动看不见"和"impl 1200s 必死"两个具体痛点。先让现状能跑通同类目标，再做模型重构。
2. **schema v2 + 迁移脚本**（1d）：建新表、写迁移、跑一遍现有 state.db 验证拍平结果。先只在 dev 库验证，不动 main 行为。
3. **行为切换 + UI 改造**（1–2d）：把 mod.rs 的 insert_loop 调用点全部改成 loop+attempt 双层；Tauri 命令重命名/新增；UI LoopList/LoopDetail 改版；删除 `parent_loop_id` 相关 TS 字段。

每步独立可验、独立可回滚。

## 8. 风险 & 回滚

- **迁移脚本错挂**：少数边界（循环引用、跨 worktree 的奇怪老链、孤儿消息）可能归属错。缓解：优先用 `old_loop_id -> new_attempt_id` 精确映射消息，保留 `loops_v1_bak` / `loop_messages_v1_bak`，迁移前 dump 一份；任何归属歧义记 warn 日志便于排查。
- **worktree 信任边界放宽过度**：取消 home-only 后必须保留"同仓 + 可信根"两道校验。`D:\git` 可以通过配置或 repo 同级派生规则放行，但不能接受 Claude 回报的任意磁盘路径。
- **driver 流式破坏 Codex stdout 解析**：codex 的 stdout 是 JSONL，按行读其实正合用；但要小心 Windows 下混入的 GBK 噪声行（[driver.rs:13](../crates/agent-session/src/driver.rs:13) 已记），lossy 解码逻辑不能丢。回滚：保留旧 `run_capture` 走 wait_with_output 的分支，按 cfg 切换。
- **timeout 改长导致真正卡死的不可见**：必须配合 §4.4 的 stream + 心跳，否则只是把"快速失败"换成"慢速失败"。两件事必须同步上。
- **UI attempt 切换器复杂度**：先实现单层 tabs，不做拖拽/重排；attempts 数量上限不强制，但 > 10 时给个折叠样式。

## 9. id=13 的处置（即时手动）

loop 13 现在已经被 stop 标为 `aborted/stopped_tracking`，但后续补录到了 Claude 实现结果：

```text
WORKTREE: D:\git\toolkit.worktrees\codeloop-multi-entry
```

该目录真实存在，是 `D:\git\toolkit` 的同仓 worktree；当前失败点是 `relocate_to_worktree()` 的 home-only 校验把 `D:\git` 判为越界，导致 `loops.worktree_path` 没有写入。

本设计落地后应先按 §3.3 放宽信任边界，再用补录逻辑把 `worktree_path` 回填到 loop/attempt。回填后，UI 应提供"从 worktree 继续复核"，而不是要求重建 implementation loop。

按本设计落地前，若要临时收尾，可手动标记：

```sql
-- 在 codeloop/state.db
UPDATE loops
   SET status='aborted',
       final_verdict='aborted_by_user',
       error='手动收尾：已补录到 WORKTREE，但当前 home-only worktree 校验拒绝 D:\git 路径；放宽可信根后可回填并继续复核。',
       updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
 WHERE id=13;
```

不推荐直接重建 implementation loop；更好的临时方案是先修 `relocate_to_worktree()` 的可信根规则，让 `D:\git\toolkit.worktrees\codeloop-multi-entry` 可被接受，再从该 worktree 继续 Codex 复核。

## 10. 不动的部分

- codeloop-core 的 prompt 模板（`DEFAULT_*_TEMPLATE` / `STANDING_BLOCK`）：模板不感知 attempt，每个 attempt 起来时 `first_turn=true`（除非沿用同一会话），渲染规则不变。
- toolkit-server 的 `cross_review` kind：本期只动桌面端。HTTP 路径下 loop ⇄ attempt 是否同样有价值另开一个 RFC 评估。
- ASK_USER 协议 / verdict 解析 / worktree relocate 逻辑：全部沿用。
