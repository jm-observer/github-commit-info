import { invoke } from '@tauri-apps/api/core'

// ── 与后端 net_policy 命令对齐的类型 ───────────────────────────────────────────

// direct/wg/proxy 可用作规则或默认出口；blackhole 主要用于默认阻断。
export type Route = 'direct' | 'wg' | 'proxy' | 'blackhole'
export type RuleKind = 'process-path' | 'process-name' | 'domain-suffix' | 'domain-keyword' | 'ip-cidr'

export interface Rule {
  kind: RuleKind
  value: string
  route: Route
}

export interface RuleSet {
  rules: Rule[]
  groups: unknown[]
}

/** AmneziaWG 混淆参数（对应 mihomo amnezia-wg-option）。填了即让 mihomo 以 AmneziaWG 方式
 *  握手，破坏原生 WireGuard 固定包特征以规避 DPI 丢包。客户端/服务端参数必须完全一致。 */
export interface AmneziaConfig {
  jc: number
  jmin: number
  jmax: number
  s1: number
  s2: number
  s3: number
  s4: number
  h1: number
  h2: number
  h3: number
  h4: number
}

export type WgDialerProxyKind = 'socks5' | 'http'

export interface WgDialerProxy {
  type: WgDialerProxyKind
  server: string
  port: number
  username: string
  password: string
  udp: boolean
  subscription_slot?: number | null
}

export interface ProxySubscription {
  name: string
  url: string
  interval_secs: number
}

export interface ProxySubscriptions {
  first: ProxySubscription | null
  second: ProxySubscription | null
  active: number | null
}

export interface ProxyNode {
  name: string
  type: string
  alive: boolean
  delay_ms: number | null
}

export interface LocalProxyListeners {
  /** 本机 SOCKS5 监听端口。 */
  socks_port: number
  /** 本机 HTTP 代理端口；HTTPS 请求通过 CONNECT 转发。 */
  http_port: number
}

export interface WgConfig {
  server: string
  port: number
  ip: string
  private_key: string
  public_key: string
  pre_shared_key: string
  mtu: number
  /** 可选：AmneziaWG 混淆。缺省 = 标准 WireGuard。导入带 Jc/H1.. 的 .conf 时自动带出。 */
  amnezia?: AmneziaConfig | null
  /** 可选：通过本地 Clash Verge/Mihomo 代理建立 WireGuard endpoint 连接。 */
  dialer_proxy?: WgDialerProxy | null
}

export interface Settings {
  wg: WgConfig
  dns_bootstrap: string[]
  lan_ranges: string[]
  killswitch_enabled: boolean
  block_ipv6: boolean
  /** 默认出口（未命中规则的兜底）：'blackhole'（全阻断）或 'wg'（全走海外）。 */
  default_route: Route
  /** 主开关：是否「启动即生效」（持久化，开启后每次启动自动恢复策略）。 */
  enabled: boolean
  /** L2 域名嗅探（mihomo sniffer）：补全 TLS SNI / HTTP Host / QUIC 域名，只增强观察数据。默认关。 */
  sniffer_enabled: boolean
  proxy_subscriptions: ProxySubscriptions
  local_proxy: LocalProxyListeners
}

export interface FirewallStatus {
  default_outbound: string
  rule_count: number
  active: boolean
}

export interface Status {
  platform_supported: boolean
  wg_configured: boolean
  killswitch_enabled: boolean
  applied: boolean
  mihomo_running: boolean
  tun_ready: boolean
  protected: boolean
  protection_validated: boolean
  firewall: FirewallStatus | null
  /** 默认出口（'blackhole' 全阻断 / 'wg' 全走海外）。 */
  default_route: Route
  /** 主开关：是否已启用「启动即生效」。 */
  enabled: boolean
  /** 当前进程是否以管理员身份运行（改防火墙/建 TUN 需要）。false → 提示并禁用「开始观察」。 */
  elevated: boolean
  /** 记录库是否已降级为内存（磁盘打开失败，历史将在重启后丢失）。 */
  record_store_degraded?: boolean
}

// ── 记录 / 进程树 / 路由 / 临时直连（minor 2） ─────────────────────────────────

export interface RequestLogEntry {
  ts_ms: number
  conn_id: string
  process: string
  process_path: string
  host: string
  dest_ip: string
  dest_port: string
  network: string
  outbound: string
  rule: string
}

export interface LifecycleEvent {
  ts_ms: number
  kind: string
  detail: string
}

