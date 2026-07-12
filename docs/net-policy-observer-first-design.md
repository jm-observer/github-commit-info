# 网络出口策略：观察者优先 + 生命周期 + 防火墙跟随姿态

> 承接 [net-policy-validation-report.md](net-policy-validation-report.md)（真机验证结论）与
> [unified-desktop-shell-design.md](unified-desktop-shell-design.md) §14（原始设计）、
> [net-policy-ui-redesign.md](net-policy-ui-redesign.md)（信息架构重排）。本文记录**行为模型的一次转向**：
> 从「默认走海外 + 强依赖 SBN」改为「**观察者优先** + 姿态可配 + 与 SBN 解耦」，以及随之定下的
> **关闭/停止语义**与**防火墙跟随姿态**。仅 Windows。

## 1. 理念转变

旧模型：配好 WireGuard → 应用 → 未命中流量默认走海外，防火墙 kill-switch 常挂。问题：强依赖 SBN
（没配 WG 连启动都不行）、默认就接管全部流量、观察/测试门槛高。

新模型：**先当观察者，再逐项管控，最后才收紧**。用户旅程是一根连续的旋钮，从左往右拧多少收多少：

| 姿态 (`default_route`) | mihomo 兜底 `MATCH` | 含义 | kill-switch |
|---|---|---|---|
| **直连·观察**（默认，`Direct`）| `DIRECT` | 引擎在路里但原样直连，只看不改 | **不挂** |
| **海外·全VPN**（`Wg`）| `wg-out` | 未命中规则的流量走 WireGuard | 挂（fail-closed）|
| **阻断·收紧**（`Blackhole`）| `REJECT-DROP` | 默认拒绝，只放行白名单 | 挂（fail-closed）|

`default_route` 缺省为 `Direct`（`config.rs::default_default_route`）。**观察必须让流量流经 TUN**（否则拿不到
域名级 host↔出口关联），所以观察不是零接触——功能透明，但存在一个 TUN 网卡 + fake-ip DNS。

## 2. 与 SBN(WireGuard) 解耦

- WG 变为**可选**：`generate_config` 仅在 WG 合法时输出 `wg-out` proxy，否则 `proxies: []`；纯直连/黑洞
  也能起 TUN。
- 校验按需：`NetPolicySettings::validate` 只在 `default_route==Wg` 时强校 WG；`validate_combined` 再查
  「有规则/默认出口指向海外但 WG 缺失」的跨表一致性，给可读报错而非起栈超时。
- 防火墙 `validate_fw_inputs` 允许空 WG server（kill-switch 放行的是 mihomo.exe 整进程，不用 WG IP）。

## 3. 生命周期：启动即生效 + 关闭/停止语义

**核心决定：关窗/崩溃 = 保持；只有软件里手动「停止」才拆除。**

| 事件 | mihomo 进程 | 路由/防火墙 | 下次启动 |
|---|---|---|---|
| 关窗 / 崩溃 / 强杀 | 保持运行 | 保持 | `setup()` 重开**接管**（还活着，读 `generated/config.yaml` 的 secret 重连）或**自动恢复**（`enabled=true` 且已死 → 后台 `do_apply`）|
| 手动「停止」/「紧急停止」 | 优雅拆除（API 关 TUN→确认 Meta 拆→按 pid/名结束）| 撤除还原 | **不再自动恢复** |

- `enabled`（`settings.enabled`，默认 false）= 「用户想让策略常驻」的持久意图。开启后每次启动自动恢复。
- **手动停止清 `enabled`**：`do_stop` 在拆除成功后把 `enabled` 落成 false，使「紧急停止」与「主开关关闭」
  一致——手动停 = 停且下次不自动重来。关窗/崩溃**不走** `do_stop`，故 `enabled` 保留、下次接管/恢复。
- 为什么不在关窗时拆：真机验证（§0.8.2bis）证明**硬杀 mihomo 会残留 wintun 路由导致断网**，安全拆除需
  「API 关 TUN → 轮询确认 Meta 消失 → 才结束进程」，要数秒；崩溃/强杀当场没机会跑，所以保持 + 下次补救
  才是安全选择。

## 4. 防火墙 kill-switch 跟随姿态

不变式：**kill-switch 在 ⟺ `applied && killswitch_enabled && default_route != Direct`**。

- **观察(直连)不挂防火墙**：直连时没有拦任何东西，kill-switch 无安全意义，且会把全局
  `DefaultOutboundAction=Block`——万一孤儿 mihomo 自己挂了会**砖网**。不挂只消除「防火墙砖网」这一层。
  > ⚠️ **观察姿态并非崩溃免疫**：§3 的真机结论（硬杀 mihomo 会残留 wintun 路由导致断网，§0.8.2bis）
  > 对观察姿态同样成立——mihomo **异常崩溃**（非优雅退出）时 `strict-route` 的路由可能残留，
  > 即使没有防火墙，流量也未必"自动回到正常直连"，可能需要重启应用（`setup` 自动恢复/接管）或
  > 手工清理才能恢复。「崩溃后 wintun 适配器/路由是否随进程消失」尚未真机验证，在验证通过前
  > 不应宣称观察模式对引擎崩溃完全透明。
