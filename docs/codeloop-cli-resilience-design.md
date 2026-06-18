# Codeloop 韧性与两阶段工作流设计

> 目标读者：维护 `zero-desktop` 复核循环（codeloop）的人。
> 状态：草案（试用前定稿，逐步实现）。
> 关联代码：`crates/agent-session/`（driver/store/watch）、`crates/codeloop-core/`（parse/prompt）、
> `crates/zero-desktop/src/modules/codeloop/`（mod/db）、`crates/zero-desktop/ui/src/modules/codeloop/`。

## 0. 这份文档涵盖什么

把围绕复核循环的几项改造一次性定下来，它们互相咬合：

1. **CLI 契约韧性**（§2–§3）——外部 `codex`/`claude` CLI 的调用方式与输出格式随版本漂移会让功能
   静默失效，建立"早发现、不静默、易修复"的机制。
2. **交互式自检面板 + 双模式探针**（§4）——面板里主动点一下做前置检查，可选做实发往返校验。
3. **预览交互台 + 首轮预热免重发**（§5）——单会话预览/手动驱动台，并让首轮说明块可由外部预热代替。
4. **运行时可翻转的自动确认**（§6）——逐步确认看放心了，一键转全自动，列表里也能随时切。
5. **两阶段工作流**（§7）——design 复核通过 → 弹"实现配置确认"窗 → 进入 implementation 实现。
6. **界面布局重构**（§8）——顶部新建常驻 + 下方"列表 / 选中记录只读详情"左右分栏。

## 1. 背景与核心矛盾

复核循环把 `codex` 与 `claude` 两个**外部 CLI** 当成可驱动的会话后端：每轮把 prompt 拼成命令行参数
spawn 子进程，再从 stdout / 会话文件读回结果。整个机制依赖一整套**隐式契约**——这些不是我们能控制的
API，而是各 CLI 版本里"碰巧如此"的行为。**任一契约随 CLI 升级而变，复核循环就会失效，且不少是静默的**
（解析出空串而非报错，循环看着在跑、实则空转）。

本设计不改核心复核逻辑，围绕两件事展开：① 把这套脆弱契约加固到"断了立刻知道"；② 在其之上补齐
"自检 → 预热 → design 复核 → 转自动 → 实现"的完整工作流与配套界面。

---

## 2. CLI 契约清单（隐患盘点）

下表是所有"会随 CLI 版本漂移而断裂"的耦合点。`失效模式` 区分**硬失败**（报错、可见）与**静默降级**
（无报错、结果错或空，最危险）。

### 2.1 命令调用方式（argv）

| # | 契约 | 位置 | 失效模式 |
|---|---|---|---|
| C1 | Codex `codex exec -s workspace-write -c approval_policy="never" --cd <repo> resume --json <id> <prompt>` | `agent-session/src/driver.rs` `codex_argv()` | 硬失败/静默 |
| C2 | Claude `claude -p <prompt> --resume <id> --permission-mode acceptEdits` | `driver.rs` `claude_argv()` | 硬失败/静默 |
| C3 | 新建 Codex 会话 argv（无 `resume <id>`） | `driver.rs` `codex_create_argv()` | 硬失败 |
| C4 | 裸命令名 `codex`/`claude` 在 PATH | `driver.rs` `send()`/`run_capture()` | 硬失败（已有清晰报错） |
| C5 | Windows npm cmd-shim → node 直跑解析 | `driver.rs` `resolve_program()`/`npm_shim_to_node()` | 硬失败 |
| C6 | Claude resume 必须在原始 cwd | `driver.rs` `send()` | 静默 |

### 2.2 stdout 输出格式

| # | 契约 | 位置 | 失效模式 |
|---|---|---|---|
| O1 | Codex JSONL `item.completed → item.agent_message → text` | `driver.rs` `parse_codex_stdout()` | **静默（空回复）** |
| O2 | Codex 旧格式兜底 `task_complete.last_agent_message` / `agent_message.message` | 同上 | 静默 |
| O3 | 新会话 id 取 `thread.started.thread_id` | `driver.rs` `parse_codex_thread_id()` | 硬失败 |
| O4 | Claude stdout 为**纯文本**，整段 trim | `driver.rs` `parse_claude_stdout()` | **静默（格式变 JSON 即错）** |

### 2.3 会话文件路径与 JSON 字段

