# net-policy 控制台重设计 —— 全局流程视图

> **状态**：设计稿（2026-06-15）。尚未动代码。
> **目标**：把现有平铺式 net-policy 页面改造成「一条可视的流水线」，让用户**全局看清整个出口策略的运行时数据通路与正确性状态**，并让 apply / 验证 / 排障更易操作。
> **关联**：
> - 实现现状：`crates/zero-desktop/src/modules/net_policy/`（后端）+ `crates/zero-desktop/ui/src/modules/net-policy/NetPolicyPage.tsx`（前端）
> - 真机验证依据：[net-policy-validation-report.md](net-policy-validation-report.md)（尤其 §0.8.2 权威结论表、§0.9.2 原始证物、§0.10 实现产物验证）
> - 设计上游：docs/unified-desktop-shell-design.md §14

---

## 1. 现状与问题

现在的 `NetPolicyPage.tsx` 是一串平铺面板：4 个状态灯 → WireGuard 表单 → 分流规则 → 进程发现 → 验证。功能齐全，但**讲不出流程的故事**：

1. **看不到数据通路**。报告里有一条真实链路：
   `本机应用 → TUN(Meta) → DNS 劫持(fake-ip) → 规则引擎 → DIRECT 本地出口 / wg-out 海外隧道 → 物理网卡(kill-switch 围栏) → WG endpoint → 海外出口 IP`。
   UI 里完全没有这条线，用户无法回答「我的流量到底怎么走」。
2. **apply 是黑盒**。后端实际是分阶段的（`apply_base` → `start` → 等待引擎就绪 → `apply_tun`；当前仅固定 6s 后查控制器，**未轮询 TUN**，见 §3.3），但前端失败时只给「应用失败: <err>」，定位不到卡在哪一步。
3. **状态语义微妙却被埋没**。4 个布尔位（`applied` / `protected` / `protection_validated` / `firewall.active`）组合出多个含义不同的状态（不受保护预览 / 已阻断·隧道未连通 / 实验保护 / 受保护 fail-closed），现在散落在几个琥珀色条件框里（`NetPolicyPage.tsx:242-262`）。
4. **没有实时性**。`status` 只在动作后刷新，无轮询、无活的出口 IP、无连接活动。
5. **正确性的全局视图缺失**。报告有 VP-01..12 一整张矩阵 + 证据分级（§0.8.2），app 里只有一次性 3 个 verify case，二者没打通。

---

## 2. 设计主线

把「平铺面板」改成「一条可视的流水线」。页面自上而下：

```
┌─ 保护状态横幅（4 布尔 → 1 个有名字的状态 + 一句话 + 下一步）──── 出口 IP ─┐
├─ 数据通路全景图（节点按实时状态点亮；物理网卡画成 kill-switch 围栏）──────┤
├─ 应用流程分步 stepper ──────────┬─ 验证矩阵 VP（证据分级 + 跑实时检查）─┤
├─ WireGuard 出口 + 设置（保留）──┴──────────────────────────────────────┤
├─ 分流规则（保留，叠加活跃连接聚合，非累计命中）──────────────────────────┤
└─ 进程发现（保留）────────────────────────────────────────────────────┘
```

---

## 3. 功能设计

### 3.1 数据通路全景图（核心新功能）

页面中央放一张实时拓扑图。每个节点用后端状态点亮（绿=正常 / 黄=就绪中或降级 / 灰=未起 / 红=阻断）：

| 节点 | 数据来源 |
|---|---|
| 本机应用 / 连接数 | **需补**：mihomo connections API |
| TUN 网卡 (Meta) | 已有 `status.tun_ready` |
| DNS 劫持 fake-ip | 已有 `verify` 的 dns-hijack case |
| 规则引擎 + 活跃连接聚合 | 规则数已有；**活跃连接需补** connections API（见下方约束） |
| DIRECT 本地出口 / wg-out 海外隧道 双分支 | **需补**：connections 的 `chains` 字段（报告 §0.9.2 已证 `chains=wg-out` 可取） |
| 物理网卡 kill-switch 围栏 + 白名单明细 | 已有 `firewall.rule_count`；**白名单明细需补**一个 list 命令 |
| 海外出口 IP | 已有 `verify` 的 exit-ip case，提到状态横幅展示 |

