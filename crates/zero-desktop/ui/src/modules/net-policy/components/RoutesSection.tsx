import { useCallback, useEffect, useState } from 'react'
import { ListOrdered, RefreshCw, Trash2 } from 'lucide-react'
import { NetPolicyAPI, type Route, type RouteEntry, type RuleKind } from '../api/tauri-client'

/**
 * 生效路由（含优先级）：把「内置 LAN + 临时例外 + 程序组 + 用户规则 + 兜底 MATCH」按 mihomo 实际
 * 匹配顺序展开（priority 越小越先命中）。用户规则（source=rule）可一键删除；其余为内置/派生，不可删。
 */

function routeBadge(r: Route) {
  const map: Record<Route, string> = {
    direct: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300',
    wg: 'bg-blue-100 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300',
    blackhole: 'bg-rose-100 text-rose-700 dark:bg-rose-950/40 dark:text-rose-300',
  }
  const label: Record<Route, string> = { direct: '直连', wg: '海外', blackhole: '阻断' }
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

export function RoutesSection({ busy }: { busy: boolean }) {
  const [routes, setRoutes] = useState<RouteEntry[]>([])
  const [loading, setLoading] = useState(false)
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
    try {
      await NetPolicyAPI.deleteRule({ kind: r.kind as RuleKind, value: r.value, route: r.route })
      await refresh()
    } catch (e) {
      setErr(String(e))
    }
  }

  return (
    <details className="group rounded-lg border border-gray-200 dark:border-gray-800">
      <summary className="flex cursor-pointer list-none items-center gap-2 border-b border-gray-200 px-4 py-2 select-none hover:bg-gray-50 dark:border-gray-800 dark:hover:bg-gray-800/40">
        <span className="text-[10px] text-gray-400 transition-transform group-open:rotate-90">▶</span>
        <ListOrdered size={15} className="text-gray-500" />
        <h2 className="text-sm font-semibold">生效路由（优先级）</h2>
        <div className="ml-auto flex items-center gap-2 text-xs text-gray-500" onClick={(e) => e.stopPropagation()}>
          <span>{routes.length} 条</span>
          <button
            className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
            onClick={() => void refresh()}
            disabled={loading}
          >
            <RefreshCw size={12} className={loading ? 'animate-spin' : ''} /> 刷新
          </button>
        </div>
      </summary>
      <div className="p-3">
        {err && <p className="mb-2 text-[11px] text-red-600 dark:text-red-400">{err}</p>}
        <div className="overflow-x-auto rounded border border-gray-200 dark:border-gray-800">
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
                  <td className="px-2 py-1">{routeBadge(r.route)}</td>
                  <td className="px-2 py-1">{sourceBadge(r.source)}</td>
                  <td className="px-2 py-1 text-right">
                    {r.deletable ? (
                      <button
                        className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-red-600 hover:bg-red-50 disabled:opacity-50 dark:text-red-400 dark:hover:bg-red-950/30"
                        onClick={() => void del(r)}
                        disabled={busy}
                        title="删除该规则并热加载"
                      >
                        <Trash2 size={11} /> 删除
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
          优先级 = mihomo 匹配顺序，首个命中即生效。删除仅对「用户规则」，删后需自行「应用/热载」使之生效。
        </p>
      </div>
    </details>
  )
}