| # | 契约 | 位置 | 失效模式 |
|---|---|---|---|
| F1 | Codex 索引 `~/.codex/session_index.jsonl`（`id`/`thread_name`/`updated_at`） | `store.rs` | 静默 |
| F2 | Codex 事件 `~/.codex/sessions/<Y>/<M>/<D>/rollout-*-<id>.jsonl`（含 archived） | `store.rs` | 静默 |
| F3 | Codex 状态 `event_msg.payload.type ∈ {task_started,task_complete}` | `store.rs` `codex_status()` | 静默 |
| F4 | Codex cwd `session_meta.cwd`/`turn_context.cwd`（含顶层兜底） | `store.rs` `codex_cwd()` | 静默 |
| F5 | Claude 事件 `~/.claude/projects/<encoded_cwd>/<sessionId>.jsonl` | `store.rs` | 静默 |
| F6 | Claude 状态 `assistant.message.stop_reason ∈ {end_turn/stop_sequence/stop→Idle, tool_use→Generating}` | `store.rs` `claude_status()` | **静默（直接影响 wait_for_idle）** |
| F7 | Claude cwd 首个含 `cwd` 字段的事件 | `store.rs` `claude_cwd()` | 静默 |
| F8 | 空闲判定：状态 Idle 且会话文件 mtime 两拍不变 | `agent-session/src/watch.rs` `is_idle_stable()` | 静默（永不空闲→超时中止） |

### 2.4 文本标记协议（我方定义、要求模型遵守）

| # | 契约 | 位置 | 失效模式 |
|---|---|---|---|
| P1 | `VERDICT: PASS` / `NEEDS_WORK`（取**最后一次匹配** `VERDICT:` 的行，大小写不敏感） | `codeloop-core/src/parse.rs` `parse_verdict()` | 半静默（连续 2 次 `AbortedParse`） |
| P2 | `ASK_USER: {json}`（取**第一条匹配** `ASK_USER:` 的行，JSON 失败回退纯文本） | `parse.rs` `parse_ask_user()` | 静默（漏判→不挂起自顾自走） |
| P3 | `WORKTREE: <绝对路径>`（取**最后一次匹配** `WORKTREE:` 的行） | `parse.rs` `parse_worktree_path()` | 静默（重定位失败→悄悄留在原仓库） |
| P4 | prompt 占位符与说明块（`STANDING_BLOCK`/`WORKTREE_INSTRUCTION`） | `codeloop-core/src/prompt.rs` | 静默（改模板与 parse 端不同步） |

**结论**：上列 22 个耦合点（C1–C6、O1–O4、F1–F8、P1–P4）过半是**静默降级**。最大的风险不是"会不会断"，而是"断了没人知道"。

## 3. 韧性设计原则与分层加固

**原则**：① 契约集中、单一事实源；② 静默必须变响（带原始片段冒泡到 UI/日志）；③ 可旁路修复
（尽量改配置/加兜底分支即修，不必发版）；④ 升级早知道（黄金样本测试）；⑤ 不破坏现状（增量演进）。

**第 1 层（P0 止血）——把静默变响**

- **空回复 = 显式错误**：`parse_codex_stdout`/`parse_claude_stdout` 解析出空（或 Codex 没匹配上任何已知
  schema）时，不返回空串，而是结构化错误带 `raw_tail`；上层以基础设施错结束本轮，`drive()` emit
  `error` 并把片段写进 `loops.error` 与进度，UI 提示"CLI 输出无法识别（可能版本变更），原始片段：…"。
- **解析失败带原文回传**：`AbortedParse` 时把最后一轮 Codex 原文尾部一并 emit，让用户区分"模型没按
  格式"还是"CLI 输出整个变样"。
- **状态未知要可见**：连续 N 拍 `Unknown`（非正常流转）提前判定"状态字段疑似变更"并带因中止，不干
  等 10 分钟超时。

**第 2 层（P1）——契约常量集中化 + 黄金样本回归测试**

把散落的 argv 形态、字段名、路径、标记收敛到一处（先做成集中常量，**暂不**上运行时可配 DSL，避免过度
设计）。在 `crates/agent-session/tests/fixtures/` 落**真实采样**的 stdout/会话 jsonl 片段，把 §2 整张表
逐条钉成断言。CLI 升级后重新采样跑 `cargo test`，**红的那条就是破掉的契约**，定位成本从"用户试用炸了
再 debug"降到一次测试。样本同时是活文档。

**待评估**：是否主动给 Claude 加 `--output-format json` 把格式由我方锁定（需确认目标版本支持）。

---

## 4. 交互式自检面板 + 双模式探针

