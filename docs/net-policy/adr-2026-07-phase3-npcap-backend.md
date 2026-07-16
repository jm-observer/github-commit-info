# ADR：net-policy 抓包 Phase 3 后端（npcap vs 维持 pktmon）

- 状态：**已评估——暂不采用 npcap，维持 pktmon（MVP 决定，可后续按需重开）。**
- 关联：[net-policy-capture-design.md](net-policy-capture-design.md) §4（引擎选型）、§15 Phase 3（可选 ADR）、
  §16（已决策/待决）。
- 日期：2026-07-16。

> 设计 §15 把 Phase 3 定为「可选 ADR：npcap 后端与 BPF/PID 精细过滤 / 分段抓取或实时预览 / pcapng
> 自定义 block」。本文即该 ADR，给出「是否引入 npcap」的决策记录，**不新增依赖、不写代码**。

## 1. 背景

Phase 2a 已用 Windows 内置 pktmon 落地全 TUN + 定向抓包（真机 spike 见
[validation-report](net-policy-capture-validation-report.md)）。pktmon 的已知限制（设计 §4/§5.2）：

- 过滤器**不区分源/目的**、无 BPF、无 PID 过滤；同时最多 32 条。
- 定向抓包是「开始时按当前连接端点冻结的快照」，不是按 PID 的持续内核过滤。
- 运行期热更过滤器行为未验证，MVP 冻结。

npcap（WinPcap 继任者）能提供 BPF 表达式、更接近 libpcap 的语义，理论上支持 PID/方向/布尔表达式过滤。

## 2. 选项

| 选项 | 优点 | 代价/风险 | 
|---|---|---|
| **A. 维持 pktmon（现状）** | 零新增依赖、系统内置、无驱动分发/许可问题；已真机验证 | 过滤能力弱（32 条、不分源目的、无 PID/BPF） |
| B. 引入 npcap 后端 | BPF 表达力、PID/方向过滤、实时流式预览可行 | **需安装内核驱动**（分发 + 许可评估：npcap 商业分发需授权）；与 mihomo Wintun 驱动共存需真机验证；供应链 + 卸载复杂度上升；仓库规约禁止未经用户同意新增依赖 |

## 3. 决策与理由

**维持 pktmon，暂不引入 npcap。** 理由：

1. **需求未达门槛**：本产品抓包定位是「从观察表一键定向快照 + 全 TUN 短抓 → 导 Wireshark」，pktmon 的
   端点级过滤已覆盖主用例；BPF/PID 精细过滤是「nice-to-have」，当前无强需求。
2. **依赖与许可成本高**：npcap 是内核驱动 + 有商业分发许可条款，属「未经用户明确同意不新增依赖」规约
   的典型对象；引入需单独许可评估、签名/分发、与 Wintun 共存真机验证、卸载清理——成本远超收益。
3. **可回退**：pktmon 后端与 core DTO（`CaptureTarget`/`CaptureEndpoint`/`plan_filters`）已把「解析端点→
   过滤器」抽象出来；未来若确需 BPF/PID，可在 `net-policy-agent::capture` 加 `NpcapBackend` 与 pktmon 并存，
   不动 core 协议——重开本 ADR 即可，无沉没成本。

## 4. 重开条件（何时再评估 npcap）

- 出现「必须按 PID 精确、持续过滤」或「运行期动态跟踪新连接不丢包」的硬需求；
- 或需要实时 Wireshark 流式预览（pktmon 的 circular ETL + 事后 etl2pcap 无法满足）；
- 且用户明确同意引入 npcap 依赖并接受其许可/分发约束。

届时按 §4/§18 的依赖 ADR 规格（版本锁定、许可、资产、CVE、签名、卸载）补写，并真机验证与 Wintun 共存。

## 5. 后果

- 正面：抓包保持零新增依赖、系统内置、易分发；MVP 范围清晰。
- 负面：BPF/PID/方向过滤与实时预览暂不提供；定向抓包保持「开始时端点快照」语义（UI 已明示）。
