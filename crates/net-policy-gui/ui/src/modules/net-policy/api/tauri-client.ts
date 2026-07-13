import { invoke } from '@tauri-apps/api/core'

// ── 与后端 net_policy 命令对齐的类型 ───────────────────────────────────────────

// direct/wg 用作单条规则出口；wg/blackhole 用作默认出口（default_route）。
export type Route = 'direct' | 'wg' | 'blackhole'
export type RuleKind = 'process-path' | 'process-name' | 'domain-suffix' | 'ip-cidr'

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
  source: string
  deletable: boolean
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
}
