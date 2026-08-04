# 远程命令执行 / 远程排查底座设计（remote-exec）

给独立 `toolkit-worker` 增加远程执行 PowerShell 的能力。worker 主动连接公网 controller
（G10 `toolkit-server`），因此对方机器位于局域网或 NAT 后也能接受排查命令并回传结果。

首个消费场景是交易模拟项目的远程问题排查。设计分两期落地：

- **第一期：可信环境下的同步远程排查闭环。** 只做 Windows/PowerShell、单任务、同步 `/run`，优先尽快可用。
- **第二期：可靠任务与服务模式安全闭环。** 增加异步任务、排队、取消、本地控制面、重启与失联收敛。

两期都坚持 exec 与现有 egress 通道使用独立凭据、独立路由和独立审计。第二期在第一期协议上扩展，
不推翻第一期实现。

---

## 1. 目标与边界

### 1.1 目标

- worker 主动出连，保持 NAT 友好，不要求对方开放端口。
- operator 下发 PowerShell 脚本，获得 stdout、stderr、退出码和执行耗时。
- 默认禁止远程执行，必须由对方显式开启。
- worker 身份、我方 operator 身份和审计归属均可验证。
- 脚本超时后杀死进程树，输出和请求体都有硬上限。

### 1.2 非目标

- 远程桌面、通用 RMM、横向扫描和端口转发。
- 交互式 PTY、持久 shell 会话和流式终端。
- 第一阶段不承诺异步查询、远程取消、跨 controller 重启恢复或精确投递。
- 文件上传/下载不属于本设计两期的必选范围，确有需求时单独设计。

### 1.3 适用范围

第一期只用于团队成员或紧密合作方的可信 Windows 机器，并要求 worker 前台运行。半信任外部机器、
后台服务模式和“对方随时能看到并拔线”的场景，必须等第二期本地控制面完成后再开放。

---

## 2. 总体架构

复用现有 worker 主动出连、长轮询和断线重连模型，但不复用 egress 的消息通道和共享 token。

```text
operator                 controller                     worker
   |                         |                             |
   | POST /api/web/exec/run  |                             |
   |------------------------>|                             |
   |                         |<-- GET /exec/next ----------|  长轮询
   |                         |--- ExecRequest ------------>|
   |                         |                             |  PowerShell
   |                         |<-- POST /exec/result --------|
   |<-------- ExecOutcome ---|                             |
```

代码边界：

- `worker-core`：执行脚本、捕获输出、超时杀树、临时文件和本地审计。
- `toolkit-worker`：注册、心跳、拉取任务、回传结果和命令行参数。
- `toolkit-server`：exec 路由、鉴权、同步任务槽、集中审计和凭据管理 CLI。
- exec 调度状态放在独立 `exec_coordinator` 模块，不把任务状态机继续塞入 `egress-pool`；两者只共享稳定的
  `worker_id` 概念。

`zero-desktop` 只保留现有 egress 观测能力，不内嵌高权限执行端。未来若需要 GUI，单独做轻量托盘壳。

---

## 3. 两期范围总览

| 能力 | 第一期 | 第二期 |
|---|---|---|
| 平台与 shell | Windows + PowerShell | 按需求增加 Linux/bash |
| 调用方式 | 同步 `/run` | `/submit` + `/result`，保留 `/run` |
| 每 worker 并发 | 1；忙时拒绝 | 有界队列，可配置并发 |
| 投递状态 | `completed/timed_out/spawn_failed/unknown` | 完整任务状态机和 at-most-once 语义 |
| 中止 | 超时、worker 前台 Ctrl+C | operator cancel + 本地 stop/pause/resume |
| 运行方式 | 前台 | 前台 + systemd/后台服务 |
| 重启与失联 | 当前 `/run` 返回 `unknown` | instance 感知、reaper 和可查询终态 |
| 审计 | controller + worker，默认不留正文 | 增加留存、轮转和本地实时观察 |

---

## 第一期：同步远程排查闭环

### 4. 第一期安全模型

#### 4.1 显式开关

- worker 只有带 `--allow-exec` 才注册 exec 通道。
- 未开启时完全不请求 exec 端点。
- controller 对未注册 exec 能力的 worker 返回 `404 worker_not_exec_capable`。

#### 4.2 per-worker 凭据

controller 的 `toolkit.db` 新增：

```sql
CREATE TABLE IF NOT EXISTS exec_worker_creds (
    worker_id   TEXT PRIMARY KEY,
    secret_hash TEXT NOT NULL,
    salt        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    revoked_at  INTEGER
);
```