export interface ProcessNode {
  pid: number
  ppid: number
  name: string
  path: string
  children: ProcessNode[]
}

/** 一条生效路由（priority = 匹配顺序；source = builtin_lan/temp_except/group/rule/default）。 */
export interface RouteEntry {
  priority: number
  kind: string
  value: string
  route: Route
  /** 实际生效出口；不等于 route 即发生了降级（出口不可用按 fallback 处理）。undefined = 与 route 相同。 */
  applied_route?: Route
  source: string
  deletable: boolean
}

/** Windows 实际系统路由（GUI 直接读取，不依赖 agent 或策略引擎）。 */
export interface SystemRoute {
  destination_prefix: string
  next_hop: string
  interface_alias: string
  interface_index: number
  route_metric: number
  interface_metric: number
  protocol: string
  state: string
  address_family: string
}

export interface ProcessRef {
  kind: 'process_path' | 'process_name'
  value: string
}

export interface TempDirectStatus {
  active: boolean
  until_ms: number | null
  remaining_secs: number
  except: ProcessRef[]
}

export interface ProcessCandidate {
  pid: number
  name: string
  path: string
  remotes: string[]
}

export interface VerifyCase {
  id: string
  name: string
  status: string
  observed: string
}

export interface VerifyReport {
  mihomo_running: boolean
  cases: VerifyCase[]
}

// ── 抓包（minor 5，抓包设计 §10/§12） ─────────────────────────────────────────
/** 抓包目标：All 全 TUN，或定向进程/域名/IP（与 core CaptureTarget 的 tag=target/content=value 对齐）。 */
export type CaptureTarget =
  | { target: 'all' }
  | { target: 'process'; value: ProcessRef }
  | { target: 'domain'; value: string }
  | { target: 'ip'; value: string }

export interface CaptureOpts {
  /** 每包保留字节数；0=完整包（高敏感）。默认 128。 */
  snap_len: number
  /** circular 容量上限（MiB）。默认 128，范围 16–512。 */
  file_size_mib: number
  /** 时间上限（秒）。默认 120，范围 10–600。 */
  max_secs: number
}

export type CaptureState =
  | 'preparing' | 'running' | 'stopping' | 'converting' | 'done' | 'failed' | 'orphaned'

export type CaptureStopReason = 'user' | 'timeout' | 'agent_restart' | 'error'

export interface CaptureSession {
  id: string
  state: CaptureState
  target: CaptureTarget
  opts: CaptureOpts
  endpoint_count: number
  started_ms: number
  ended_ms: number | null
  stop_reason: CaptureStopReason | null
  /** 仅 done 时给文件名（capture.pcapng），不给绝对路径。 */
  file_name: string | null
  bytes: number | null
  known_limits: string[]
  error: { kind: string; message: string } | null
}

export interface CaptureChunk {
  offset: number
  data_base64: string
  eof: boolean
}

// ── L4 应用明文（Decrypt*/DecryptCa*，抓包设计 §17） ──────────────────────────

/** 精确进程实例（防 PID 复用：pid + 创建时间 + 规范化路径）。 */
export interface ProcessInstanceRef {
  pid: number
  created_at_100ns: number
  path: string
}

/** 脱敏档：default=核心凭据强制脱敏（不可关）；raw=保留原文（UI 须红标 + 更短时限）。 */
export type RedactProfile = 'default' | 'raw'

/** 解密目标：精确进程实例 + 必填域名 allowlist（无 All target）。 */
export interface DecryptTarget {
  process: ProcessInstanceRef
  domains: string[]
}

export interface DecryptOpts {
  /** 时长上限（秒）。默认 60，范围 10–300。 */
  max_secs: number
  /** 总字节配额。默认 64MiB，最大 256MiB。 */
  max_total_bytes: number
  /** 单请求/响应正文上限。默认 1MiB，最大 16MiB。 */
  max_body_bytes: number
  /** 默认 false：只记方法/URL/状态/头，不落正文。 */
  capture_bodies: boolean
  /** 默认 false：临时阻 UDP/443 逼 QUIC 回退 TCP（§17.7，改变应用行为，须单独确认）。 */
  force_tcp_for_quic: boolean
  redact_profile: RedactProfile
}

export type DecryptState =
  | 'checking_ca' | 'preparing' | 'decrypting' | 'stopping' | 'finalizing' | 'done' | 'failed'

export type CaState = 'absent' | 'installed' | 'broken'

