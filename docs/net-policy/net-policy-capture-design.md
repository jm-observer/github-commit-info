# net-policy 抓包（Packet Capture）设计

> 承接 [net-policy-observer-first-design.md](net-policy-observer-first-design.md)（观察者优先 + TUN 数据面）与
> [net-policy-daemon-gui-split-design.md](net-policy-daemon-gui-split-design.md)（四 crate + 版本化命名管道 +
> §15 Windows 服务化）。本文描述如何在现有出口管控底座上增加**域名嗅探**与**可导入 Wireshark 的
> pcapng 抓包**。仅 Windows。
>
> **状态：Phase 1（sniffer）完整；Phase 2a 全 TUN 抓包真机 E2E 已跑通（core→agent→协议→client→CLI→
> 合法 pcapng）；Phase 2b 定向解析+GUI 已落地（定向 E2E 待补）；Phase 4 引擎 spike 已验证，但产品能力
> 自动导流、调用者 SID 绑定、代理会话认证和 CA 私钥保护已闭环；仍需完成 §18 真机回归矩阵。**
> - Phase 1 已在 `net-policy-core` 落地（`NetPolicySettings::sniffer_enabled` 开关 + `generate_config`
>   输出 `sniffer` 块 + YAML 单测 + GUI 设置页开关，**默认关闭**）。
> - Phase 0 真机 spike 已过（0.228，见 [validation-report](net-policy-capture-validation-report.md)）：
>   pktmon→pcapng 管道验证（filter add/list/remove、start/stop、`etl2pcap --component-id` → pcapng 魔数
>   `0A 0D 0D 0A` 合法），解除 Phase 2 编码闸。
> - **Phase 2a 真实后端已落地**：`net_policy_core::capture`（DTO / §9 校验 / §5.2 过滤器预算 / 状态机，全单测）
>   + `net_policy_agent::capture`（`PktmonBackend` shell 封装 + 组件解析 + `CaptureStore` manifest/配额 +
>   `CaptureManager` 单会话）+ 协议升到 **1.5**（`Capture*` + 11 错误码 + `Hello.capabilities`）。agent 对
>   `All` target 执行真实抓包，pktmon 探测通过时声明 `capture_v1`；外部 pktmon 占用/TUN 组件缺失有诚实错误码。
> - **仍待**：Phase 2a 完整绿路需 mihomo TUN 起栈才能真机端到端跑（当前测试机未 apply、无 TUN）；Phase 2b
>   定向抓包（fake-ip 端点解析）+ GUI；agent 崩溃恢复（租约/orphaned）。
> - 真机验证 sniffer DNS/fake-ip 回填、纯 IP、TLS、QUIC、ECH/无 SNI 降级仍待做，未验证前不改新安装默认。
>
> - **Phase 4（L4 MITM）**：引擎/data-plane ADR 已落 [adr-2026-07-phase4-mitm-engine.md](adr-2026-07-phase4-mitm-engine.md)；
>   协议 **1.6**（`Decrypt*`/`DecryptCa*` + `DecryptCaExportPublic` + 10 个 `decrypt_*` 错误码 + `decrypt_v1`
>   能力，§17.8）；机器无关逻辑 `net_policy_core::decrypt` 已落地（会话/CA/目标 DTO + §17.5 校验 + **§17.6
>   脱敏 golden** + 状态机）。自研 `net-policy-mitm` 方案 B spike 已真实解密。**四项后续开发已落代码 + 单测**：
>   ① mihomo 自动导流 config-gen（`DecryptDivert`/`divert_lines`/`active_divert`，§17.3 方案 B）；② CA 私钥
>   **DPAPI machine-scope 保护**（`private/ca.key.dpapi`，磁盘永不明文）+ GUI **真装 `CurrentUser\Root`**（certutil
>   + PowerShell DER SHA-256 实查）；③ GUI **应用明文页**（每域名计数 + Raw 红标）；④ **QUIC/pinning 降级审计**
>   （透传/拒证 per-domain 计数 + force-TCP REJECT，§17.7/§17.9）；④ 命名管道客户端 token SID 与 CA owner
>   强绑定；⑤ loopback CONNECT 使用随机会话 Basic Auth，凭据只注入 mihomo outbound。不存在环境变量绕过。
>   §18 真机矩阵尚未完成的组合会作为验证状态记录，不再用不安全开关绕过安全边界。
>
> 文中的 pktmon 命令已按当前开发机帮助文本及 Microsoft 文档校正；TUN 组件定位、fake-ip 包面地址、过滤器
> 运行期行为仍必须按 §15 在目标 Windows 真机验证，未验证项不得作为产品承诺。

## 0. 结论

net-policy 已让整机受管出站流量经过 mihomo TUN，抓包应复用这个数据面，而不是再安装一套旁路驱动：

- **Phase 1**：启用 mihomo `sniffer`，补全 TLS SNI、HTTP Host、QUIC 域名；只增强观察数据，不改路由。
- **Phase 2**：由 LocalSystem `net-policy-agent` 独占编排 Windows 内置 pktmon，在 mihomo TUN 组件上抓 ETL，
  停止后转换为 pcapng。
- **Phase 4（独立可选能力）**：在用户安装并启用专用调试 CA 后，仅对明确选择的进程/域名做 TLS MITM，输出
  HTTP 层明文。L4 默认关闭，与 L1–L3 分离，不因普通抓包自动启用。
- MVP 同时只允许一个会话；过滤条件在开始前冻结。按进程/域名抓包是**基于当前连接端点的定向快照**，不是按
  PID 的持续内核过滤。
- L1–L3 默认不做 TLS MITM、不安装根证书，也不承诺看到 HTTPS 请求体；只有独立启用 Phase 4 才进入明文能力。

## 1. 目标、非目标与边界

### 1.1 目标

1. 从观察表一键抓取当前进程或域名相关的数据包。
2. 支持整个 TUN 的限时、限容量抓包。
3. 生成 Wireshark 可打开的 `.pcapng`，并携带足够的会话元数据用于回溯。
4. agent/GUI 崩溃、断线或重启后，会话结果可解释、可清理，不误停用户自己的 pktmon 会话。
5. 默认最小化采集范围和保留时间，避免无界占用磁盘。

### 1.2 非目标

- **不解密 HTTPS/TLS/QUIC 载荷**，不做 MITM，不修改系统信任根。
- 不提供实时 Wireshark 流式接口、远程上传、远程控制或长期后台录制。
- 不保证捕获被 `route-exclude-address` 排除的 LAN 流量，也不保证捕获未进入 TUN 的 IPv6 流量。
- MVP 不安装 npcap，不承诺 BPF、PID、方向或任意布尔表达式过滤。
- 抓包不参与路由决策和 fail-closed；抓包失败不得影响 mihomo、防火墙或现有网络姿态。
- L4 不以“抓包选项”混入 L3；它是需要单独安装组件、信任 CA、二次授权和独立验收的后续能力，详见 §17–§18。

## 2. 能力分层

