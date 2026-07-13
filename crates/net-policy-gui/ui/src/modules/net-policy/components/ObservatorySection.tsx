import { useState } from 'react'
import { Ban, Globe2, Network, Trash2, ShieldCheck } from 'lucide-react'
import type { BlockedEntry, DomainAssoc } from '../api/tauri-client'

/**
 * Phase 4 可观测区：
 *  - 「被阻断尝试」feed：默认黑洞下「什么被挡了」——每条一键「直连放行 / 海外放行」（→ 加规则 + 热载）。
 *  - 「域名 ↔ IP / 进程」关联表：累积自历次活跃连接，回答「谁在连什么、解析到哪些 IP」。
 *
 * 数据由全局 ProbeContext 3s 轮询喂入；本组件只渲染 + 透传放行/清空动作。
 */

function ago(ms: number): string {
  if (!ms) return ''
  const sec = Math.max(0, Math.floor((Date.now() - ms) / 1000))
  if (sec < 60) return `${sec}s 前`
  if (sec < 3600) return `${Math.floor(sec / 60)}m 前`
  return `${Math.floor(sec / 3600)}h 前`
}

function Section({
  title,
  icon,
  right,
  children,
  defaultOpen = true,
}: {
  title: string
  icon: React.ReactNode
  right?: React.ReactNode
  children: React.ReactNode
  defaultOpen?: boolean
}) {
  return (
    <details className="group rounded-lg border border-gray-200 dark:border-gray-800" open={defaultOpen}>
      <summary className="flex cursor-pointer list-none items-center gap-2 border-b border-gray-200 px-4 py-2 select-none hover:bg-gray-50 dark:border-gray-800 dark:hover:bg-gray-800/40">
        <span className="text-[10px] text-gray-400 transition-transform group-open:rotate-90">▶</span>
        {icon}
        <h2 className="text-sm font-semibold">{title}</h2>
        {right && (
          <div className="ml-auto" onClick={(e) => e.stopPropagation()}>
            {right}
          </div>
        )}
      </summary>
      <div className="p-3">{children}</div>
    </details>
  )
}

