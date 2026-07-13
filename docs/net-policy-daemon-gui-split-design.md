# 网络策略独立：每用户特权 agent + 瘦 GUI（收敛版）设计

> 承接 [net-policy-observer-first-design.md](net-policy-observer-first-design.md)（行为模型：观察者优先 /
> 生命周期 / 防火墙跟随姿态）与 [zero-desktop-firewall-extraction-design.md](zero-desktop-firewall-extraction-design.md)
> （拆分与独立仓规划）。
>
> **本版是一次刻意的收敛（保留核心、砍掉外围）。** 早期草稿把「特权执行面与 GUI 分离」这个正确方向
> 扩张成了「通用网络策略守护平台」（HTTP 风格路由 / loopback 应急面 / 浏览器控制台 / SCM 多用户 / WFP 重写 /
> 独立仓 / 为任意未来前端设计协议）。经架构评审确认：**当前真正需要的不是平台，而是一个可靠、最小、可恢复的
> 每用户 Windows 特权网络代理 + 现有 UI 的客户端适配**。故本文把守护更名为 **`net-policy-agent`**，用**版本化
> typed 管道协议**替代 HTTP 路由，并把 SCM / 多用户 / WFP / HTTP 面 / **独立 git 仓**等**移出主设计、降为未来
> ADR**（§8/§9）——**逻辑边界现在按三 crate + UI 拆，Git 仓暂不拆**。仅 Windows。

---

## 1. 真正要解决的七条需求 → 进程边界不可避免

产品实际需要（取自前置文档）：

1. 系统级**观察**所有网络连接；
2. 按**域名 / IP / 进程**修改出口；
3. 支持**直连 / VPN / 全阻断**三姿态；
4. VPN/阻断姿态下可靠 **fail-closed**；
5. **GUI 关闭后策略继续**；
6. 出错能**安全恢复**，不把机器永久断网；
7. 日常**不反复弹 UAC**。

第 5、7 条合在一起推出一个硬事实：**必须有一个比 GUI 活得更久、拥有管理员权限、负责 mihomo 与防火墙
生命周期的进程。** 因此下面这条进程边界**基本不可避免**——这是本方案唯一真正"不可省"的结论：

```
普通权限 UI  →  受控 IPC  →  每用户特权 agent  →  mihomo / TUN / Windows Firewall
```

## 2. 收敛后的目标架构（三层，不是"守护平台 + 多前端"）

```
┌──────────────────────────────────────────────┐
│ net-policy UI（普通权限）                        │
│ 只负责展示 / 编辑 / 确认；不知道管道或 agent 的存在  │
└───────────────────────┬──────────────────────┘
                        │ 版本化命名管道协议（typed，§4）
┌───────────────────────▼──────────────────────┐
│ net-policy-agent                              │
│ 交互用户身份 + 管理员 token（每用户提权后台代理）    │
│ 单实例 · 计划任务拉起 · 持久意图恢复 · 唯一副作用所有者 │
└───────────┬──────────────────────────────────┘
            │
     ┌──────▼──────┐        ┌──────────────┐
     │ mihomo / TUN │        │ Win Firewall  │
     └─────────────┘        └──────────────┘
```

**为什么叫 agent 不叫 daemon**：`daemon/service` 会把设计一路滑向 `LocalSystem` / 登录前启动 / 多用户 / SCM /
机器级数据。但当前需要的是**一个每用户的提权后台代理**，不是机器级系统服务。`net-policy-agent` 贴合它的
职责与身份模型。

## 3. 代码边界：三个 Rust crate + 现有 UI

比"17 个 HTTP 路由"更贴产品、更好测的边界（**UI 不是 crate**，是现有 React 前端）：

| 组件 | 提权 | 依赖 | 职责 |
|---|---|---|---|
| **`net-policy-core`**（crate）| — | **不依赖** Tauri/axum/管道/服务/GUI | 纯业务库：settings/rules 类型、**校验（= 安全边界，§3.1）**、mihomo 配置生成、状态模型、**operation 状态机**、规则计算。**内含极小 `protocol` 子模块**（线协议类型），供 client/agent 共用而不牵连配置生成等重逻辑。今天 `config.rs`/`valid.rs` + `engine.rs::generate_config` 纯函数下沉 |
| **`net-policy-agent`**（crate）| 是 | core + Windows | 唯一副作用层：起停/接管 mihomo、Windows Firewall、观察器、恢复持久意图、单实例、operation 执行、**命名管道 server**、安装/卸载计划任务。今天 `engine.rs`/`firewall.rs`/`observe.rs`/`process_watch.rs`/`win.rs` + `mod.rs` 执行体 |
| **`net-policy-client`**（crate）| 否 | **仅 `core::protocol`**（不拉全 core）| 普通权限客户端库：连管道、请求/响应序列化、**版本协商**、订阅事件、断线重连。**Tauri 后端与 CLI 都复用它** |
| **UI**（非 crate）| 否 | — | 现有 React 页面**原样保留**，只经 Tauri command 调 client；前端不知 agent/管道存在 |