| 层级 | 能拿到什么 | 手段 | 本设计 |
|---|---|---|---|
| L1 流级元数据 | 域名 / IP / 进程 / 出口 / 规则 / 字节数 | mihomo `/connections`（已有） | 已有，作为定向线索 |
| L2 协议嗅探 | TLS SNI、HTTP Host、QUIC 域名 | mihomo `sniffer` | Phase 1 |
| L3 数据包 | 五元组、时序、握手、明文 HTTP、密文载荷 | pktmon 抓 TUN → pcapng | Phase 2 |
| L4 应用明文 | HTTP(S) 请求/响应头与正文、WebSocket 消息 | TLS MITM + 专用调试 CA | Phase 4，默认关闭 |

“全包”只表示 `--pkt-size 0` 不主动截断单包，**不表示协议解密，也不表示操作系统/驱动绝不丢包**。只有用户
另行启用 L4 会话时才产生应用明文；L3 pcapng 始终保持原始线上密文。

## 3. 抓包点与可见性

mihomo 当前配置使用 `tun.enable=true`、`stack=gvisor`、`auto-route=true`。应用包先被路由到 TUN，mihomo
终止或转发该流，再创建 direct/WireGuard 等 outbound。因此首选抓包点是 **mihomo TUN 对应的 pktmon
component**：

- 这里是应用侧视图，适合看 TCP/UDP、TLS ClientHello、HTTP 明文头和包时序。
- WireGuard 出口的包在这里尚未成为外层隧道密文；若要排查 WG 外层，需要另开物理网卡抓包，MVP 不做。
- `route-exclude-address` 中的 LAN 流量绕过 TUN，抓不到是预期盲区。
- IPv6 是否进入 TUN取决于顶层 `ipv6`、TUN 地址与系统环境；UI 应按实际配置提示，不能笼统宣称“一定抓不到”。

### 3.1 fake-ip 的关键口径

当前 DNS 为 `enhanced-mode: fake-ip`。应用发往 TUN 的包可能以 `198.18.0.0/16` fake-ip 为目标，而 mihomo
`/connections.metadata.destinationIP`、内部解析后的真实 IP、最终 outbound IP 可能不是同一口径。

因此实现不得直接假设“连接表 destinationIP 就是 TUN 包上的 IP”或“它一定是真实 IP”。定向过滤统一引入
`CaptureEndpoint { capture_ip, port, network, source }`：

1. `capture_ip` 必须是**在目标 TUN component 上实际可匹配的地址**；
2. `source` 记录它来自连接表、fake-ip 映射还是用户输入；
3. Phase 2 开发前先用无过滤短抓做 golden probe，确认当前 mihomo 版本各字段与 TUN 包面的对应关系；
4. 无法得到可靠 `capture_ip` 时，拒绝定向启动并提示改用全 TUN 短抓，不能静默抓空文件。

## 4. 引擎选型与已确认约束

| 方案 | 依赖 | 过滤能力 | 输出 | 结论 |
|---|---|---|---|---|
| pktmon | Windows 10/11 内置，无新增驱动 | MAC/IP/端口/协议/VLAN；最多 32 条；不区分源/目的 | ETL，停止后转 pcapng | MVP |
| npcap | 需安装驱动并评估再分发许可 | BPF，表达力更强 | 直接 pcap/pcapng | 后续 ADR |

pktmon 是**机器级共享设施**：capture 状态、component ID 和过滤器都不是 net-policy 私有资源；component ID
还可能在重启或驱动重载后变化。MVP 必须每次开始前重新探测，禁止缓存跨重启 ID。

当前开发机确认的命令形态如下（实现使用 `std::process::Command` 参数数组，禁止拼 shell 字符串）：

```text
pktmon list --json
pktmon filter list
pktmon filter add <name> -i <ip[/cidr]> -p <port> -t <TCP|UDP>
pktmon start --capture --comp <component-id> --pkt-size <n> \
  --file-name <capture.etl> --file-size <MiB> --log-mode circular
pktmon status
pktmon counters --json
pktmon stop
pktmon etl2pcap <capture.etl> --out <capture.pcapng> --component-id <component-id>
pktmon filter remove
```

注意：

- 转换命令是 **`etl2pcap`**；不能写成不存在于当前系统的 `pktmon pcapng`。
- `--pkt-size 0` 抓完整包；默认 128 字节。MVP 默认 128 字节，用户显式选择“完整包”才传 0。
- `--file-size` 单位是 MiB；`circular` 到上限后覆盖最旧数据，所以容量上限语义是“最多保留最近 N MiB”，
  不是精确触发自动停止。时间上限由 agent 定时停止。
- pktmon 的同一包可能在多个网络栈 component 出现多份快照；开始和转换都限定同一个 TUN component，仍须用
  golden probe 验证是否存在重复。

## 5. 定向抓包语义

pktmon 不支持 PID 过滤。“按进程/域名抓”由 agent 在**开始时**把目标解析为当前可见的包面端点，再生成
pktmon 过滤器；过滤集在本会话中冻结。

### 5.1 Target 解析

| Target | 解析规则 | 空结果 |
|---|---|---|
| `All` | 不添加过滤器，只限定 TUN component | 允许 |
| `Process` | 按 `process_path` 优先、`process_name` 兜底匹配当前 `/connections`，提取去重端点 | 拒绝并提示先产生流量 |
| `Domain` | 规范化域名；优先匹配当前连接 `host`，再查 Observatory 的近期关联；只使用已验证的包面 IP | 拒绝并提示全量短抓 |
| `Ip` | 解析为 `IpAddr`/CIDR；默认理解为 TUN 包面 IP | 非法输入返回 `validation` |

进程目标沿用 `ProcessRef::{ProcessPath, ProcessName}`，但 UI 从连接行发起时必须优先传完整路径；同名进程无法
区分实例是 MVP 的已知限制，不宣称“按 PID 精确抓”。

### 5.2 过滤器预算

- pktmon 同时最多 32 条过滤器；一条远端 `(IP, port, protocol)` 对应一条命名过滤器。
- 去重、排序后若端点超过 32 条，MVP 返回 `capture_filter_limit`，同时给出端点数量；**不自动退化为全量抓**，
  避免用户以为是定向抓却落盘无关流量。
- IP/端口过滤不区分源和目的，因此可能捕获到另一条恰好使用相同 IP 或端口的流量；manifest 必须记录该限制。
- 不在运行期追加过滤器。原因是全局过滤器的热更新行为未验证，且重启 capture 会制造丢包窗口。需要持续跟踪
  新连接时，先做真机实验，再决定新增“分段重启”或 npcap 后端。

## 6. 生命周期与状态机

```text
preparing ──探测/解析/加过滤器──> running ──用户停止/超时──> stopping
    │                                  │                         │
    └──────────────失败──────────────> failed                    ▼
                                                  converting ──> done
                                                       │
                                                       └──────> failed
```

状态定义：

| 状态 | 含义 | 是否有可下载文件 |
|---|---|---|
| `preparing` | 校验参数、检查 pktmon 占用、定位组件、解析过滤器 | 否 |
| `running` | pktmon 已确认处于 capture 状态 | 否 |
| `stopping` | 正在停止 agent 所属会话并清过滤器 | 否 |
| `converting` | ETL 已封口，转换 pcapng | 否 |
| `done` | pcapng 和 manifest 已原子提交 | 是 |
| `failed` | 任一步失败；保留可诊断错误，清理可证明属于本会话的资源 | 通常否 |
| `orphaned` | agent 重启后发现租约与系统状态无法安全归属 | 否，需修复/清理 |

