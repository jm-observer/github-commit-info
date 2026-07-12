import { useState, useMemo } from 'react'
import { Pin, Search } from 'lucide-react'
import type { Connection, DomainAssoc, Rule, RuleSet } from '../api/tauri-client'
import { NetPolicyAPI } from '../api/tauri-client'

/**
 * 观察主表：将活跃连接按 host/IP 聚合，叠加现有规则（pinned 行置顶），
 * 每行可直接改路（saveRule + reload）。
 *
 * 接受 connections（来自 ProbeContext.conns.connections）和 rules（本页本地状态）。
 * onRulesChange：放行动作后通知父组件刷新 rules / 触发 reload。
 */

type RouteLabel = 'direct' | 'wg' | 'blackhole'

function outboundToRoute(outbound: string): RouteLabel {
  if (outbound === 'DIRECT') return 'direct'
  if (outbound === 'wg-out' || outbound.toLowerCase().includes('wg')) return 'wg'
  return 'blackhole'
}

const ROUTE_BADGE: Record<RouteLabel, { cls: string; label: string }> = {
  direct: {
    cls: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-300',
    label: '直连',
  },
  wg: {
    cls: 'bg-blue-100 text-blue-700 dark:bg-blue-950/50 dark:text-blue-300',
    label: 'VPN',
  },
  blackhole: {
    cls: 'bg-red-100 text-red-700 dark:bg-red-950/50 dark:text-red-300',
    label: '阻断',
  },
}

interface GroupedRow {
  /** 域名（或空）。 */
  host: string
  /** 最近一条连接的目标 IP。 */
  ip: string
  /** 所有涉及进程（去重）。 */
  processes: string[]
  /** 当前出口（取第一条连接的 outbound）。 */
  outbound: string
  /** 是否有已落地规则（pinned）。 */
  pinned: boolean
  /** 是否是纯 IP 行（无 host 域名）。 */
  isRawIp: boolean
  /**
   * 纯 IP 行从 dnsMap 反查到的关联域名（fake-ip 陷阱：有域名时应优先按域名下发规则，
   * 否则该 IP 规则只对「程序直连裸 IP」的流量生效，命中不了走代理走域名解析的流量）。
   */
  assocDomain: string
}

function buildRows(connections: Connection[], rules: Rule[], dnsMap: DomainAssoc[]): GroupedRow[] {
  const map = new Map<string, GroupedRow>()
  // IP → 域名反查（dnsMap 每行是 domain -> ips[]，这里建 ip -> domain 索引，取第一个匹配的域名即可）。
  const ipToDomain = new Map<string, string>()
  for (const d of dnsMap) {
    for (const ip of d.ips) {
      if (!ipToDomain.has(ip)) ipToDomain.set(ip, d.domain)
    }
  }

  for (const c of connections) {
    const key = c.host || c.destination_ip
    if (!key) continue
    const existing = map.get(key)
    if (existing) {
      if (c.process && !existing.processes.includes(c.process)) {
        existing.processes.push(c.process)
      }
    } else {
      map.set(key, {
        host: c.host || '',
        ip: c.destination_ip,
        processes: c.process ? [c.process] : [],
        outbound: c.outbound,
        pinned: false,
        isRawIp: !c.host,
        assocDomain: !c.host && c.destination_ip ? ipToDomain.get(c.destination_ip) ?? '' : '',
      })
    }
  }

  // Pin rows that have a matching rule.
  for (const r of rules) {
    const val = r.value
    for (const [key, row] of map) {
      if (
        (r.kind === 'domain-suffix' && (key === val || key.endsWith('.' + val))) ||
        (r.kind === 'ip-cidr' && row.ip && val.split('/')[0] === row.ip)
      ) {
        row.pinned = true
        break
      }
    }
  }

  // Also add rule targets that aren't currently in active connections (pinned-only rows).
  for (const r of rules) {
    if (r.kind !== 'domain-suffix' && r.kind !== 'ip-cidr') continue
    const key = r.value.split('/')[0] // strip CIDR
    if (!map.has(key)) {
      map.set(key, {
        host: r.kind === 'domain-suffix' ? r.value : '',
        ip: r.kind === 'ip-cidr' ? key : '',
        processes: [],
        outbound: r.route === 'direct' ? 'DIRECT' : r.route === 'wg' ? 'wg-out' : 'REJECT-DROP',
        pinned: true,
        isRawIp: r.kind === 'ip-cidr',
        assocDomain: r.kind === 'ip-cidr' ? ipToDomain.get(key) ?? '' : '',
      })
    }
  }

  const rows = Array.from(map.values())
  // Pinned rows first, then by host name.
  rows.sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1
    return (a.host || a.ip).localeCompare(b.host || b.ip)
  })
  return rows
}