- `do_apply`：`killswitch = settings.killswitch_enabled && default_route != Direct`；观察姿态还会**主动撤掉**
  上次会话残留的 Block 规则（异常退出未清），保证不变式处处成立。
- `net_policy_reload`：姿态切换的热路径（前端改 `default_route` = 存设置 + reload）。reload 在热载 mihomo
  配置后**按姿态对齐防火墙**：切到阻断/VPN 补挂 `apply_base`+`apply_tun`、切回直连 `remove`。顺序安全：
  mihomo 配置先 reload 成新 `MATCH`（已按新规则丢/放），再动防火墙，无泄漏窗口。

## 5. 逐项放行与热加载

- 规则模型：`Route` ∈ {Direct, Wg, Blackhole}；`Rule` = ProcessPath/ProcessName/DomainSuffix/IpCidr + route。
- 加/删规则、改姿态都走 `net_policy_reload`（mihomo `PUT /configs` 原地热载，不重启隧道、不断流）。
- **姿态约束**：黑洞/海外姿态强制 `block_ipv6=true`（settings 校验拒绝关闭）——mihomo `ipv6:false`
  时 TUN 不接管 v6 路由，v6 的封锁完全靠防火墙 KS-IPv6Block；关掉它 IPv6 公网会绕过策略直接出物理
  网卡。观察姿态下 v6 流量同样不经 TUN，**观察面板对 IPv6 是盲的**（已知限制）。
- **操作互斥**：apply / stop / reload 经模块级 `ops` 锁串行——它们都会写 `generated/config.yaml`、
  动防火墙、改运行态,并发交错(如 apply 等 TUN 起栈的 ~7s 窗口里穿插 reload)会交叉覆盖。
- **失败回滚的 fail-closed 取舍**：受控 reapply(此前已受保护)失败时**保留** kill-switch(宁断网
  不静默放开),前端呈现"防火墙残留"危险态,用户用「紧急停止」显式解除;全新 apply 失败才撤半装
  防火墙回基线。
- 观察主表每行可直接改路（域名→DOMAIN-SUFFIX、IP→IP-CIDR 补 /32·/128）；已有规则的目标改路会**先删旧
  规则再加**，避免 mihomo 首条命中导致改路静默失效。

## 6. 可观测性

- **观察主表**（前端）：把 `net_policy_connections` 的活跃连接按 host/IP 聚合 + 叠加规则（pinned 置顶），
  每行显示 目标 / 解析IP / 进程 / 当前出口 / 改路下拉——看与管同行。
- **被阻断 feed**：默认黑洞下 REJECT 连接瞬关、`/connections` 抓不到，故常驻消费 mihomo `/logs`
  WebSocket，解析 `using REJECT*` 的行入环形缓冲（`observe.rs`），每条一键放行。
- **域名↔IP/进程 关联**：累积历次活跃连接（`Observatory::ingest_connections`），命中次数按
  mihomo 连接 ID 去重（同一持续连接跨 3s 轮询只计一次）。
- **已知限制（内存态）**：被阻断 feed（环形 200 条）与域名关联（600 域名）都只存内存，应用重启即
  清零；长时间观察→批量整理放行清单的工作流需要时再落 SQLite（暂不做）。
- **fake-ip 与 IP 规则的陷阱**：fake-ip 模式下带域名的流量按域名匹配，`IP-CIDR,…,no-resolve`
  只对程序**直连裸 IP**（不经 DNS）的流量生效。观察主表对有域名关联的行应引导用 DOMAIN-SUFFIX
  改路；纯 IP 行才用 IP-CIDR，并在 UI 提示其生效范围。

## 7. 前置条件与落地约定

- **必须管理员**：改全局防火墙 + 建 TUN 网卡都要提权。`win::is_elevated()` 前置检测，未提权在 apply 前
  给可读错误 + 前端禁用「开始观察」（`status.elevated`）。
- **mihomo 二进制**：放 `<workspace>/net-policy/mihomo-windows-amd64.exe`（**一层**，该目录开机由
  `ensure_workspace` 自动建），或设 `MIHOMO_BIN`。官方 MetaCubeX/mihomo，Windows 取 `-compatible` 变体。
- **workspace**：`resolve_workspace` 统一走 `custom_utils::args::workspace(arg, APP)` →
  `$HOME/.config/<app>`（`-w` / `ZERO_DESKTOP_WORKSPACE` 优先）。
- **配置文件**：mihomo 以 `-d <net-policy> -f <net-policy/generated/config.yaml>` 启动（只给 `-d` 会去
  `<dir>/config.yaml` 找不到 → 跑默认配置 → 控制器/TUN 都不对）。

## 8. 与旧文档的关系

- 旧「默认走海外 + 常挂 kill-switch」的表述（`unified-desktop-shell-design.md` §14 / 模块头注释）以本文
  为准更新为观察者优先。
- 真机拆除顺序、防火墙白名单模型（KS-mihomo/LO/LAN/TUN/IPv6Block）等底层结论仍以
  `net-policy-validation-report.md` 为权威源，本文不重复。