> ⚠️ **「命中数」只承诺「当前活跃连接按出口/规则聚合」，不承诺累计命中**。mihomo connections API 是当前连接快照；当前 `Rule` 没有稳定 ID（按 `kind+value` 区分），重复规则、规则顺序变化、`MATCH` 默认规则下，按 rule 反推到 UI 列表会不准。
> - 全景图的双分支计数 = 当前连接按 `chains`（DIRECT / wg-out）聚合 —— 这准确可做。
> - 规则列表的「命中」= 当前活跃连接按 route/rule 粗聚合，标注为「活跃」而非「累计」。
> - 若确实要每条规则的累计命中，需引入**规则 ID** 或 mihomo 日志/事件采样设计，列为后续项，本期不做。

**关键画法**：物理网卡画成一个虚线「围栏」框，框内列出**当前模型实际放行的规则**，并显著标注「其它一律 Block」。fail-closed 的含义因此一眼可懂。

**围栏必须反映当前代码的真实规则**（v2.2/v2.3 已从旧 RemoteAddress 白名单换成「程序放行」模型，见 `firewall.rs` `base_rules_ps`）：

| 规则 | 放行内容 |
|---|---|
| `KS-mihomo` | `Program=mihomo.exe` 出物理网卡（覆盖 WG 握手 / 上游 DNS / DIRECT 拨号） |
| `KS-LO` | loopback `127.0.0.0/8` |
| `KS-LAN` | LAN（`lan_ranges`，出物理网卡） |
| `KS-IPv6Block` | 显式 Block `2000::/3`（仅当 `block_ipv6` 开启） |
| `KS-TUN` | TUN 接口（`InterfaceAlias=Meta`）出站，阶段 B 在 mihomo 起栈后补 |
| 默认 | 三 Profile `DefaultOutboundAction=Block` |

> ⚠️ **不要再画 `KS-WGep` / `KS-DNS` / `WG endpoint:port` / `DNS bootstrap:53` 这套旧 RemoteAddress 白名单**——已不在代码里（报告 §0.9.2 用的是旧模型）。
>
> ⚠️ **新「程序放行」模型尚未真机复测 fail-closed**（报告 §0.10.1：`FIREWALL_MODEL_VALIDATED=false`，VP-08/09/10 待复测）。围栏或保护横幅须显著标注「实验保护·待 VP-08/09/10 复测」，不可呈现为已坐实的 fail-closed。

这一张图就是用户要的「全局看整个流程」。

### 3.2 保护状态横幅（4 布尔 → 1 个有名字的状态）

把布尔组合收敛成单一横幅，配色 + 一句话 + 下一步建议：

状态判定**按下表自上而下，先命中先生效**（顺序很重要——危险态优先于「未应用」）：

| 状态 | 触发条件 | 含义 / 下一步 |
|---|---|---|
| **防火墙仍生效·引擎未受管** | `!applied && firewall.active && firewall.rule_count > 0` | 红（高优）。检测到 `NetPolicy-KillSwitch` 规则组仍在 + `DefaultOutboundAction=Block`，但运行态未恢复 `applied`（重启后引擎没起 / secret 失效 / mihomo 不可鉴权，见 `mod.rs` `setup`）。用户实际被 Block 卡住却以为「未应用」。动作：**重新应用** 或 **紧急停止**（撤防火墙回基线）。 |
| 未应用 | `!applied && !(firewall.active && firewall.rule_count > 0)` | 灰。策略未生效，且无 net-policy 残留规则。 |
| 不受保护预览 | `applied && !firewall.active` | 黄。mihomo 在跑但 kill-switch 未开 → 异常时可能泄漏到本地出口。生产请开 kill-switch。 |
| 已阻断·隧道未连通 | `applied && firewall.active && !(mihomo_running && tun_ready)` | 红。fail-closed 成立（不泄漏）但当前**无法联网**。检查 WG/引擎或重新应用。 |
| 实验保护 | `protected && !protection_validated` | amber。kill-switch 生效，但新防火墙「程序放行」模型尚未真机复测 VP-08/09/10（常量 `FIREWALL_MODEL_VALIDATED=false`）。**当前实际就是这个态**（见 §3.1 ⚠️）。 |
| 受保护 · fail-closed | `protected && protection_validated` | 绿。未知流量默认海外；引擎/隧道断开则物理网卡全阻断，不泄漏。 |

