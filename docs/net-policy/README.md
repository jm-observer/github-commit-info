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
