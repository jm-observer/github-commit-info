import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

// ── 类型（与 src/modules/g10_deploy/{registry,mod}.rs 对齐） ──────────────────

export interface DeployDef {
  script: string
  args: string[]
}

/**
 * 安装时注入 systemd unit 的环境变量（KEY=VAL + 可选备注）。
 * value 不应含逗号（部署链路用逗号分隔多条 -Env）。**端口即其中的 `<SERVICE>_BIND` 一条**。
 */
export interface EnvVar {
  key: string
  value: string
  /** 备注（可选；说明该变量用途，如「监听地址 host:port」）。 */
  note: string
}

export interface ServiceDef {
  name: string
  label: string
  note: string
  repo_dir: string
  /** HTTP 健康端点（面板可编辑；https 自签也能探，探测已放宽证书校验）。 */
  health_url: string
  remote_service: string | null
  /** 服务 web 后台地址（局域网直连）；空串 = 无后台，不显示按钮。 */
  web_url: string
  /**
   * 同一后台的外网地址（caddy 子域名入口，形如 `https://<svc>.for-memory.site:38788`）。
   * 空串 = 该服务没有外网入口 → 外网档「打开后台」置灰。
   */
  web_url_wan: string
  /** 安装时动态注入 systemd unit 的环境变量；部署时拼成 `-Env KEY=VAL,...` 传给脚本。 */
  env: EnvVar[]
  deploy: DeployDef | null
  /** 上次部署成功的时间（RFC3339 UTC）；仅部署成功后由后端写入，从未部署过则为 null。 */
  last_deployed_at?: string | null
}

export interface ServiceList {
  services: ServiceDef[]
  warning: string | null
}

export interface ProbeResult {
  name: string
  reachable: boolean
  status: string | null
  remote_version: string | null
  remote_commit?: string | null
  latency_ms: number | null
  error: string | null
}

export interface LocalVersion {
  name: string
  git_hash: string | null
  dirty: boolean
  error: string | null
}

export interface DeployLog {
  name: string
  stream: 'stdout' | 'stderr'
  line: string
}

export interface DeployDone {
  name: string
  success: boolean
  code: number | null
  error: string | null
}

// ── 命令封装 ────────────────────────────────────────────────────────────────

/** 当前生效的网络路径（设置页同源命令 `net_resolve_status`）。 */
export interface NetStatus {
  mode: 'auto' | 'lan' | 'wan'
  /** auto 探测后实际选中的路径。 */
  picked: 'auto' | 'lan' | 'wan'
  g10_base: string
  configured: boolean
  reachable: boolean | null
}

export const G10DeployAPI = {
  listServices: () => invoke<ServiceList>('g10_list_services'),
  /**
   * 当前生效的网络路径。面板用它做两件事：页头展示走的是局域网还是外网；
   * 决定「打开后台」跳 `web_url`（局域网直连）还是 `web_url_wan`（caddy 子域名入口）。
   */
  netStatus: () => invoke<NetStatus>('net_resolve_status'),
  saveServices: (services: ServiceDef[]) =>
    invoke<void>('g10_save_services', { services }),
  probe: (name: string) => invoke<ProbeResult>('g10_probe_service', { name }),
  localVersion: (name: string) => invoke<LocalVersion>('g10_local_version', { name }),
  /** 当前正在部署中的服务名列表（进页恢复"部署中"状态用）。 */
  deployingServices: () => invoke<string[]>('g10_deploying_services'),
  deploy: (name: string) => invoke<void>('g10_deploy', { name }),
}

/** 在系统默认浏览器打开指定 URL（命令由后端 `open_url` 提供，入参 `{ url }`）。 */
export function openUrl(url: string): Promise<void> {
  return invoke<void>('open_url', { url })
}

// ── 部署事件订阅 ──────────────────────────────────────────────────────────────

export function onDeployLog(cb: (log: DeployLog) => void): Promise<UnlistenFn> {
  return listen<DeployLog>('g10-deploy://log', e => cb(e.payload))
}

export function onDeployDone(cb: (done: DeployDone) => void): Promise<UnlistenFn> {
  return listen<DeployDone>('g10-deploy://done', e => cb(e.payload))
}