`NetPolicyPage.tsx:242-262` 已算出后 5 个态，**但缺第一行「防火墙仍生效·引擎未受管」这个危险态**——需新增判定 + 醒目动作。`status.firewall`（含 `active` 与 `rule_count`）已在返回值里，无需后端改造。

> ⚠️ **务必带上 `rule_count > 0`**。后端 `firewall::status()`（`firewall.rs`）的 `active` 仅取 Domain Profile 的 `DefaultOutboundAction == Block`，**不区分这是 net-policy 装的还是用户/企业本来的默认 Block 策略**。只判 `active` 会把「系统本就默认 Block」误报成「net-policy 残留」。叠加「`NetPolicy-KillSwitch` 规则组存在」(`rule_count > 0`) 才能确认是本功能的残留；若想更稳妥，文案可写「检测到出站默认 Block，可能是 net-policy 残留或系统策略」。

### 3.3 应用流程分步可见

把 `apply` 的真实阶段做成 stepper（pending / running / ok / fail）：

1. 校验配置
2. 装防火墙基线（`apply_base`）—— fail-closed 就位
3. 启动 mihomo 引擎（`start`）
4. 等待引擎就绪
5. 补 TUN 白名单（`apply_tun`）
6. 验证连通

失败时高亮到具体步 + 错误原文 + 修复提示。

> ⚠️ **stepper 不能比后端实际能力更乐观**。当前 `net_policy_apply`（`mod.rs`）第 4 步是 `sleep(6s)` 后**只查一次外部控制器是否可达**（`engine::running`），**并没有轮询 TUN/Meta 起栈**。所以：
> - 若按现状落地，第 4 步应如实写「等待引擎就绪（固定 6s 后查控制器）」，不要写「轮询 TUN 起栈 N/14」。
> - 若要做更可诊断的真·分步（含 TUN 轮询），**需先补后端**：把固定 sleep 改成轮询 `engine::tun_up()` 直到 Meta 出现或超时（`graceful_stop` 已有 14×500ms 轮询 Meta 消失的范式可复用）。

**需后端改造**：`net_policy_apply` 当前是一个 async 命令一次性返回 `NetPolicyStatus`，改为通过 Tauri event channel 逐阶段 emit 进度。

### 3.4 验证矩阵（VP）进 app

把报告 §0.8.2 权威表搬进来，每项 = 名称 + 证据强度徽章（✅实测·有原始证物 / ◑实测·仅摘要 / ▢研究层 / ✗未测）+ 「跑实时检查」按钮（可跑的：出口 IP、DNS 劫持 fake-ip、引擎在线；不可跑的只展示报告结论）。

这是「正确性的全局视图」，与全景图的「运行时全局视图」互补，也把那份很深的验证报告与 app 连起来。初始数据可硬编码自 §0.8.2，后续可让能实测的项覆盖静态结论。

> ⚠️ **不能把 §0.8.2 的 ✅ 直接当成「当前代码模型」的结论**。§0.8.2 的 fail-closed 相关项（VP-08/09/10）是在**旧 RemoteAddress 白名单模型**上取证的；当前代码已换成 Program 放行模型，报告 §0.10.1 明确该新模型**未真机复测**（`FIREWALL_MODEL_VALIDATED=false`）。因此 VP 矩阵需区分两列证据：
> - **报告历史结论**（旧模型，§0.8.2 原值）—— 可展示，但标注「旧模型」。
> - **当前代码模型状态** —— VP-08/09/10 显示为「待复测 / 进生产前阻塞」，不显示 ✅。
> 当 `FIREWALL_MODEL_VALIDATED` 翻成 `true` 后，再让当前列继承 ✅。

