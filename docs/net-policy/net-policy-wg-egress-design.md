# 网络出口架构最终决议

## 1. 决议摘要

net-policy 将 Direct、WireGuard 和代理订阅统一建模为三类出口（Egress）。它们在产品和策略层处于同一层面，策略只负责决定流量导向哪个出口；出口自身的配置、健康检查和可用性由后台管理。

但三类出口的底层连接语义不同，不能强行设计成完全相同的“持续在线”模型：

| 出口 | 底层语义 | 最终方案 |
|---|---|---|
| Direct | 系统已有的物理网络能力 | 视为始终可用的基线出口，按需承载流量 |
| WireGuard / AmneziaWG | 可持续保活的隧道 | 当前继续使用 mihomo 内置 outbound；暂不拆成独立 Wintun/外部进程 |
| 代理订阅 | 配置来源和按需建立的代理连接 | 订阅由后台维护，实际节点连接继续由 mihomo 按需管理 |

本阶段不再推进“Rust 版 mihomo + Rust 版 AmneziaWG”方案，也不为追求统一的在线状态而重写数据面。当前实现的目标收敛为：

1. 用 mihomo 提供 WireGuard、代理节点和规则分流能力；
2. 用 agent 管理出口配置、健康状态、策略选择和 UI 展示；
3. 明确区分“出口可用”和“当前有流量使用”；
4. 如果未来确实需要真实的 WG latest-handshake、rx/tx 和独立存活期，再单独评估官方 AmneziaWG Windows/Wintun 方案。

## 2. 为什么不采用 Rust 版 mihomo

目前没有成熟、可直接替代 MetaCubeX mihomo 的 Rust 实现，能够同时覆盖本项目需要的 TUN、规则、DNS、代理订阅、控制器和 Windows 行为。第三方 Rust 项目不能作为当前产品的稳定基础。

重写路由器不会自动解决 WireGuard 的交接问题，仍然需要处理：

- 业务流量如何进入 WG 适配器；
- WG endpoint 如何绕开自身 TUN，避免路由环；
- WG 不可用时如何阻止裸连；
- mihomo、agent 和外部隧道之间的启动、退出、崩溃恢复顺序。

因此，切换 Rust mihomo 的成本远大于当前需求带来的收益。

## 3. 出口模型

### 3.1 统一抽象

逻辑上所有出口都提供以下能力：

```text
id() -> EgressId
kind() -> Direct | WireGuard | Proxy
status() -> EgressStatus
health_check() -> HealthReport
start() / stop() / reconnect()
```

统一抽象只表示管理和策略接口统一，不表示三种出口都拥有相同的网络连接状态。

策略关系为：

```text
匹配条件 -> 出口
```

策略切换只改变 mihomo 的流量目标，默认不应因为规则变化而主动重启出口。

### 3.2 Direct

Direct 不建立远端会话，也没有握手或 keepalive。后台只需报告：

- 物理网卡和默认网关；
- 基础网络探测结果；
- 是否被当前策略选中。

Direct 是可用性基线，不需要显示“启动连接”按钮。

### 3.3 WireGuard / AmneziaWG

当前使用 mihomo 内置的 userspace WireGuard outbound。它是 mihomo 进程内的对象，不创建 Windows 网络适配器；其实际存活期受 mihomo 配置加载和 outbound 生命周期影响。

因此当前只能准确表达：

- mihomo 是否已加载该 outbound；
- 最近一次经 mihomo 发起的探测是否成功；
- 最近一次探测时间、延迟和错误；
- 当前策略是否选择它。

当前不能伪造或推断以下真实指标：

- Windows 网卡是否存在；
- `wg show` 风格的 latest-handshake；
- 真实 peer rx/tx；
- 完全独立于 mihomo 的隧道存活期。

探活循环的作用是减少懒握手造成的误判，让 WG 在未被策略选中时仍能被定期探测；它不等于把 WG 变成独立隧道。

### 3.4 代理订阅

代理订阅不是一条持续存在的隧道，而是“节点配置来源”。必须把以下状态分开：

| 状态层 | 含义 |
|---|---|
| 订阅状态 | 拉取、解析、过期和更新时间 |
| 节点状态 | 当前节点最近探测延迟、失败和不可用 |
| 使用状态 | 当前是否有业务连接或流量经过该节点 |

订阅由 agent 负责刷新、解析和保存元数据；节点实际连接继续由 mihomo 按需创建和复用。没有业务流量时，不应为了展示“在线”而强行维持每个代理节点的连接。

刷新订阅与连接节点是两个动作：刷新订阅不应无条件中断当前可用节点；切换节点或节点配置发生变化时，才需要让 mihomo 重新建立相关连接。

## 4. 后台职责边界

```text
net-policy-agent
  ├─ 出口配置与生命周期意图
  ├─ 订阅刷新和元数据
  ├─ 健康探测与状态聚合
  ├─ fallback / fail-closed 决策
  └─ 向前端提供统一 Egress DTO

mihomo
  ├─ TUN / 本地代理入口
  ├─ 规则、进程、域名和 IP 匹配
  ├─ WireGuard outbound 数据面
  ├─ 代理节点数据面
  └─ 按策略建立和复用实际连接
```

