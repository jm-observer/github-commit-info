import { invoke } from '@tauri-apps/api/core'

// ── 与后端 toolkit_core::exec_requests / exec_creds 对齐的类型 ────────────────

/** 一条远程节点的权限申请。 */
export interface ExecRequestRow {
  worker_id: string
  /** 人类可读名（worker 端 --label，默认取主机名）。 */
  label: string
  hostname: string
  /** windows | linux | … */
  os: string
  state: 'pending' | 'approved' | 'rejected'
  /** unix 秒 */
  requested_at: number
  decided_at: number | null
  approved_by: string | null
  /** 批准时确定的凭据到期时间（unix 秒）。 */
  expires_at: number | null
}

/** 已签发的凭据概览（不含 secret）。 */
export interface ExecCredRow {
  worker_id: string
  created_at: number
  revoked_at: number | null
  /** null = 长期有效（手工签发） */
  expires_at: number | null
}

/** 在线的远程节点。 */
export interface ExecWorkerRow {
  worker_id: string
  instance_id: string
  hostname: string | null
  powershell: string | null
  online: boolean
  busy: boolean
  seconds_since_heartbeat: number
}

export const ExecAPI = {
  listRequests: () => invoke<{ requests: ExecRequestRow[] }>('exec_list_requests'),
  listWorkers: () => invoke<{ workers: ExecWorkerRow[] }>('exec_list_workers'),
  listCreds: () => invoke<{ creds: ExecCredRow[] }>('exec_list_creds'),
  /** 批准并授权 hours 小时。 */
  approve: (workerId: string, hours: number) =>
    invoke<unknown>('exec_approve_request', { workerId, hours }),
  reject: (workerId: string) => invoke<unknown>('exec_reject_request', { workerId }),
}
