import { useCallback, useEffect, useState } from 'react'
import { RefreshCw, Trash2 } from 'lucide-react'
import { NetPolicyAPI, type Route, type RouteEntry, type Rule, type RuleKind } from '../api/tauri-client'

/**
 * 生效路由（含优先级）：把「内置 LAN + 临时例外 + 程序组 + 用户规则 + 兜底 MATCH」按 mihomo 实际
 * 匹配顺序展开（priority 越小越先命中）。用户规则（source=rule）可一键删除；其余为内置/派生，不可删。
 */

function routeBadge(r: Route) {
  const map: Record<Route, string> = {
    direct: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300',
    wg: 'bg-blue-100 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300',
    proxy: 'bg-violet-100 text-violet-700 dark:bg-violet-950/40 dark:text-violet-300',
    blackhole: 'bg-rose-100 text-rose-700 dark:bg-rose-950/40 dark:text-rose-300',
  }
  const label: Record<Route, string> = { direct: '直连', wg: '海外', proxy: '代理订阅', blackhole: '阻断' }
  return <span className={`rounded px-1.5 py-0.5 text-[11px] ${map[r]}`}>{label[r]}</span>
}

function sourceBadge(s: string) {
  const map: Record<string, string> = {
    builtin_lan: 'text-gray-500',
    temp_except: 'text-amber-600 dark:text-amber-400',
    group: 'text-violet-600 dark:text-violet-400',
    rule: 'text-sky-600 dark:text-sky-400',
    default: 'text-gray-600 dark:text-gray-300',
  }
  const label: Record<string, string> = {
    builtin_lan: '内置',
    temp_except: '临时例外',
    group: '程序组',
    rule: '用户规则',
    default: '兜底',
  }
  return <span className={`text-[11px] ${map[s] ?? 'text-gray-500'}`}>{label[s] ?? s}</span>
}

export function RoutesSection({ busy, onDeleteRule }: { busy: boolean; onDeleteRule: (rule: Rule) => Promise<void> }) {
  const [routes, setRoutes] = useState<RouteEntry[]>([])
  const [loading, setLoading] = useState(false)
  const [deleting, setDeleting] = useState<string | null>(null)
  const [err, setErr] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    setLoading(true)
    setErr(null)
    try {
      setRoutes(await NetPolicyAPI.routes())
    } catch (e) {
      setErr(String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh])

  const del = async (r: RouteEntry) => {
    const key = `${r.kind}:${r.value}`
    setDeleting(key)
    setErr(null)
    try {
      await onDeleteRule({ kind: r.kind as RuleKind, value: r.value, route: r.route })
      await refresh()
    } catch (e) {
      setErr(String(e))
    } finally {
      setDeleting(null)
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-gray-500">按 mihomo 的实际匹配顺序排列，首个命中即生效。</p>
        <div className="flex items-center gap-2 text-xs text-gray-500">
          <span>{routes.length} 条</span>
          <button
            className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
            onClick={() => void refresh()}
            disabled={loading}
          >
            <RefreshCw size={12} className={loading ? 'animate-spin' : ''} /> 刷新
          </button>
        </div>
      </div>
      <div>
        {err && <p className="mb-2 text-[11px] text-red-600 dark:text-red-400">{err}</p>}
        <div className="overflow-x-auto rounded-lg border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
          <table className="w-full text-left text-xs">
            <thead className="bg-gray-50 text-gray-500 dark:bg-gray-800/40">
              <tr>
                <th className="px-2 py-1 font-medium">#</th>
                <th className="px-2 py-1 font-medium">类型</th>
                <th className="px-2 py-1 font-medium">匹配</th>
                <th className="px-2 py-1 font-medium">出口</th>
                <th className="px-2 py-1 font-medium">来源</th>
                <th className="px-2 py-1 text-right font-medium">操作</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
              {routes.map((r) => (
                <tr key={`${r.priority}-${r.kind}-${r.value}`} className={r.source === 'temp_except' ? 'bg-amber-50/50 dark:bg-amber-950/10' : ''}>
                  <td className="px-2 py-1 text-gray-400">{r.priority}</td>
                  <td className="px-2 py-1 font-mono text-gray-600 dark:text-gray-400">{r.kind}</td>
                  <td className="px-2 py-1 font-mono">{r.value || <span className="text-gray-400">（全部）</span>}</td>
                  {/* 计划出口 vs 实际出口：出口被停用时二者不同，必须都看得见（出口设计 §8.6）。 */}
                  <td className="px-2 py-1">
                    {r.applied_route && r.applied_route !== r.route ? (
                      <span
                        className="inline-flex items-center gap-1"
                        title={`该出口当前不可用，实际按 fallback 走「${r.applied_route === 'blackhole' ? '阻断' : '直连'}」`}
                      >
                        <span className="line-through opacity-50">{routeBadge(r.route)}</span>
                        <span className="text-amber-600 dark:text-amber-400">→</span>
                        {routeBadge(r.applied_route)}
                      </span>
                    ) : (
                      routeBadge(r.route)
                    )}
                  </td>
                  <td className="px-2 py-1">{sourceBadge(r.source)}</td>
                  <td className="px-2 py-1 text-right">
                    {r.deletable ? (
                      <button
                        className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-red-600 hover:bg-red-50 disabled:opacity-50 dark:text-red-400 dark:hover:bg-red-950/30"
                        onClick={() => void del(r)}
                        disabled={busy || deleting !== null}
                        title="删除该规则并热加载"
                      >
                        <Trash2 size={11} /> {deleting === `${r.kind}:${r.value}` ? '删除中…' : '删除'}
                      </button>
                    ) : (
                      <span className="text-[11px] text-gray-300 dark:text-gray-600">—</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        <p className="mt-2 px-1 text-[11px] text-gray-500 dark:text-gray-400">
          优先级 = mihomo 匹配顺序，首个命中即生效。只有用户规则可删除；引擎运行中会自动热加载。
          出口列显示「计划出口 → 实际出口」时，说明该出口已被停用，流量按其 fallback 策略处理。
        </p>
      </div>
    </div>
  )
}