export function ObserverTable({
  connections,
  rules,
  applied,
  busy,
  dnsMap,
  /** 未提权时（改路可能触发 reload 落防火墙/TUN）禁用改路交互，给出友好提示而非撞后端原始错误。 */
  canReroute = true,
  onRulesChange,
  onFlash,
}: {
  connections: Connection[]
  rules: RuleSet
  applied: boolean
  busy: boolean
  dnsMap?: DomainAssoc[]
  canReroute?: boolean
  onRulesChange: (rs: RuleSet) => void
  onFlash: (kind: 'ok' | 'err', text: string) => void
}) {
  const [actionBusy, setActionBusy] = useState<string | null>(null)
  const [search, setSearch] = useState('')

  const rows = useMemo(() => buildRows(connections, rules.rules, dnsMap ?? []), [connections, rules, dnsMap])

  const filteredRows = useMemo(() => {
    const q = search.trim().toLowerCase()
    if (!q) return rows
    return rows.filter(
      (r) =>
        (r.host && r.host.toLowerCase().includes(q)) ||
        (r.ip && r.ip.toLowerCase().includes(q)) ||
        r.processes.some((p) => p.toLowerCase().includes(q)),
    )
  }, [rows, search])

  const reroute = async (row: GroupedRow, route: RouteLabel) => {
    const target = row.host || row.ip
    if (!target) return
    const key = `${target}:${route}`
    setActionBusy(key)
    try {
      // fake-ip 陷阱：纯 IP 行若能从 dnsMap 反查到关联域名，优先按 DOMAIN-SUFFIX 下发——
      // 否则该 IP 规则只对「程序直连裸 IP」的流量生效，命中不了走 fake-ip 解析的同域名流量。
      const isIp = (row.isRawIp || !row.host) && !row.assocDomain
      const ruleObj: Rule = isIp
        ? {
            kind: 'ip-cidr',
            value: `${row.ip}${row.ip.includes(':') ? '/128' : '/32'}`,
            route,
          }
        : { kind: 'domain-suffix', value: row.host || row.assocDomain, route }

      // 后端 saveRule 现为 upsert（同 kind+value 的旧规则会被先移除再追加），不再需要前端先删后加。
      const rs = await NetPolicyAPI.saveRule(ruleObj)
      onRulesChange(rs)
      if (applied) {
        try {
          await NetPolicyAPI.reload()
        } catch (e) {
          // 热载失败：规则已落盘但引擎运行态未变，需强制拉一次真实规则集同步显示（不留旧乐观状态）。
          onFlash('err', `${target} 改路已保存，但引擎热载失败：${String(e)}——规则尚未生效，请重试或重新应用。`)
          try {
            onRulesChange(await NetPolicyAPI.listRules())
          } catch {
            // listRules 也失败就保留当前 rs，不再进一步处理。
          }
          return
        }
      }
      onFlash('ok', `已设定 ${target} → ${ROUTE_BADGE[route].label}`)
    } catch (e) {
      onFlash('err', `改路失败: ${String(e)}`)
      // saveRule 本身失败：不清楚落盘状态是否与显示一致，强制同步真实规则集。
      try {
        onRulesChange(await NetPolicyAPI.listRules())
      } catch {
        // 忽略：保留原状态。
      }
    } finally {
      setActionBusy(null)
    }
  }

  return (
    <details className="group rounded-xl border border-gray-200 dark:border-gray-800" open>
      {/* ── 摘要行（折叠控制） ── */}
      <summary className="flex cursor-pointer list-none items-center gap-2 border-b border-gray-200 px-4 py-2.5 select-none hover:bg-gray-50 dark:border-gray-800 dark:hover:bg-gray-800/40">
        <span className="text-gray-400 transition-transform group-open:rotate-90">▶</span>
        <h2 className="text-sm font-semibold">观察主表</h2>
        <span className="text-xs text-gray-500">域名/IP 聚合 · 进程关联 · 可直接改路</span>
        {rows.length > 0 && (
          <span className="ml-auto text-xs text-gray-400">
            {search ? `${filteredRows.length} / ${rows.length}` : rows.length} 条
          </span>
        )}
      </summary>

      {/* ── 搜索栏 ── */}
      <div className="flex items-center gap-2 border-b border-gray-100 px-4 py-2 dark:border-gray-800">
        <Search size={13} className="shrink-0 text-gray-400" />
        <input
          type="search"
          placeholder="搜索域名 / IP / 进程…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          onClick={(e) => e.stopPropagation()}
          className="flex-1 bg-transparent text-sm outline-none placeholder:text-gray-400 dark:placeholder:text-gray-600"
        />
        {/* 图例 */}
        <div className="ml-auto flex items-center gap-3 text-[11px] text-gray-400">
          <span className="inline-flex items-center gap-1">
            <span className="inline-block h-2 w-2 rounded-full bg-gray-400" /> 直连
          </span>
          <span className="inline-flex items-center gap-1">
            <span className="inline-block h-2 w-2 rounded-full bg-blue-500" /> VPN
          </span>
          <span className="inline-flex items-center gap-1">
            <span className="inline-block h-2 w-2 rounded-full bg-red-500" /> 阻断
          </span>
        </div>
      </div>

      {/* ── 表格 ── */}
      {filteredRows.length === 0 ? (
        <div className="px-4 py-6 text-center text-sm text-gray-500">
          {!applied
            ? '开始观察后，这里列出每条连接的 域名/IP → 出口，可逐行改路。'
            : search
              ? `没有匹配「${search}」的记录。`
              : '引擎运行中，暂无活跃连接。'}
        </div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-gray-100 text-[11px] uppercase tracking-wide text-gray-400 dark:border-gray-800">
                <th className="px-4 py-1.5 font-medium">目标</th>
                <th className="px-2 py-1.5 font-medium">当前出口</th>
                <th className="px-2 py-1.5 font-medium">改路</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
              {filteredRows.map((row) => {
                const key = row.host || row.ip
                const routeVal = outboundToRoute(row.outbound)
                const badge = ROUTE_BADGE[routeVal]
                // 未提权且策略已应用：改路会触发 reload（落防火墙/TUN），未提权下必然失败——禁用并友好提示，
                // 不让用户撞后端原始错误字符串。
                const rerouteDisabled = !!actionBusy || busy || (applied && !canReroute)
                const rerouteTitle = applied && !canReroute
                  ? '未以管理员身份运行——改路需要热载防火墙/TUN，请先以管理员身份重启后再试。'
                  : row.isRawIp && !row.assocDomain
                    ? 'fake-ip 模式下，带域名的流量按域名匹配，IP 规则只对程序直连裸 IP 的流量生效。'
                    : row.isRawIp && row.assocDomain
                      ? `已关联域名 ${row.assocDomain}：改路将按该域名下发（更可靠）。`
                      : undefined

                return (
                  <tr key={key} className={row.pinned ? 'bg-sky-50/30 dark:bg-sky-950/10' : undefined}>
                    <td className="px-4 py-2 align-top">
                      <div className="flex items-center gap-1.5">
                        {row.pinned && (
                          <span title="已有规则">
                            <Pin size={11} className="shrink-0 text-sky-500" />
                          </span>
                        )}
                        <span className="font-mono text-xs" title={key}>
                          {key}
                        </span>
                        {row.isRawIp && (
                          <span
                            className="rounded bg-gray-200 px-1 py-px text-[10px] text-gray-500 dark:bg-gray-700 dark:text-gray-400"
                            title="fake-ip 模式下，带域名的流量按域名匹配，IP 规则只对程序直连裸 IP 的流量生效。"
                          >
                            IP
                          </span>
                        )}
                        {row.isRawIp && row.assocDomain && (
                          <span
                            className="rounded bg-sky-100 px-1 py-px text-[10px] text-sky-700 dark:bg-sky-950/50 dark:text-sky-300"
                            title={`关联域名 ${row.assocDomain}：改路将优先按该域名下发规则。`}
                          >
                            → {row.assocDomain}
                          </span>
                        )}
                      </div>
                      {(row.ip || row.processes.length > 0) && (
                        <div className="mt-0.5 pl-4 text-[11px] text-gray-400">
                          {row.ip && row.host && <span className="mr-1.5">{row.ip}</span>}
                          {row.processes.slice(0, 3).join(' · ')}
                          {row.processes.length > 3 && <span> +{row.processes.length - 3}</span>}
                        </div>
                      )}
                    </td>
                    <td className="px-2 py-2 align-top">
                      <span className={`inline-flex rounded px-1.5 py-0.5 text-xs ${badge.cls}`}>
                        {badge.label}
                      </span>
                    </td>
                    <td className="px-2 py-2 align-top">
                      <select
                        className="rounded border px-1.5 py-0.5 text-xs dark:border-gray-700 dark:bg-gray-800"
                        value=""
                        disabled={rerouteDisabled}
                        title={rerouteTitle}
                        onChange={(e) => {
                          const v = e.target.value as RouteLabel
                          if (v) void reroute(row, v)
                          e.target.value = ''
                        }}
                      >
                        <option value="">改路…</option>
                        <option value="direct">直连</option>
                        <option value="wg">走VPN</option>
                        <option value="blackhole">阻断</option>
                      </select>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
    </details>
  )
}