只有 `running` 可执行 Stop；对 `stopping/converting/done` 重复 Stop 应幂等返回当前状态。Delete 对运行态返回
`capture_busy`，不得隐式 Stop。

## 7. agent 架构与互斥

```text
GUI（普通用户）
  │ 版本化命名管道
  ▼
net-policy-agent（LocalSystem / Session 0）
  ├─ CaptureManager：状态机、租约、超时、恢复
  ├─ TargetResolver：/connections + Observatory → CaptureEndpoint
  ├─ PktmonBackend：探测、过滤、start/stop、etl2pcap
  └─ CaptureStore：manifest、pcapng、配额、分块读取
       │
       ├─ mihomo controller（只读连接快照）
       └─ pktmon（机器级共享设施）
```

- `CaptureManager` 挂在 `AgentState`，拥有独立 capture mutex；同一时刻只允许一个会话。
- 抓包与 apply/reload/stop 不共用现有 `op_flag`，但有明确冲突规则：`preparing` 期间禁止网络长操作；进入
  `running` 后如用户发起 apply/reload/stop，先返回 `capture_conflict`，由用户先停止抓包。避免 TUN component
  在抓取中消失或变更。
- 读取状态和下载不占长锁；外部命令在 `spawn_blocking` 中执行，并设置进程超时。
- 所有 pktmon 调用记录退出码与 stderr 到 agent 日志；协议只返回稳定错误码和脱敏摘要。

## 8. 机器级资源所有权与崩溃恢复

开始前按顺序执行：

1. `pktmon status`：若已有 capture/trace 在运行，返回 `capture_engine_busy`，**绝不调用 stop**。
2. `pktmon filter list`：若已有任意过滤器，返回 `capture_filters_busy`，**绝不 remove 他人过滤器**。
3. `pktmon list --json`：按 TUN 的 interface GUID/alias/description 联合匹配 component；零个或多个候选均拒绝。
4. 原子写 `active-capture.json` 租约，包含 session ID、ETL 绝对路径、component ID、启动时间与 agent instance ID。
5. 添加命名过滤器并 start；只有 `status` 确认运行后才进入 `running`。

agent 重启时：

- 有租约且 pktmon 未运行：将原会话标为 `failed(agent_restarted)`；若 ETL 可读，尝试转换为“恢复产物”。
- 有租约且 pktmon 在运行：只有系统状态中的输出路径、component 与租约均匹配时才认为是本产品会话，并执行
  stop → convert → cleanup；任何一项无法证明则标 `orphaned`，不调用全局 stop/remove。
- 无租约但 pktmon 在运行：视为外部会话，完全不碰。
- 清过滤器放在 best-effort finally；只因开始前已要求过滤器为空，才允许在本会话成功取得所有权后调用
  `filter remove`。

## 9. 存储、配额与原子性

服务模式目录固定为 `%ProgramData%\net-policy\captures\`：

```text
captures/
  active-capture.json
  <session-id>/
    capture.etl.tmp
    capture.pcapng.tmp
    manifest.json.tmp
    capture.pcapng
    manifest.json
```

- 完成顺序：停止 → 转换 → 校验 pcapng 非空且头合法 → 写 manifest → fsync/关闭 → rename 去掉 `.tmp` →
  删除 ETL。只有两份最终文件都存在才标 `done`。
- `.etl` 与 `.pcapng` 同等敏感；失败后默认删除临时文件。若为诊断保留，必须显式标记并进入同一配额。
- 默认 `max_secs=120`、`file_size_mib=128`、`snap_len=128`；允许范围分别为 10–600 秒、16–512 MiB、
  64–65535 或 0。`0` 仅表示完整包，不表示无限容量。
- 总配额默认 1 GiB、最多 10 个完成会话；每次 Start/Delete/agent 启动时清理。先删最旧 `done/failed`，绝不删
  活跃会话；仍无空间则拒绝新会话。
- 开始前检查目标卷可用空间至少为 `2 * file_size_mib + 128 MiB`，覆盖 ETL 与转换期间 pcapng 共存。
- captures 目录 ACL 仅允许 SYSTEM 与 Administrators；普通 GUI 不获得真实路径，只能经受控管道分块读取。

manifest 至少包含：schema 版本、session ID、target、解析后的端点与来源、截断长度、过滤器、TUN component
标识、mihomo 版本、协议版本、开始/结束时间、停止原因、ETL/pcapng 字节数、转换结果、已知限制。不得写入
WireGuard 私钥、controller secret 或完整命令行 secret。

## 10. 管道协议扩展（建议 protocol 1.5）

当前协议为 `1.4`。本能力纯追加请求/响应，建议 minor 升到 `1.5`，并给 `Hello` 响应追加
`capabilities: Vec<String>`（新客户端字段 `#[serde(default)]`，旧客户端会忽略未知字段）。agent 仅在 pktmon 探测
通过时声明 `capture_v1`。

```rust
enum CaptureTarget {
    All,
    Process(ProcessRef),
    Domain(String),
    Ip(String),
}

struct CaptureOpts {
    snap_len: u32,       // 0=完整包；其余为每包保留字节数
    file_size_mib: u32, // pktmon circular 上限
    max_secs: u64,
}

enum CaptureState { Preparing, Running, Stopping, Converting, Done, Failed, Orphaned }
enum CaptureStopReason { User, Timeout, AgentRestart, Error }

// Request（新增）
CaptureStart  { target: CaptureTarget, opts: CaptureOpts }
CaptureStop   { id: String }
CaptureGet    { id: String }
CaptureList
CaptureDelete { id: String }
CaptureRead   { id: String, offset: u64, len: u32 }

// Response（新增）
CaptureSession { session: CaptureSession }
CaptureSessions { sessions: Vec<CaptureSession> }
CaptureChunk { id: String, offset: u64, data_base64: String, eof: bool }
```

协议约束：

- ID 由 agent 生成并按固定格式校验；客户端不能提供路径。
- `CaptureRead` 仅允许 `done` 会话；`len` 原始字节硬上限 512 KiB。base64 后仍远低于现有 8 MiB 帧上限。
- offset 必须小于等于文件长度；响应携带实际 offset 与 EOF，客户端按序写临时文件，最终校验总长度。
- `CaptureSession` 不暴露服务端绝对路径，只给 `file_name`、`bytes`、时间和结构化错误。
- 新增稳定错误码：`capture_unsupported`、`capture_engine_busy`、`capture_filters_busy`、
  `capture_component_not_found`、`capture_target_empty`、`capture_filter_limit`、`capture_conflict`、
  `capture_not_found`、`capture_busy`、`capture_storage_full`、`capture_convert_failed`。

## 11. L2 域名嗅探（Phase 1）

`net-policy-core::mihomo::generate_config` 增加：

```yaml
sniffer:
  enable: true
  parse-pure-ip: true
  override-destination: false
  sniff:
    HTTP:
      ports: [80, 8080-8880]
    TLS:
      ports: [443, 8443]
    QUIC:
      ports: [443, 8443]
```

