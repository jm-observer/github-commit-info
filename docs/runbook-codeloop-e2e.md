# Codeloop 跨会话复核循环 — 本机端到端 runbook

> 适用：在**本机 Windows**起一个 toolkit-server 实例，用双栏视图选一对 Codex / Claude
> 会话，启动自动复核循环并在挂起时应答。设计见
> `docs/toolkit-rfc/2026-06-15-cross-session-review-loop/plan.md`。

## 0. 前置

- 本机已装并登录 `codex` 与 `claude` CLI，且二者在 PATH 中。
- 会话存储在本机用户 home：`~/.codex`、`~/.claude`（不在 g10）。
- 待复核的 Codex / Claude 会话**同属一棵 git 工作树**（§4.1 校验，否则 400）。

## 1. 起本机 server 实例

```powershell
# 单独的本机 workspace，别复用 g10 实例
$env:TOOLKIT_WORKSPACE = "$env:USERPROFILE\.config\toolkit-server"
cargo run -p toolkit-server
# 默认监听见 config；浏览器打开 http://127.0.0.1:<port>/codeloop
```

会话存储自动从 `~/.codex` / `~/.claude` 读取（`Store::from_env`）。

## 2. 选会话 + 启动循环

1. 顶部两个下拉分别选 Claude 会话与 Codex 会话（`↻` 刷新清单）。
2. 填 `target_path`（相对工作树根，如 `docs/foo.md`；也可绝对路径）。
3. 选 `mode`（design / implementation）、`max_rounds`（默认 5）。
4. 需要先等当前 Claude 轮次结束再接管时，勾「等 Claude 空闲」。
5. 点「启动复核循环」→ `POST /api/web/codeloop/submit`。
   - 服务端先做三方 repo root 一致性校验，不一致返回 400（提示三方实际路径）。
   - 通过则起 `cross_review` 任务，状态条显示轮次 / phase / 最近 VERDICT。

## 3. 应答挂起（ASK_USER）

循环中 agent 遇决策岔路会输出 `ASK_USER: {json}` 并挂起。视图轮询到
`phase==awaiting_input` 时弹模拟弹窗：点选项按钮或自由输入 → `POST /api/web/codeloop/{task_id}/answer`
写 `codeloop_io`，任务下次轮询取走答案 resume 提问方会话。

## 4. 约束（务必遵守）

- **跑循环时别在桌面端碰这两个会话**（单写者约束，§2），跑完重开桌面端看结果。
- **别在挂起（awaiting_input）时重启 server**：重启会把 running 任务标 `interrupted`，
  挂起不自动续（MVP 限制，§10.3）。
- 非交互权限：循环让 codex/claude 在该仓自由改文件（codex `-s workspace-write` +
  `approval_policy="never"`；claude `--permission-mode acceptEdits`）。**仅对本机可信仓库跑**，
  不要开 `danger-full-access` / `bypassPermissions`。

## 5. ✅ CLI 命令真机固化结论（2026-06-15 实跑核实）

driver 的两条命令已在本机各实跑一次（codex-cli 0.130.0 / claude 2.1.170，Windows）确认。
`agent-session/src/driver.rs` 的 `parse_codex_stdout` 已据此修正（见下「重要修正」）。

### Codex（已验证）

```powershell
codex exec -s workspace-write -c approval_policy="never" --cd <repo_root> resume --json <codex_session_id> "<prompt>"
```

- 结果：exit 0，能 resume、不卡审批，stdout 为事件 JSONL。
- **实测 `--json` 事件 schema**（与 rollout 文件格式不同！）：
  - `{"type":"thread.started","thread_id":...}`
  - `{"type":"turn.started"}`
  - `{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"..."}}` ← **回复在此**
  - `{"type":"turn.completed","usage":{input_tokens,output_tokens,...}}`
- **解析（已固化）**：取末个 `item.completed` 且 `item.type=="agent_message"` 的 `item.text`；
  退化兼容旧 `task_complete.last_agent_message` / `agent_message.message`。