参照已有的 `/api/web/llm` `POST /ping` 连通性自测，给 codeloop 配一个**用户主动点**的自检面板，把 §2
里大量"静默"提前暴露在"点启动那一刻"。分三档，风险/同意递增：

| 档 | 做什么 | spawn | 动仓库/会话 | 同意 | 覆盖契约 |
|---|---|---|---|---|---|
| **被动检查** | CLI 在 PATH？选中会话文件可定位、可解出 cwd 与状态？三方仓库一致性？ | 否 | 否 | 点开即跑 | C4/C5、F1–F8 |
| **版本探针** | `codex --version`/`claude --version`，记录版本号 | 是(无害) | 否 | 默认开 | C5、版本记录 |
| **实发往返** | 发最小探针、读回，验证 argv→stdout 解析→回复非空全链路 | 是 | **会(需隔离)** | **显式勾选** | 合成探针→C3/O3、O1/O4、空回复；resume 路径 C1/C2/C6 须由首轮预热探针(§5)覆盖 |

**双模式探针**（实发往返这一档）：

- **轻量合成探针**（两端各走一次性**新建**会话，互不依赖；**注意它覆盖的是新建路径，不是 resume 路径**）：
  - Codex 端：一次性新建会话（`codex_create_argv`）+ 降权只读沙箱（`-s read-only`，若版本支持）。
    覆盖 **C3（新建 argv）/O3（`thread.started`）/O1（输出解析）** + C4/C5（spawn/解析）+ 空回复；
    **不**覆盖 C1 的 `resume --json <id>` 路径。
  - Claude 端：不带 `--resume` 的一次性 `claude -p <prompt>`（即新建临时会话，**非** resume 真实会话）+
    只读权限模式（如 `--permission-mode plan`，待确认目标版本支持）。覆盖 **新建调用 + O4（纯文本输出）** +
    C4/C5 + 空回复；**不**覆盖 C2 的 resume argv 与 C6 的原 cwd 约束。当前 `driver.rs` 只有带 `--resume`
    的 `claude_argv()`，故需新增一个 Claude 探针 argv。
  - 两端探针 prompt 均为"只回复 `PROBE_OK`、不要执行任何操作"，回复能解析出固定 token → 可断言、可自动化、
    不污染真实会话。
- **resume 路径（C1/C2/C6）的覆盖**：合成探针走新建会话，天然碰不到 resume；要验证 resume argv 与原 cwd
  约束，只能由**首轮预热探针**（§5，经预览台对真实会话 resume 发送，走 `codex_argv`/`claude_argv`）覆盖。
  即"合成探针验新建路径、首轮预热探针验 resume 路径"，两者合起来才是完整的实发往返覆盖。
- **首轮预热探针**：见 §5——用真实首轮提示词当探针，既验契约又建会话又免重发。

> ⚠️ 安全收口：Codex 默认 `-s workspace-write` + `approval_policy="never"`，**实发探针若直接 resume 真实
> 会话，模型可能真动文件、且污染会话历史**。故实发往返必须 ① 用户显式同意；② 探针降权只读 + 一次性
> 会话（合成探针），或 ③ 由用户在预览台手动发出（首轮预热探针，天然同意）。

**面板形态**：`LoopStatusBar` 加「环境自检」按钮 → 弹 checklist 面板（仿 `AskUserModal`）。每行一个检查项
带 ✓/✗/⚠ + 详情，失败项展开**原始片段**（即 §3 的"静默变响"）。实发往返几行用复选框
`☐ 允许实发探针（会真实调用一次 CLI）` 门控，不勾即 skip。

**后端**：新增 `codeloop_preflight(input, { live: bool }) -> PreflightReport`，逐项跑、逐项返回；
`PreflightReport ≈ Vec<{ id, label, tier, status: pass|fail|warn|skipped, detail, raw_excerpt? }>`。

---

## 5. 预览交互台 + 首轮预热免重发

**预览交互台**：会话旁加「预览」→ 弹单会话窗口，复用现有 `codeloop_session_messages` + `MessageColumn`
（`TrackModal` 现为双栏版，单栏即其裁剪）。窗口底部加输入框 → 调 `driver::send` 发给这一个会话、回复
刷进预览。后端加薄命令 `codeloop_send_one(session_ref, text) -> reply`。

> 注意：输入框的"发送"与循环**同等权力**（Codex `workspace-write`/Claude `acceptEdits`），是"手动驱动台"
> 而非只读预览，UI 要让用户清楚"这一发是真发"。