- 顶层及协议级 `override-destination` 都保持 false，Phase 1 只观察，不因嗅探结果改变实际目标或路由。
- `parse-pure-ip=true` 只对缺少域名的连接尝试嗅探；需用单测锁定生成 YAML，并在真机确认 `/connections.host`
  是否按预期回填。
- 配置开关建议为 `sniffer_enabled`，默认先关闭；真机验证无兼容性回归后再评估新安装默认开启。
- 嗅探不是权威 DNS：ECH、非标准端口、应用私有协议、无 SNI TLS、QUIC 版本差异都可能使域名为空。
- Observatory 对域名来源增加 `dns|sniffer|unknown`（若 mihomo API 无法区分则记 `unknown`），不要把嗅探结果
  伪装成 DNS 解析事实。

## 12. UI 设计

- 观察表行操作：“抓此进程”“抓此域名”；弹出确认框，展示将命中的当前端点数、截断长度、时长和容量。
- 抓包页支持 `All/Process/Domain/IP`，默认“包头 128 B、2 分钟、128 MiB”；选择完整包时显示高敏感警告。
- 运行态只展示可靠指标：目标、已用时长、时间上限、开始时间。pktmon counters 可作为“组件包计数（估算）”，
  必须标注非最终 pcap 包数；最终大小只在转换后展示。
- 完成态提供“保存 pcapng”“删除”“查看 manifest 摘要”。保存由 GUI 调 `CaptureRead` 分块落到用户选择的位置。
- 显式提示：HTTPS/QUIC 内容仍是密文；LAN/未入 TUN 的 IPv6 不在本次抓包；进程/域名过滤是开始时快照，
  新连接不自动纳入。

## 13. 安全与隐私

- 抓包属于高敏感操作。Start 前必须二次确认；完整包模式单独强调可能包含 Cookie、Authorization、明文 HTTP、
  DNS 查询及业务载荷。
- 请求仍受现有命名管道 DACL 约束。若未来支持多交互用户，必须先增加“会话所有者 SID”，否则任一 IU 都可
  读取机器级抓包；在此之前产品范围保持单用户管理员机器。
- agent 对 target、域名、IP/CIDR、端口、大小、时长和读取范围重新校验，不信任 GUI。
- 外部命令只传固定二进制 `System32\pktmon.exe` 与结构化参数；路径 canonicalize 后必须在 captures 根内，防
  路径穿越、reparse point 与任意文件读取。
- 不自动上传、不写剪贴板、不在普通日志记录包内容；删除需同时删除 pcapng、manifest 与残留临时文件。
- pcapng 不做“安全内容净化”；用户导出后由用户负责保管。

## 14. 故障模型

| 场景 | 必须结果 |
|---|---|
| GUI 断线/退出 | 抓包按原上限继续；重连后 `CaptureGet/List` 对齐 |
| CaptureStart 中途失败 | 回滚已添加过滤器和临时文件；不得影响网络策略 |
| 用户已有 pktmon 会话/过滤器 | 返回 busy；绝不 stop/remove |
| TUN component 找不到或不唯一 | 拒绝启动并给诊断信息；不退化抓全网卡 |
| agent 崩溃/SCM 重启 | 按 §8 归属恢复；不能证明所有权则 orphaned、不碰全局状态 |
| mihomo/TUN 消失 | watchdog 检测后停止自有抓包并标 failed；现有 fail-closed 逻辑独立运行 |
| `pktmon stop` 失败 | 会话标 failed/orphaned；不转换仍在写入的 ETL |
| ETL 转换失败 | 标 `capture_convert_failed`，清理临时 pcapng；按诊断策略短期保留 ETL |
| 磁盘不足 | Start 前拒绝；运行中仍出错则 stop、清临时文件、保留结构化错误 |
| GUI 下载中断 | 服务端文件不变；客户端可从已写 offset 继续 |
| Delete 重复调用 | 幂等成功；运行态明确返回 busy |

## 15. 分阶段落地与验收门槛

### Phase 0：真机 spike（阻断 Phase 2 编码）

在目标 Windows 真机以管理员执行并保存证物：

1. `pktmon list --json` 能唯一定位 mihomo TUN，记录重启前后 component ID 变化。
2. 无过滤抓 10 秒，确认 TUN 上 TCP、UDP、TLS、QUIC 的可见性与是否重复。
3. 对 fake-ip 域名抓包，对照 `/connections`，钉死 `host/destinationIP/真实 outbound/TUN 包面 IP` 关系。
4. 验证 1 条和 32 条过滤器、运行期 add/remove 的真实行为；MVP 即使支持也仍默认冻结。
5. 验证 `--pkt-size 128/0`、`--file-size` circular、超时 stop、`etl2pcap --component-id`。
6. 强杀 agent、重启服务、转换失败、磁盘不足、外部 pktmon 已占用等恢复场景。

输出 `docs/net-policy/net-policy-capture-validation-report.md`。第 1–3 项未通过时，不进入 Phase 2。

### Phase 1：sniffer

- ✅ core 配置字段（`sniffer_enabled`，默认关）、YAML 生成与单测（`mihomo.rs` 锁定 `override-destination:
  false` + 三协议端口 + 块位置）；
- ✅ GUI 设置页开关（`WgConfigForm`「观察增强」区，热加载生效）；
- ⏳ 观察数据来源字段（`dns|sniffer|unknown`）：mihomo `/connections` 能否区分来源须真机确认，暂不硬编，
  按 §11 无法区分则记 `unknown`；
- ⏳ 真机验证 DNS/fake-ip、纯 IP、TLS、QUIC、ECH/无 SNI 降级；
- ⏳ 确认 `override-destination=false` 下路由结果与启用前一致。

### Phase 2 协议/纯逻辑层（已落地，机器无关）

- ✅ `net_policy_core::capture`：`CaptureTarget/CaptureOpts/CaptureEndpoint/CaptureFilter/CaptureState/
  CaptureStopReason/CaptureSession/CaptureManifest` 类型；`CaptureOpts::validate`（§9 边界）、`plan_filters`
  （§5.2 去重 + 32 条预算 + 不退化全量）、`is_valid_session_id`/`format_session_id`（防路径穿越）、
  `validate_read_window`（512 KiB 上限）、状态机 helper——全带单测。
- ✅ 管道协议 1.5：`Request::Capture{Start,Stop,Get,List,Delete,Read}` + `Response::Capture{Session,
  Sessions,Chunk}` + 11 个 `capture_*` 稳定错误码 + `Hello.capabilities`（§10）。client 加 `has_capability`。
- ✅ agent 骨架：dispatch 已接线，真实后端未实现前诚实返回 `capture_unsupported`、不声明 `capture_v1`。

### Phase 2a：全 TUN 抓包（真实后端，Phase 0 spike 已过）

- ✅ `CaptureManager/PktmonBackend/CaptureStore`、状态机、单会话锁、配额（`net-policy-agent::capture`，
  单测覆盖组件解析/store/配额）；
