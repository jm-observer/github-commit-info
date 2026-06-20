# Codeloop 单记录续跑模型 — 设计文档

> 状态：草案 v2（简化版，撤掉 attempt 分表）
> 范围：`crates/zero-desktop/src/modules/codeloop/*` + `crates/zero-desktop/ui/src/modules/codeloop/*` + `crates/agent-session/src/driver.rs`
> 关联：[codeloop-multi-entry-design.md](codeloop-multi-entry-design.md)、[codeloop-mini-feature-design.md](codeloop-mini-feature-design.md)、[runbook-codeloop-e2e.md](runbook-codeloop-e2e.md)

## 1. 背景

现行 codeloop 把「**一行 `loops` = 一次完整循环跑动**」绑死：

- 停了就续不上：[`codeloop_stop`](../crates/zero-desktop/src/modules/codeloop/mod.rs:1310) 把行直接标 `aborted/stopped_tracking`，没有续跑入口；想继续只能新建一条 loop + `parent_loop_id` 自连。
- implementation 1200s 卡死后只能再 insert 一条来重试。
- design pass → 进 implementation 在概念上是同一意图的下一步，DB 上却是两条独立记录靠 `parent_loop_id` 串。
- 实测：2026-06-19 的 state.db 里出现 10→12→13 这条由 `parent_loop_id` 链起来的连环新行，loop 13 仍 stuck 在 stopped_tracking。

问题面收敛成一句：**没有"在同一条记录上多次尝试"的概念**。

## 2. 设计取向：保持一条 loop，重试只是状态变化

桌面端低并发场景下没必要把"一次工作意图"和"一次跑动"分开存。沿用现有 `loops` + `loop_messages` 两张表的形态，**给 loops 加少量字段表达"已尝试过 N 次 + 上次停在哪步"**，"继续"就是把 status 翻回 running、消息接着往后追加。

收益：

- **schema 改动量极小**——给 loops 加 2 列，删 1 列（`parent_loop_id` 停用），不动 messages。
- **既有数据无需迁移**——幂等 ALTER 即可，loop 13 都能保留。
- **UI 改动量极小**——列表加一个 attempts_count chip，详情加一个「继续」按钮，**不需要 attempts tabs 切换器**。
- **mental model 一致**——一条记录 = 一段工作。

代价（可接受）：

- 看不到"尝试 1 用 13min 失败 → 尝试 2 用 5min PASS"的结构化历史。桌面端单人用，从 system 消息时间戳推得出来，不需要专门数据结构。
- `total_rounds` 变成"累计已 review 次数"而非"通过用了几轮"。够用。

## 3. 数据模型

### 3.1 改动（增量 ALTER）

```sql
-- 已有列保留：id / created_at / updated_at / target_* / repo_root / mode / max_rounds
-- / wait_for_idle / step_confirm / use_worktree / claude_session / codex_session
-- / claude_cwd / codex_cwd / status / final_verdict / total_rounds / worktree_path
-- / error / entry_kind / design_doc_path / seed_review_path / seed_review_inline_hash

-- 新增（幂等 ALTER）：
ALTER TABLE loops ADD COLUMN last_phase TEXT;
  -- 'implementing' | 'implemented' | 'codex_review' | 'claude_revise' | 'awaiting_user' | 'finalized'
  -- NULL = 老记录或刚创建未推进；UI"继续"按钮按此字段决定是否启用、续跑时给哪种 prompt
ALTER TABLE loops ADD COLUMN attempts_count INTEGER NOT NULL DEFAULT 1;
  -- 当前是第几次尝试（含首次）。每次"继续" +1。仅展示用。

-- 停用（不读、不写新值；保留列以兼容老行，不做 DROP）：
-- loops.parent_loop_id —— 续跑改为原地，血缘消失
```

`loop_messages` **结构完全不动**——`loop_id` / `ts` / `round` / `kind` / `verdict` / `content` 全部沿用，继续往后追加。`round` 不再每次"继续"重置——直接接续上次的最大值 +1，UI 上看消息流像一段连续的轨迹（中间可由 `kind=system` content="重新继续（第 N 次尝试）" 当章节标记）。

