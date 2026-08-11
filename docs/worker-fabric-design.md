# worker 通用通道底座设计（worker fabric）

把 `toolkit-worker` 从「两条并列的专用长轮询」升级为「**一条通用传输 + 若干上层 channel**」，
并在其上收编 zero-desktop 的部署与健康探测，使「在哪台机器上执行」成为一个可选维度。

本文是 [remote-exec-design.md](remote-exec-design.md) 第二期的**上位规划**：remote-exec 第二期
承诺的「异步任务 / 排队 / 取消 / 本地控制面」在本文中作为 P1 的一部分落地，其协议扩展不推翻
第一期实现。

> 关联：[remote-exec-design.md](remote-exec-design.md)（exec 面，第一期已落地） ·
> [remote-exec-todo.md](remote-exec-todo.md)（第一期遗留） ·
> [distributed-worker-design.md](distributed-worker-design.md)（egress 面）

---

## 1. 现状基线

### 1.1 worker 里其实是两套

| | 连接 | 注册 | 心跳 | 取任务 |
|---|---|---|---|---|
| egress 面 | 独立 | `/workers/register` | 10s，独立 | `GET /egress/next` 长轮询 |
| exec 面 | 独立 | `/exec/register` | 10s，独立 | `GET /exec/next` 长轮询 |

两者只共用**进程**与**稳定 `worker_id`**（`w-<sm3(物理网卡 MAC 排序拼接 + 主机名)[..8]>`）。
凭据、路由、调度状态、审计、中止语义全部独立——这是刻意的，见 §2.2。

### 1.2 zero-desktop 侧的两件事

| | 现在在哪执行 | 依赖 |
|---|---|---|
| 健康探测 | G10 上的 toolkit-server，`GET /api/web/probe`，目标 **host 限死 loopback** | 无，已是最短路径 |
| 一键部署 | **开发机本地**，`pwsh -File <repo>/deploy-g10.ps1`（[g10_deploy/mod.rs:510](../crates/zero-desktop/src/modules/g10_deploy/mod.rs:510)） | 本地源码树、Docker 跨编译镜像、cargo 缓存、scp 密钥 |

注意「部署」并不在 G10 上跑，它是**本地构建 → 推产物**。这决定了它搬到 worker 上的收益场景
与探测完全不同（§4.1 / §4.2）。

---

## 2. 关键决策

### 2.1 不用 SSH 做传输

「远程节点做成 SSH 通道」的诱惑在于 SSH 自带 exec / SFTP / 端口转发三件套。但它与本底座的
根本前提冲突：**worker 在 NAT / 防火墙后，只能主动出连**。在此前提上跑 SSH 只有两条路，代价都不可接受：

| 路子 | 代价 |
|---|---|
| worker 侧起 sshd + 反向隧道 | 等于在对方机器上开常驻可登录入口；Windows OpenSSH server 的配置与 ACL 坑多；端口占用、密钥分发、审批语义全部要另建 |
| controller 侧起 sshd，worker `ssh -R` 打洞 | 隧道进程挂了无人收敛，得靠 autossh 兜；现有心跳 / 在途任务 / 审计模型全部作废 |

更重要的是，**现有控制面是 SSH 给不了的**：临时权限申请 → 桌面端批准 N 小时、per-worker 凭据
到期自动回落申请态、脚本只记 SM3 短哈希不记正文的审计、超时 `taskkill /T /F` 杀树。换成 SSH
等于「用 SSH 替掉传输，再把控制面原样重新长出来一遍」。

**结论：借鉴 SSH 的「一条连接多路复用若干 channel」这个概念，不借鉴 SSH 协议本身。**

### 2.2 合并管道，不合并闸门

「exec 和 egress 能不能合并」要分两层看：

- **可以合并的（传输层）**：连接建立、断线重连、注册、心跳、`worker_id` 派生、自更新。这些
  现在是实打实的两套重复实现，抽成 `worker-transport` 是净收益。