- ✅ `All` target、Stop/List/Get/Delete/Read 协议 handler 已接线；`capture_v1` 在 pktmon 探测通过时声明；
- ✅ 外部 pktmon 冲突保护（§8#1/#2：`status`/`filter list` 已占用 → `capture_engine_busy`/
  `capture_filters_busy`，绝不 stop/remove 他人资源）；组件唯一定位（零/多候选拒绝，不退化抓物理网卡）；
  时间上限到时自动 stop；stop→etl2pcap→pcapng 魔数校验→写 manifest→删 ETL。
- ✅ **完整绿路真机端到端已验证**（0.228，apply 直连观察姿态起 Meta TUN 后）：`capture-start` 定位组件
  `Id=76` → 真实 HTTPS/DNS 流量 → `capture-stop` → etl2pcap → **pcapng 魔数合法** → `capture-list` →
  `capture-save` 分块下载 1432B 合法 pcapng（见 [validation-report](net-policy-capture-validation-report.md)）。
  驱动经新增的 `net-policy` CLI 子命令 `set-route/apply/capture-*`。
- ⏳ agent 崩溃恢复（租约 `active-capture.json` + orphaned 归属）、同包多组件去重观察为后续增量。

### Phase 2b：定向抓包与 GUI（解析逻辑已落地，GUI + 真机口径待做）

- ✅ 端点解析：`net_policy_agent::capture::resolve_endpoints`（Process 按路径优先/名兜底、Domain 子域后缀、
  Ip 匹配连接表 → `CaptureEndpoint`，空结果 `capture_target_empty`，单测覆盖）；`plan_filters` 32 条预算
  （超限 `capture_filter_limit`，不静默退化）；start 时加 pktmon 命名过滤器、stop finally 清除。
- ✅ fake-ip 口径：取连接表 `destinationIP`（fake-ip 模式即 TUN 包面地址），manifest known_limits 标注
  «未经真机 golden probe 确认»（诚实，§3.1）。
- ✅ GUI 抓包页（net-policy-gui：`CapturePage` + Shell「抓包」导航 + `NetPolicyAPI.capture*` + 6 个
  `net_policy_capture_*` tauri 命令 + client `capture_*` 方法）：全 TUN 抓包 + 会话列表 + 停止/删除 +
  Start 二次确认 + 完整包高敏感红标 + 分块 `CaptureRead` → 浏览器 Blob 下载 pcapng（无新增依赖）；tsc + cargo 通过。
- ⏳ 观察表行「抓此进程/抓此域名」入口（定向 target 从连接行发起，接线已备，UI 入口待接）；
- ⏳ 真机 golden probe 钉死 fake-ip 四种地址口径；定向抓包端到端故障矩阵（需 TUN）。

### Phase 3（可选 ADR）——已评估

- ✅ [adr-2026-07-phase3-npcap-backend.md](adr-2026-07-phase3-npcap-backend.md)：**暂不采用 npcap，维持
  pktmon**（需求未达门槛 + 内核驱动/许可成本高；core 抽象已留 `NpcapBackend` 并存的回退路径，可按需重开）。
- ⏸ BPF/PID 精细过滤、分段抓取/实时预览、pcapng 自定义 block：按上 ADR 的「重开条件」延后。

### Phase 4：应用明文（独立安全闸）

- 按 §17 完成引擎/数据面 spike 与 ADR；
- CA 生成、按用户安装/撤销、私钥保护与过期轮换；
- 精确进程实例 + 域名 allowlist、明文会话状态机、配额与导出；
- pinning/mTLS/QUIC/ECH 的显式降级与审计；
- 通过 §18 的安全、兼容性和卸载验收后才能进入产品构建。

### 15.1 完成定义

除仓库通用的 `cargo clippy -- -D warnings`、`cargo fmt --check`、`cargo test` 外，抓包功能必须满足：

- 单元测试：参数校验、target 解析、过滤器预算、状态转换、路径/offset 校验、配额清理、manifest schema。
- 假后端测试：每个 pktmon 失败点都能回滚且不误 stop/remove 外部资源。
- Windows 集成测试：生成的 pcapng 可被 Wireshark/tshark 打开，snaplen 与 target 过滤符合预期。
- 安全测试：普通用户不能直接读 captures；路径穿越/reparse point/超大读取被拒；日志不含包载荷或 secret。
- 网络回归：抓包启动、运行、停止、失败均不改变三姿态、路由规则、kill-switch 与 watchdog 结果。

## 16. 已决策项与待决问题

### 已决策

- MVP 后端 = pktmon；单会话；只抓 TUN component。
- 过滤集开始时冻结；超过 32 条拒绝；不静默退化全量。
- 默认 128 字节包头、2 分钟、128 MiB circular；完整包需显式选择。
- 只经管道下载，不暴露 `%ProgramData%` 路径；ETL 转换成功后删除。
- 抓包与网络长操作冲突时，先停抓包，不让 capture 生命周期干扰策略生命周期。
- L4 默认关闭且不能由 L3 自动升级；只允许定向会话，不提供“全机器全部明文”快捷入口。

### 待真机结论

- Wintun 是否稳定暴露为可单独选择的 pktmon component，以及唯一匹配字段。
- fake-ip 下 TUN 包面地址与 mihomo controller 字段的精确关系。
- pktmon counters 能否提供足够可靠的运行态估算值。
- agent 崩溃后 `pktmon status` 是否暴露足够字段证明会话归属。
- pcapng 是否仍含同包多快照；如有，MVP 是接受并提示，还是转换阶段去重。

## 17. L4 应用明文设计

### 17.1 安全定位与启用前提

L4 的本质是主动终止客户端 TLS，再由代理建立第二条到服务端的 TLS 连接。代理为目标域名动态签发叶子证书，
客户端因信任 net-policy 专用 CA 而接受该连接。它能看到 HTTP 请求/响应、Cookie、Authorization、表单、文件与
WebSocket 消息，也会改变 TLS 握手、证书链、网络时序和部分协议特征。

因此 L4 不是 L3 的“更完整模式”，而是独立的高风险诊断功能，必须满足以下前提：

1. 用户先阅读风险说明并显式创建专用 CA；仅安装 CA 不自动开始拦截。
2. 每次明文会话再次确认目标、时长、正文上限、导出范围和兼容性策略。
3. 默认只允许一个**精确进程实例**，可再收窄到域名 allowlist；MVP 不提供 `All` target。
4. UI 常驻红色“TLS 解密中”状态，显示剩余时间、目标进程、域名和 CA 指纹；托盘/GUI 退出不隐藏该事实。
5. 到时、用户停止、目标进程退出、agent/解密引擎异常或网络策略切换时立即撤销临时转发规则并结束会话。
6. CA 信任和明文产物均可一键清除；卸载必须验证 CA、私钥、临时规则和引擎资产均已移除。

L4 只允许用于用户拥有或明确获授权分析的设备、账号与流量。产品文案不得以“抓包”弱化其拦截性质。

### 17.2 引擎选型（需单独 ADR 与依赖批准）