**搬迁性质（修正初稿"近零改"）**：`config.rs`/`observe.rs` 的逻辑主体确可近零改下沉，**但 `engine.rs::mihomo_bin`
必须改**——它今天从用户可写的 workspace（+ 用户可控的 `MIHOMO_BIN` 环境变量）取 mihomo.exe，在提权 agent 下
是**直接提权路径**（§5 二进制完整性）；`firewall.rs` 的 PowerShell 脚本生成也须当**安全边界**重审（§3.1）。所以
engine/firewall 的**资产定位与输入处理要改，不是纯搬家**。

### 3.1 跨完整性级别的输入边界（安全，不只是业务校验）

agent 以管理员身份**消费用户可写的输入**（settings/rules/WG 配置/workspace 路径/进程路径/网卡名/endpoint/
防火墙参数）——这是一条**跨完整性级别（普通用户 → 管理员）的输入边界**，不是普通配置校验。硬约束：

- **`net-policy-core` 的 validation 是安全边界**，不只是业务校验；**agent 不信任 client 已校验，服务端必须重校**。
- 所有传给 **PowerShell / 命令行 / 文件路径 / Win32 API** 的值必须**结构化处理**（参数化/安全编码），杜绝用户
  数据逃逸成 PowerShell 代码或命令（今天 `firewall.rs::ps_squote` 是起点，须系统化覆盖每个注入点并加测）。
- **禁止经配置传任意命令 / 脚本 / 二进制路径 / DLL 路径**——workspace 只存 JSON/YAML/状态数据，**绝不指定要
  提权执行的程序**。
- 任何路径做**规范化 + 允许目录校验 + reparse point/symlink 防护**（防用软链把"受保护目录"绕成用户可写目标）。
- 配置文件设**大小上限**，防超大输入拖垮提权 agent（DoS）。

## 4. 内部协议：版本化 typed 管道（不是 REST）

只有一个明确客户端（Tauri GUI，外加复用同库的 CLI）。**没有必要**为 curl / 浏览器 / 未来托盘 / 第三方脚本
设计通用 REST 资源模型。命名管道上跑一个**版本化、类型明确的内部协议**即可：

```rust
// 版本化握手：客户端连上先协商 PROTOCOL_VERSION；不兼容 → agent 拒绝写操作并提示升级。
enum Request {
    GetStatus,
    GetSettings,           SaveSettings(Settings),
    ListRules,             SaveRule(Rule),          DeleteRule(RuleTarget),
    GetConnections,        Blocked,   ClearBlocked, DnsMap,
    ListProcessCandidates, ParseWgConf(String),     Verify,
    Apply,                 Stop,      SetEnabled(bool),  Reload,
    SubscribeEvents,       GetCurrentOperation,
}
enum Response { Status(Status), Settings(Settings), Rules(RuleSet), /* …逐 Request 定型… */ Ok, Error { kind, message } }
enum Event    { ApplyProgress(step,name,status,detail), OperationFinished(OperationResult) }
```

要点：

- **覆盖今天全部 17 个操作面**（`get_status` / `connections` / `settings` / `rules` upsert·delete /
  `process_candidates` / `apply` / `emergency_stop` / `set_enabled` / `reload` / `blocked` / `clear_blocked` /
  `dns_map` / `verify` / `parse_wg_conf`）——枚举定义协议的完整面，比 HTTP 动词映射更直接，类型即契约。
- **响应逐 Request 定型**：`Apply`/`Stop`/`SetEnabled`/`Reload` 回 `Status`；`SaveRule`/`DeleteRule` 回 `RuleSet`；
  `SaveSettings`/`ClearBlocked` 回 `Ok`——**与今天前端各命令返回一致，React/TS 侧零改**（GUI 只经 Tauri command
  调 client，见 §3）。
- **错误**：`Error { kind, message }`，`kind` 为稳定枚举（`not_elevated` / `mihomo_unreachable` /
  `operation_conflict` / `wg_missing` / `rule_not_found` / `validation` / `version_incompatible`…）。
- **版本兼容要谨慎（评审点 5，别只"只读可降级"）**：连响应结构都不能可靠解析时只读也不安全。规则：**major 不同
  → 拒绝所有业务请求，只允许最小握手信息交换**；**major 同、minor 不同 → 按 capability 协商，双方只调都声明支持
  的请求**；响应**加字段**兼容，**改/删字段必须升 major**；**不把 Rust enum 的默认 serde 表示当永久线协议**（显式
  定义 envelope 与 tag）。
- **apply 语义（保住 GUI 契约）**：`Apply` 在 agent 内 **spawn 独立 operation** 持锁执行，**client 请求 await 到
  operation 终态才回 `Status`**——**断线只取消这次 await、不取消 operation**。并发：有 apply/stop/reload/enabled
  在跑时再来的**立即 `operation_conflict`（不排队）**。进度经 `SubscribeEvents` 的事件流推 `ApplyProgress`；
  默认链路 `agent operation → 管道事件 → GUI 的 Rust client → Tauri event → ApplyStepper`（末端仍是 Tauri 事件，
  **`ApplyStepper` 今天就 `listen` 它，几乎不动**）。

