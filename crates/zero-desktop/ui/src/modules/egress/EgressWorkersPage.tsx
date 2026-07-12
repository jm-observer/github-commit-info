import { useCallback, useEffect, useState } from 'react'
import { Network, RefreshCw } from 'lucide-react'
import { EgressAPI, type WorkerStatus } from './api/tauri-client'

/**
 * 出口 Worker 列表 —— 观测 egress-pool 当前注册的 worker：出口 IP / 在线状态 /
 * 最近心跳时间 / 占用的 type。只读观测面，无写操作。
 *
 * 拉 G10 toolkit-server 的 `/api/web/egress/workers`（经 egress_list_workers Tauri
 * 命令中转，形态与 net-policy / llm 模块一致）。手动刷新 + 5s 定时轮询。
 */

const POLL_INTERVAL_MS = 5000

// 与 net-policy ObservatorySection 的 ago() 同口径（相对时间展示）。
function ago(ms: number): string {
  if (!ms) return ''
  const sec = Math.max(0, Math.floor((Date.now() - ms) / 1000))
  if (sec < 60) return `${sec}s 前`
  if (sec < 3600) return `${Math.floor(sec / 60)}m 前`
  return `${Math.floor(sec / 3600)}h 前`
}

function formatHeartbeat(ms: number): string {
  if (!ms) return '—'
  const abs = new Date(ms).toLocaleString()
  return `${abs}（${ago(ms)}）`
}

export default function EgressWorkersPage() {
  const [workers, setWorkers] = useState<WorkerStatus[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [lastFetchedAt, setLastFetchedAt] = useState<number | null>(null)

  const refresh = useCallback(async () => {
    setLoading(true)
    try {
      const snapshot = await EgressAPI.listWorkers()
      setWorkers(snapshot.workers ?? [])
      setError(null)
      setLastFetchedAt(Date.now())
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

  const onlineCount = workers.filter(w => w.online).length

  return (
    <div className="mx-auto max-w-4xl space-y-5">
      <div className="flex items-center gap-2">
        <Network className="text-blue-600" />
        <h1 className="text-lg font-semibold">出口 Worker</h1>
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
        当前注册在出口代理池（egress-pool）里的 worker：出口 IP、在线状态、最近心跳时间、占用的 type。
        每 {POLL_INTERVAL_MS / 1000}s 自动刷新一次，也可手动刷新。
      </p>

      {error && (
        <div className="rounded-md bg-red-100 px-4 py-2 text-sm text-red-800 dark:bg-red-950/50 dark:text-red-300">
          {error}
        </div>
      )}

      <div className="grid grid-cols-2 gap-3">
        <div className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-900">
          <div className="text-[11px] uppercase tracking-wide text-gray-400">在线</div>
          <div className="mt-1 text-2xl font-semibold text-green-600">{onlineCount}</div>
          <div className="text-xs text-gray-500">/ 共 {workers.length} 台</div>
        </div>
        <div className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-900">
          <div className="text-[11px] uppercase tracking-wide text-gray-400">最近刷新</div>
          <div className="mt-1 text-sm font-medium text-gray-700 dark:text-gray-200">
            {lastFetchedAt ? new Date(lastFetchedAt).toLocaleTimeString() : '—'}
          </div>
          <div className="text-xs text-gray-500">{loading ? '刷新中…' : '自动轮询中'}</div>
        </div>
      </div>

      <section className="overflow-hidden rounded-lg border border-gray-200 dark:border-gray-800">
        <table className="w-full text-sm">
          <thead className="bg-gray-50 text-left text-xs uppercase tracking-wide text-gray-500 dark:bg-gray-800/60 dark:text-gray-400">
            <tr>
              <th className="px-4 py-2 font-medium">ID</th>
              <th className="px-4 py-2 font-medium">接口</th>
              <th className="px-4 py-2 font-medium">绑定源 IP</th>
              <th className="px-4 py-2 font-medium">出口 IP</th>
              <th className="px-4 py-2 font-medium">在线状态</th>
              <th className="px-4 py-2 font-medium">最近心跳</th>
              <th className="px-4 py-2 font-medium">占用的 Type</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-200 dark:divide-gray-800">
            {workers.length === 0 && (
              <tr>
                <td className="px-4 py-6 text-center text-gray-500" colSpan={7}>
                  {loading ? '加载中…' : '暂无注册的 worker'}
                </td>
              </tr>
            )}
            {workers.map(w => (
              <tr key={w.id} className="hover:bg-gray-50 dark:hover:bg-gray-800/40">
                <td className="px-4 py-2 font-mono text-xs">{w.id}</td>
                <td className="px-4 py-2 text-xs">{w.interface ?? '—'}</td>
                <td className="px-4 py-2 font-mono text-xs">{w.local_address ?? '—'}</td>
                <td className="px-4 py-2 font-mono text-xs">
                  {w.egress_ip === 'unknown' ? (
                    <span className="text-gray-400">探测失败</span>
                  ) : (
                    w.egress_ip
                  )}
                </td>
                <td className="px-4 py-2">
                  <span
                    className={`inline-flex items-center gap-1.5 rounded px-1.5 py-0.5 text-xs ${
                      w.online
                        ? 'bg-green-100 text-green-800 dark:bg-green-950/50 dark:text-green-300'
                        : 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400'
                    }`}
                  >
                    <span className={`h-1.5 w-1.5 rounded-full ${w.online ? 'bg-green-500' : 'bg-gray-400'}`} />
                    {w.online ? '在线' : '离线'}
                  </span>
                </td>
                <td className="px-4 py-2 text-xs text-gray-600 dark:text-gray-300">
                  {formatHeartbeat(w.last_heartbeat_ms)}
                </td>
                <td className="px-4 py-2">
                  {w.types_held.length === 0 ? (
                    <span className="text-xs text-gray-400">—</span>
                  ) : (
                    <div className="flex flex-wrap gap-1">
                      {w.types_held.map(t => (
                        <span
                          key={t}
                          className="rounded bg-blue-100 px-1.5 py-0.5 text-xs text-blue-800 dark:bg-blue-950/50 dark:text-blue-300"
                        >
                          {t}
                        </span>
                      ))}
                    </div>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
  )
}
