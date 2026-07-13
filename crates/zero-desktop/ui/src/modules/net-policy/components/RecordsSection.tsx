import { useCallback, useEffect, useState } from 'react'
import { History, ListTree, ScrollText, RefreshCw, Trash2 } from 'lucide-react'
import {
  NetPolicyAPI,
  type LifecycleEvent,
  type ProcessNode,
  type RequestLogEntry,
} from '../api/tauri-client'

/**
 * 记录区：
 *  - 进程请求记录（SQLite requests 表，采样自 mihomo 活跃连接，含进程/路径/目标/出口/规则）；
 *  - 生命周期事件（agent 启停 / 策略 apply·stop / 临时直连开关）；
 *  - 当前进程树。
 * 均按需拉取（展开时刷新）+ 手动刷新；请求/事件可清空（隐私）。
 */

function ts(ms: number): string {
  if (!ms) return ''
  return new Date(ms).toLocaleString()
}

function SubSection({
  title,
  icon,
  count,
  onRefresh,
  onClear,
  loading,
  children,
}: {
  title: string
  icon: React.ReactNode
  count: number
  onRefresh: () => void
  onClear?: () => void
  loading: boolean
  children: React.ReactNode
}) {
  return (
    <details
      className="group rounded-lg border border-gray-200 dark:border-gray-800"
      onToggle={(e) => (e.currentTarget as HTMLDetailsElement).open && onRefresh()}
    >
      <summary className="flex cursor-pointer list-none items-center gap-2 border-b border-gray-200 px-4 py-2 select-none hover:bg-gray-50 dark:border-gray-800 dark:hover:bg-gray-800/40">
        <span className="text-[10px] text-gray-400 transition-transform group-open:rotate-90">▶</span>
        {icon}
        <h2 className="text-sm font-semibold">{title}</h2>
        <div className="ml-auto flex items-center gap-2 text-xs text-gray-500" onClick={(e) => e.stopPropagation()}>
          <span>{count} 条</span>
          <button
            className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
            onClick={onRefresh}
            disabled={loading}
          >
            <RefreshCw size={12} className={loading ? 'animate-spin' : ''} /> 刷新
          </button>
          {onClear && (
            <button
              className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
              onClick={onClear}
              disabled={loading || count === 0}
            >
              <Trash2 size={12} /> 清空
            </button>
          )}
        </div>
      </summary>
      <div className="p-3">{children}</div>
    </details>
  )
}

function TreeNode({ node, depth }: { node: ProcessNode; depth: number }) {
  return (
    <div>
      <div className="flex items-center gap-2 py-0.5 text-xs" style={{ paddingLeft: depth * 14 }}>
        <span className="font-mono text-gray-800 dark:text-gray-200">{node.name || '(?)'}</span>
        <span className="text-gray-400">#{node.pid}</span>
        {node.path && (
          <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-gray-400" title={node.path}>
            {node.path}
          </span>
        )}
      </div>
      {node.children.map((c) => (
        <TreeNode key={c.pid} node={c} depth={depth + 1} />
      ))}
    </div>
  )
}