worktree 校验路径已经在 Phase 0 修复（[mod.rs:944 `relocate_to_worktree()`](../crates/zero-desktop/src/modules/codeloop/mod.rs:944) 改成 "home OR 同仓派生"），本设计直接依赖。

### 3.2 worktree_path 由 Claude 报、ZD 落库

执行权归 Claude（沿用 [`WORKTREE_INSTRUCTION`](../crates/codeloop-core/src/prompt.rs:88) 让它 `git worktree add` 并单行回报 `WORKTREE: <abs>`）。ZD 只负责：

1. **流式解析回报**（Phase 1 已做）：driver 按行读 stdout，看到 `WORKTREE:` 行立刻 → `relocate_to_worktree()` 校验 → `db.set_worktree(loop_id, path)`，**即使下一秒被 kill 也保住**。
2. **复用**：之后所有"继续" / Codex spawn 都用 loop.worktree_path 作 cwd / `--cd`，不让 Claude 再建。
3. **不清理**：删 loop 时不 `git worktree remove`，UI 提示用户自己清。

为什么 ZD 必须记 worktree_path 而不是依赖 Claude 记忆：**Codex 是另一个 CLI、另一个会话**，spawn 时必须由 ZD 显式传 `--cd <worktree>`，它不会去问 Claude，也没法跨会话探听。同理"继续"按钮按下后要立刻知道两端的 cwd，进程崩溃后恢复也要立即知道——靠 Claude 记忆不可靠也耗一次 CLI。

不引入 `worktree_source` 字段——既然 ZD 全程不创建不清理，"Claude 报的"和"用户粘贴的"语义上一样，分了用不上。

### 3.3 last_phase 写入责任

在每个状态跃迁处显式 update 一次（driver 调用层做，不靠从 messages 反推）：

| 时机 | last_phase 值 |
|---|---|
| spawn 实现首步前 | `implementing` |
| 收到 claude_implement 完成 | `implemented` |
| 发送 Codex 复核前 | `codex_review` |
| 收到 Codex verdict needs_work | `claude_revise` |
| 发送 Claude 修订前 | `claude_revise` |
| 任一端触发 ASK_USER | `awaiting_user` |
| 终态（pass / max_rounds / aborted / failed） | `finalized` |

UI "继续"按钮按这字段决定该用哪种 prompt 续跑：
- `implementing` / 未设置 → 沿用首次 implementation prompt（worktree 已落库则附 `WORKTREE_REUSE_NOTICE`，否则沿用 `WORKTREE_INSTRUCTION` 让 Claude 重建）。
- `implemented` / `codex_review` / `claude_revise` → 跳过实现首步直接进 Codex 复核（即"从 worktree 继续复核"语义）。
- `awaiting_user` → 把用户回答送给挂起的那一端。MVP 阶段不实现，回退到从 Codex 复核重起。
- `finalized` → "再次复核"按钮（done 的情况），同上跳过实现进 Codex。

## 4. 行为与 API

### 4.1 生命周期对比

| 场景 | 现状 | 改后 |
|---|---|---|
| 新建 design / implement / review_seed 入口 | `insert_loop(...)` | 不变，只是新增 `last_phase` 字段写入 |
| design pass → 进 impl | 新 loop + `parent_loop_id` | **仍是新 loop**（design 和 implementation 目标不同，是真正的两段工作；不强行合并）。但 UI 不再显示"承接自 #N"——血缘字段废弃 |
| impl 失败重试 / stop 后续跑 | 必须新建 loop | **同一 loop 上点"继续"**：status 翻回 running，attempts_count +1，messages 接着追加 |
| 从外部 worktree 继续复核 | `insert_loop(... resume_worktree_path=...)` | 不变，仍是独立 loop（首次创建，没有可"继续"的 loop） |
| stop | loop 终态 `aborted/stopped_tracking`，无下文 | loop 标 `aborted/stopped_tracking`，但"继续"按钮启用——再次 running 时 attempts_count +1 |
| 进程崩溃恢复 | open() 把 running 标 aborted | 不变；"继续"按钮可用即可 |

### 4.2 Tauri 命令

