import { useMemo, useState } from 'react'
import { Globe2, Network, ShieldCheck, Trash2 } from 'lucide-react'
import type { BlockedEntry, DomainAssoc } from '../api/tauri-client'

function ago(ms: number): string {
  if (!ms) return ''
  const sec = Math.max(0, Math.floor((Date.now() - ms) / 1000))
  if (sec < 60) return `${sec}s 前`
  if (sec < 3600) return `${Math.floor(sec / 60)}m 前`
  return `${Math.floor(sec / 3600)}h 前`
}

export function BlockedFeed({
  blocked,
  busy,
  canAllow = true,
  onAllow,
  onClear,
}: {
  blocked: BlockedEntry[]
  busy: boolean
  canAllow?: boolean
  onAllow: (entry: BlockedEntry, route: 'direct' | 'wg') => void
  onClear: () => void
}) {
  const allowDisabled = busy || !canAllow
  const allowTitleSuffix = !canAllow
    ? '——未以管理员身份运行，放行需要热载防火墙/TUN，请先以管理员身份重启后再试。'
    : ''

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-gray-500">仅记录命中默认黑洞 / REJECT 的连接；放行会立即新增规则并热加载。</p>
        <div className="flex shrink-0 items-center gap-2 text-xs text-gray-500">
          <span>{blocked.length} 条</span>
          <button
            className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-1 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
            onClick={onClear}
            disabled={busy || blocked.length === 0}
          >
            <Trash2 size={12} /> 清空
          </button>
        </div>
      </div>

      {blocked.length === 0 ? (
        <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">
          暂无被阻断记录。切换到“阻断·收紧”并产生流量后，未被规则放行的目标会出现在这里。
        </div>
      ) : (
        <div className="overflow-hidden rounded-lg border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
          <ul className="divide-y divide-gray-100 text-sm dark:divide-gray-800">
            {blocked.map((entry, index) => (
              <li key={`${entry.network}|${entry.host}|${entry.dest_port}|${index}`} className="flex items-center gap-2 px-3 py-2">
                <span className="w-9 shrink-0 rounded bg-gray-100 px-1 text-center text-[10px] uppercase text-gray-600 dark:bg-gray-800 dark:text-gray-300">
                  {entry.network}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs" title={`${entry.host}:${entry.dest_port} · ${entry.rule} → ${entry.outbound}`}>
                  {entry.host}<span className="text-gray-400">:{entry.dest_port}</span>
                  {entry.count > 1 && <span className="ml-1 text-amber-600">×{entry.count}</span>}
                </span>
                <span className="hidden shrink-0 text-[11px] text-gray-400 sm:inline">{ago(entry.last_ms)}</span>
                <button
                  className="shrink-0 rounded bg-amber-100 px-2 py-0.5 text-xs text-amber-800 hover:bg-amber-200 disabled:opacity-50 dark:bg-amber-950/50 dark:text-amber-300"
                  onClick={() => onAllow(entry, 'direct')}
                  disabled={allowDisabled}
                  title={`新增本地直连规则并热加载${allowTitleSuffix}`}
                >
                  直连放行
                </button>
                <button
                  className="shrink-0 rounded bg-blue-100 px-2 py-0.5 text-xs text-blue-800 hover:bg-blue-200 disabled:opacity-50 dark:bg-blue-950/50 dark:text-blue-300"
                  onClick={() => onAllow(entry, 'wg')}
                  disabled={allowDisabled}
                  title={`新增海外出口规则并热加载${allowTitleSuffix}`}
                >
                  海外放行
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      <p className="text-[11px] text-gray-500 dark:text-gray-400">
        域名目标会生成 DOMAIN-SUFFIX，裸 IP 会生成 IP-CIDR；按程序放行请前往“策略编排”。
      </p>
    </div>
  )
}

export function DomainMap({ dnsMap }: { dnsMap: DomainAssoc[] }) {
  const [showAll, setShowAll] = useState(false)
  const rows = showAll ? dnsMap : dnsMap.slice(0, 12)

  if (dnsMap.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-gray-300 px-4 py-8 text-center text-sm text-gray-500 dark:border-gray-700">
        暂无关联数据。产生活跃连接后，会在这里累积域名、解析 IP 与发起进程。
      </div>
    )
  }

  return (
    <div className="space-y-2">
      <div className="overflow-hidden rounded-lg border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
        <table className="w-full text-left text-xs">
          <thead className="bg-gray-50 text-gray-500 dark:bg-gray-800/40">
            <tr>
              <th className="px-3 py-2 font-medium">域名</th>
              <th className="px-3 py-2 font-medium">IP</th>
              <th className="px-3 py-2 font-medium">进程</th>
              <th className="px-3 py-2 text-right font-medium">次数</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
            {rows.map((entry) => (
              <tr key={entry.domain}>
                <td className="px-3 py-2 font-mono">{entry.domain}</td>
                <td className="px-3 py-2 font-mono text-gray-600 dark:text-gray-400">
                  {entry.ips.length ? entry.ips.join(', ') : '—'}
                </td>
                <td className="px-3 py-2 text-gray-600 dark:text-gray-400">
                  {entry.processes.length ? entry.processes.join(', ') : '—'}
                </td>
                <td className="px-3 py-2 text-right text-gray-500">{entry.count}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="flex items-center justify-between gap-3">
        <p className="flex items-center gap-1 text-[11px] text-gray-500 dark:text-gray-400">
          <ShieldCheck size={11} /> fake-ip 模式下部分 IP 为映射地址（198.18.x.x）。
        </p>
        {dnsMap.length > 12 && (
          <button className="inline-flex items-center gap-1 text-xs text-sky-600 hover:underline" onClick={() => setShowAll((value) => !value)}>
            {showAll ? <Globe2 size={12} /> : <Network size={12} />}
            {showAll ? '收起' : `查看全部 ${dnsMap.length} 个`}
          </button>
        )}
      </div>
    </div>
  )
}

/**
 * 进程维度视图：把域名维度的 dnsMap 重聚合成「一行一个进程」——该进程的活跃连接数
 * + 它访问过的所有域名。上面「活跃连接」已是域名/目标视角，这里补进程视角。
 */
export function ProcessMap({ dnsMap }: { dnsMap: DomainAssoc[] }) {
  const [showAll, setShowAll] = useState(false)

  const procs = useMemo(() => {
    const m = new Map<string, { process: string; domains: Set<string>; ips: Set<string>; count: number }>()
    for (const e of dnsMap) {
      const list = e.processes.length ? e.processes : ['(unknown)']
      for (const p of list) {
        let cur = m.get(p)
        if (!cur) {
          cur = { process: p, domains: new Set(), ips: new Set(), count: 0 }
          m.set(p, cur)
        }
        cur.domains.add(e.domain)
        e.ips.forEach((ip) => cur!.ips.add(ip))
        cur.count += e.count
      }
    }
    return Array.from(m.values()).sort((a, b) => b.count - a.count || b.domains.size - a.domains.size)
  }, [dnsMap])

  const rows = showAll ? procs : procs.slice(0, 10)

  if (procs.length === 0) {
    return (
      <div className="rounded-lg border border-dashed border-gray-300 px-4 py-8 text-center text-sm text-gray-500 dark:border-gray-700">
        暂无关联数据。产生活跃连接后，会在这里按进程累积它访问过的所有域名。
      </div>
    )
  }

  return (
    <div className="space-y-2">
      <div className="overflow-hidden rounded-lg border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900">
        <table className="w-full text-left text-xs">
          <thead className="bg-gray-50 text-gray-500 dark:bg-gray-800/40">
            <tr>
              <th className="px-3 py-2 font-medium">进程</th>
              <th className="px-3 py-2 text-right font-medium">连接数</th>
              <th className="px-3 py-2 text-right font-medium">域名数</th>
              <th className="px-3 py-2 font-medium">访问的域名</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
            {rows.map((p) => (
              <tr key={p.process} className="align-top">
                <td className="whitespace-nowrap px-3 py-2 font-medium">{p.process}</td>
                <td className="px-3 py-2 text-right text-gray-500">{p.count}</td>
                <td className="px-3 py-2 text-right text-gray-500">{p.domains.size}</td>
                <td className="px-3 py-2">
                  <div className="flex flex-wrap gap-1">
                    {Array.from(p.domains).map((d) => (
                      <span
                        key={d}
                        className="rounded bg-gray-100 px-1.5 py-0.5 font-mono text-[11px] text-gray-600 dark:bg-gray-800 dark:text-gray-300"
                      >
                        {d}
                      </span>
                    ))}
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="flex items-center justify-between gap-3">
        <p className="text-[11px] text-gray-500 dark:text-gray-400">
          按进程聚合：连接数 = 该进程活跃连接累计，右侧是它访问过的所有域名。
        </p>
        {procs.length > 10 && (
          <button className="inline-flex items-center gap-1 text-xs text-sky-600 hover:underline" onClick={() => setShowAll((value) => !value)}>
            {showAll ? <Globe2 size={12} /> : <Network size={12} />}
            {showAll ? '收起' : `查看全部 ${procs.length} 个进程`}
          </button>
        )}
      </div>
    </div>
  )
}