- **不可以合并的（控制面）**：凭据、审批、审计、吊销。理由就写在现有设计里——批准一台机器
  执行任意命令是**高一档的安全边界**，所以桌面端用独立的 `exec_token` 而非 `g10_token`。
  出口面是「借你家网线」，执行面是「在你家机器上跑任意代码」，合并成一套 token 是纯粹的降级。

一句话：**要合并的是管道，不是闸门。**

### 2.3 「执行位置」是新增维度，不是替换

本地部署直连、G10 loopback 探测，各自都是当前场景下的最优路径，**不改默认、不删直连**。
worker 是**多出来的一个执行位置**（runner），默认值保持现状，兼容零成本。

---

## 3. 目标形态

```text
worker ⇄ controller：一条持久连接（WS 或 HTTP/2 双向流）
  ├─ channel: exec      一次性脚本；P1 起支持异步 + 分片流式输出
  ├─ channel: file      上传 / 下载（scp 的位置）
  ├─ channel: forward   TCP 端口转发（ssh -L/-R 的位置）
  ├─ channel: egress    HTTP 代发 —— 本质是 forward 的一个特例，P3 收编
  └─ channel: pty       交互式终端（待定，见 §6）
```

分层职责（沿用现有 crate 边界）：

| crate | 职责变化 |
|---|---|
| `worker-transport`（新） | 连接 / 重连 / 注册 / 心跳 / channel 多路复用与帧编解码。**不含任何业务语义** |
| `worker-core` | 维持现状：执行、有界捕获、超时杀树、本地 JSONL 审计。P1 增「分片输出回调」 |
| `toolkit-worker` | 由「两个循环」收敛为「一个传输 + 若干 channel handler」 |
| `toolkit-server` | 各 channel 的路由与鉴权**保持独立**（§2.2），仅传输侧共用 |

---

## 4. 上层能力

### 4.1 健康探测：解开单机限制

现状 `check_loopback`（[web.rs:80](../crates/toolkit-server/src/routes/web.rs:80)）把目标限死
loopback，等价于**这个面板只能探 G10 一台机器**。今天够用；一旦出现第二台节点上的服务，probe
就扩不了了——而 worker 天然一台一个、天然在对方 loopback 内部。

设计：探测请求带 `runner` 维度。

| runner | 语义 | 状态 |
|---|---|---|
| `local` | 桌面端直连探测 | 已有 |
| `g10`（默认） | 现有 `/api/web/probe` | 已有，不动 |
| `worker:<id>` | 经 worker 在其本机视角探自己的 loopback | 新增（P3） |

今天就把 probe 换成走 worker 是**纯亏**：toolkit-server 本身就在 G10 上，插一跳只是多一个会挂的进程。

### 4.2 一键部署：worker 真正该吃的

用 worker 跑部署，只有当**发起端与构建机不是同一台**时才有意义。今天它俩是同一台，所以现在
接进去是负收益。但这些场景很快成立：

- 人在外面，要触发家里构建机部署一个修复；
- 构建机是专用机器（Docker 镜像与 cargo 缓存都在那），而操作从笔记本发起；
- 部署要变成可被 cron / zero 触发的**能力**，而不是只能人肉点桌面端按钮。

**前置条件很硬**：docker 交叉编译要跑数分钟，面板还要逐行 emit 日志；而 remote-exec 第一期是
同步 `/run` 一发一收。直接搬必然超时。所以顺序不可颠倒——**先有异步任务 + 流式输出，部署才可能上 worker**。

**另一个必须一并解决的点**：registry 里 `repo_dir` 是硬编码的 `D:\git\<repo>` 本地路径
（[registry.rs:102](../crates/zero-desktop/src/modules/g10_deploy/registry.rs:102)）。执行位置
一旦可变，仓库根就必须跟着 runner 走——由 worker 在注册时上报自己的仓库根，或在 registry 中
按 runner 分别配置。

---

## 5. 分期

每期都自成闭环、可独立验收，且不破坏前一期已落地的路径。

### P1 — 传输归一 + 异步执行骨架 ★一切的前置