- `codeloop_start`：参数 / 返回值**不变**，继续创建新 loop。
- **新增 `codeloop_continue(loop_id)`**：要求 loop 当前 status ∈ {`aborted`, `failed`, `done`}。后端做：
  1. 读 loop（验证 status / 取 worktree_path / last_phase / sessions）。
  2. `UPDATE loops SET status='running', attempts_count=attempts_count+1, updated_at=now`。
  3. 追加一条 `kind=system` 消息：`重新继续（第 N 次尝试，上次停在 last_phase=X）`。
  4. 构造 LoopCtx：若 `last_phase ∈ {implemented, codex_review, claude_revise, finalized}` 且 worktree_path 非空 → 等同于现在 `resume_from_worktree=true` 路径，跳过实现首步。否则按完整流程从实现首步走（Claude 会话历史还在，prompt 附 `WORKTREE_REUSE_NOTICE`）。
  5. spawn drive 任务，登记到 `CodeloopState.running`。
- `codeloop_stop(loop_id)`：行为不变（abort + 标 aborted/stopped_tracking）。
- `codeloop_list_loops` / `loop_messages` / `loop_detail`：返回结构加上新字段 `last_phase` / `attempts_count`，其余不变。
- 删除：`parent_loop_id` 相关字段从入参 DTO 移除（保留 DB 列但不读）。

### 4.3 内存状态

`CodeloopState.running` 维持 `HashMap<loop_id, RunningLoop>`——一个 loop 同一时刻最多一个 running 任务（continue 前必须 stop）。完全不变。

### 4.4 driver 流式 + worktree 早落库

**已在 Phase 1 落地**：driver 按行读 stdout，`on_line` 回调把 CLI 行实时 emit `codeloop_stream` 事件；implementation 首步用 `TURN_TIMEOUT_IMPL=3600s`。续跑场景同样走这条链路——`continue` 命令调 send_and_resolve_with，看到 WORKTREE 立刻落库。

详情见 [driver.rs](../crates/agent-session/src/driver.rs) 与 [mod.rs `send_and_resolve_with`](../crates/zero-desktop/src/modules/codeloop/mod.rs)。

### 4.5 边界与约束

- **同一 loop 同时只能一个 running 任务**：`codeloop_continue` 若发现 loop.status='running' 直接 reject。
- **续跑 prompt 选择硬约束**：只看 `last_phase` + `worktree_path`，不靠 Claude/Codex 会话记忆。会话沿用是 bonus，不是依赖。
- **round 计数延续**：续跑后下一轮 round = 上次最大 round + 1（用 `recorded_rounds(loop_id)` 即可）。不重置。
- **finalized 状态的"继续"**：done/pass 的 loop 也允许点"再次复核"——开启 review 一次。少见但合理（用户改了几个字想再过一遍）。
- **删 loop**：级联删 messages（外键 ON DELETE CASCADE 现在就是这么写的，[db.rs:368](../crates/zero-desktop/src/modules/codeloop/db.rs:368)）；worktree 不动。

### 4.6 Prompt 模板调整

[codeloop-core](../crates/codeloop-core/src/prompt.rs) 同步加 / 改：

- **新增 `WORKTREE_REUSE_NOTICE`**：复用既有 worktree 时附加的提示段，占位符 `{WORKTREE_PATH}`，内容：「沿用之前的工作树 `<path>`（你之前在此创建过），继续在此目录下工作；**不要再 `git worktree add` 新建**，不要切换目录」。
- **新增 `DEFAULT_CLAUDE_IMPLEMENT_RESUME_TEMPLATE`**：implementation 续跑首步 prompt，第一句给「上次实现在 ⟨原因⟩ 中断，请继续完成未做的部分，完成后用一句话概述本轮落地内容」，不附 `WORKTREE_INSTRUCTION`（cwd 已稳定）。占位符 `{LABEL}`。
- **`WORKTREE_INSTRUCTION` 本身不动**：仍只在「首次 implementation 且 worktree_path 未落库」时附。
- 其余模板（review/revise/implement 首次）维持不变。

## 5. Timeout 拆分