/** CA 信任状态（私钥永不出现）。 */
export interface CaStatus {
  state: CaState
  thumbprint: string | null
  subject: string | null
  not_after_ms: number | null
  /** 安装到的用户 SID（CurrentUser\Root）。 */
  owner_sid: string | null
  /** 固定 current_user。 */
  store_scope: string | null
}

/** 每域名处理计数（§17.9：不只显示总"成功"）。 */
export interface DomainCounters {
  decrypted: number
  passthrough: number
  pinned: number
  quic: number
  failed: number
}

export interface DecryptSession {
  id: string
  state: DecryptState
  target: DecryptTarget
  opts: DecryptOpts
  started_ms: number
  ended_ms: number | null
  /** 键为规范化域名。 */
  per_domain: Record<string, DomainCounters>
  error: { kind: string; message: string } | null
}

export type DecryptArtifact = 'manifest' | 'http_jsonl' | 'flows'

// ── 统一出口（Egress*，minor 8，出口设计 §8.8） ────────────────────────────────
// 核心约束：出口是否已启动/已连接，与当前是否有业务流量经过它，是两个独立问题。
// `EgressStatus` 把「生命周期」（lifecycle）与「策略是否选中」（selected/usage）分成两个
// 字段——前端不得由其一推断另一个，卡片必须同时展示两者。

export type EgressKind = 'direct' | 'wire_guard' | 'proxy'

/**
 * 出口数据面归谁所有——决定「停掉 mihomo 后这个出口还在不在」。
 *
 * 第一阶段 WG/代理都是 `mihomo-managed`：隧道随引擎存亡、reload 会重建。UI **必须**把这点讲
 * 明白，否则「已就绪」会被误读成「这是个能独立存活的出口」。
 */
export type EgressManagement = 'mihomo-managed' | 'system' | 'independent'

export const EGRESS_MANAGEMENT_LABELS: Record<EgressManagement, string> = {
  'mihomo-managed': '由 mihomo 承载',
  system: '系统承载',
  independent: '独立进程',
}

/** 该出口的可用性是否依赖 mihomo 进程在跑。 */
export function egressDependsOnEngine(m: EgressManagement): boolean {
  return m === 'mihomo-managed'
}

export type EgressLifecycle =
  | 'stopped' | 'starting' | 'connecting' | 'ready' | 'degraded' | 'reconnecting' | 'failed'

/** 生命周期展示文案（与后端 `EgressLifecycle::label()` 对齐）。 */
export const EGRESS_LIFECYCLE_LABELS: Record<EgressLifecycle, string> = {
  stopped: '已停止',
  starting: '启动中',
  connecting: '连接中',
  ready: '已就绪',
  degraded: '降级',
  reconnecting: '重连中',
  failed: '失败',
}

/** 该生命周期是否承载业务流量（与后端 `EgressLifecycle::accepts_traffic()` 对齐）。 */
export function egressAcceptsTraffic(l: EgressLifecycle): boolean {
  return l === 'ready' || l === 'degraded'
}

export type HealthState = 'unknown' | 'healthy' | 'degraded' | 'unhealthy'

export interface HealthReport {
  state: HealthState
  /** 探测完成时刻（epoch 毫秒）；0=从未探测。 */
  checked_at_ms: number
  latency_ms?: number | null
  /** 探测目标（脱敏后的 URL / 主机）。 */
  target?: string | null
  error?: string | null
}

export interface WireGuardDetail {
  /** 脱敏 endpoint（如 1.2.3.x:51820）。 */
  endpoint: string
  local_ip: string
  mtu: number
  /** 是否启用 AmneziaWG 混淆。 */
  obfuscation: boolean
  via_dialer_proxy: boolean
  /**
   * 最近一次**经该 outbound 主动探测成功**的时刻（epoch 毫秒，0=从未）。
   *
   * 这不是 WireGuard 的 latest-handshake——数据面在 mihomo 进程内，拿不到 peer 握手时间。
   * 决议 §6.2 禁止展示虚假的 latest-handshake / rx-tx / 网卡状态，UI 措辞必须是「探测」。
   */
  last_probe_ok_at_ms: number
}

export interface ProxyDetail {
  subscription: string
  node: string
  node_delay_ms?: number | null
  node_alive: boolean
  node_count: number
  refreshed_at_ms: number
}

export interface DirectDetail {
  /** 物理出口网卡别名（已排除 mihomo 的 Meta TUN；未知时空串）。 */
  interface: string
  /** 物理默认网关（未知时空串）。 */
  gateway: string
}

