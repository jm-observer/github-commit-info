import { useCallback, useEffect, useMemo, useState } from 'react'
import { History, ListTree, RefreshCw, ScrollText, Search, Trash2 } from 'lucide-react'
import { NetPolicyAPI, type LifecycleEvent, type ProcessNode, type RequestLogEntry } from '../api/tauri-client'

type RecordView = 'requests' | 'events' | 'processes'

function ts(ms: number): string {
  return ms ? new Date(ms).toLocaleString() : ''
}

function TreeNode({ node, depth }: { node: ProcessNode; depth: number }) {
  return (
    <div>
      <div className="flex items-center gap-2 py-0.5 text-xs" style={{ paddingLeft: depth * 14 }}>
        <span className="font-mono text-gray-800 dark:text-gray-200">{node.name || '(?)'}</span>
        <span className="text-gray-400">#{node.pid}</span>
        {node.path && (
          <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-gray-400" title={node.path}>{node.path}</span>
        )}
      </div>
      {node.children.map((child) => <TreeNode key={child.pid} node={child} depth={depth + 1} />)}
    </div>
  )
}

export function RecordsSection() {
  const [view, setView] = useState<RecordView>('requests')
  const [requests, setRequests] = useState<RequestLogEntry[]>([])
  const [events, setEvents] = useState<LifecycleEvent[]>([])
  const [tree, setTree] = useState<ProcessNode[]>([])
  const [loading, setLoading] = useState<Record<RecordView, boolean>>({ requests: false, events: false, processes: false })
  const [search, setSearch] = useState('')

  // 请求记录搜索：按 进程 / 域名 / IP 过滤（仅「网络请求」视图）。
  const filteredRequests = useMemo(() => {
    const q = search.trim().toLowerCase()
    if (!q) return requests
    return requests.filter(
      (r) =>
        (r.process || '').toLowerCase().includes(q) ||
        (r.process_path || '').toLowerCase().includes(q) ||
        (r.host || '').toLowerCase().includes(q) ||
        (r.dest_ip || '').toLowerCase().includes(q),
    )
  }, [requests, search])

  const loadRequests = useCallback(async () => {
    setLoading((state) => ({ ...state, requests: true }))
    try { setRequests(await NetPolicyAPI.requests(500)) } catch { /* 用户可手动重试 */ }
    finally { setLoading((state) => ({ ...state, requests: false })) }
  }, [])

  const loadEvents = useCallback(async () => {
    setLoading((state) => ({ ...state, events: true }))
    try { setEvents(await NetPolicyAPI.events(200)) } catch { /* 用户可手动重试 */ }
    finally { setLoading((state) => ({ ...state, events: false })) }
  }, [])

  const loadProcesses = useCallback(async () => {
    setLoading((state) => ({ ...state, processes: true }))
    try { setTree(await NetPolicyAPI.processTree()) } catch { /* 用户可手动重试 */ }
    finally { setLoading((state) => ({ ...state, processes: false })) }
  }, [])

  useEffect(() => {
    if (view === 'requests') void loadRequests()
    if (view === 'events') void loadEvents()
    if (view === 'processes') void loadProcesses()
  }, [view, loadRequests, loadEvents, loadProcesses])

  const refresh = () => {
    if (view === 'requests') void loadRequests()
    if (view === 'events') void loadEvents()
    if (view === 'processes') void loadProcesses()
  }

  const clear = async () => {
    if (view === 'requests') {
      await NetPolicyAPI.clearRequests()
      await loadRequests()
    } else if (view === 'events') {
      await NetPolicyAPI.clearEvents()
      await loadEvents()
    }
  }

  const counts: Record<RecordView, number> = { requests: requests.length, events: events.length, processes: tree.length }
  const tabs: { key: RecordView; label: string; icon: React.ReactNode }[] = [
    { key: 'requests', label: '网络请求', icon: <History size={14} /> },
    { key: 'events', label: '生命周期', icon: <ScrollText size={14} /> },
    { key: 'processes', label: '进程树', icon: <ListTree size={14} /> },
  ]

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="inline-flex rounded-lg bg-gray-100 p-1 dark:bg-gray-800">
          {tabs.map((tab) => (
            <button
              key={tab.key}
              className={`inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition-colors ${
                view === tab.key ? 'bg-white font-medium text-gray-900 shadow-sm dark:bg-gray-700 dark:text-white' : 'text-gray-500'
              }`}
              onClick={() => setView(tab.key)}
            >
              {tab.icon} {tab.label} <span className="text-xs text-gray-400">{counts[tab.key]}</span>
            </button>
          ))}
        </div>
        <div className="flex items-center gap-2 text-xs text-gray-500">
          <button
            className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-1 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
            onClick={refresh}
            disabled={loading[view]}
          >
            <RefreshCw size={12} className={loading[view] ? 'animate-spin' : ''} /> 刷新
          </button>
          {view !== 'processes' && (
            <button
              className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-1 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
              onClick={() => void clear()}
              disabled={loading[view] || counts[view] === 0}
            >
              <Trash2 size={12} /> 清空
            </button>
          )}
        </div>
      </div>

      {view === 'requests' && (
        <>
          {requests.length > 0 && (
            <div className="flex items-center gap-2 rounded-lg border border-gray-200 bg-white px-3 py-2 dark:border-gray-800 dark:bg-gray-900">
              <Search size={13} className="shrink-0 text-gray-400" />
              <input
                type="search"
                placeholder="搜索进程 / 域名 / IP…"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                className="flex-1 bg-transparent text-sm outline-none placeholder:text-gray-400 dark:placeholder:text-gray-600"
              />
              <span className="text-xs text-gray-400">{search ? `${filteredRequests.length} / ${requests.length}` : requests.length} 条</span>
            </div>
          )}
          {requests.length === 0 ? (
            <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">
              暂无网络请求。mihomo 运行并产生流量后，活跃连接会每 3 秒按「进程+目标」去重、只刷新时间。
            </div>
          ) : filteredRequests.length === 0 ? (
            <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">
              没有匹配「{search}」的记录。
            </div>
          ) : (
            <div className="max-h-[32rem] overflow-auto rounded-lg border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
              <table className="w-full text-left text-[11px]">
                <thead className="sticky top-0 bg-gray-50 text-gray-500 dark:bg-gray-800/60">
                  <tr>
                    <th className="px-3 py-2 font-medium">时间</th><th className="px-3 py-2 font-medium">进程</th>
                    <th className="px-3 py-2 font-medium">目标</th><th className="px-3 py-2 font-medium">出口</th>
                    <th className="px-3 py-2 font-medium">规则</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
                  {filteredRequests.map((request, index) => (
                    <tr key={`${request.conn_id}-${index}`}>
                      <td className="whitespace-nowrap px-3 py-2 text-gray-400">{ts(request.ts_ms)}</td>
                      <td className="px-3 py-2 font-mono" title={request.process_path}>{request.process || '(?)'}</td>
                      <td className="px-3 py-2 font-mono text-gray-600 dark:text-gray-400">
                        {request.host || request.dest_ip}<span className="text-gray-400">:{request.dest_port}</span>
                      </td>
                      <td className="px-3 py-2 font-mono">{request.outbound}</td>
                      <td className="px-3 py-2 text-gray-500">{request.rule}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <p className="text-[11px] text-gray-500 dark:text-gray-400">
            数据保存在 <code>~/.config/net-policy-agent/net-policy/net-policy.db</code>，按「进程+目标」去重、上限 10 万条，可随时清空。
          </p>
        </>
      )}

      {view === 'events' && (
        events.length === 0 ? (
          <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">暂无生命周期事件。</div>
        ) : (
          <ul className="divide-y divide-gray-100 rounded-lg border border-gray-200 bg-white px-3 text-xs dark:divide-gray-800 dark:border-gray-800 dark:bg-gray-900">
            {events.map((event, index) => (
              <li key={index} className="flex items-center gap-2 py-2">
                <span className="whitespace-nowrap text-gray-400">{ts(event.ts_ms)}</span>
                <span className="rounded bg-gray-100 px-1.5 py-0.5 font-mono text-[10px] dark:bg-gray-800">{event.kind}</span>
                {event.detail && <span className="min-w-0 flex-1 truncate text-gray-500" title={event.detail}>{event.detail}</span>}
              </li>
            ))}
          </ul>
        )
      )}

      {view === 'processes' && (
        tree.length === 0 ? (
          <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">未读取到当前进程树。</div>
        ) : (
          <div className="max-h-[36rem] overflow-auto rounded-lg border border-gray-200 bg-white p-3 dark:border-gray-800 dark:bg-gray-900">
            {tree.map((node) => <TreeNode key={node.pid} node={node} depth={0} />)}
          </div>
        )
      )}
    </div>
  )
}
