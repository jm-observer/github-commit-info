import { useCallback, useEffect, useMemo, useState } from 'react'
import { RefreshCw, Search } from 'lucide-react'
import { NetPolicyAPI, type SystemRoute } from '../api/tauri-client'
import { usePageHeaderActions } from '../PageHeaderContext'
import { btn, controlCls } from '../uiHelpers'

// 页标题 / 说明由 NetPolicyShell 的页头统一提供，这里只渲染工具栏 + 表格，避免重复页头。
export function SystemRoutesPage() {
  const [routes, setRoutes] = useState<SystemRoute[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [family, setFamily] = useState<'all' | 'IPv4' | 'IPv6'>('all')
  const [iface, setIface] = useState('all')
  const [protocol, setProtocol] = useState('all')
  const [search, setSearch] = useState('')

  const refresh = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      setRoutes(await NetPolicyAPI.systemRoutes())
    } catch (reason) {
      setError(String(reason).replace(/^Error:\s*/i, ''))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => { void refresh() }, [refresh])

  // 路由刷新注入 Shell 页头右侧（取代默认的全局刷新），页内不再重复放刷新按钮。
  usePageHeaderActions(
    (
      <button className={btn()} disabled={loading} onClick={() => void refresh()}>
        <RefreshCw size={14} className={loading ? 'animate-spin' : ''} /> 刷新
      </button>
    ),
    [loading, refresh],
  )

  // 下拉选项从当前路由集动态汇总（去重排序），空值不进列表。
  const interfaces = useMemo(
    () => Array.from(new Set(routes.map((r) => r.interface_alias).filter(Boolean))).sort(),
    [routes],
  )
  const protocols = useMemo(
    () => Array.from(new Set(routes.map((r) => r.protocol).filter(Boolean))).sort(),
    [routes],
  )

  const visibleRoutes = useMemo(() => {
    const q = search.trim().toLowerCase()
    return routes.filter((route) =>
      (family === 'all' || route.address_family === family) &&
      (iface === 'all' || route.interface_alias === iface) &&
      (protocol === 'all' || route.protocol === protocol) &&
      (q === '' ||
        route.destination_prefix.toLowerCase().includes(q) ||
        route.next_hop.toLowerCase().includes(q) ||
        route.interface_alias.toLowerCase().includes(q) ||
        route.protocol.toLowerCase().includes(q) ||
        String(route.interface_index).includes(q)),
    )
  }, [routes, family, iface, protocol, search])

  const filterActive = family !== 'all' || iface !== 'all' || protocol !== 'all' || search.trim() !== ''
  const resetFilters = () => {
    setFamily('all')
    setIface('all')
    setProtocol('all')
    setSearch('')
  }

  return (
    <div className="space-y-4">
      {/* 单行工具栏：搜索 + 地址族/接口/协议（同款下拉，统一样式）+ 计数 + 刷新 */}
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <div className="flex w-full items-center gap-2 rounded-md border border-gray-300 px-2 py-1 dark:border-gray-700 sm:w-64">
          <Search size={13} className="shrink-0 text-gray-400" />
          <input
            type="search"
            placeholder="搜索前缀 / 下一跳 / 接口 / 协议…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-full bg-transparent text-xs outline-none placeholder:text-gray-400 dark:placeholder:text-gray-600"
          />
        </div>
        <select className={controlCls} aria-label="地址族" value={family} onChange={(e) => setFamily(e.target.value as typeof family)}>
          <option value="all">地址族：全部</option>
          <option value="IPv4">IPv4</option>
          <option value="IPv6">IPv6</option>
        </select>
        <select className={controlCls} aria-label="接口" value={iface} onChange={(e) => setIface(e.target.value)}>
          <option value="all">接口：全部</option>
          {interfaces.map((name) => (
            <option key={name} value={name}>{name}</option>
          ))}
        </select>
        <select className={controlCls} aria-label="协议" value={protocol} onChange={(e) => setProtocol(e.target.value)}>
          <option value="all">协议：全部</option>
          {protocols.map((name) => (
            <option key={name} value={name}>{name}</option>
          ))}
        </select>
        {filterActive && (
          <button className="text-gray-500 underline-offset-2 hover:text-gray-800 hover:underline dark:hover:text-gray-200" onClick={resetFilters}>
            重置
          </button>
        )}
        <span className="ml-auto text-gray-500">{filterActive ? `${visibleRoutes.length} / ${routes.length}` : routes.length} 条</span>
      </div>

      {error && <p className="text-sm text-red-600 dark:text-red-400">{error}</p>}
      {!error && !loading && routes.length > 0 && visibleRoutes.length === 0 && (
        <p className="text-sm text-gray-500">没有匹配当前筛选条件的路由。</p>
      )}
      {!error && !loading && routes.length === 0 && <p className="text-sm text-gray-500">未读取到系统路由。</p>}

      <div className="overflow-x-auto rounded-lg border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
        <table className="w-full text-left text-xs">
          <thead className="bg-gray-50 text-gray-500 dark:bg-gray-800/40">
            <tr>
              <th className="px-3 py-2 font-medium">目标前缀</th>
              <th className="px-3 py-2 font-medium">下一跳</th>
              <th className="px-3 py-2 font-medium">接口</th>
              <th className="px-3 py-2 text-right font-medium">总 Metric</th>
              <th className="px-3 py-2 font-medium">协议 / 状态</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
            {visibleRoutes.map((route, index) => (
              <tr key={`${route.address_family}-${route.destination_prefix}-${route.next_hop}-${route.interface_index}-${index}`}>
                <td className="px-3 py-2 font-mono whitespace-nowrap">{route.destination_prefix}</td>
                <td className="px-3 py-2 font-mono whitespace-nowrap">{route.next_hop || '在链路上'}</td>
                <td className="px-3 py-2">
                  <div>{route.interface_alias || `ifIndex ${route.interface_index}`}</div>
                  <div className="text-[11px] text-gray-400">{route.address_family} · ifIndex {route.interface_index}</div>
                </td>
                <td className="px-3 py-2 text-right font-mono">{route.route_metric + route.interface_metric}</td>
                <td className="px-3 py-2">
                  <div>{route.protocol || '—'}</div>
                  <div className="text-[11px] text-gray-400">{route.state || '—'}</div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <p className="text-[11px] text-gray-500">总 Metric = 路由 Metric + 接口 Metric；数值越小通常优先。该页只读，不修改任何系统网络设置。</p>
    </div>
  )
}