/** 类型相关详情，三选一（缺省全空也合法）。 */
export interface EgressDetail {
  wireguard?: WireGuardDetail | null
  proxy?: ProxyDetail | null
  direct?: DirectDetail | null
}

export interface EgressUsage {
  /** 是否是当前默认出口（兜底 MATCH）。 */
  is_default: boolean
  /** 有多少条规则/程序组指向它。 */
  rule_count: number
}

/** 出口向路由器暴露的目标（mihomo outbound 名 / direct / reject）。 */
export interface RouteTarget {
  kind: 'mihomo-outbound' | 'direct' | 'reject'
  name: string
}

export type EgressFallback = 'block' | 'direct'

/** 前端出口 DTO：生命周期（lifecycle）与策略选中（selected/usage）分两个字段，不得互相推断。 */
export interface EgressStatus {
  id: string
  name: string
  kind: EgressKind
  /** 数据面归属。`mihomo-managed` 时「已就绪」只代表当前引擎里这条出口通。 */
  management: EgressManagement
  lifecycle: EgressLifecycle
  /** 当前是否被策略导流（= usage.is_default || usage.rule_count > 0）。 */
  selected: boolean
  usage: EgressUsage
  /** 当前实际经该出口的活跃连接数，不等于策略选中数量。 */
  active_connections: number
  /** 配置是否完整可用（WG 校验通过 / 订阅已激活）；false 时不该出现在「Ready」。 */
  configured: boolean
  unconfigured_reason?: string | null
  health: HealthReport
  detail: EgressDetail
  route: Route
  route_target: RouteTarget
  /** 出口不可用时该出口上的规则如何处理。 */
  fallback: EgressFallback
  reconnect_count: number
  /** 最近一次生命周期变迁时刻（epoch 毫秒）。 */
  changed_at_ms: number
  last_error?: string | null
}

/** 「策略选了它但它承载不了」——UI 必须显式警告（与后端 `selected_but_unusable()` 对齐）。 */
export function egressSelectedButUnusable(e: EgressStatus): boolean {
  return e.selected && !egressAcceptsTraffic(e.lifecycle)
}

/** 前端 `listen('net-policy://egress-changed')` 订阅的频道名——**与 APPLY_PROGRESS_EVENT 分开
 *  推送**，只更新出口卡片，不得据此推断导流策略变化。 */
export const EGRESS_CHANGED_EVENT = 'net-policy://egress-changed'

/** DecryptOpts 默认值（与 core DecryptOpts::default 对齐）。 */
export const DEFAULT_DECRYPT_OPTS: DecryptOpts = {
  max_secs: 60,
  max_total_bytes: 64 * 1024 * 1024,
  max_body_bytes: 1024 * 1024,
  capture_bodies: false,
  force_tcp_for_quic: false,
  redact_profile: 'default',
}

// ── 活跃连接快照（P0-1，net_policy_connections） ───────────────────────────────

export interface Connection {
  chains: string[]
  outbound: string
  host: string
  destination_ip: string
  destination_port: string
  process: string
  rule: string
  network: string
}

export interface ConnectionsSnapshot {
  available: boolean
  total: number
  wg_count: number
  proxy_count: number
  direct_count: number
  other_count: number
  by_process: Record<string, number>
  connections: Connection[]
}

// ── Phase 4 可观测：被阻断 feed + 域名↔IP/进程 关联 ────────────────────────────

/** 一条被阻断尝试（默认黑洞下「什么被挡了」，按 network|host|port 去重）。 */
export interface BlockedEntry {
  network: string
  /** 目标 host（域名或 IP 字面量）。 */
  host: string
  /** host 是 IP 时等于 host；域名时为空。 */
  dest_ip: string
  dest_port: string
  rule: string
  outbound: string
  count: number
  last_ms: number
}

/** 域名↔IP/进程 关联的一行（累积自历次活跃连接）。 */
export interface DomainAssoc {
  domain: string
  ips: string[]
  processes: string[]
  count: number
  last_ms: number
}

// ── apply 进度事件（Phase 2，listen('net-policy://apply-progress')） ────────────

export const APPLY_PROGRESS_EVENT = 'net-policy://apply-progress'

export interface ApplyProgress {
  step: number
  name: string
  status: 'running' | 'ok' | 'fail'
  detail: string | null
}

/** apply 的 6 个阶段（与后端 APPLY_STEPS 对齐，索引从 0 起）。 */
export const APPLY_STEPS = [
  '校验配置',
  '装防火墙基线',
  '启动引擎',
  '等待 TUN 起栈',
  '补 TUN 白名单',
  '验证连通',
]

