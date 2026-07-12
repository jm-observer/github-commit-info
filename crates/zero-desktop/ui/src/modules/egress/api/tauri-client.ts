import { invoke } from '@tauri-apps/api/core'

// ── 与后端 egress-pool::WorkerStatus 对齐的类型 ────────────────────────────────

export interface WorkerStatus {
  id: string
  egress_ip: string
  online: boolean
  seconds_since_heartbeat: number
  /** 最近一次心跳的绝对时间戳（unix epoch 毫秒，wall-clock）。 */
  last_heartbeat_ms: number
  /** worker 代发绑定的网卡名（Linux `--interface`），无绑定则 null。 */
  interface: string | null
  /** worker 代发绑定的本地源 IP（`--local-address`），无绑定则 null。 */
  local_address: string | null
  /** 当前被占用的 type 列表（同类型独占的可见状态）。 */
  types_held: string[]
}

export interface WorkersSnapshot {
  workers: WorkerStatus[]
}

// 命令以 egress_ 前缀，集中包装（仿 llm 模块）。
export const EgressAPI = {
  /** 出口代理 worker 列表（只读观测面）。 */
  listWorkers: () => invoke<WorkersSnapshot>('egress_list_workers'),
}