该表加入 `DDL_V1`，不 bump `SCHEMA_VERSION`，与仓库现有“纯新增表”的迁移约定一致。

- secret 使用安全随机生成的至少 32 字节高熵值。
- 存储 `sm3(salt || secret)` 的 hex；`salt` 为随机 16 字节。复用 workspace 已有 `sm3`，不新增依赖。
- worker 通过 `--exec-secret-file` 读取明文，禁止把 secret 放进命令行参数。
- secret 文件要求 Unix `0600` 或 Windows 仅当前用户可读的 ACL。
- 内部 exec 请求都带 `x-worker-id`、`x-exec-secret`、`x-instance-id`。
- controller 每次请求都查询凭据库；已吊销或不匹配统一返回 `401`。

管理命令遵守项目 stdout 契约，只输出一行紧凑 JSON：

```text
toolkit-server exec-cred add --worker-id <id>
toolkit-server exec-cred revoke --worker-id <id>
toolkit-server exec-cred list
```

`add` 只在首次创建时输出一次明文 secret，由 operator 通过带外方式交付。

第一期的 `revoke` 表示阻止该 worker 后续领取任务和回传结果，**不是正在执行命令的 emergency stop**。
需要立即停止时由对方在 worker 前台按 Ctrl+C；远程可靠停止在第二期实现。

#### 4.3 operator 鉴权

`/api/web/exec/*` 只认专用 `TOOLKIT_EXEC_TOKEN`，不接受 `TOOLKIT_API_TOKEN` 代替。支持多枚
`token → operator` 映射，operator 由 controller 根据命中的 token 注入，调用方不能在 body 中指定。

- 未配置 exec token 时，不挂载 `/api/web/exec/*` 路由。
- 已配置但请求缺少或使用错误 token 时返回 `401`。
- exec Router 与当前全局 `require_token` 分层装配，避免同时要求两个 token。

#### 4.4 传输安全

启用 exec 的 worker 只接受 `https://` controller。开发测试仅允许 `127.0.0.1` 或 `localhost` 使用 HTTP。

---

### 5. 第一期协议

#### 5.1 内部端点

| 端点 | 用途 |
|---|---|
| `POST /api/internal/exec/register` | 上报 `worker_id`、`instance_id` 和 PowerShell 能力 |
| `POST /api/internal/exec/heartbeat` | 执行长命令期间维持在线状态 |
| `GET /api/internal/exec/next` | 长轮询领取同步任务 |
| `POST /api/internal/exec/result` | 回传任务终态 |

`register` 后 controller 记录当前 `instance_id`。其他内部端点除校验 secret 外，还必须校验请求的
`x-instance-id` 等于当前实例；旧实例返回 `409 stale_instance`。

#### 5.2 消费端点

| 端点 | 成功码 | 用途 |
|---|---|---|
| `GET /api/web/exec/workers` | 200 | 只列在线且允许 exec 的 worker |
| `POST /api/web/exec/run` | 200 | 同步执行一个 PowerShell 脚本并等待结果 |

`/run` 请求：

```jsonc
{
  "worker_id": "worker-a",
  "script": "Get-ChildItem",
  "args": [],
  "cwd": null,
  "env": {},
  "timeout_secs": 60,
  "stdout_limit_bytes": 1048576,
  "stderr_limit_bytes": 1048576
}
```

controller 生成不可复用的 server `id`，并注入 operator：

```jsonc
{
  "id": "server-uuid",
  "operator": "alice",
  "shell": "powershell",
  "script": "Get-ChildItem",
  "args": [],
  "cwd": null,
  "env": {},
  "timeout_secs": 60,
  "stdout_limit_bytes": 1048576,
  "stderr_limit_bytes": 1048576
}
```

worker 回传：

```jsonc
{
  "id": "server-uuid",
  "state": "completed",
  "exit_code": 0,
  "stdout": "...",
  "stderr": "",
  "stdout_truncated": false,
  "stderr_truncated": false,
  "duration_ms": 1234,
  "error": null
}
```

对 operator 统一投影为：

```jsonc
{
  "state": "completed",
  "source": "worker",
  "id": "server-uuid",
  "exec": { "...": "ExecResponse" },
  "reason": null
}
```

#### 5.3 同步任务槽

第一期每个 worker 只有一个任务槽，不建设通用队列和持久结果表：