**首轮预热免重发**：现有优化已把常驻说明块（`STANDING_BLOCK` 定位 + ASK_USER 协议）只在持续会话首轮
发一次（`render_*_prompt(first_turn=true)`，见 `mod.rs` `codex_first_turn`）。本设计让这件事可由**外部
预热**代替：

- 预览台输入框**预填真实首轮提示词** → 用户看一眼、可改、点发送 → 回复显示在预览里。这一动作同时
  ① 验证 send+解析全链路（即实发往返探针）；② 建立真实会话上下文（这次 standing block "算数"）；
  ③ 用户亲眼亲手发出 → workspace-write 授权天然解决。
- 之后给循环一个信号"首轮已在外部建立 → 跳过 standing block"。**预览台是单会话预热（一次只热一端）**，
  故标志必须**按 provider 分开**：`StartInput` 加 `established: { codex: bool, claude: bool }`（或两个独立
  bool）。`drive()` 各端独立判断——只把已预热那端的首轮 `first_turn` 置否，**未预热端仍照常发
  `STANDING_BLOCK`**，绝不能一个总开关把两端首轮一起跳过（否则未预热端丢失目标定位与 ASK_USER 协议）。
- **拍板**：循环怎么知道某端已预热 → **显式标志**（在该端会话的预览台预热后，出现"此端已预热，启动时
  跳过其首轮说明"勾选，映射到对应 provider 的 established 字段）。不走"自动扫会话历史检测 standing
  block"——那又多一条要维护的文本匹配契约，违背 §3 减少隐式契约的初衷。

> 探针角色辨析：standing block 作"连通性探针"可以（验链路通），作"正确性探针"不行（无可断言的确定
> token）。需要可断言的纯链路检查时用合成探针 `PROBE_OK`；需要顺便建会话/免重发时用首轮预热探针。

---

## 6. 运行时可翻转的自动确认

逐步确认（`step_confirm`）的本质是"盯着它头几轮是否按用户意愿走"；放心后应能一键转全自动。把
`step_confirm` 从"启动时固定"变为"运行时可翻转"。

- **后端**：`LoopCtx.step_confirm` 改 `Arc<AtomicBool>`，`confirm_gate` **每次进门实时读**（`mod.rs` 两处
  确认门 `claude_to_codex` / `codex_to_claude`）。新增 `codeloop_set_auto_confirm(enabled: bool)`。
  `status()` 进度里带上当前是逐步还是自动，供两个 UI 入口反映真实状态。
- **入口 1（确认弹窗）**：`ConfirmGateModal` 在「不同意(停止)」「确认发送」旁加复选框
  `☐ 确认后自动继续，不再逐步确认`；勾选 + 确认发送 = `codeloop_confirm(seq,true)` + `set_auto_confirm(true)`。
- **入口 2（列表）**：`LoopList` 中**正在运行**的记录加 `逐步确认/自动` 双向切换 → `set_auto_confirm`。
  切到自动那一刻若正好有确认门挂着（`awaiting_confirm`），顺手用 Approve 唤醒放行，否则卡死等不会来的点击。
- **必须守住的边界**：自动确认**只跳过 confirm gate，绝不自动回答 ASK_USER**。两者在 `mod.rs` 是不同
  机制——确认门是"要不要发过去"的程序化关卡（可安全跳过）；ASK_USER 是模型抛出的"方案 A 还是 B"真
  需人拍板的岔路（必须照停）。`set_auto_confirm` 只动 `pending_confirm`，不碰 `pending`。
- 可选：翻转时同步更新 DB `loops.step_confirm`，让记录如实反映"从第几轮起转自动"。

---

## 7. 两阶段工作流（design 复核 → implementation 实现）

把现有的两个互斥 `mode` 串成一条流水线：

- **阶段一（防复/校验）= design 模式**：对需求/设计文档反复复核到 `VERDICT: PASS`。
- **过渡**：PASS 后**不自动往下冲**，弹"实现配置确认窗"。
- **阶段二（开始实现）= implementation 模式的一次新 codeloop**：用户在弹窗里确认或修改执行配置，点
  「开始」才进入实现。**本设计承接的执行配置 = 现有数据模型已有的字段**：claude/codex 会话、`mode`
  （置 implementation）、`max_rounds`、`use_worktree`、以及 §4–§6 的探针/自动确认等开关。
  - **范围说明**：更宽的"agent / workflow"等执行方式不在本设计的数据模型与命令清单内（`loops` 表与
    `StartInput` 均无对应字段），列为**未来扩展、字段待定义**，本期阶段二不实现，以免文档承诺无法落地
    的配置项。