// 所有命令以 net_policy_ 前缀，集中包装（仿 SpeechAPI）。
export const NetPolicyAPI = {
  getStatus: () => invoke<Status>('net_policy_get_status'),
  getConnections: () => invoke<ConnectionsSnapshot>('net_policy_connections'),
  getProxyNodes: () => invoke<ProxyNode[]>('net_policy_proxy_nodes'),
  testProxyNode: (name: string) => invoke<ProxyNode>('net_policy_test_proxy_node', { name }),
  getSettings: () => invoke<Settings>('net_policy_get_settings'),
  saveSettings: (settings: Settings) => invoke('net_policy_save_settings', { settings }),
  parseWgConf: (content: string) => invoke<WgConfig>('net_policy_parse_wg_conf', { content }),
  listRules: () => invoke<RuleSet>('net_policy_list_rules'),
  /** upsert：同目标（kind+value，忽略 route）的旧规则会被后端先移除再追加，返回最新 RuleSet。 */
  saveRule: (rule: Rule) => invoke<RuleSet>('net_policy_save_rule', { rule }),
  /** 按 kind+value 匹配删除（忽略 route）；找不到返回 Err「未找到该规则（可能已被删除）」。 */
  deleteRule: (rule: Rule) => invoke<RuleSet>('net_policy_delete_rule', { rule }),
  listProcessCandidates: () => invoke<ProcessCandidate[]>('net_policy_list_process_candidates'),
  apply: () => invoke<Status>('net_policy_apply'),
  emergencyStop: () => invoke<Status>('net_policy_emergency_stop'),
  /** 主开关（启动即生效）：开 → 立即 apply；关 → 若在运行则停。 */
  setEnabled: (enabled: boolean) => invoke<Status>('net_policy_set_enabled', { enabled }),
  /** 热重载（逐项放行不重启隧道）：已应用时原地生效，未应用为 no-op。 */
  reload: () => invoke<Status>('net_policy_reload'),
  /** 被阻断尝试 feed（默认黑洞下「什么被挡了」）。 */
  blocked: () => invoke<BlockedEntry[]>('net_policy_blocked'),
  /** 清空被阻断 feed。 */
  clearBlocked: () => invoke('net_policy_clear_blocked'),
  /** 域名↔IP/进程 关联快照。 */
  dnsMap: () => invoke<DomainAssoc[]>('net_policy_dns_map'),
  verify: () => invoke<VerifyReport>('net_policy_verify'),

  // ── 记录 / 进程树 / 路由 / 临时直连（minor 2） ─────────────────────────────
  /** 历史进程请求记录（最近 limit 条，倒序；后端上限 1000）。 */
  requests: (limit = 500) => invoke<RequestLogEntry[]>('net_policy_requests', { limit }),
  /** 生命周期事件（启停 / 策略 / 临时直连，最近 limit 条）。 */
  events: (limit = 200) => invoke<LifecycleEvent[]>('net_policy_events', { limit }),
  /** 生效路由列表（含优先级/来源/是否可删）。 */
  routes: () => invoke<RouteEntry[]>('net_policy_routes'),
  /** Windows 当前系统路由表；纯只读，不依赖 agent 或策略是否启用。 */
  systemRoutes: () => invoke<SystemRoute[]>('net_policy_system_routes'),
  /** 当前进程树。 */
  processTree: () => invoke<ProcessNode[]>('net_policy_process_tree'),
  /** 临时直连状态（剩余时间等）。 */
  tempStatus: () => invoke<TempDirectStatus>('net_policy_temp_status'),
  /** 开启临时直连（限时应急）：durationSecs 后自动还原；except 进程被强制 Blackhole。 */
  tempDirectOn: (durationSecs: number, except: ProcessRef[]) =>
    invoke<TempDirectStatus>('net_policy_temp_direct_on', { durationSecs, except }),
  /** 提前解除临时直连。 */
  tempDirectOff: () => invoke<TempDirectStatus>('net_policy_temp_direct_off'),
  /** 清空请求记录（隐私）。 */
  clearRequests: () => invoke('net_policy_clear_requests'),
  /** 清空生命周期事件。 */
  clearEvents: () => invoke('net_policy_clear_events'),

  // ── 连接重置 / 运行日志（minor 3） ────────────────────────────────────────
  /** 关闭 mihomo 所有活跃连接，逼流量用新出口重连（切姿态后 best-effort 调用）。 */
  resetConnections: () => invoke<void>('net_policy_reset_connections'),
  /** mihomo / WireGuard 运行日志（最近 limit 行；后端上限 1000）。 */
  getMihomoLog: (limit = 500) => invoke<string[]>('net_policy_get_mihomo_log', { lines: limit }),

  // ── 抓包（minor 5，抓包设计 §10/§12） ──────────────────────────────────
  captureStart: (target: CaptureTarget, opts: CaptureOpts) =>
    invoke<CaptureSession>('net_policy_capture_start', { target, opts }),
  captureStop: (id: string) => invoke<CaptureSession>('net_policy_capture_stop', { id }),
  captureGet: (id: string) => invoke<CaptureSession>('net_policy_capture_get', { id }),
  captureList: () => invoke<CaptureSession[]>('net_policy_capture_list'),
  captureDelete: (id: string) => invoke<void>('net_policy_capture_delete', { id }),
  captureRead: (id: string, offset: number, len: number) =>
    invoke<CaptureChunk>('net_policy_capture_read', { id, offset, len }),

  // ── L4 应用明文（minor 6，抓包设计 §17） ──────────────────────────────────
  /** 查 CA 信任状态。 */
  decryptCaStatus: () => invoke<CaStatus>('net_policy_decrypt_ca_status'),
  /** 生成专用调试 CA（agent 侧 + DPAPI 私钥保护；不装信任库）。 */
  decryptCaCreate: () => invoke<CaStatus>('net_policy_decrypt_ca_create'),
  /** 装公钥进 CurrentUser\Root（弹 Windows 确认框）→ 实查验证 → 交 agent 复核。 */
  decryptCaInstall: () => invoke<CaStatus>('net_policy_decrypt_ca_install'),
  /** 移除本产品 CA（信任库证书 + agent 私钥/记录）。 */
  decryptCaRemove: () => invoke<CaStatus>('net_policy_decrypt_ca_remove'),
  decryptStart: (target: DecryptTarget, opts: DecryptOpts) =>
    invoke<DecryptSession>('net_policy_decrypt_start', { target, opts }),
  decryptStop: (id: string) => invoke<DecryptSession>('net_policy_decrypt_stop', { id }),
  decryptGet: (id: string) => invoke<DecryptSession>('net_policy_decrypt_get', { id }),
  decryptList: () => invoke<DecryptSession[]>('net_policy_decrypt_list'),
  decryptDelete: (id: string) => invoke<void>('net_policy_decrypt_delete', { id }),
  decryptRead: (id: string, artifact: DecryptArtifact, offset: number, len: number) =>
    invoke<CaptureChunk>('net_policy_decrypt_read', { id, artifact, offset, len }),

  // ── 统一出口（minor 8，出口设计 §8.8） ────────────────────────────────────
  // 六个操作语义互不重叠，全部回出口全量清单；改导流策略走 saveSettings/saveRule，不在这里。
  /** 出口全量清单。 */
  egressList: () => invoke<EgressStatus[]>('net_policy_egress_list'),
  /** 启动出口（渲染进配置 + 立即探测）。不改变任何导流规则。 */
  egressStart: (id: string) => invoke<EgressStatus[]>('net_policy_egress_start', { id }),
  /** 停止出口（从配置摘除；指向它的规则按 fallback 处理，默认阻断）。 */
  egressStop: (id: string) => invoke<EgressStatus[]>('net_policy_egress_stop', { id }),
  /** 重置该出口上的存量连接并重新探测（不改导流；mihomo-managed 出口无隧道可重建）。 */
  egressReconnect: (id: string) => invoke<EgressStatus[]>('net_policy_egress_reconnect', { id }),
  /** 仅测试连接：探测一次，不改生命周期也不改导流策略。 */
  egressProbe: (id: string) => invoke<EgressStatus[]>('net_policy_egress_probe', { id }),
  /** 设置出口不可用时的处理方式（阻断 / 明确允许回落直连）。 */
  egressSetFallback: (id: string, fallback: EgressFallback) =>
    invoke<EgressStatus[]>('net_policy_egress_set_fallback', { id, fallback }),
  /** 刷新代理订阅，不主动重连当前节点。 */
  egressRefreshSubscription: (id: string) =>
    invoke<EgressStatus[]>('net_policy_egress_refresh_subscription', { id }),
  /** 切换代理订阅当前节点。 */
  egressSelectNode: (id: string, node: string) =>
    invoke<EgressStatus[]>('net_policy_egress_select_node', { id, node }),
}