- worker 不存在或未注册 exec：`404 worker_not_exec_capable`。
- worker 心跳超时：`409 worker_offline`。
- 任务槽已有任务：`409 worker_busy`。
- 请求超过硬上限：body 返回 `413`，字段返回 `422`。
- controller 将任务放入槽并等待 worker 领取，领取等待默认 30 秒。
- worker 领取后，controller 最多等待 `timeout_secs + result_grace`。
- 成功回传 `completed/timed_out/spawn_failed` 时，`/run` 返回 `200` 和 `ExecOutcome`。
- 领取前超时返回 `504 not_picked_up`，同时只清理属于该 `id` 的槽位。
- 已领取后 worker 失联或 controller 无法确认结果，返回 `502 unknown`，明确提示“命令可能已执行，禁止自动重试”。

任务槽的放入、领取、结果校验和按 `id` 清理必须原子化。`exec/result` 必须同时匹配 `id`、worker_id 和
instance_id，避免其他 worker 或旧实例提交结果。

第一期是单次同步调用语义，不提供 `request_id` 幂等，也不宣称跨 controller 重启恢复。controller 重启后，
调用方只能得到连接失败或 `unknown`。

---

### 6. 第一期执行语义

#### 6.1 PowerShell 启动

- `script` 原样写入带 BOM 的 UTF-8 `user.ps1`，保证 `param()` 和 `#requires` 仍位于脚本首部。
- `wrapper.ps1` 只负责设置 UTF-8 编码并执行 `& "$PSScriptRoot\user.ps1" @Args`。
- 使用 `powershell.exe -NoProfile -NonInteractive -File wrapper.ps1 <args>`，不拼接 shell 命令字符串。
- stdout/stderr 按字节并发读取，UTF-8 lossy 解码。

#### 6.2 超时与进程树

- 任务独占 `tokio::process::Child`。
- 超时或 worker 收到 Ctrl+C 时，Windows 使用 `taskkill /T /F /PID <pid>` 杀死整棵进程树。
- kill 完成后继续 `wait()` 回收子进程。
- `timed_out` 的 `exit_code` 为 `null`，真实正常退出才填写退出码。

#### 6.3 有界捕获与输入上限

- stdout/stderr 分别有读取阶段的有界缓冲；达到上限后设置 `*_truncated=true`，但继续排空管道防止死锁。
- HTTP body 在 JSON 提取前通过 Axum `DefaultBodyLimit` 限制，超限返回 `413`。
- controller 和 worker 双重校验字段上限。

第一期默认值：

| 项目 | 默认值 | 硬上限 |
|---|---:|---:|
| `timeout_secs` | 60 秒 | 3600 秒 |
| script | — | 1 MiB |
| stdout/stderr | 各 1 MiB | 各 8 MiB |
| args | 0 | 128 项 |
| env | 0 | 128 项 |

另行限制单个 arg、env key/value、cwd 的长度及 args/env 累计字节数。

#### 6.4 临时文件

- 临时目录为 `remote-exec/tmp/<id>/`。
- 目录和脚本文件限制为当前用户可访问。
- 任务结束后立即删除；worker 启动时清扫崩溃遗留目录。
- 路径只使用 controller 生成并校验过的 `id`，不得使用调用方输入拼接目录名。

---

### 7. 第一期审计

controller 和 worker 双写审计。默认记录：

- operator、worker_id、server id、开始/结束时间；
- shell、cwd、脚本 SM3 哈希；
- 状态、退出码、输出字节数、是否截断、duration；
- 鉴权失败、领取失败、结果归属失败和 worker 失联。

默认不记录脚本、args、env、stdout、stderr 正文，因为其中可能包含凭据。审计目录限制为当前用户可访问，
默认保留 30 天并轮转。若未来允许完整正文审计，必须由独立显式开关启用并给出敏感信息警告。

---

### 8. 第一期落地与验收

#### 8.1 实现清单

- `toolkit-core`：增加 `exec_worker_creds` DDL。
- `worker-core`：PowerShell wrapper、有界捕获、超时杀树、临时文件和本地审计。
- `toolkit-server`：`exec_coordinator` 单任务槽、内部/消费路由、专用鉴权、集中审计、`exec-cred` CLI。
- `toolkit-worker`：`--allow-exec`、`--exec-secret-file`、instance id、register/heartbeat/next/result 循环和 Ctrl+C。

#### 8.2 验收标准

