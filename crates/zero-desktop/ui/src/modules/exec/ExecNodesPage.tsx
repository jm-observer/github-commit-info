import { useCallback, useEffect, useState } from 'react'
import { ShieldCheck, RefreshCw, Check, X, Monitor } from 'lucide-react'
import {
  ExecAPI,
  type ExecCredRow,
  type ExecRequestRow,
  type ExecWorkerRow,
} from './api/tauri-client'

/**
 * 远程节点（remote-exec）——**权限审批面**。
 *
 * 对方机器上跑 `toolkit-worker run`（零参数）后会自行提交一条权限申请，停在「等待批准」；
 * 你在这一页点批准并选时长，它拿到带到期时间的凭据后自动开始工作。到期后自动失效、
 * 会重新出现在待审批里。
 *
 * 注意：到期/拒绝只能阻止它**领取新命令**，杀不掉已经在跑的命令（那是第二期的远程中止）。
 */

const POLL_INTERVAL_MS = 5000

/** 批准时长预设（小时）。20h 是默认档：够覆盖一个完整工作日 + 时差。 */
const HOUR_PRESETS = [1, 8, 20, 72]

function fromNow(unixSec: number | null): string {
  if (!unixSec) return '—'
  const diff = unixSec * 1000 - Date.now()
  const abs = Math.abs(diff)
  const mins = Math.floor(abs / 60000)
  if (mins < 60) return diff > 0 ? `${mins} 分钟后` : `${mins} 分钟前`
  const hours = Math.floor(mins / 60)
  if (hours < 48) return diff > 0 ? `${hours} 小时后` : `${hours} 小时前`
  return diff > 0 ? `${Math.floor(hours / 24)} 天后` : `${Math.floor(hours / 24)} 天前`
}