**已在 Phase 1 落地**：`TURN_TIMEOUT_DEFAULT=1200s` + `TURN_TIMEOUT_IMPL=3600s`，implementation 首步用后者。续跑场景的 implementation_resume 也用 `TURN_TIMEOUT_IMPL`（同种工作量）。

## 6. UI 改动

### 6.1 列表（LoopList）

- 一行 = 一条 loop（不变）。
- 标题右侧加一个 attempts_count chip——只在 ≥ 2 时显示（如 `× 2`、`× 3`）。
- aborted/failed/done 行右侧出现"继续"快捷按钮（design 模式 done 时不显示，避免误操作）。
- 删除"承接自 #N"二级行（`parent_loop_id` 不再读）。

### 6.2 详情（LoopDetail）

- 顶部信息条加一行：`上次停在：implementing` / `已通过` 等（按 last_phase 渲染人话）。
- 消息流：完全沿用现状（按 ts 升序展示，`kind=system` 的"重新继续"消息当章节分隔）。
- 操作按钮（按 status 变化）：
  - `running` → `停止`（不变）。
  - `aborted` / `failed` → **`继续`**（新）+ `删除`。
  - `done`（pass） → `再次复核`（新，本质同 continue）+ `删除`。
- 不引入 attempts tabs 切换器——所有消息按时序一条流。

### 6.3 实时 stream 面板

**已在 Phase 1 落地**（runtime.rs 加了 `stream_line` + `EV_STREAM`）。详情页订阅 `codeloop_stream` 事件滚动显示即可——这部分 UI 还没接，本设计 §6 落地时一并加上。

## 7. 落地顺序

按已交付 → 待交付划：

0. ✅ **`relocate_to_worktree()` 信任边界修复**（Phase 0，已 ship）。
1. ✅ **driver 流式 + timeout 拆分**（Phase 1，已 ship）。
2. ⏳ **schema 增量 ALTER + db helpers**（0.5d）：[db.rs](../crates/zero-desktop/src/modules/codeloop/db.rs) 加 `last_phase` / `attempts_count` 两列 + `set_last_phase` / `bump_attempts` / `reset_running_for_continue` 三个方法；LoopRow 加两字段。
3. ⏳ **last_phase 写入点 + codeloop_continue 命令**（1d）：mod.rs 在所有阶段跃迁点调 `set_last_phase`；新加 `codeloop_continue` Tauri 命令 + 续跑路径处理（含 prompt 选择）；codeloop-core 加 `WORKTREE_REUSE_NOTICE` + `DEFAULT_CLAUDE_IMPLEMENT_RESUME_TEMPLATE`。
4. ⏳ **UI**（0.5d）：LoopList 加 attempts_count chip + 继续按钮；LoopDetail 顶部加 last_phase 显示 + 操作按钮按 status 切换；订阅 `codeloop_stream` 加滚动面板；删除 `parent_loop_id` 相关 TS 类型与"承接自"展示。

每步独立可验。

## 8. id=13 的处置

按本设计不动 schema 数据。loop 13 当前状态是 `aborted/stopped_tracking`，落地 Phase 0+1 后 `relocate_to_worktree()` 已能接受 `D:\git\toolkit.worktrees\*`；落地 Phase 2-4 后，用户在它详情页能直接点"继续"——后端按 last_phase = NULL 走完整 implementation 流程（worktree 已存在则用现有的并附 `WORKTREE_REUSE_NOTICE`）。

如果想立刻清掉这条 stuck 记录，单独 `DELETE FROM loops WHERE id=13;` 即可（messages 级联删）；不影响其它数据。

## 9. 不动的部分

- `loop_messages` 表结构。
- codeloop-core 既有模板（`DEFAULT_*_TEMPLATE` / `STANDING_BLOCK` / `WORKTREE_INSTRUCTION`）。
- toolkit-server 的 `cross_review` kind（本期只动桌面端）。
- ASK_USER 协议 / verdict 解析 / multi-entry 入口（doc_review / implement / review_seed）。
- worktree 执行权：仍归 Claude，ZD 只解析 + 落库 + 复用。
- 现有 `mode`（design / implementation）字段——保留，跟 entry_kind 一起共同决定渲染哪个 prompt 模板。