- 老数据库迁移后自动创建凭据表，`SCHEMA_VERSION` 不变，重复迁移幂等。
- `exec-cred add/revoke/list` 都只输出一行紧凑 JSON；secret 只在 add 时显示一次。
- 未配置 exec token 时 exec 路由不存在；错误 token 返回 `401`；exec 路由不再要求 `TOOLKIT_API_TOKEN`。
- 非 loopback HTTP controller 在启用 exec 时被 worker 拒绝。
- 未开 `--allow-exec` 的 worker 不注册 exec，`/run` 返回 `404`。
- 正常执行 `Get-ChildItem` 返回 stdout、stderr、退出码和 duration，operator 正确写入双端审计。
- 中文脚本、`param()` 和 args 执行正确且不乱码。
- `Start-Sleep 999` 配 5 秒超时后返回 `timed_out`，并确认没有残留子孙进程。
- 同一 worker 同时第二次 `/run` 返回 `409 worker_busy`。
- 伪造 worker_id、旧 instance 或不匹配任务 id 的结果均被拒绝。
- worker 领取前失联返回 `504`；领取后失联返回 `502 unknown`，不自动重投。
- worker 前台 Ctrl+C 会杀死当前进程树后退出。
- 超大 body 返回 `413`，字段超过上限返回 `422`。

---

## 第二期：可靠任务与服务模式安全闭环

### 9. 第二期范围

第二期解决第一期明确留下的限制：异步调用、排队、远程取消、后台服务、本地实时可见以及各种失联竞态。

新增能力：

- `/submit`、`/result` 和 `/cancel`。
- 每 worker 有界任务队列和可配置并发。
- `request_id` 活动期幂等。
- controller 结果表、任务终态保护和 TTL reaper。
- 独立 `cancel-next` 长轮询通道。
- 本地 `exec-watch`、`exec-stop`、`exec-pause`、`exec-resume`。
- worker/controller 重启、凭据吊销和响应丢失时的显式收敛。
- 后台服务模式；按真实需求增加 Linux/bash。

---

### 10. 第二期任务模型

#### 10.1 存储状态

`ExecTask.state`：

```text
queued | picked |
completed | timed_out | spawn_failed | killed |
cancelled | not_picked_up | unknown
```

- `queued`：已接受但 worker 尚未领取。
- `picked`：controller 已交付，无法证明 worker 是否已经启动。
- `unknown`：任务可能执行过，但 controller 无法确认结果；禁止自动重投。
- 其他状态均为终态。

不设置 `running`：除非新增 worker `started` 上报，否则 controller 无法区分“响应正在传输”和“进程已经启动”。
`accepted` 只用于 API 投影，不存入状态机。

`ExecResultStore` 以 `request_id` 索引：

```text
ExecTask {
  id,
  request_id,
  assigned_worker_id,
  assigned_instance_id,
  state,
  terminal,
  created_at
}
```

`assigned_*` 只在领取时写入，排队阶段不绑定 instance。

#### 10.2 API envelope

`/run`、`/submit` 和 `/result` 统一返回：

```jsonc
{
  "state": "accepted|completed|timed_out|spawn_failed|killed|cancelled|not_picked_up|unknown",
  "source": "worker|controller",
  "id": "server-uuid",
  "request_id": "client-key",
  "exec": null,
  "reason": null
}
```

| 端点 | 说明 |
|---|---|
| `POST /api/web/exec/run` | 入队并等待终态 |
| `POST /api/web/exec/submit` | `202 accepted` |
| `GET /api/web/exec/result/{request_id}` | 未终态 `202 accepted`，终态 `200` |
| `POST /api/web/exec/cancel` | 取消排队或已领取任务 |

`request_id` 由调用方生成并在活动期内唯一；server `id` 永不复用，是结果归属和取消的权威键。

#### 10.3 原子终态保护

- 所有终态转换只允许从非终态原子转入，先到者胜，不可覆盖。
- worker 结果必须匹配 `id + request_id + assigned_worker_id + assigned_instance_id`。
- 相同 worker 对相同 `id` 重试内容完全一致的结果，幂等返回 `200`。
- 迟到结果撞见 `cancelled/not_picked_up/unknown`，或内容与既有结果不同，返回 `409 already_terminal`。

---

### 11. 第二期排队、超时与重启

| 阶段 | 默认期限 | 收敛结果 |
|---|---:|---|
| queued 等领取 | 30 秒 | 删除队列项，置 `not_picked_up` |
| picked 等结果 | `timeout_secs + grace` | 置 `unknown/pickup_lost` |
| worker 执行 | `timeout_secs` | 杀树，置 `timed_out` |
| 结果上传 | 30 秒 | 有限重试，本地记 `result_delivery_failed` |

后台 reaper 负责队列过期、picked 超时、终态 TTL、tombstone TTL 和凭据吊销收敛。