| 方案 | 优点 | 代价/风险 | 结论 |
|---|---|---|---|
| **mitmproxy/mitmdump sidecar** | 成熟的 HTTP/1.1、HTTP/2、WebSocket 与动态证书能力；Windows 支持 Local Capture，可按进程名/PID | 新增较大的第三方可执行资产与 Python/OpenSSL 供应链；需验证 Session 0、进程过滤、升级和许可 | 首选 spike |
| mihomo → 本地显式 MITM 代理 | 可复用现有进程/域名规则，网络出口仍由 mihomo 统一选择 | 必须验证 mihomo HTTP outbound 链路、原目标/SNI 保真、WG 出口组合及递归规避 | 备选 spike |
| 自研 Rust TLS/HTTP MITM | 可完全控制协议和存储 | TLS、HTTP/2/3、证书、WebSocket、流控与安全维护成本极高 | 不采用 |
| FiddlerCore/商业 SDK | Windows 集成成熟 | 许可、闭源依赖和分发约束 | 仅 ADR 对比 |

仓库规约禁止未经用户明确同意新增依赖。因此本文只定义边界，**不得因本设计直接加入 mitmproxy 或其它 SDK**；
Phase 4 开工前必须提交 ADR，列明版本锁定、许可证、资产大小、CVE/更新策略、签名/哈希校验和卸载方案，并取得
用户批准。

### 17.3 两种候选数据面

#### 方案 A：mitmproxy Local Capture（首选验证）

```text
目标应用(PID) ──Local Capture──> mitmdump ──普通出站──> mihomo TUN ──> direct / wg-out
                         │
                         └── flows.mitm + 结构化明文索引
```

- agent 以 sidecar 方式启动固定安装目录中的 `mitmdump.exe`，只选择一个 PID；记录 PID、进程创建时间和规范化
  路径，防 PID 复用。
- mitmdump 自身 PID 必须排除，且其出站仍交给现有 TUN/路由策略，避免形成捕获递归。
- 域名 allowlist 由 MITM 引擎在 TLS/HTTP 层执行；非 allowlist 连接只允许原样透传，不得生成明文记录。
- 优点是无需修改 mihomo 路由规则，且比 pktmon 端点快照更接近真正 PID 过滤。
- 阻断项：必须真机证明 LocalSystem/Session 0 能选择交互用户进程、不会与 Wintun 冲突、目标进程退出能自动停、
  不会捕获 agent/mihomo/mitmdump 自身。任一项失败就不能采用。

#### 方案 B：mihomo 定向路由到本地 MITM

```text
目标应用 ──> mihomo TUN ──进程+域名临时规则──> 127.0.0.1 MITM ──> 原默认出口
```

- agent 在会话期间生成最高优先级临时规则，只把目标 TCP HTTP(S) 送往 loopback MITM outbound；停止时事务化回滚。
- MITM 的服务端连接必须保留该流原本应使用的 direct/WG 出口，不能因为进入 localhost 代理而绕过原策略或
  kill-switch；需为上游出口建立明确映射，禁止用当前默认出口猜测。
- 必须证明原目标域名/IP、SNI、端口和进程关联在链路中不丢失，且 loopback 不被 TUN 再次捕获形成环路。
- 当前尚无真机证据证明该组合成立；验证前不能把示例 mihomo YAML 写入生产配置生成器。

Phase 4 spike 同时验证 A/B，按“最少改动现有数据面、精确目标、无递归、出口语义不变、可彻底回滚”选择；若两者
均不满足则 L4 保持不实现，不能用全局系统代理替代。

### 17.4 CA 与私钥生命周期

#### CA 生成和存储

- 每台设备生成唯一 `net-policy Local Inspection CA <device-id>`；禁止内置、跨设备复用或上传 CA 私钥。
- 根私钥只存 `%ProgramData%\net-policy\mitm\private\`，目录/文件 ACL 仅 SYSTEM 与 Administrators；再使用
  Windows DPAPI machine scope 加密。manifest 只记录证书 SHA-256 指纹、序列号和有效期。
- 若选定引擎只能读取 PEM 文件，agent 仅在会话启动时把 DPAPI 密文解到该会话的 SYSTEM-only 临时目录，句柄关闭后
  启动 sidecar；会话结束/崩溃恢复时立即删除。不得把明文 PEM 放进长期引擎配置目录。Phase 4a 必须验证引擎能从
  显式 CA 路径启动，不能接受它在用户 profile 中自行生成另一套 CA。
- CA 建议有效期 30 天；叶子证书按域名签发、最长 24 小时并设缓存上限。到期不静默续期，需 GUI 再确认。
- 私钥不得经命名管道导出、不得进入日志/崩溃转储/备份包；引擎进程只获得最小必要访问，退出后清理内存与临时
  叶子证书。

#### 信任安装

- 默认仅把**公钥证书**装入发起操作的交互用户 `CurrentUser\Root`，不写 `LocalMachine\Root`。这样不能覆盖的
  服务/其它用户应用视为不支持，而不是扩大机器级信任。
- GUI 显示证书主题、指纹、有效期和 Windows 确认界面；安装成功后重新读取证书库，按指纹验证，不能只信命令
  退出码。
- agent 持久化 `owner_sid + thumbprint + store_scope`。Remove 只能按三者精确删除本产品证书，禁止按模糊主题名
  批量删证书。
- CA 已安装但私钥缺失、指纹不符或过期时进入 `ca_broken`，禁止启动会话并引导“移除旧信任 → 重新创建”。
- uninstall、CA 过期和用户点击“关闭明文能力”都必须先停会话，再删除信任，最后安全删除私钥与明文产物。

### 17.5 目标模型与会话状态机

L4 不能复用只含进程名/路径的 `ProcessRef`，需定义实例身份：

```rust
struct ProcessInstanceRef {
    pid: u32,
    created_at_100ns: u64, // Windows process creation time，防 PID 复用
    path: String,           // agent 重新读取并 canonicalize
}

struct DecryptTarget {
    process: ProcessInstanceRef,
    domains: Vec<String>,  // 必填，精确域名或受限 suffix；最多 32 条
}

struct DecryptOpts {
    max_secs: u64,           // 默认 60，范围 10–300
    max_total_bytes: u64,    // 默认 64 MiB，最大 256 MiB
    max_body_bytes: u64,     // 单请求/响应正文默认 1 MiB，最大 16 MiB
    capture_bodies: bool,    // 默认 false：只采集方法/URL/状态/头
    force_tcp_for_quic: bool,// 默认 false；见 §17.7
    redact_profile: RedactProfile,
}
```

域名必须转小写、去尾点并经 IDNA 规范化；suffix 只能表达 `example.com` 及其子域，禁止客户端传任意正则。IP
目标不进入 L4 MVP，因为缺少可靠域名时无法安全签发/校验证书。目标进程路径或创建时间变化立即结束会话。

```text
checking_ca -> preparing -> decrypting -> stopping -> finalizing -> done
      │            │             │             │           │
      └────────────┴─────────────┴─────────────┴──────────> failed