export function RecordsSection() {
  const [requests, setRequests] = useState<RequestLogEntry[]>([])
  const [events, setEvents] = useState<LifecycleEvent[]>([])
  const [tree, setTree] = useState<ProcessNode[]>([])
  const [loading, setLoading] = useState<{ req: boolean; ev: boolean; tree: boolean }>({
    req: false,
    ev: false,
    tree: false,
  })

  const loadReq = useCallback(async () => {
    setLoading((l) => ({ ...l, req: true }))
    try {
      setRequests(await NetPolicyAPI.requests(500))
    } catch {
      /* 后端会以结构化错误返回；此处静默，用户可点刷新重试 */
    } finally {
      setLoading((l) => ({ ...l, req: false }))
    }
  }, [])
  const loadEv = useCallback(async () => {
    setLoading((l) => ({ ...l, ev: true }))
    try {
      setEvents(await NetPolicyAPI.events(200))
    } catch {
      /* noop */
    } finally {
      setLoading((l) => ({ ...l, ev: false }))
    }
  }, [])
  const loadTree = useCallback(async () => {
    setLoading((l) => ({ ...l, tree: true }))
    try {
      setTree(await NetPolicyAPI.processTree())
    } catch {
      /* noop */
    } finally {
      setLoading((l) => ({ ...l, tree: false }))
    }
  }, [])

  const clearReq = async () => {
    await NetPolicyAPI.clearRequests()
    await loadReq()
  }
  const clearEv = async () => {
    await NetPolicyAPI.clearEvents()
    await loadEv()
  }

  useEffect(() => {
    void loadEv()
  }, [loadEv])

  return (
    <div className="space-y-2">
      {/* 进程请求记录 */}
      <SubSection
        title="进程请求记录"
        icon={<History size={15} className="text-indigo-500" />}
        count={requests.length}
        onRefresh={() => void loadReq()}
        onClear={() => void clearReq()}
        loading={loading.req}
      >
        {requests.length === 0 ? (
          <p className="py-2 text-sm text-gray-500">
            暂无记录——mihomo 在跑并产生流量后，活跃连接会被采样记入（每 3s，去重）。
          </p>
        ) : (
          <div className="max-h-80 overflow-auto rounded border border-gray-200 dark:border-gray-800">
            <table className="w-full text-left text-[11px]">
              <thead className="sticky top-0 bg-gray-50 text-gray-500 dark:bg-gray-800/60">
                <tr>
                  <th className="px-2 py-1 font-medium">时间</th>
                  <th className="px-2 py-1 font-medium">进程</th>
                  <th className="px-2 py-1 font-medium">目标</th>
                  <th className="px-2 py-1 font-medium">出口</th>
                  <th className="px-2 py-1 font-medium">规则</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
                {requests.map((r, i) => (
                  <tr key={`${r.conn_id}-${i}`}>
                    <td className="whitespace-nowrap px-2 py-1 text-gray-400">{ts(r.ts_ms)}</td>
                    <td className="px-2 py-1 font-mono" title={r.process_path}>
                      {r.process || '(?)'}
                    </td>
                    <td className="px-2 py-1 font-mono text-gray-600 dark:text-gray-400">
                      {r.host || r.dest_ip}
                      <span className="text-gray-400">:{r.dest_port}</span>
                    </td>
                    <td className="px-2 py-1 font-mono">{r.outbound}</td>
                    <td className="px-2 py-1 text-gray-500">{r.rule}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <p className="mt-2 px-1 text-[11px] text-gray-500 dark:text-gray-400">
          存于 <code>~/.config/net-policy-agent/net-policy/net-policy.db</code>；保留上限 10 万条，可随时清空（隐私）。
        </p>
      </SubSection>

      {/* 生命周期事件 */}
      <SubSection
        title="生命周期事件"
        icon={<ScrollText size={15} className="text-teal-500" />}
        count={events.length}
        onRefresh={() => void loadEv()}
        onClear={() => void clearEv()}
        loading={loading.ev}
      >
        {events.length === 0 ? (
          <p className="py-2 text-sm text-gray-500">暂无事件。</p>
        ) : (
          <ul className="divide-y divide-gray-100 text-xs dark:divide-gray-800">
            {events.map((e, i) => (
              <li key={i} className="flex items-center gap-2 py-1">
                <span className="whitespace-nowrap text-gray-400">{ts(e.ts_ms)}</span>
                <span className="rounded bg-gray-200 px-1.5 py-0.5 font-mono text-[10px] dark:bg-gray-700">{e.kind}</span>
                {e.detail && <span className="min-w-0 flex-1 truncate text-gray-500" title={e.detail}>{e.detail}</span>}
              </li>
            ))}
          </ul>
        )}
      </SubSection>

      {/* 进程树 */}
      <SubSection
        title="进程树"
        icon={<ListTree size={15} className="text-amber-500" />}
        count={tree.length}
        onRefresh={() => void loadTree()}
        loading={loading.tree}
      >
        {tree.length === 0 ? (
          <p className="py-2 text-sm text-gray-500">点「刷新」加载当前进程树。</p>
        ) : (
          <div className="max-h-96 overflow-auto rounded border border-gray-200 p-2 dark:border-gray-800">
            {tree.map((n) => (
              <TreeNode key={n.pid} node={n} depth={0} />
            ))}
          </div>
        )}
      </SubSection>
    </div>
  )
}