export function ObservatorySection({
  blocked,
  dnsMap,
  busy,
  /** 未提权且策略已应用时放行会触发 reload 必然失败，禁用按钮并提示，而不是让用户撞后端原始错误。 */
  canAllow = true,
  onAllow,
  onClear,
}: {
  blocked: BlockedEntry[]
  dnsMap: DomainAssoc[]
  busy: boolean
  canAllow?: boolean
  onAllow: (entry: BlockedEntry, route: 'direct' | 'wg') => void
  onClear: () => void
}) {
  const allowDisabled = busy || !canAllow
  const allowTitleSuffix = !canAllow
    ? '——未以管理员身份运行，放行需要热载防火墙/TUN，请先以管理员身份重启后再试。'
    : ''
  const [showMap, setShowMap] = useState(false)
  const mapRows = showMap ? dnsMap : dnsMap.slice(0, 12)

  return (
    <div className="space-y-3 rounded-xl border border-rose-200/70 bg-rose-50/30 p-3 dark:border-rose-900/40 dark:bg-rose-950/10">
      <div className="flex items-center gap-2 px-1 text-xs font-semibold uppercase tracking-wide text-rose-700 dark:text-rose-300">
        <span>③ 可观测 · 逐项放行</span>
        <span className="font-normal normal-case text-rose-600/70 dark:text-rose-400/70">
          看「什么被挡了」→ 一键放行；看域名↔IP/进程关联
        </span>
      </div>

      {/* 被阻断尝试 feed */}
      <Section
        title="被阻断尝试（命中默认黑洞 / REJECT）"
        icon={<Ban size={15} className="text-rose-500" />}
        right={
          <div className="flex items-center gap-2 text-xs text-gray-500">
            <span>{blocked.length} 条</span>
            <button
              className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
              onClick={onClear}
              disabled={busy || blocked.length === 0}
            >
              <Trash2 size={12} /> 清空
            </button>
          </div>
        }
      >
        {blocked.length === 0 ? (
          <p className="py-2 text-sm text-gray-500">
            暂无被阻断记录——应用策略并产生流量后，被默认出口拦下的连接会出现在这里（需 mihomo 在跑）。
          </p>
        ) : (
          <ul className="divide-y divide-gray-200 text-sm dark:divide-gray-800">
            {blocked.map((b, i) => (
              <li key={`${b.network}|${b.host}|${b.dest_port}|${i}`} className="flex items-center gap-2 py-1.5">
                <span className="w-9 shrink-0 rounded bg-gray-200 px-1 text-center text-[10px] uppercase text-gray-600 dark:bg-gray-700 dark:text-gray-300">
                  {b.network}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs" title={`${b.host}:${b.dest_port}  · 规则 ${b.rule} → ${b.outbound}`}>
                  {b.host}
                  <span className="text-gray-400">:{b.dest_port}</span>
                  {b.count > 1 && <span className="ml-1 text-amber-600">×{b.count}</span>}
                </span>
                <span className="hidden shrink-0 text-[11px] text-gray-400 sm:inline">{ago(b.last_ms)}</span>
                <button
                  className="shrink-0 rounded bg-amber-100 px-2 py-0.5 text-xs text-amber-800 hover:bg-amber-200 disabled:opacity-50 dark:bg-amber-950/50 dark:text-amber-300"
                  onClick={() => onAllow(b, 'direct')}
                  disabled={allowDisabled}
                  title={`加一条放行规则（本地直连）并热加载${allowTitleSuffix}`}
                >
                  直连放行
                </button>
                <button
                  className="shrink-0 rounded bg-blue-100 px-2 py-0.5 text-xs text-blue-800 hover:bg-blue-200 disabled:opacity-50 dark:bg-blue-950/50 dark:text-blue-300"
                  onClick={() => onAllow(b, 'wg')}
                  disabled={allowDisabled}
                  title={`加一条放行规则（走海外 SBN）并热加载${allowTitleSuffix}`}
                >
                  海外放行
                </button>
              </li>
            ))}
          </ul>
        )}
        <p className="mt-2 px-1 text-[11px] text-gray-500 dark:text-gray-400">
          放行按目标维度：host 是域名 → 加 DOMAIN-SUFFIX；是 IP → 加 IP-CIDR。按「程序」放行请用上方「现状 → 扫描进程」。
        </p>
      </Section>

      {/* 域名 ↔ IP / 进程 关联 */}
      <Section
        title="域名 ↔ IP / 进程 关联"
        icon={<Globe2 size={15} className="text-sky-500" />}
        right={
          <span className="text-xs text-gray-500">
            {dnsMap.length} 个域名{dnsMap.length > mapRows.length ? `（显示前 ${mapRows.length}）` : ''}
          </span>
        }
      >
        {dnsMap.length === 0 ? (
          <p className="py-2 text-sm text-gray-500">暂无数据——产生活跃连接后，域名与其解析 IP、发起进程的关联会累积在这里。</p>
        ) : (
          <>
            <div className="overflow-hidden rounded border border-gray-200 dark:border-gray-800">
              <table className="w-full text-left text-xs">
                <thead className="bg-gray-50 text-gray-500 dark:bg-gray-800/40">
                  <tr>
                    <th className="px-2 py-1 font-medium">域名</th>
                    <th className="px-2 py-1 font-medium">IP</th>
                    <th className="px-2 py-1 font-medium">进程</th>
                    <th className="px-2 py-1 text-right font-medium">次数</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
                  {mapRows.map((d) => (
                    <tr key={d.domain}>
                      <td className="px-2 py-1 font-mono">{d.domain}</td>
                      <td className="px-2 py-1 font-mono text-gray-600 dark:text-gray-400">
                        {d.ips.length ? d.ips.join(', ') : <span className="text-gray-400">—</span>}
                      </td>
                      <td className="px-2 py-1 text-gray-600 dark:text-gray-400">
                        {d.processes.length ? d.processes.join(', ') : <span className="text-gray-400">—</span>}
                      </td>
                      <td className="px-2 py-1 text-right text-gray-500">{d.count}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {dnsMap.length > 12 && (
              <button
                className="mt-2 inline-flex items-center gap-1 text-[11px] text-sky-600 hover:underline"
                onClick={() => setShowMap((v) => !v)}
              >
                <Network size={12} /> {showMap ? '收起' : `展开全部 ${dnsMap.length} 个`}
              </button>
            )}
          </>
        )}
        <p className="mt-2 flex items-center gap-1 px-1 text-[11px] text-gray-500 dark:text-gray-400">
          <ShieldCheck size={11} /> fake-ip 模式下部分 IP 为映射地址（198.18.x.x）；直连命中的为真实解析 IP。
        </p>
      </Section>
    </div>
  )
}