### 4.1 线协议定稿（enum 只是形态，落地前须钉死这些，评审点 4）

typed enum 选对了，但它**还不是完整线协议**。落地前定稿：

- **帧格式**：`4 字节小端长度前缀 + UTF-8 JSON`（比逐行 JSON 更稳，二进制/换行不敏感）；**设最大帧长上限**（防超大帧）。
- **统一 envelope**：`{ "version": 1, "id": 42, "kind": "request|response|event", "payload": {…} }`；`id` = request ID，
  用于请求↔响应配对。
- **两条连接分工（推荐）**：**请求/响应走一条连接、事件订阅走另一条独立连接**——比在同一连接复用简单，且**避免慢
  事件消费者阻塞普通请求**。
- **并发**：单请求连接**串行**（一问一答）即可满足 GUI；不引入同连接并发多请求的复杂度。
- **operation**：长操作带 **operation ID**；事件与 `GetCurrentOperation` 用它关联。
- **断线事件丢失语义**：事件流**不保证不丢**（订阅期外的事件可能错过）——GUI 重连后**以 `GetCurrentOperation` +
  `GetStatus` 的真实态为准对齐**，不依赖补发历史事件。
- **超时/取消**：客户端超时**只取消这次等待（await）**，**不取消 agent 侧 operation**（与 apply 语义一致）。
- **未知字段/未知 variant**：解析**容忍未知字段**（前向兼容）；遇未知关键 variant 归类为协议错误而非静默忽略。

## 5. 身份与安装：每用户最高权限计划任务

**当前实际环境 = 单机、单用户、用户本身是管理员。** 就按这个定：

- **身份**：登录触发的**「最高权限计划任务，以交互用户身份运行」**。它直接化解身份对齐——`custom_utils::args::workspace`
  解析出的 `$HOME/.config/zero-desktop` 就是桌面用户那份现有 workspace，token/管道 DACL 按当前用户授权名副其实，
  且 mihomo/TUN 在**桌面会话**内起栈（与今天 Tauri 子进程同环境，**无 Session 0 问题**）。
  > **前提写死**：`/RL HIGHEST` 用的是该用户**本身的 elevated token**——**只适用于「目标交互用户=本机管理员」的
  > 单用户环境**。标准用户 + 独立管理员凭据、多用户、登录前生效，都**不在本版范围**（属未来 ADR，§8）。