设计要点：

- **「开始实现」是挂在"已 PASS 的 design 记录"上的上下文动作**，不是全局按钮（位置见 §8 详情区）。
- 它拿源记录配置**预填**实现配置弹窗（复用 §4 的启动配置 + 探针自检弹窗，触发时机从"全新启动"变成
  "PASS 后承接"）→ 用户改/确认 → `codeloop_start_implementation(source_loop_id, config)` 启动阶段二。
- **血缘**：实现记录在 DB 关联源设计记录（`loops` 加 `parent_loop_id`），列表里能看出"#13 impl ← #12
  design"。
- 阶段二本身仍是一次 codeloop（implementation 模式），照样享受 §4–§6 的自检/预热/自动确认。

---

## 8. 界面布局（顶部新建常驻 + 下方左右分栏）

**问题**：现 `CodeloopPage` 点列表某条记录会把信息**回填进顶部新建表单**——"看历史"和"配新循环"抢同
一块区域，看了就没法新建。**解法**：拆成"新建表单"与"选中记录只读详情"两块（master-detail）。

**选定布局**（顶部新建常驻 + 下方左右分栏）：

```
┌──────────────────────────────┐
│ 新建区(常驻)       [开始启动] │   ← 选 claude/codex 会话、target、mode、
├───────────┬──────────────────┤      max_rounds、worktree…+「环境自检」按钮
│ 历史列表   │ 选中记录只读详情 │
│ #12 PASS   │ 状态 / transcript│   ← 详情区：状态、transcript（只读），
│ #11 run    │                  │      底部上下文操作：design 且 PASS → [开始实现]
│ #10 done   │ [开始实现]       │      running → [自动确认 开关]（§6 入口 2）
└───────────┴──────────────────┘
```

- 新建区始终可用、不被选中记录污染；详情区有完整横向宽度放 transcript，竖向不至于太长。
- 详情区按记录状态显示不同上下文操作：design+PASS→「开始实现」；running→「逐步确认/自动」切换 +
  「停止」；其余→只读回看。
- 「预览」入口（§5）放在新建区的会话选择旁（预热用）与详情区（回看选中记录的会话）。

---

## 9. 后端改动清单（汇总）

**新增 Tauri 命令**：

- `codeloop_preflight(input, { live }) -> PreflightReport`（§4）
- `codeloop_send_one(session_ref, text) -> reply`（§5 预览台/预热）
- `codeloop_set_auto_confirm(enabled)`（§6）
- `codeloop_start_implementation(source_loop_id, config) -> loop_id`（§7）

**结构/数据模型**：

- `LoopCtx.step_confirm`：`bool` → `Arc<AtomicBool>`（§6）。
- `StartInput` 加按 provider 分开的 `established: { codex: bool, claude: bool }`（§5）。
- `loops` 表加 `parent_loop_id`（§7 血缘）、可选记录 CLI 版本号（§4 版本探针）。
- `TurnResult`/`send()`：空回复 → 结构化错误带 `raw_tail`（§3）。
- 契约常量集中化模块 + `tests/fixtures/` 黄金样本（§3 第 2 层）。

---

## 10. 落地顺序（建议）

1. **P0 止血**：空回复/无法识别 → 结构化错误 + 原文回传 UI（§3 第 1 层）。最小改动消除静默空转。
2. **P0 自动确认**：`step_confirm` 运行时可翻转 + 弹窗复选框 + 列表开关（§6）。改动小、体感强。
3. **P1 自检面板**：`codeloop_preflight` 三档（被动 + 版本探针 + 合成探针）（§4）。
4. **P1 布局重构**：新建/详情拆分为选定布局（§8），为预览与"开始实现"腾出位置。
5. **P2 预览台 + 首轮预热**：`codeloop_send_one` + 预览窗 + 按 provider 分开的 `established`（§5）。
6. **P2 两阶段**：实现配置确认窗 + `codeloop_start_implementation` + `parent_loop_id`（§7）。
7. **P2 韧性体系化**：契约常量集中 + 黄金样本回归测试（§3 第 2 层）。
8. **P3 评估**：契约外置可配 DSL、Claude 显式 `--output-format json`。

> 1、2 两步即可让试用阶段"少踩坑、好观察"；建议先合这两步再放心试用，其余按序加固。