```

- `checking_ca` 验证 owner SID、信任库指纹、私钥和引擎资产；`preparing` 才建立临时捕获/路由。
- 只有引擎健康检查、目标过滤器和回滚记录均就绪后才进入 `decrypting`。
- `stopping` 先撤流量拦截，确认新连接不再进入 MITM，再让旧连接排空（最多 3 秒）并强制结束 sidecar。
- `finalizing` 完成脱敏索引、产物校验和原子 rename。任何失败均先恢复网络，再处理文件。
- agent 重启后**默认不恢复 L4 会话**：清理可证明属于本产品的临时规则/sidecar，标
  `failed(agent_restarted)`；CA 信任保留但不自动解密。

### 17.6 明文数据模型、脱敏与存储

L4 产物与 pcapng 分目录保存：

```text
%ProgramData%\net-policy\decrypt\<session-id>\
  manifest.json
  flows.mitm          # 引擎原生流文件；仅显式选择“保留原始明文”时存在
  http.jsonl          # 结构化索引，一行一个 request/response/websocket event
  bodies\<sha256>     # 可选正文，按会话配额限制
```

结构化索引由一个**固定随安装包发布、启动前校验哈希**的最小引擎 addon 写入。production 禁止从 workspace、请求
参数或环境变量加载任意 Python 脚本/插件；否则普通用户可借 LocalSystem sidecar 执行代码。默认不启用引擎原生
flow 落盘，只有用户选择保留原始明文时才为本会话打开。

默认 `capture_bodies=false`，只记录：时间、进程实例、scheme/host/port/path、HTTP 方法/版本/状态、内容类型、
头部键名及脱敏后的值、请求/响应字节数、TLS 版本/ALPN、上游 IP/出口和错误。启用正文后：

- `Authorization`、`Proxy-Authorization`、`Cookie`、`Set-Cookie`、`X-Api-Key` 等默认值替换为
  `[REDACTED]`；URL query 中 `token/key/signature/password/session` 等键默认脱敏。
- `multipart/form-data` 文件正文默认不落盘，只记录字段名、文件名、媒体类型和大小；用户必须第三次确认才能保留。
- 未知二进制正文默认只记录 SHA-256、媒体类型与大小；文本正文按 UTF-8 安全解码并设单体上限。
- `RedactProfile::Default` 不允许关闭核心凭据脱敏；只有 `Raw` 会话可保留，UI 必须显示不可消除的红色标识，
  最长 60 秒且禁止自动保留。
- 流式/无限响应在达到单体上限后截断并记录 `body_truncated=true`，不能继续消耗总配额。

明文目录 ACL 与 §9 一致，但总配额独立：默认 256 MiB、最多 5 个会话、默认保留 24 小时；Raw 会话在 GUI
成功导出或 15 分钟后删除，以先到者为准。删除采用正常文件删除并明确说明 SSD/日志型文件系统上不承诺物理安全
擦除。

导出格式：

- 默认导出脱敏后的 `http.jsonl + manifest` ZIP；
- 可选导出引擎原生 flow，要求安装同版本分析工具；
- HAR 仅作为后续能力，因为 HAR 对 WebSocket、流式正文和某些二进制信息并不完整；导出时必须标注损失。

### 17.7 TLS/协议兼容性与降级

| 场景 | 预期行为 |
|---|---|
| 使用 Windows 用户信任库的普通 TLS 客户端 | 可解密，仍需验证 HTTP/1.1、HTTP/2、WebSocket |
| Certificate Pinning / 自带 CA bundle | 客户端通常拒绝伪造叶子证书；标 `client_rejected_cert`，不得宣称已解密 |
| mTLS | 默认不支持；不采集/代理客户端私钥，连接失败后提示排除该域名 |
| QUIC / HTTP/3 | 默认旁路且记录 `quic_not_decrypted`；不假装拥有明文 |
| ECH / 无 SNI | 若无法从目标 allowlist 和原始目标唯一确定域名则拒绝 MITM，不能签通配“猜测证书” |
| 非 HTTP TLS | 默认原样透传，只记录协议不支持；不把任意 TLS 载荷当 HTTP 解析 |
| 证书错误/上游证书无效 | 严格校验上游证书并向客户端返回失败；禁止 `skip-cert-verify` |

`force_tcp_for_quic=true` 时，agent 可在**目标进程 + allowlist 域名**范围临时阻断 UDP/443，促使支持回退的应用
改用 TCP/TLS。它会改变应用行为，且并非所有客户端都会回退，因此：

- 必须单独确认，默认关闭；
- 临时规则有会话 ID、明确优先级和事务化回滚；
- 仅当 mihomo 规则能同时约束进程、域名、UDP/443 时启用，不能扩大为全机 UDP/443；
- 回退失败只报告 `quic_fallback_failed`，不得继续扩大阻断范围。

对于 pinning 失败，MVP 不做自动“失败后透明重放”：原请求可能不可安全重放，且 POST/支付等有副作用。用户可
停止会话、把域名移出 allowlist 后重试。产品不得提供绕过 pinning、注入应用或 patch 客户端校验的能力。

### 17.8 L4 协议扩展（建议 protocol 1.6）

L3 的 protocol 1.5 只声明 `capture_v1`。L4 建议升到 1.6，并仅在 CA、引擎与平台探测通过后声明
`decrypt_v1`：

```rust
// CA 管理
DecryptCaStatus
DecryptCaCreate           // 生成私钥，只返回公钥 DER 分块句柄/指纹
DecryptCaConfirmInstalled { thumbprint: String, owner_sid: String }
DecryptCaRemove

// 会话
DecryptStart  { target: DecryptTarget, opts: DecryptOpts }
DecryptStop   { id: String }
DecryptGet    { id: String }
DecryptList
DecryptDelete { id: String }
DecryptRead   { id: String, artifact: DecryptArtifact, offset: u64, len: u32 }
```

新增稳定错误码：`decrypt_unsupported`、`decrypt_ca_missing`、`decrypt_ca_broken`、`decrypt_target_stale`、
`decrypt_engine_unhealthy`、`decrypt_conflict`、`decrypt_limit_reached`、`decrypt_client_rejected_cert`、
`decrypt_quic_not_supported`、`decrypt_finalize_failed`。

- CA Create 与 ConfirmInstalled 分开：agent 生成 CA 后，GUI 在当前用户上下文安装公钥，再把指纹/SID 交给 agent
  复核；单个请求不能静默完成整条信任链。
- `DecryptRead` 复用 §10 的 512 KiB base64 分块与路径隔离；artifact 使用枚举，客户端不能传文件名。
- 明文会话与 L3 pktmon 会话、apply/reload/stop 全部互斥。CA Status/List/Get/Read 不占网络长操作锁。
- 每次 Start/Stop/CA Create/Install Confirm/Remove/Raw Export 写入不可由“清空普通事件”删除的安全审计日志；日志
  只记元数据和指纹，不记正文/凭据。

### 17.9 UI

- “应用明文”单独页面，不放在“完整包”复选框旁；首次进入只展示原理、风险和“创建调试 CA”。
- CA 卡片显示作用域、owner、指纹、有效期、私钥状态和“一键移除”。CA 安装后但未解密时显示“已信任，未拦截”。
- Start 必须从当前活跃进程实例选择，再输入域名 allowlist；不接受仅进程名的后台模糊目标。
- 会话中显示每域名 `decrypted / passthrough / pinned / quic / failed` 计数，不能只显示总“成功”。
- 明文详情默认折叠敏感头和正文，查看 Raw 内容需临时二次确认且不写前端持久缓存。
- 停止按钮始终可见；网络姿态切换时提示先停止明文会话，不能由 GUI 在后台自动替用户确认。

### 17.10 WireGuard 代理订阅（双槽位）

为支持 WireGuard 出口所在网络封锁外层 UDP、但允许通过代理访问订阅节点的场景，net-policy 内置
mihomo 订阅管理，不再依赖 Clash Verge 常驻进程：

- 设置中固定两个订阅槽位（名称、HTTP(S) URL、更新间隔）和一个激活槽位；保存后运行中的 mihomo 热加载。
- WireGuard 上游代理来源可选“手工 SOCKS5/HTTP”或“订阅”。订阅模式生成 `proxy-providers`、`wg-dialer`
  选择组，并将 WG outbound 的 `dialer-proxy` 指向该组；mihomo 负责拉取订阅、解析节点和自动更新。
- 任意时刻只生成当前激活订阅 provider，切换只改变 provider 引用，不修改 WireGuard 密钥和路由规则。
- URL 仅接受 `http://`/`https://`，名称、更新间隔和槽位引用在 agent 保存前校验。订阅凭据和节点内容不写日志，
  配置只保留订阅 URL；订阅下载失败由 mihomo 报告，策略保持 fail-closed。