- **二进制完整性（安全硬前提，唯一真正阻断实现阶段的问题，评审点 1）**：agent 以最高权限自启，**若普通用户能替换
  它执行/加载的任何可执行资产，就是"下次登录以管理员执行任意代码"的持久后门**。这一条**推翻了「engine.rs 近零改
  搬迁」的判断**——今天 `engine::mihomo_bin()` 优先读用户可控的 `MIHOMO_BIN`、否则从**用户可写的 workspace** 取
  mihomo.exe，agent 提权后即「普通用户替换 workspace 里 mihomo.exe → 触发 Apply/自动恢复 → agent 以管理员启动
  被替换的 exe」。硬约束：
  - **所有可执行资产**（`net-policy-agent.exe` / 其 DLL / **`mihomo.exe` / `wintun.dll`**）装到 `%ProgramFiles%\net-policy\`，
    **普通用户只读/可执行、无写权**；任务 Action 用**绝对路径**指向该目录，工作目录也固定在此。
  - **production agent 完全忽略 `MIHOMO_BIN`**（该覆盖只允许在**非安装的前台开发模式**用，且**不得与已安装的提权
    任务共存**）。
  - agent 启动 mihomo **前**，**规范化其绝对路径并校验落在受保护安装目录内**（reparse point/symlink 防护，§3.1），
    否则拒绝启动。
  - **workspace 只存 JSON/YAML/状态数据**，**绝不保存或指定要提权执行的程序/DLL/PowerShell 脚本**。
  - **updater 同时安全更新 agent + mihomo + wintun** 等资产（已提权 agent 执行 + **校验签名/完整性**，非"普通用户把
    新文件丢进待替换目录就换"）。
  - **PowerShell 脚本注入审计**：`firewall.rs` 由 settings/rules/workspace 生成的**每个**注入点（进程路径/网卡名/
    endpoint/CIDR…）必须安全编码，不让用户数据逃逸成 PowerShell 代码——否则 exe 安全了，脚本注入仍借 agent 提权
    （见 §3.1）。
  - **列首要安全验收项**：安装后用普通账户实测**无法替换任一可执行资产 / 无法写安装目录 / 无法经配置注入代码**。
- **单实例**：命名 Mutex + 9090 controller 预检（复用今天 apply 的「9090 已有 controller」诊断），防与已装实例
  抢端口 / 交叉写 config.yaml。
- **崩溃拉起**：`ONLOGON` 只保证登录时起，**不含崩溃重启**；须配 Task Scheduler 的 `RestartOnFailure`（Interval/
  Count）+ `MultipleInstancesPolicy`，并**真机验证「非零退出」与「强杀」是否都触发重启**（通常需任务 XML / COM /
  Task Scheduler API，非 `schtasks` 一行）。这是每用户任务相对 SCM 的已知弱项，靠配置逼近。
- **安装子命令**：`net-policy-agent install / uninstall / run`（`run` = 前台调试）。安装弹一次 UAC。观察姿态默认
  不挂防火墙（`enabled` 默认 false + 观察者优先语义），全新安装零副作用。

## 6. TUN 与诚实定位

**需要 TUN，不可回避。** 判据是需求性质：

| 目标 | 手段 | 需 TUN |
|---|---|---|
| 只想「看谁在联网」 | ETW / WFP 事件 / 系统连接表（侵入性低）| 否 |
| **按域名/进程改出口 + VPN + 全阻断** | **TUN + mihomo 数据面** | **是** |

现有需求含第二类（三姿态 + 逐项改路），故 **mihomo TUN 是合理且必需的数据面**，继续用没问题。

**但定位要诚实**：只要启动 TUN、接管 DNS（`dns-hijack`）、修改路由，**即使当前姿态是 Direct，它就已经是一个
系统级网络控制器**，不是"轻量观察工具"。「观察者优先」是**产品姿态/生命周期的立场**（先只看、再逐项管、最后
收紧），**不是**"无侵入"的技术承诺。产品文案与风险提示须如实反映——这与观察者文档 §2「观察不是零接触」一致，
本文强化之。

## 7. 故障模型（**核心质量闸**）

网络控制软件的质量**不取决于**用了 typed 管道还是 HTTP、SSE 还是订阅帧，而取决于**下列场景是否有确定结果、
且被真机验证**。这张表是本方案的验收骨架——**全绿才算合格，架构再漂亮但这里不确定就是不合格**：

| 故障场景 | 必须定义的结果 | 落地机制（今天已有 / 需补） |
|---|---|---|
| GUI 崩溃 | 策略继续 | agent 独立进程，天然（§1）|
| GUI ↔ agent 断线 | operation 继续，GUI 可恢复观察 | operation 与连接解绑 + `GetCurrentOperation`（§4）|
| agent 崩溃 | 自动拉起；受保护姿态不泄漏 | 计划任务 `RestartOnFailure`（§5）+ 拉起后 `setup` 接管存活 mihomo / 按 `enabled` 恢复 |
| mihomo 崩溃 | VPN/阻断姿态**保持 fail-closed** | 防火墙 `R-mihomo` 白名单：进程没了 → 规则不匹配 → 物理全 Block（观察者文档 §4）|
| apply 半途失败 | 回滚到前态，或明确停在**安全阻断态** | 今天 `do_apply` 的事务化回滚 + fail-closed 优先（reapply 失败保留 kill-switch）|
| 防火墙恢复失败 | **不假装成功**，`repair` **分级**（下）| 有可信快照→精确恢复；无→拒绝猜测，只清本产品规则 |
| 配置损坏 | **不自动应用未知配置** | 今天 `setup` 已「设置损坏拒绝自动应用」；agent 沿用 |
| 协议版本不兼容 | major 差→拒绝所有业务请求；minor 差→capability 协商 | 管道握手版本协商（§4/§4.1）|
| **agent 挂掉/二进制损坏 + 防火墙仍 Block** | **在线救援够不着 → 离线提权救援** | 见下「两级救援」：`net-policy-agent repair-offline`（弹 UAC，最小固定恢复）|
| 用户打不开 GUI | CLI 能 `status`/`stop`；连不上 agent 时 `repair-offline` | 救援 CLI 复用 `net-policy-client`（§3），离线路径独立提权，不依赖 GUI/浏览器 |
| 系统关机 / 注销 | 不依赖危险的强杀清理 | 沿用观察者文档「关窗=保持，仅手动停止才优雅拆」——不在关机时抢拆 |

**两级救援（评审点 2：在线 CLI 在 agent 挂掉时救不了）**——最需要 `repair` 的场景恰是「agent 起不来/二进制损坏 +
防火墙仍默认 Block + GUI 和普通权限 CLI 都连不上 agent」，此时经管道修不了任何东西。故分两级：

- **在线救援**：`net-policy status` / `stop` / `repair` 经管道请求 agent（agent 活着时）。
- **离线救援**：连不上 agent 时的**明确提权恢复入口** `net-policy-agent repair-offline`（弹 UAC），**只做最小、固定
  的恢复动作**：不读复杂业务配置、不启动 mihomo、**只删本产品拥有的防火墙规则**、按已保存快照恢复原始 Profile、
  输出紧凑 JSON、**幂等**。安装时可额外生成一个「恢复网络」快捷方式指向它。

**`repair` 结果必须分级（评审点 6：不能简单等于强制 remove）**——防火墙恢复依赖此前保存的 Profile 快照
（`firewall.rs`「按状态文件还原原 `DefaultOutboundAction`，实测原值 `NotConfigured`，不可盲设 `Allow`」）；快照缺失/
损坏时「回基线」并非总可判定（原态可能是 `NotConfigured` 也可能本就是 `Block`，盲设 `Allow` 会篡改用户原有安全
策略，只删规则不还原 Profile 会继续断网）。故 repair 返回四态之一，**不把所有情况都包装成"已回基线"**：

| 结果 | 含义 |
|---|---|
| `repaired_exactly` | 有可信快照，精确恢复原 Profile |
| `removed_owned_rules_only` | 只清本产品规则，**Profile 未动**（无可信快照时的安全默认）|
| `baseline_unknown` | 缺可信快照，**拒绝猜测**（报危险态，待用户决断）|
| `forced_not_configured` | 用户**显式确认**后的最后手段，强设 `NotConfigured` |

**status 以真实机器状态为准**：agent 重启后内存态丢失，`GetCurrentOperation` 对未见终态的操作返回
`interrupted/unknown`，由 `setup` 按「mihomo 是否在跑 / 防火墙是否 active」重新对齐（复用今天 `recover_interrupted`
+ `compute_status`），**绝不谎报成功/失败**。

## 8. 范围：现在做 / 以后做 / 不做（首版）

| 现在必须做 | 以后可做（未来 ADR） | 首版明确不做 |
|---|---|---|
| 特权 agent 与 GUI 分进程 | DPAPI / Credential Manager 存私钥 | loopback HTTP 面 |
| 每用户最高权限计划任务（§5）| 独立发布 + updater | 浏览器应急控制台 |
| **全部可执行资产（agent/mihomo/wintun）安装目录 ACL + 忽略 `MIHOMO_BIN`**（§5）| 完整 CLI（超出 status/stop/repair）| axum-over-pipe / 通用 REST 语义 |
| 命名管道 DACL + 版本化 typed 协议 + 线协议定稿（§4/§4.1）| WFP 双层（保 fail-closed 的持久基线 + 动态细粒度，§9）| SCM / LocalSystem / 机器级数据 / 多用户仲裁 |
| 跨完整性输入边界 + PowerShell 注入审计（§3.1）| **独立 git 仓**（先拆逻辑边界，仓后拆）| WFP 重写 |
| 单实例 + mihomo 接管/优雅停止 | | **独立 GUI 产品**（首版复用现有 Tauri UI）|
| apply 独立 operation + 冲突策略（§4）| | 为"任意未来前端"提前设计协议 |
| 二进制完整性 / status 以真实态为准（§5/§7）| | |
| **救援 CLI：status / stop / repair + 离线 `repair-offline`**（§7）| | |
| **完整的断线 / 崩溃 / 重启真机验收（§7 全表）** | | |

原则：**把"内部进程边界"过早做成"公共平台契约"是主要过度设计源**。只有一个客户端时，命名管道一个版本化
typed 协议就够；HTTP 应急面会重新引入 token/CORS/端口发现/浏览器攻击面/两套 transport 一致性测试——而"GUI 坏了
还能停"已由救援 CLI 解决。**未来 ADR ≠ 现在并列设计**：把尚不存在的多用户/机器级产品从主设计移出，避免团队
持续为它支付复杂度。

## 9. 备选与已否决（记录，防重复论证）

| 备选 | 结论 | 一句话理由 |
|---|---|---|
| **极薄提权助手**（逻辑全留不提权侧）| 否决 | 「关窗=保持」要求持久化的东西活在提权侧——薄助手省不掉 agent，只把带状态时序（`graceful_stop` 关TUN→确认→杀）劈碎跨进程 |
| **loopback TCP + token 控制面** | 否决（用命名管道）| loopback TCP 任何本地进程可连、套接字层无法按 SID 限权；命名管道 DACL 是 OS 级强制访问控制。token 只防误连/盲调，**不隔离同一用户 SID 下进程**（要更强需代码签名/broker，首版不承诺）|
| **采用 clash-verge-service 生态** | 否决 | 本项目核心资产是「观察者优先」定制行为模型（三姿态/kill-switch 跟随/半残留自愈/被阻断 feed/verify），套别人的服务 helper 要把最独特那层塞进别人生命周期假设，失控风险高。其安装/DACL 细节可借鉴 |
| **WFP 动态 sublayer 替 PowerShell** | 延后（未来 ADR，且须双层）| PowerShell 冷启动慢/规则残留是真痛点；但**裸动态会话随 agent 崩溃删规则 = kill-switch 消失 = fail-open**，与「异常保持阻断」冲突。要用须双层（持久最小阻断基线 + 动态细粒度），且重写 + 重跑白名单验证，风险大，非首版 |
| **SCM 机器级服务 / 多用户** | 延后（未来 ADR）| 当前单用户/用户=管理员，每用户提权任务已足；SCM 连带 ProgramData 迁移 + SID 仲裁 + Session 0 验证，非必要不上 |
| **HTTP-over-pipe / REST 资源模型** | 否决 | 单一内部客户端不需要通用 REST；typed enum 协议更直接、类型即契约、无 DELETE-body/CORS/端口发现之累 |

## 10. 与既有文档

- **[net-policy-observer-first-design.md] / [net-policy-validation-report.md]**：行为模型、真机拆除顺序、防火墙
  白名单模型全部**继承不变**，`net-policy-core`/`net-policy-agent` 就是它们的落地代码原样搬家。§6 的 TUN 诚实
  定位强化其 §2「观察不是零接触」。
- **[zero-desktop-firewall-extraction-design.md]**：拆仓/身份改名规划仍以那份为准，但**独立 git 仓在本版明确
  延后**（§8）——先在现 workspace 内按四 crate 边界拆好、跑通故障模型，再谈物理拆仓。

## 11. 实现状态（本轮落地）

已按 §3 边界落地**三 Rust crate + CLI**（在现 workspace 内，未拆独立仓，符合 §8）：

| crate | 状态 | 内容 |
|---|---|---|
| `crates/net-policy-core` | ✅ 编译 + 15 单测通过 | config/valid/mihomo 配置生成/types/operation/**protocol**（版本化 typed 线协议 + 4 字节长度前缀编解码 + 版本协商 + 错误码）。零框架依赖。 |
| `crates/net-policy-client` | ✅ 编译 | 命名管道客户端：Hello 握手 + 17 操作 typed 方法 + 事件订阅（独立连接）+ 忙时重试。Tauri 后端与 CLI 共用。 |
| `crates/net-policy-agent` | ✅ 编译 + 2 单测 | 移植 engine/firewall/observe/connections/process_watch/verify/win；新增 **paths（二进制完整性/§5.3）+ state（operation 作业追踪 + broadcast 进度）+ ops（apply/stop/reload/enabled 独立作业 + 断线不亡 + 事务回滚）+ security（管道 DACL）+ server（接受循环 + 握手 + 分发 + 事件流）+ install（登录最高权限计划任务 + RestartOnFailure）+ repair（离线提权救援 + 四态分级）**。 |
| `crates/net-policy-cli`（bin `net-policy`） | ✅ 编译 | 救援 CLI：status / stop / repair（在线；连不上引导 `repair-offline`）。 |

**端到端已验证（真机 Windows，未提权）**：起 `net-policy-agent run` → 管道 server 就绪 → `net-policy status`
经 Hello 握手取回完整 `NetPolicyStatus`（原生读注册表防火墙状态）→ `net-policy`（agent 停机时）正确报
`agent_unreachable` 并引导离线救援。IPC/协议/作业模型/状态计算/客户端反序列化整链路通。

**故障模型（§7）落地对照**：GUI 崩溃=agent 独立进程（✅结构）；断线不亡=ops 的 detached 作业（✅结构）；
版本不兼容=握手 major gate（✅）；配置损坏=setup 拒绝自动应用（✅继承）；status 以真实态为准 +
`GetCurrentOperation`（✅）；repair 四态分级（✅）；两级救援 status/stop/repair + repair-offline（✅）。

### 11.1 第二轮评审修复（全部落地 + 编译/e2e 验证）

| # | 评审问题 | 修复 |
|---|---|---|
| 1 | 安装未装 mihomo/wintun → apply 必失败 | `install --mihomo <src> [--wintun <src>]` 提权复制到受保护目录 + **原子校验**（缺 mihomo 不返回 installed）|
| 2 | DACL 用宽泛 `IU` + SD 失败降级 | 改用**当前进程 token 的用户 SID**（SY + user SID，不放 BA/IU）；**SD 构建失败 → server 拒绝启动（fail-closed）** |
| 3 | 新 agent/client 未接入 zero-desktop | **已接**：`net_policy/mod.rs` 重写为瘦客户端桥接（17 command 委托 `net-policy-client`，进度事件订阅 re-emit）；**旧进程内 9 个副作用子文件已删除**（消除双实现）|
| 4 | 未用 custom-utils logger / prod 契约 | agent+cli 接 `custom_utils::logger::logger_feature` + `prod = ["custom-utils/prod"]`（AGENTS.md）|
| 5 | CLI 在线 `repair` 实为 stop | 新增协议 `Request::Repair`（minor→1）；agent 端 `graded_repair` 分级；CLI `repair` 走它、不再等价 stop |
| 6 | 无快照 stop 擅设 NotConfigured | `firewall::remove` 无快照时**只删本产品规则、不动 Profile**；强设 NotConfigured 仅 `repair --force` |
| 7 | Save* 不受 operation 互斥 | `SaveSettings/SaveRule/DeleteRule` 在长操作期间返回 `operation_conflict` |
| 8 | 事件 Lagged 静默丢弃 | 新增 `Event::ResyncRequired`；订阅端落后 → 发此帧并关闭订阅，GUI 以真实态对齐 |
| 9 | 事件订阅不校验响应 | `subscribe_events` 严格校验 Hello + SubscribeEvents 两个响应（版本不兼容立即报错）|

**e2e 复验（真机 Windows，未提权）**：agent 以用户 SID DACL 正常起（fail-closed 未触发=SID 获取成功）；
`net-policy status` 往返；`net-policy repair` 返回分级 `removed_owned_rules_only`（与 stop 区分）；zero-desktop
瘦桥接编译通过。

**仍待真机（需管理员 + mihomo 二进制，本环境无法验证）**：
- apply/TUN/firewall/计划任务安装/DACL 拒绝越权 的**提权真机 e2e**（= §7 全表验收，P0/P1 硬门槛）。
- GUI↔agent 的**运行时联调**（Tauri app + agent 同时起、点击走查）——桥接已编译通过，运行时行为待真机点验。
- 多用户/精确到指定客户端 SID 的 DACL 仲裁属未来 ADR（§8）。

## 12. 新增：记录 / 临时直连 / 路由视图（minor 2 协议）

按用户需求补的一组功能，协议升到 **1.2**（向后兼容加请求）。配置目录改到 **`~/.config/net-policy-agent`**。

| 功能 | 落地 |
|---|---|
| **进程请求记录**（持久） | SQLite `<workspace>/net-policy/net-policy.db` 的 `requests` 表；agent 常驻采样器每 3s 拉 `/connections`、按 mihomo 连接 id 去重写入（含进程名/路径/host/ip/port/出口/规则），保留上限 10 万行、周期 prune。`GetRequests{limit}` 查最近记录。|
| **进程树** | `GetProcessTree`：`Get-CimInstance Win32_Process` → 父子邻接 → 嵌套 `ProcessNode`（防环）。|
| **临时直连（限时应急）** | `SetTempDirect{duration_secs, except}`：把 mihomo 兜底 MATCH 临时改 DIRECT、`except` 进程强制 Blackhole（不让敏感流量在隧道故障时泄漏到直连）；到期定时器自动解除 + reload 还原（`gen` 代次防被后续操作误清）；`ClearTempDirect` 提前解除；`GetTempDirect` 看剩余时间。kill-switch 仍按用户姿态挂（DIRECT 经 mihomo 拨号出物理由 R-mihomo 放行）。|
| **路由视图 + 优先级 + 删除** | `GetRoutes`：`core::routes::effective_routes` 计算有序生效路由（`priority`=匹配顺序、`source`=builtin_lan/temp_except/group/rule/default、`deletable`）；同一函数渲染 mihomo 规则行，保证「看到的=跑的」。删除复用 `DeleteRule`（按 kind+value）。|
| **生命周期记录** | `events` 表 + `GetEvents{limit}`：`agent_start`（setup）/`agent_stop`（Ctrl-C 信号，best-effort）/`policy_applied`（apply 成功）/`policy_stopped`（stop 成功）/`temp_direct_on`·`temp_direct_off`（含 expired/manual）。|

**新增控制面**：core `routes` 模块 + `types`（RequestLogEntry/LifecycleEvent/ProcessNode/RouteEntry/TempDirectStatus）+ `config::TempDirect`；agent `store.rs`（rusqlite）+ `ptree.rs` + state 采样器/temp 态 + ops temp 操作；protocol 7 个新请求；client 7 个新方法；CLI 7 个子命令（requests/events/routes/tree/temp-status/temp-on/temp-off）；zero-desktop 7 个新 Tauri command（已注册）。

**e2e 验证（真机，未提权，无 mihomo）**：events 记录 agent_start；routes 带优先级/来源；**temp-on 后 routes 正确显示 `secret.exe→blackhole` + 兜底 `match→direct`**，temp-off 还原；进程树嵌套正确；SQLite 库自动创建。GUI 的 TS/UI（请求记录表/进程树/临时直连按钮/路由优先级视图）待前端接线（Tauri command 已就绪）。

### 12.1 记录功能评审修复（全部落地 + e2e）

| # | 问题 | 修复 |
|---|---|---|
| 1 [P0] | 临时直连吞掉 reload 失败 → 状态分裂 | temp set/clear **事务化**：reload 失败即回滚 temp 态并返回错误；过期还原改为**带重试的 `expire_temp`**（持续失败记 `temp_direct_expire_gave_up` 危险事件，不谎报已关）|
| 2 [P1] | SQLite 错误被伪装成空/成功 | `recent_requests/events` 返回 `Result`（server 映射 `Error`，不谎报空）；写失败 `log::warn`+dropped 计数；降级到内存库置 `degraded`，经 **`status.record_store_degraded`** 暴露 |
| 3 [P1] | 单查询 5 万行可能超 8MiB 帧 | server 把 `GetRequests/GetEvents` 的 limit **硬上限 1000**（避免编码失败断管道）|
| 4 [P1] | minor 兼容未真正实现 | client 新功能经 **`request_v2` 门控**：`server_version.minor < 2` 即报 `version_incompatible`，不发旧 agent 无法反序列化的帧 |
| 5 [P1] | 新功能未进前端 API | `tauri-client.ts` 补全 5 个新类型 + 7 个 `NetPolicyAPI` 方法（requests/events/routes/processTree/tempStatus/tempDirectOn/tempDirectOff）；UI 组件仍待接 |
| 6 [P2] | 缺清理/隐私 | 加 `ClearRequests`/`ClearEvents`（协议+client+CLI）；status 暴露降级态。retention 可配/暂停记录/DB ACL 列后续 |
| 7 [P2] | 安装原子性不足 | install **先验证所有源资产再动 %ProgramFiles%**（mihomo 源/已存在缺一即 bail，不留半装 agent）。完整"temp 目录 + 原子替换"列后续 |

协议 minor 仍为 2（本轮加的 `ClearRequests/ClearEvents` 是兼容追加）。

## 13. 提权真机验收 + 修复（2026-07-14）

在 Windows 11 真机（内网测试机 192.168.0.228）完成提权 e2e——此前 §7 / §11.3 列为待做的「提权真机验收」全部走通。

**验收路径**：`net-policy-agent install --mihomo <src> --wintun <src>`（提权把 agent/mihomo/wintun 原子复制到 `%ProgramFiles%\net-policy\` + 装登录最高权限计划任务）→ 计划任务起 production agent → zero-desktop 内嵌网络策略页连 agent → apply 三姿态。

| 姿态 | 结果 |
|---|---|
| **观察·直连** | apply 起 mihomo TUN + `MATCH,DIRECT`，**不挂 kill-switch**（observer-first），6 步 ApplyStepper 全绿；LAN 排除（`192.168.0.0/16`）保 RDP/SSH 全程不断。✅ |
| **海外·全VPN** | apply 装 kill-switch（`DefaultOutboundAction=Block` + 5 条 KS 白名单）**fail-closed** + mihomo 全局走 **AmneziaWG 混淆隧道**（curl trace 出口 IP=服务端美国，翻墙 IP 直通）+ LAN 排除保连接。✅ |

GUI↔agent 桥接（命名管道连接、三姿态切换、ApplyStepper 事件流、`parseWgConf` 导入 AmneziaWG 的 `amnezia` 参数全链路）真机通过。

### 13.1 修复的 3 个真 bug

| # | bug | 修复 |
|---|---|---|
| 1 | `store.rs::recent_requests` 的 SQL 用 `\` 续行符，Rust 吃掉换行+前导空格，`outbound,rule` 与 `FROM` 粘成 `ruleFROM` → `requests` 查询 SQL 语法错 | 续行符前补空格（`rule \` + `FROM`） |
| 2 | 单实例残留进程占着命名管道，新 agent 建同名管道被拒 → 启动即「拒绝访问」崩溃；单实例预检只查 9090 controller，没兜住管道名冲突 | 诊断确认根因（预检覆盖管道名 + 友好报错列后续） |
| 3 | **`firewall.rs::base_rules_ps` 的 KS-mihomo `-Program` 用了 `resolve_mihomo_bin` production 分支 `canonicalize` 出的 `\\?\C:\...` extended-length 路径 → `New-NetFirewallRule` 报 HRESULT 0x80070057「应用程序包含无效的字符」→ 海外/阻断姿态首次 apply 装 KS-mihomo 必失败**（观察姿态不装此规则，故一直没暴露；这是提权 production 才踩的真实生产坑） | 传 `-Program` 前剥掉 `\\?\` 前缀（本地盘 `\\?\C:\...` → `C:\...`） |

### 13.2 遗留优化点

- **海外姿态 DNS**：`engine::generate_config`（`mihomo.rs`）生成的 DNS 段用国内 bootstrap nameserver + `remote-dns-resolve:false`，TUN 模式下被墙**域名**在本地解析被污染（纯 IP 如 `1.1.1.1` 秒通、`google.com` 解析失败）。应让被墙域名走隧道内 DNS（如 nameserver `https://1.1.1.1/dns-query#wg-out` DoH-over-tunnel，代理模式已验证有效）。
- **`status.firewall.active` 读取不一致**：`win.rs::native_status`（读注册表）与 `Get-NetFirewallProfile`（CIM）不一致——实际 `DefaultOutboundAction=Block` 时 native 误报 `active:false`（`rule_count` 正确）。

> 真机测试机连接方式见记忆 `net-policy-228-test-machine`；AmneziaWG 混淆隧道穿透验证见 `amneziawg-obfuscation-verified` + `D:\git\toolkit\awg-clients\`（已 gitignore）。
