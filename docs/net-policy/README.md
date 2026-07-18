# 网络出口策略（net-policy）文档集

> 系统级网络出口管控：**观察所有连接 → 按 域名/IP/进程 改出口 → 三姿态（直连·观察 / 海外·全 VPN /
> 阻断·收紧）+ 可靠 fail-closed**。仅 Windows。特权执行面（`net-policy-agent`）与瘦客户端 GUI 分离，
> 底座 = mihomo(Clash.Meta) + WireGuard（含 AmneziaWG 混淆） + Windows Firewall。

## 文档地图

| 文档 | 职责 | 何时看 |
|---|---|---|
| [net-policy-observer-first-design.md](net-policy-observer-first-design.md) | **行为模型**：观察者优先、三姿态、kill-switch 跟随姿态、生命周期（关窗保持 / 手动停才拆）、与 SBN 解耦 | 想懂「为什么这么设计」 |
| [net-policy-daemon-gui-split-design.md](net-policy-daemon-gui-split-design.md) | **架构 + 落地实录**：四 crate 边界、版本化命名管道协议、§13 真机验收、§14 独立 GUI、**§15 真机健壮性复盘（6 条 daemon 缺陷 + fail-closed 复测）** | 动 agent/GUI/协议前 |
| [net-policy-validation-report.md](net-policy-validation-report.md) | **真机验证报告**：VP 矩阵、fail-closed 证物、防火墙白名单模型（§0.8.2 = 唯一权威结论表） | 查「某结论有没有真机证据」 |
| [net-policy-ui-redesign.md](net-policy-ui-redesign.md) | **UI 信息架构**：保护横幅状态机、历史结论 vs 实时状态分列 | 改前端展示前 |
| [net-policy-capture-status.md](net-policy-capture-status.md) | **★ 抓包/解密落地状态总表**：Phase 1–4 进度、已落代码、真机验证、未决问题（P-1..P-7）、0.228 现状、下一步。**查「现在到哪了」先看这页** | 每次接手先看 |
| [net-policy-capture-design.md](net-policy-capture-design.md) | **抓包设计**：在 TUN 咽喉位叠加 L2 协议嗅探 + L3 全量/定向 pcap（pktmon 内置引擎、进程/域名一键定向、导 Wireshark）+ §17–18 L4 HTTPS 解密（独立安全闸） | 做抓包功能前 |
| [net-policy-capture-validation-report.md](net-policy-capture-validation-report.md) | **抓包 Phase 0 真机 spike 报告**（0.228）：pktmon→pcapng 管道验证（filter/start/stop/etl2pcap，pcapng 魔数合法），§15 验收矩阵状态 | 查抓包真机证据 |
| [adr-2026-07-phase3-npcap-backend.md](adr-2026-07-phase3-npcap-backend.md) | **抓包 Phase 3 ADR**：暂不采用 npcap、维持 pktmon（需求未达门槛 + 驱动/许可成本）；含重开条件 | 想给抓包加 BPF/PID 前 |
| [adr-2026-07-phase4-mitm-engine.md](adr-2026-07-phase4-mitm-engine.md) | **L4 引擎依赖 ADR**：锁定 mitmproxy 12.2.3（MIT，SHA-256 记录）；§6 记 0.228 真机——Defender 查杀已由安装程序加排除解决，`mitmdump --version` 跑通；引擎部署已进 `install-mitm-engine`。数据面/CA 仍待验 | 动 L4/引擎前 |
| [net-policy-wg-egress-design.md](net-policy-wg-egress-design.md) | **统一出口（Egress）模型**（第一阶段已落地，真机 E2E 未做）：Direct/WireGuard/Proxy 同层，各自 health/probe/reconnect + fail-closed 停用（不隐式回落直连）；前端分开展示「生命周期」与「当前策略」。**注意 §12.1 的定性：本阶段是「独立健康状态管理」而非「独立出口生命周期」——数据面仍由 mihomo 承载，停掉引擎出口即消失，§9 后三条验收点不成立**；§12.2 有逐条对照表，第二阶段主线是把 WG 挪到独立引擎 | 想让 WG 独立常驻 / 看出口状态语义前 |

## 代码模块（四 crate + GUI）

- `net-policy-core`：领域类型 + 配置/规则解析 + mihomo 配置生成 + WG/AmneziaWG 解析（纯逻辑）。
- `net-policy-agent`：**唯一特权副作用所有者**——mihomo / 防火墙 / 观察器 / 长操作 / 安装 + 管道 server。
- `net-policy-client`：命名管道客户端（typed 版本化协议）。
- `net-policy-cli`（`net-policy`）：在线救援 CLI（status / stop / repair / requests / ...）。
- `net-policy-gui`：独立 Tauri v2 桌面 app（瘦客户端，只连管道，不持有特权副作用）。

## 建议阅读顺序

1. **observer-first**（行为模型 / 理念）→ 2. **daemon-gui-split**（架构 + §15 现状与缺陷）→
3. **validation-report**（真机证据）→ 4. **ui-redesign**（前端）。

## 当前现状与关键待办（截至 2026-07-14）

- **能力已通**：三姿态 apply / 热切换、AmneziaWG 混淆穿透被封家宽、DNS 走隧道解析（墙外域名不污染）、
  fail-closed（VP-08/09/10 在当前 Program 放行模型上真机复测通过）。
- **进生产阻塞**（详见 daemon-gui-split §15，按优先级）：
  - **D-4** agent 日志不落文件（debug build 走控制台、计划任务无控制台）→ 崩溃无迹可查（研究前提）；
  - **D-2 / D-3** mihomo 无 watchdog + 崩溃重起撞 9090 TIME_WAIT → 崩溃 / 断网不自愈；
  - **D-1** agent 绑登录会话（`Interactive` 计划任务，非服务）→ 注销 / 断 RDP → agent+mihomo 一起死、
    防火墙残留 → 断网；根治需改 **Windows 服务（Session 0 常驻）**；
  - **D-5** 切姿态偶发崩溃，疑 Mutex 中毒连锁 panic（待 D-4 落地后定位）。

## 关联

- **拆独立 git 仓**（`zero-desktop-firewall`）：规划中，**未落地**（身份改名 / 托盘自启 / `git filter-repo`）。
- GUI 与 zero-desktop 内嵌版**并存**（两份前端，后续删内嵌）。
- 上游语境见 [../unified-desktop-shell-design.md](../unified-desktop-shell-design.md) §14（原始设计）。