- 同 instance 重注册视为网络重连，保留 queued 和 picked。
- instance 变化视为进程重启：保留 queued，旧 picked 置 `unknown/worker_restarted`。
- controller 重启若仍使用易失结果表，则所有在途任务视为 unknown；结果持久化属于第二期实现时的明确选项。
- 凭据吊销后拒绝所有新请求，queued 置 `cancelled`，picked 置 `unknown/credential_revoked`。

吊销仍不等于可靠杀进程。若目标是紧急停止，应先通过 cancel/pause 确认 kill，再吊销 secret；如果 worker 已失联，
只能报告 unknown，不能声称已停止。

---

### 12. 第二期远程取消

worker 在执行期间保持独立的 `GET /api/internal/exec/cancel-next` 长轮询。

- queued：controller 从队列删除任务并原子置 `cancelled`。
- picked：controller 把 `CancelMsg{id}` 放入该 worker 的有界取消队列。
- 取消队列满时 `/cancel` 返回 `503 cancel_backpressure`，不得静默丢弃。
- worker 执行登记表保存 `id → (pid, cancel_trigger)`，`Child` 仍归执行任务独占。
- worker 用同一把锁原子完成“检查 pending cancel + 注册 cancel trigger”，消除取消先到、进程后登记的竞态。
- kill 成功后 worker 回传 `killed`；无法确认时 controller 只能返回 `unknown`。

`pending_cancel` 和 tombstone 均以不可复用的 server `id` 为键，并有 TTL。

---

### 13. 第二期本地控制面

后台服务模式必须提供对方本地可见和可中止能力，不依赖 prod 日志输出到控制台。

#### 13.1 发现与鉴权

- worker 监听 `127.0.0.1:<随机端口>`。
- 启动时原子写 `remote-exec/ctl.json = {addr, token, instance_id}`。
- 文件限制为当前用户可读；每次启动轮换 token 并覆盖旧描述文件。
- 本地 CLI 连接后同时校验 token 和 instance id。

#### 13.2 本地命令

- `exec-watch`：实时显示 server id、operator、开始时间和正在执行的完整命令；只放内存 ring buffer，不落盘正文。
- `exec-stop [id|--all]`：杀对应或全部进程树。
- `exec-pause`：写持久 paused flag，停止领取新任务。
- `exec-resume`：清除 paused flag，重新注册 exec 通道。

pause 对本机立即生效；通知 controller 是 best-effort。网络可用时，worker 调用已认证的 `/api/internal/exec/pause`，
controller 立即停止派发并取消 queued 任务；网络不可用时，controller 依靠在线 TTL 收敛，不能承诺立即感知。

---

### 14. 第二期验收

- `/submit`、`/result`、`/run` 使用统一 `ExecOutcome`。
- 活动期重复 `request_id` 返回 `409 duplicate_request`。
- queued 超时变为 `not_picked_up`，picked 丢失变为 `unknown`，调用方不会永久等待。
- 取消 queued 任务立即得到 `cancelled`；取消 picked 任务最终得到 `killed` 或明确的 `unknown`。
- cancel 先于进程登记到达也不会漏杀。
- worker 重启后旧 picked 任务置 `unknown/worker_restarted`，queued 可由新实例领取。
- 旧实例持相同 secret 调用 next/result/cancel-next/pause 均返回 `409 stale_instance`。
- result HTTP 应答丢失后，相同结果重试幂等成功；冲突结果不能覆盖终态。
- `exec-watch` 能看到当前任务，`exec-stop` 能精确杀树。
- paused flag 在 worker/service 重启后仍有效；网络断开时 controller 最迟在在线 TTL 后停止派发。
- 凭据吊销后不再领取任务，在途任务收敛为 cancelled 或 unknown，但不错误声称进程已停止。

---

## 15. 两期之外的可选能力

以下能力不应顺带混入上述两期：

- 文件上传/下载；
- 流式 stdout/stderr chunk；
- 持久 shell、交互式 PTY；
- worker × operator allowlist；
- 在线注册码签发；
- Argon2 等口令型 KDF；
- Windows Job Object、Unix 原生 signal API；
- 专职托盘 GUI。

需要其中任一能力时，按实际消费场景单独立项并评估依赖。

---

## 附：与现有 worker 的关系

传输方向、pull 模型和 NAT 友好沿用 [distributed-worker-design.md](distributed-worker-design.md)。exec 是同一 fleet
上的独立命令执行面：共享稳定 `worker_id`，但使用独立凭据、路由、调度状态、背压、审计和中止语义。