- 订阅模式仍要求 WireGuard endpoint 可校验；没有激活订阅时可回退到该代理槽位，未配置代理则保持直连/原有
  WireGuard 行为。

## 18. L4 实施与验收门槛

### 18.1 Phase 4a：引擎与数据面 spike

> **进展（0.228，2026-07-16）**：引擎锁定 = mitmproxy 12.2.3（ADR）；Defender 查杀已由安装程序加排除解决，
> `mitmdump --version` 跑通；引擎部署已进 `install-mitm-engine`。
>
> **数据面 spike（`mode local:curl.exe`，直连姿态 fake-ip 生效）实测**（详见 ADR §6.2）：
> ✅ **WinDivert 加载并与 mihomo Wintun 共存无冲突**（spike 后 `mihomo_running/tun_ready` 仍 true）；
> ✅ **Local Capture 拦截目标进程**；✅ **真实解密成立**（解出明文 `GET / Host: example.com`）；
> 🚫 **方案 A × fake-ip 上游冲突**：WinDivert 在 DNS 解析后抓包，mitmdump 拿到的是 fake-ip 目的地，直连该
> fake-ip 做上游 TLS 失败（502）。→ 方案 A 在 fake-ip 下上游路由不成立，需转 **方案 B**（mihomo 按域名把目标
> 进程送 loopback MITM）或让上游走 mihomo 代理，为后续 spike。**CA 信任库安装本轮未做**（数据面未通前装
> CA 无用且留 MITM 根证书；用 `--cacert` 证明解密，未碰信任库——实测两个 Root 存均 0 张 mitmproxy 证书）。

1. 锁定候选引擎版本，验证签名/哈希、许可证、离线安装、冷启动、升级和彻底卸载。
2. 在 Windows 服务 LocalSystem/Session 0 下，验证对交互用户 PID 的选择、PID 复用防护与进程退出停止。
3. 验证与 mihomo gVisor TUN、direct/WG、kill-switch、watchdog、DNS fake-ip 同时工作且不递归、不绕路。
4. 验证域名 allowlist 外真正透传且不产生明文；进程外流量完全不进入解密引擎。
5. 对 A/B 数据面形成 ADR；未得到确定结果则停止，不进入 CA 安装。

### 18.2 Phase 4b：CA 安全闭环

- CurrentUser Root 安装/查询/精确删除；多 Windows 用户隔离；过期、私钥丢失、指纹篡改恢复。
- 私钥 ACL + DPAPI、崩溃转储/日志扫描、普通用户读取/替换/reparse point 攻击测试。
- agent/GUI/安装器/卸载器各种退出顺序均不遗留临时规则；卸载后浏览器证书库无本产品 CA。
- CA 存在但 agent/引擎未运行时不改变任何连接行为。

### 18.3 Phase 4c：协议与应用兼容性

- HTTP/1.1 keep-alive、HTTP/2 多路复用、WebSocket、重定向、压缩、chunked、流式大正文、二进制与异常断线。
- Chrome/Edge、Windows WebView2、reqwest/curl，以及至少一个 pinning、自带 CA、mTLS、QUIC/HTTP3 样本。
- 上游证书过期/域名不匹配/撤销不可用时 fail-closed，不设置 `skip-cert-verify`。
- force TCP 只影响目标进程+域名，停止/崩溃后 UDP/443 规则完全回滚。

### 18.4 Phase 4d：数据与隐私

- 默认脱敏 golden tests；凭据键大小写、重复头、query/form/json/multipart、压缩正文均覆盖。
- 单正文/总会话/目录配额，磁盘写满，导出中断续传，保留期与 Raw 15 分钟删除。
- UI、Tauri、agent 日志、panic 日志、SQLite、事件流均扫描确认不含正文、Cookie、Authorization 或 CA 私钥。
- 非 owner SID 不能 List/Get/Read/Delete/Remove CA；未来多用户支持前该项不通过就保持单用户范围。

### 18.5 L4 完成定义

只有同时满足以下条件才可标记“支持应用明文”：

- 用户能明确知道何时信任 CA、何时正在解密、哪些目标被解密，以及如何立即停止/移除；
- pinning、mTLS、QUIC/ECH 和非 HTTP TLS 均有诚实、可观测、不会扩大拦截面的失败结果；
- direct/WG/阻断三姿态和 fail-closed 结论不因 L4 改变，解密引擎失败不造成策略泄漏；
- agent/GUI/引擎崩溃与卸载后无临时转发/阻断规则，CA/私钥可验证地清除；
- 默认产物已脱敏、受 ACL 与配额保护，Raw 模式有更短时限且不可后台静默开启；
- 仓库完整修复循环及 Windows 真机矩阵全部通过，并产出
  `docs/net-policy/net-policy-decrypt-validation-report.md`。

## 19. 参考

- [Microsoft Learn: pktmon start](https://learn.microsoft.com/windows-server/administration/windows-commands/pktmon-start)
- [Microsoft Learn: pktmon command formatting and limits](https://learn.microsoft.com/windows-server/networking/technologies/pktmon/pktmon-syntax)
- [Microsoft Learn: pktmon filter](https://learn.microsoft.com/windows-server/administration/windows-commands/pktmon-filter)
- [mihomo: 域名嗅探](https://wiki.metacubex.one/config/sniff/)
- [mihomo: TUN 配置](https://wiki.metacubex.one/config/inbound/tun/)
- [mitmproxy: Proxy Modes](https://docs.mitmproxy.org/stable/concepts/modes/)
- [mitmproxy: Certificates](https://docs.mitmproxy.org/stable/concepts/certificates/)
- [mitmproxy: Certificate pinning and ignored domains](https://docs.mitmproxy.org/stable/howto/ignore-domains/)
- [mitmproxy: Installation and binary security considerations](https://docs.mitmproxy.org/stable/overview/installation/)