export default function ExecNodesPage() {
  const [requests, setRequests] = useState<ExecRequestRow[]>([])
  const [workers, setWorkers] = useState<ExecWorkerRow[]>([])
  const [creds, setCreds] = useState<ExecCredRow[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busyId, setBusyId] = useState<string | null>(null)
  /** 每行独立的时长选择，默认 20h。 */
  const [hoursById, setHoursById] = useState<Record<string, number>>({})

  const refresh = useCallback(async () => {
    setLoading(true)
    try {
      const [r, w, c] = await Promise.all([
        ExecAPI.listRequests(),
        ExecAPI.listWorkers(),
        ExecAPI.listCreds(),
      ])
      setRequests(r.requests ?? [])
      setWorkers(w.workers ?? [])
      setCreds(c.creds ?? [])
      setError(null)
    } catch (e: any) {
      setError(typeof e === 'string' ? e : (e?.message ?? String(e)))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void refresh()
    const timer = setInterval(() => void refresh(), POLL_INTERVAL_MS)
    return () => clearInterval(timer)
  }, [refresh])

  const decide = useCallback(
    async (workerId: string, approve: boolean) => {
      setBusyId(workerId)
      try {
        if (approve) await ExecAPI.approve(workerId, hoursById[workerId] ?? 20)
        else await ExecAPI.reject(workerId)
        setError(null)
        await refresh()
      } catch (e: any) {
        setError(typeof e === 'string' ? e : (e?.message ?? String(e)))
      } finally {
        setBusyId(null)
      }
    },
    [hoursById, refresh],
  )

  const pending = requests.filter(r => r.state === 'pending')
  const decided = requests.filter(r => r.state !== 'pending')
  const credOf = (id: string) => creds.find(c => c.worker_id === id)

  return (
    <div className="mx-auto max-w-4xl space-y-5">
      <div className="flex items-center gap-2">
        <ShieldCheck className="text-blue-600" />
        <h1 className="text-lg font-semibold">远程节点</h1>
        <button
          type="button"
          className="ml-auto inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm transition-colors hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
          onClick={() => void refresh()}
          disabled={loading}
        >
          <RefreshCw size={14} className={loading ? 'animate-spin' : undefined} /> 刷新
        </button>
      </div>

      <p className="text-xs text-gray-500 dark:text-gray-400">
        对方机器跑 <code className="rounded bg-gray-100 px-1 dark:bg-gray-800">toolkit-worker run</code>{' '}
        后会自动申请权限并停在等待批准。批准后按选定时长授权，到期自动失效并重新出现在待审批。
        <span className="text-amber-600 dark:text-amber-400">
          {' '}到期/拒绝只阻止它领取新命令，杀不掉已经在跑的命令。
        </span>
      </p>

      {error && (
        <div className="rounded-md bg-red-100 px-4 py-2 text-sm text-red-800 dark:bg-red-950/50 dark:text-red-300">
          {error}
        </div>
      )}

      {/* ── 待审批 ───────────────────────────────────────────── */}
      <section className="space-y-2">
        <h2 className="text-sm font-medium">
          待审批
          {pending.length > 0 && (
            <span className="ml-2 rounded-full bg-amber-100 px-2 py-0.5 text-xs text-amber-800 dark:bg-amber-950/50 dark:text-amber-300">
              {pending.length}
            </span>
          )}
        </h2>
        {pending.length === 0 ? (
          <div className="rounded-lg border border-dashed border-gray-300 px-4 py-6 text-center text-sm text-gray-500 dark:border-gray-700">
            没有待审批的申请
          </div>
        ) : (
          <div className="space-y-2">
            {pending.map(r => (
              <div
                key={r.worker_id}
                className="rounded-lg border border-amber-300 bg-amber-50/50 px-4 py-3 dark:border-amber-800 dark:bg-amber-950/20"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <Monitor size={14} className="text-gray-500" />
                  <span className="font-medium">{r.label || r.hostname || r.worker_id}</span>
                  <span className="rounded bg-gray-200 px-1.5 py-0.5 text-[11px] text-gray-700 dark:bg-gray-700 dark:text-gray-200">
                    {r.os || '未知系统'}
                  </span>
                  <span className="font-mono text-[11px] text-gray-500">{r.worker_id}</span>
                  <span className="ml-auto text-xs text-gray-500">
                    {fromNow(r.requested_at)}申请
                  </span>
                </div>
                <div className="mt-2 flex flex-wrap items-center gap-2">
                  <span className="text-xs text-gray-500">授权时长</span>
                  {HOUR_PRESETS.map(h => {
                    const active = (hoursById[r.worker_id] ?? 20) === h
                    return (
                      <button
                        key={h}
                        type="button"
                        onClick={() => setHoursById(m => ({ ...m, [r.worker_id]: h }))}
                        className={`rounded px-2 py-1 text-xs transition-colors ${
                          active
                            ? 'bg-blue-600 text-white'
                            : 'border border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800'
                        }`}
                      >
                        {h < 24 ? `${h} 小时` : `${h / 24} 天`}
                      </button>
                    )
                  })}
                  <button
                    type="button"
                    className="ml-auto inline-flex items-center gap-1 rounded-md bg-green-600 px-3 py-1.5 text-sm text-white transition-colors hover:bg-green-700 disabled:opacity-50"
                    disabled={busyId === r.worker_id}
                    onClick={() => void decide(r.worker_id, true)}
                  >
                    <Check size={14} /> 批准
                  </button>
                  <button
                    type="button"
                    className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-3 py-1.5 text-sm transition-colors hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
                    disabled={busyId === r.worker_id}
                    onClick={() => void decide(r.worker_id, false)}
                  >
                    <X size={14} /> 拒绝
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      {/* ── 在线节点 ─────────────────────────────────────────── */}
      <section className="space-y-2">
        <h2 className="text-sm font-medium">在线节点</h2>
        <div className="overflow-hidden rounded-lg border border-gray-200 dark:border-gray-800">
          <table className="w-full text-sm">
            <thead className="bg-gray-50 text-left text-xs uppercase tracking-wide text-gray-500 dark:bg-gray-800/60 dark:text-gray-400">
              <tr>
                <th className="px-4 py-2 font-medium">主机</th>
                <th className="px-4 py-2 font-medium">ID</th>
                <th className="px-4 py-2 font-medium">PowerShell</th>
                <th className="px-4 py-2 font-medium">状态</th>
                <th className="px-4 py-2 font-medium">授权剩余</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-200 dark:divide-gray-800">
              {workers.length === 0 && (
                <tr>
                  <td className="px-4 py-6 text-center text-gray-500" colSpan={5}>
                    {loading ? '加载中…' : '暂无在线节点'}
                  </td>
                </tr>
              )}
              {workers.map(w => {
                const cred = credOf(w.worker_id)
                return (
                  <tr key={w.worker_id} className="hover:bg-gray-50 dark:hover:bg-gray-800/40">
                    <td className="px-4 py-2">{w.hostname ?? '—'}</td>
                    <td className="px-4 py-2 font-mono text-xs">{w.worker_id}</td>
                    <td className="px-4 py-2 text-xs">{w.powershell ?? '—'}</td>
                    <td className="px-4 py-2">
                      <span
                        className={`inline-flex items-center gap-1.5 rounded px-1.5 py-0.5 text-xs ${
                          w.busy
                            ? 'bg-blue-100 text-blue-800 dark:bg-blue-950/50 dark:text-blue-300'
                            : 'bg-green-100 text-green-800 dark:bg-green-950/50 dark:text-green-300'
                        }`}
                      >
                        <span
                          className={`h-1.5 w-1.5 rounded-full ${w.busy ? 'bg-blue-500' : 'bg-green-500'}`}
                        />
                        {w.busy ? '执行中' : '空闲'}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-xs text-gray-600 dark:text-gray-300">
                      {cred?.expires_at ? fromNow(cred.expires_at) + '到期' : '长期'}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      </section>

      {/* ── 历史 ─────────────────────────────────────────────── */}
      {decided.length > 0 && (
        <section className="space-y-2">
          <h2 className="text-sm font-medium">已处理</h2>
          <div className="space-y-1">
            {decided.map(r => (
              <div
                key={r.worker_id}
                className="flex flex-wrap items-center gap-2 rounded border border-gray-200 px-3 py-2 text-xs dark:border-gray-800"
              >
                <span
                  className={
                    r.state === 'approved'
                      ? 'text-green-600 dark:text-green-400'
                      : 'text-gray-500'
                  }
                >
                  {r.state === 'approved' ? '已批准' : '已拒绝'}
                </span>
                <span className="font-medium">{r.label || r.worker_id}</span>
                <span className="font-mono text-gray-500">{r.worker_id}</span>
                {r.approved_by && <span className="text-gray-500">by {r.approved_by}</span>}
                {r.expires_at && (
                  <span className="ml-auto text-gray-500">{fromNow(r.expires_at)}到期</span>
                )}
              </div>
            ))}
          </div>
        </section>
      )}
    </div>
  )
}