- 抽 `worker-transport`：一条连接 + channel 多路复用；exec / egress 改为其上的两个 handler，
  **两套 register/heartbeat/长轮询收敛为一套**。控制面凭据仍各自独立（§2.2）。
- exec 由同步 `/run` 扩展为**异步任务**：提交返回 `task_id`，轮询 / 订阅取状态；保留同步
  `/run` 作为短命令的语法糖。
- **stdout/stderr 分片流式回传**，解决长命令跑完才见输出的问题。
- 补齐 remote-exec 第二期的排队与远程取消；`revoke` / 凭据过期从「只阻止领新任务」推进到
  「能中止在途任务」。
- 顺带清掉 [remote-exec-todo.md](remote-exec-todo.md) 的 TODO-2（轮询未判状态码，404 被误报为解析失败）。

**验收**：一条跑满 10 分钟并持续输出的脚本，面板能实时逐行看到、中途能取消、结束能拿到真实退出码。

### P2 — file channel

- 上传 / 下载：分片 + 校验和 + 断点续传（大产物必需）。
- 审计沿用「只记元信息 + 内容哈希，绝不记正文」。
- 路径白名单与穿越防护，参照 audioforge 下载端点已有的 canonicalize 校验做法。

**动机**：目前要往对方机器丢一个二进制或配置，只能在脚本里写 `Invoke-WebRequest`，绕且脆。
这一期成本最低、见效最快，也是 P3 部署搬迁的依赖（推产物）。

**验收**：桌面端向指定 worker 推一个数十 MB 的二进制并校验一致，中断后可续传。

### P3 — 部署与探测接入 runner 维度

- `ServiceDef` 增 `runner` 字段：部署默认 `local`，探测默认 `g10`，**现有行为零变化**。
- 部署支持 `runner: worker:<id>`：复用前端既有的 `g10-deploy://log` emit 通道，只换后端管道。
- 探测支持 `runner: worker:<id>`，解开 §4.1 的单机限制。
- 解决 `repo_dir` 随 runner 变化的问题（worker 注册上报仓库根 / registry 按 runner 配）。

**验收**：从桌面端选定一台远程构建机，完整跑通一次 toolkit-server 的交叉编译 + 部署 + 重启，
日志表现与本地部署一致；面板能同时显示 G10 与该机器上服务的健康状态。

### P4 — forward channel + 收编 egress

- 通用 TCP 转发（`ssh -L/-R` 的位置）。
- 现有 egress「worker 用本机 reqwest 代发 HTTP」降级为 forward 的一个**特例**，
  `egress-pool` 的两原语（`pool.fetch` / `pool.session`）语义不变，仅底层换管道。
- session 绑定的 cookie jar 与「同类型独占、类型间共用」策略保持不变。

**风险提示**：这一期动到已在生产使用的 egress 面，必须能灰度与回滚——保留旧路径直到新路径
稳定，不做一刀切替换。

### P5 — PTY（待定，暂不排期）

交互式终端需要 Windows ConPTY，且与「审计只记元信息不记正文」的既有原则**直接冲突**——
交互式会话没有「脚本正文」这个可哈希的边界。需要先想清楚审计语义再决定是否做。

---

## 6. 非目标

- 不做通用 RMM、远程桌面、横向扫描。
- 不合并 exec 与 egress 的凭据、审批、审计（§2.2）。
- 不把本地部署 / G10 探测的默认路径改成走 worker（§2.3）。
- 不引入 SSH 协议或依赖对方机器开放任何入站端口（§2.1）。

---

## 7. 落地顺序上的硬依赖

```text
P1（异步 + 流式 + 传输归一）
 ├──> P2（file channel）
 │     └──> P3（部署上 worker：需要流式日志 + 推产物）
 └──> P4（forward + 收编 egress）
```

P3 同时依赖 P1 的流式日志与 P2 的文件推送，是三者中最靠后的一环；P4 与 P2/P3 无依赖，可并行。
若只做一件事，做 **P1**——它同时解决「长命令看不到进度」这个当下就存在的体验问题。