agent 不应把“探测成功”包装成真实 WG handshake；mihomo reload 也不应被描述成不会影响 WG。当前架构下，规则 reload 可能重建 mihomo outbound，这是已知边界。

## 5. 故障语义

统一生命周期状态：

```text
Stopped / Starting / Connecting / Ready / Degraded / Reconnecting / Failed
```

但状态含义要按出口类型解释：

- Direct：通常为 `Ready` 或网络不可用；
- WG：`Ready` 表示 mihomo outbound 已加载且最近主动探测成功，不代表独立网卡握手状态；
- Proxy：`Ready` 表示订阅和当前节点可用，不代表节点存在持续连接。

出口状态和策略状态必须分开：

```json
{
  "lifecycle_status": "ready",
  "selected": false,
  "health": {
    "state": "healthy",
    "checked_at": "2026-07-18T12:00:00Z",
    "latency_ms": 82
  }
}
```

出口不可用时，默认 `fail-closed`。只有用户明确配置 `fallback: direct` 或其他备用出口时，才允许回退；不能因为 mihomo 或某个节点失败而静默裸连。

## 6. 前端设计决议

前端页面统一展示三类出口，但必须同时展示两个维度：

```text
出口状态：它自身是否可用
使用状态：当前策略是否把流量导向它
```

### 6.1 出口卡片

所有出口使用统一卡片结构，至少展示：

- 名称和类型；
- 生命周期状态；
- 健康状态、探测时间和延迟；
- 当前是否被策略选中；
- 最近错误；
- 与类型相关的详细信息。

必须使用明确文案，例如：

```text
健康状态：可用
当前策略：未使用
数据面：由 mihomo 承载
```

不能把“在线”理解成“正在承载流量”。

### 6.2 WireGuard 展示

当前阶段展示：

- endpoint、配置名称和混淆是否启用；
- mihomo outbound 是否已加载；
- 最近探测结果、延迟和错误；
- 当前是否被策略使用；
- 探测驱动的重连次数。

不得展示虚假的 latest-handshake、rx/tx 或 Windows 网卡状态。UI 应明确标注：`由 mihomo 承载，非独立 Windows 隧道`。

### 6.3 代理订阅展示

展示：

- 订阅名称、来源和脱敏后的更新时间；
- 最近刷新结果和过期状态；
- 当前选中节点；
- 当前节点探测延迟和可用性；
- 本地 mihomo 代理组/出口名称；
- 当前是否有规则选择该出口。

操作分开提供：刷新订阅、探测节点、切换节点、重连出口。不要显示“持续在线”这类会误导用户的状态。

### 6.4 Direct 展示

展示物理网卡、默认网关、基础连通性和策略使用情况。Direct 不显示连接按钮，只提供网络探测和查看详情。

### 6.5 策略页面

策略页面只配置：

```text
匹配条件 -> 出口
```

出口下拉框同时显示名称、类型和当前可用状态。选择 `Failed` 或 `Stopped` 的出口时，要求用户明确选择阻断、等待恢复或指定 fallback。

## 7. 当前实现的处理原则

当前代码已实现的是“统一出口管理和健康状态”，不是独立 WireGuard 数据面。后续维护应遵守以下原则：

1. 不再把 `EgressManager` 的健康状态称为真实隧道状态；
2. 不因普通规则变化把出口状态解释为已断线或已重连；
3. 修正失败出口的 fallback、持久化和前端重连操作语义；
4. 代理订阅继续由 mihomo 承载，agent 只管理订阅生命周期和状态聚合；
5. UI 明确显示 `mihomo-managed`，避免用户误以为 WG 有独立网卡；
6. 任何真正独立 WG 的工作都必须作为新的专项设计，不在当前出口抽象中偷偷实现。

## 8. 未来重新评估条件

只有出现以下硬需求时，才重新评估独立 AmneziaWG/Wintun：

- 必须展示真实 latest-handshake、peer rx/tx；
- mihomo reload 不能影响 WG 隧道；
- 停止 mihomo 后 WG 仍必须保持在线；
- 需要把 WG 作为真实 Windows 网络适配器交给其他系统组件使用。

届时优先评估官方 AmneziaWG Windows/Wintun 方案，由 Rust agent 负责编排；不以 Rust mihomo 重写或自研 Rust 混淆协议作为默认前提。交接方案必须单独完成 endpoint 主机路由、kill-switch、防路由环和不可用时 fail-closed 的 PoC。

## 关联文档

- 行为模型：[net-policy-observer-first-design.md](net-policy-observer-first-design.md)
- Daemon / GUI / kill-switch：[net-policy-daemon-gui-split-design.md](net-policy-daemon-gui-split-design.md)
- 混淆与防火墙验证：[net-policy-validation-report.md](net-policy-validation-report.md)
- 出口池与 worker：[../distributed-worker-design.md](../distributed-worker-design.md)