- ⚠️ **Windows stdout 编码坑（实测）**：stdout 会混入非 UTF-8（GBK）噪声行，例如 codex 收尾时的
  `成功: 已终止 PID xxxxx 的进程`（taskkill 输出，含 `0xb3` 字节）。`run_capture` 用
  `from_utf8_lossy` 兜底 → 噪声行变替换字符 → 作为非 JSON 行被解析器跳过。**不要**用严格 UTF-8
  整体解码 stdout。

### ⚠️ codeloop 新建的 Codex 会话「在 Codex 桌面 App 里看不到」是预期行为，非数据 bug（2026-06-18 核实）

**现象**：通过 `codeloop_new_codex_session`（→ `driver::create_codex_session`，[driver.rs:56](../crates/agent-session/src/driver.rs#L56)
的 `codex exec ... --json <prompt>`）建的 Codex 会话，在 Codex 桌面 App 的会话列表里**不显示**。

**结论：会话数据格式与参数完全正确，问题在 App 端的来源过滤，不在创建侧。** 排查依据：

- codeloop 建的会话确实写盘到 `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`，首行 `session_meta`
  的核心字段（`id`/`cwd`/`git`/`base_instructions`/`model_provider`…）与桌面 App 建的会话**逐项一致**，
  JSONL 结构完整。
- 这些会话**可被 resume**——复核循环本身就靠 `codex exec ... resume --json <id>` 续跑（§5 已验证）。
  若数据坏了，resume 起不来。
- 两类会话的**唯一实质差异**是 `session_meta.payload` 里的来源标签：

  | 创建方 | `source` | `originator` | `cli_version`（实测样本） |
  |---|---|---|---|
  | codeloop（`codex exec`） | `exec` | `codex_exec` | `0.130.0` |
  | Codex 桌面 App | `vscode` | `Codex Desktop` | `0.138~0.140-alpha` |

  桌面会话另多 `dynamic_tools`/`thread_source` 两个 App 私有字段（非必需）。

- **桌面 App 的会话列表只列它自己创建的 `source="vscode"` 会话，过滤掉 `exec`/`sdk` 来源。** 这是 App 侧
  展示策略，命令行参数无法改变——`source="exec"` 是 `codex exec` 通道固有，App 的 `vscode` 来源走的是它内部
  app-server 协议，不是 CLI 能伪装的。
- 附带注意**版本割裂**：PATH 里的 `codex`（codeloop 实际调用）是 `0.130.0`，桌面 App 自带 `0.140-alpha`；
  两者共用同一个 `~/.codex` 目录，但 App 不列 CLI 建的 exec 会话。

**行动建议**：

- 只要 codeloop 跑通（跨会话复核），**无需任何改动**——会话有效且可续跑。
- 若确实要在桌面 App 里看到它们：靠改参数/格式做不到。可调研 Codex 是否支持来源覆盖
  （如 `CODEX_INTERNAL_ORIGINATOR_OVERRIDE` 环境变量），但能否绕过 App 的 `source` 过滤需实测，未验证。

### Claude（已验证，须在会话原始 cwd 下执行）

```powershell
# 在该 Claude 会话的原始 cwd 下：
claude -p "<prompt>" --resume <claude_session_id> --permission-mode acceptEdits
```

- 结果：exit 0，原始 cwd 下能 resume，`-p` 阻塞到完成，stdout 为**干净 UTF-8 纯文本**（实测 `OK\n`）。
- 解析（已固化）：取 stdout 纯文本 trim。若改 `--output-format stream-json` 需同步改 `parse_claude_stdout`。
- 注意：`-p` 无管道输入时 stderr 会有「no stdin data received in 3s, proceeding」warning，不影响结果；
  driver spawn 不接 stdin（如需可显式 `< NUL`）。

### 重要修正

固化实测发现 driver 原 `parse_codex_stdout` 假设的 `task_complete.last_agent_message` **不出现在
`--json` stdout**（那是 rollout 文件字段）。已改为优先解析 `item.completed.item`，并补真机序列回归单测
（`parse_codex_real_json_schema` / `parse_codex_item_completed_wins_over_legacy`）。

### 验后回归（建议）

可再跑一次最小真机循环（小 target、`max_rounds=1~2`）验证：状态条轮次推进、VERDICT 解析、
PASS / MaxRounds 终态、ASK_USER 弹窗与应答 resume。本次仅固化了单轮 send/解析，未跑完整循环。
