import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

// ── 类型（与 src/modules/g10_deploy/{registry,mod}.rs 对齐） ──────────────────

export interface DeployDef {
  script: string
  args: string[]
}

export interface PortInfo {
  port: number
  note: string
}

/** 安装时注入 systemd unit 的环境变量（KEY=VAL）。value 不应含逗号（部署链路用逗号分隔多条）。 */
export interface EnvVar {
  key: string
  value: string
}

export interface ServiceDef {
  name: string
  label: string
  note: string
  repo_dir: string
  health_url: string
  remote_service: string | null
  /** 服务 web 后台地址；空串 = 无后台。 */
  web_url: string
  /** G10 上该服务所在主机（端口探测目标）。 */
  host: string
  /** 该服务监听/占用的端口清单。 */
  ports: PortInfo[]
  /** 安装时动态注入 systemd unit 的环境变量；部署时拼成 `-Env KEY=VAL,...` 传给脚本。 */
  env: EnvVar[]
  deploy: DeployDef | null
}

export interface PortStatus {
  port: number
  note: string
  /** TCP 能否连上（在监听）。 */
  open: boolean
  latency_ms: number | null
  error: string | null
}

export interface PortsResult {
  name: string
  host: string
  ports: PortStatus[]
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

export const G10DeployAPI = {
  listServices: () => invoke<ServiceList>('g10_list_services'),
  saveServices: (services: ServiceDef[]) =>
    invoke<void>('g10_save_services', { services }),
  probe: (name: string) => invoke<ProbeResult>('g10_probe_service', { name }),
  probePorts: (name: string) => invoke<PortsResult>('g10_probe_ports', { name }),
  localVersion: (name: string) => invoke<LocalVersion>('g10_local_version', { name }),
  isDeploying: () => invoke<boolean>('g10_is_deploying'),
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