### 3.5 实时监控（分两档，重探测不进快轮询）

- **快轮询（3s）**：`net_policy_get_status` + `net_policy_connections`。都是本地查询（防火墙状态 / 控制器连接表），便宜，可高频。驱动全景图节点状态、连接计数、双分支聚合。
- **慢/手动刷新（30–60s，带缓存）**：出口 IP、DNS 劫持等**重探测**。

> ⚠️ **出口 IP / DNS 检查不能进 3s 轮询**。当前 `verify`（`verify.rs`）的 exit-ip 走 `api.ipify.org`、`TimeoutSec=10`；fail-closed 或网络异常时会堆积慢请求，拖垮 UI。出口 IP 改为：手动刷新按钮 + 可选 30–60s 带缓存自动刷新；横幅展示最近一次成功值 + 时间戳。

让用户看到它是「活的」，而非快照——但活数据只来自便宜的本地查询。

---

## 4. 操作便利性增强

- **预演生成物**：CLI 已有 `zero-desktop net-policy-gen --what config|firewall`（报告 §0.10）。接成 UI 抽屉 —— 应用前先看将写入的真实 mihomo config / 防火墙脚本，降低「按下去会发生什么」的恐惧。
- **应用前预检**：管理员权限、适配器名存在、WG endpoint 可达性，先于 `apply_base` 跑；避免「装了防火墙却起不来引擎 → 断网」。
- **紧急停止二次确认**：明确告知「会停引擎 + 撤防火墙 = 回到不受保护」。

---

## 5. 需要的后端补充（按价值排序）

| # | 新增/改造 | 用途 | 优先级 |
|---|---|---|---|
| 1 | `net_policy_connections` —— 代理 mihomo connections API（带 controller secret），返回每条连接的 `chains` / 目标 / 进程 | 驱动全景图双分支聚合与「当前活跃连接」的**活数据**（非累计命中，见 §3.1 ⚠️）；性价比最高 | P0 |
| 2 | `net_policy_apply` 改为逐阶段 emit Tauri event | 驱动分步 stepper（§3.3） | P1 |
| 3 | `net_policy_list_firewall_allows` —— 列白名单明细 | 围栏 chip（§3.1） | P1 |
| 4 | `net_policy_preview { what }` —— 把 CLI 生成逻辑暴露给 UI | 预演生成物（§4） | P2 |
| 5 | `net_policy_precheck` —— 管理员/适配器/endpoint 可达性 | 应用前预检（§4） | P2 |

已有可直接复用的后端：`net_policy_get_status`（含 `FirewallStatus`）、`net_policy_get_settings` / `save_settings`、`net_policy_list_rules` / `save_rule` / `delete_rule`、`net_policy_list_process_candidates`、`net_policy_apply` / `emergency_stop`、`net_policy_verify`（含 exit-ip / dns-hijack / engine 三 case）。

---

## 6. 前端落地约定

- 现有 `NetPolicyPage.tsx` 是单文件 ~400 行。重设计时按 speech 模块的三层约定拆分：
  - `modules/net-policy/api/tauri-client.ts` —— 仿 `SpeechAPI`，集中 `invoke` 包装。
  - `modules/net-policy/components/` —— `FlowTopology.tsx`（全景图）、`ProtectionBanner.tsx`、`ApplyStepper.tsx`、`VerifyMatrix.tsx`、复用现有 `Panel` / `Light`。
  - `NetPolicyPage.tsx` —— 仅编排。
- 技术栈不变：React 18 + Tailwind + Lucide + CSS 变量（深色模式）。
- 全景图用纯 DOM/flex 布局即可（mockup 已验证形态），无需引图库。

---

## 7. 建议实施顺序

1. **全景图 + 保护状态横幅**（视觉冲击最大；除 connections 外数据后端已有）→ 先补 P0 的 `net_policy_connections`。
2. **分步 apply**（后端 event 改造 + 前端 stepper）。
3. **验证矩阵**（先静态搬 §0.8.2，再接可实测项）。
4. **预演 / 预检 / 实时轮询**等便利项。
