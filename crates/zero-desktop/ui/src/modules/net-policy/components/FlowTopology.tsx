import { Laptop, Network, Globe2, Cpu, ShieldCheck, ArrowRight, ArrowDown } from 'lucide-react'
import type { Status, ConnectionsSnapshot, Settings } from '../api/tauri-client'

/**
 * 数据通路全景图（设计 §3.1）：
 *   本机应用 → TUN(Meta) → DNS 劫持(fake-ip) → 规则引擎 → DIRECT / wg-out 双分支
 *            → 物理网卡(kill-switch 围栏) → 海外出口 IP
 *
 * 节点按实时状态点亮（绿=正常 / 黄=就绪中或降级 / 灰=未起 / 红=阻断）。
 *
 * 围栏 chips **从 settings + 当前代码已知模型推导**（firewall.rs base_rules_ps）：
 *   KS-mihomo / KS-LO / KS-LAN / KS-IPv6Block(仅 block_ipv6) / KS-TUN + 默认 Block。
 * 不渲染旧模型（KS-WGep / KS-DNS / WG-endpoint:port / DNS-bootstrap:53）。
 *
 * 双分支计数 = 当前连接按 chains 聚合（DIRECT / wg-out），标注「活跃连接」（非累计命中）。
 */

type NodeState = 'on' | 'warn' | 'off' | 'block'

const DOT: Record<NodeState, string> = {
  on: 'bg-green-500',
  warn: 'bg-amber-400',
  off: 'bg-gray-400',
  block: 'bg-red-500',
}

const RING: Record<NodeState, string> = {
  on: 'border-green-400/60 dark:border-green-500/50',
  warn: 'border-amber-400/60 dark:border-amber-500/50',
  off: 'border-gray-300 dark:border-gray-700',
  block: 'border-red-400/60 dark:border-red-500/50',
}

/** 节点取证属性：现状可直接查 / 仅应用策略后才存在。用于左下角小标注。 */
type Provenance = 'live' | 'applied'

function ProvBadge({ prov }: { prov: Provenance }) {
  if (prov === 'live') {
    return (
      <span
        className="absolute left-1.5 bottom-1.5 rounded bg-sky-100 px-1 py-px text-[9px] font-medium leading-none text-sky-700 dark:bg-sky-950/60 dark:text-sky-300"
        title="本机现状即可查（无需应用策略）"
      >
        现状可查
      </span>
    )
  }
  return (
    <span
      className="absolute left-1.5 bottom-1.5 rounded bg-violet-100 px-1 py-px text-[9px] font-medium leading-none text-violet-700 dark:bg-violet-950/60 dark:text-violet-300"
      title="仅在应用网络策略后才存在"
    >
      应用后才有
    </span>
  )
}

function Node({
  icon,
  title,
  sub,
  state,
  prov,
}: {
  icon: React.ReactNode
  title: string
  sub?: string
  state: NodeState
  prov?: Provenance
}) {
  return (
    <div className={`relative flex w-full flex-col items-center gap-1 rounded-lg border bg-white px-3 ${prov ? 'pb-5 pt-2.5' : 'py-2.5'} text-center dark:bg-gray-900 ${RING[state]}`}>
      <span className={`absolute right-2 top-2 inline-block h-2 w-2 rounded-full ${DOT[state]}`} />
      <span className="text-gray-600 dark:text-gray-300">{icon}</span>
      <span className="text-xs font-medium">{title}</span>
      {sub && <span className="text-[11px] text-gray-500 dark:text-gray-400">{sub}</span>}
      {prov && <ProvBadge prov={prov} />}
    </div>
  )
}

function HArrow() {
  return <ArrowRight className="hidden shrink-0 text-gray-300 dark:text-gray-600 md:block" size={18} />
}

function VArrow() {
  return <ArrowDown className="text-gray-300 dark:text-gray-600" size={18} />
}

/** 从 settings 推导围栏放行 chip（与 firewall.rs base_rules_ps + apply_tun 一致）。 */
function fenceChips(settings: Settings | null): { name: string; allow: string; block?: boolean }[] {
  const lan = settings?.lan_ranges?.length ? settings.lan_ranges.join(', ') : 'LAN'
  const chips: { name: string; allow: string; block?: boolean }[] = [
    { name: 'KS-mihomo', allow: 'mihomo.exe 出物理网卡' },
    { name: 'KS-LO', allow: '127.0.0.0/8' },
    { name: 'KS-LAN', allow: lan },
  ]
  if (settings?.block_ipv6) chips.push({ name: 'KS-IPv6Block', allow: '2000::/3', block: true })
  chips.push({ name: 'KS-TUN', allow: 'InterfaceAlias=Meta' })
  return chips
}

export function FlowTopology({
  status,
  conns,
  settings,
}: {
  status: Status
  conns: ConnectionsSnapshot
  settings: Settings | null
}) {
  const running = status.mihomo_running
  const tun = status.tun_ready
  const fwActive = !!status.firewall?.active
  const ruleCount = status.firewall?.rule_count ?? 0
  const appCount = conns.total

  // 节点状态推导。
  const appState: NodeState = appCount > 0 ? 'on' : running ? 'warn' : 'off'
  const tunState: NodeState = running ? (tun ? 'on' : 'warn') : 'off'
  const dnsState: NodeState = tun ? 'on' : running ? 'warn' : 'off'
  const engineState: NodeState = running ? 'on' : 'off'
  const wgState: NodeState = conns.wg_count > 0 ? 'on' : running ? 'warn' : 'off'
  const directState: NodeState = conns.direct_count > 0 ? 'on' : running ? 'warn' : 'off'
  // 围栏：默认 Block 生效（fail-closed）= block 色；未生效（预览）= warn；未应用 = off。
  const fenceState: NodeState = fwActive && ruleCount > 0 ? 'block' : status.applied ? 'warn' : 'off'
  const exitState: NodeState = conns.wg_count > 0 && tun ? 'on' : running ? 'warn' : 'off'

  const chips = fenceChips(settings)

  return (
    <section className="rounded-lg border border-gray-200 dark:border-gray-800">
      <div className="flex items-center justify-between border-b border-gray-200 px-4 py-2 dark:border-gray-800">
        <h2 className="text-sm font-semibold">数据通路全景</h2>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-gray-500">
          <span>活跃连接 {appCount}（绿=正常 · 黄=就绪中/降级 · 灰=未起 · 红=阻断）</span>
          <span className="inline-flex items-center gap-1">
            <span className="rounded bg-sky-100 px-1 py-px text-[9px] font-medium text-sky-700 dark:bg-sky-950/60 dark:text-sky-300">现状可查</span>
            <span className="rounded bg-violet-100 px-1 py-px text-[9px] font-medium text-violet-700 dark:bg-violet-950/60 dark:text-violet-300">应用后才有</span>
          </span>
        </div>
      </div>

      <div className="space-y-3 p-4">
        {/* 上游链路 */}
        <div className="flex flex-col items-stretch gap-2 md:flex-row md:items-center">
          <Node icon={<Laptop size={18} />} title="本机应用" sub={`${appCount} 活跃连接`} state={appState} prov="live" />
          <HArrow />
          <Node icon={<Network size={18} />} title="TUN (Meta)" sub={tun ? '已起栈' : running ? '起栈中' : '未起'} state={tunState} prov="applied" />
          <HArrow />
          <Node icon={<Globe2 size={18} />} title="DNS 劫持" sub="fake-ip 198.18/16" state={dnsState} prov="applied" />
          <HArrow />
          <Node icon={<Cpu size={18} />} title="规则引擎" sub={running ? 'rule mode 在线' : '离线'} state={engineState} prov="applied" />
        </div>

        <div className="flex justify-center"><VArrow /></div>

        {/* 双分支 */}
        <div className="grid grid-cols-2 gap-3">
          <Node
            icon={<Network size={18} />}
            title="DIRECT 本地直连"
            sub={`${conns.direct_count} 活跃连接`}
            state={directState}
            prov="live"
          />
          <Node
            icon={<Globe2 size={18} />}
            title="wg-out 海外隧道"
            sub={`${conns.wg_count} 活跃连接`}
            state={wgState}
            prov="applied"
          />
        </div>

        {/* 默认出口（未命中任何规则的兜底 MATCH）——直连=灰/原样观察，海外=蓝/全走 SBN，黑洞=红/空出口。 */}
        <div className="flex justify-center">
          <div
            className={`inline-flex items-center gap-2 rounded-md border px-3 py-1 text-[11px] ${
              status.default_route === 'blackhole'
                ? 'border-red-300 bg-red-50 text-red-700 dark:border-red-500/50 dark:bg-red-950/40 dark:text-red-300'
                : status.default_route === 'wg'
                  ? 'border-blue-300 bg-blue-50 text-blue-700 dark:border-blue-500/50 dark:bg-blue-950/40 dark:text-blue-300'
                  : 'border-gray-300 bg-gray-50 text-gray-600 dark:border-gray-600 dark:bg-gray-800/40 dark:text-gray-300'
            }`}
            title="未命中任何分流规则时的兜底出口"
          >
            <span className="font-semibold">默认出口</span>
            <span>
              {status.default_route === 'blackhole'
                ? '⛔ 阻断 · 空出口（未放行的一律上不去）'
                : status.default_route === 'wg'
                  ? '🌐 海外 · 全走 SBN'
                  : '↪ 直连 · 原样观察（不改流量）'}
            </span>
          </div>
        </div>

        <div className="flex justify-center"><VArrow /></div>

        {/* kill-switch 围栏 */}
        <div className={`rounded-lg border-2 border-dashed p-3 ${
          fenceState === 'block'
            ? 'border-red-400/70 dark:border-red-500/60'
            : fenceState === 'warn'
              ? 'border-amber-400/70 dark:border-amber-500/60'
              : 'border-gray-300 dark:border-gray-700'
        }`}>
          <div className="mb-2 flex items-center gap-2">
            <ShieldCheck size={16} className={
              fenceState === 'block' ? 'text-red-500' : fenceState === 'warn' ? 'text-amber-500' : 'text-gray-400'
            } />
            <span className="text-xs font-semibold">物理网卡 kill-switch 围栏</span>
            <span className="rounded bg-sky-100 px-1 py-px text-[9px] font-medium text-sky-700 dark:bg-sky-950/60 dark:text-sky-300" title="防火墙状态本机现状即可查（无需应用）">现状可查</span>
            <span className="ml-auto text-[11px] text-gray-500">
              {fenceState === 'block'
                ? `默认 Block 生效 · ${ruleCount} 条放行规则`
                : fenceState === 'warn'
                  ? '已应用但默认 Block 未生效（预览）'
                  : '未应用'}
            </span>
          </div>
          <div className="flex flex-wrap gap-1.5">
            {chips.map(c => (
              <span
                key={c.name}
                className={`rounded px-2 py-0.5 text-[11px] ${
                  c.block
                    ? 'bg-red-100 text-red-700 dark:bg-red-950/60 dark:text-red-300'
                    : 'bg-green-100 text-green-700 dark:bg-green-950/60 dark:text-green-300'
                }`}
                title={c.allow}
              >
                {c.name} · {c.allow}
              </span>
            ))}
            <span className="rounded bg-gray-800 px-2 py-0.5 text-[11px] font-semibold text-white dark:bg-gray-200 dark:text-gray-900">
              其它一律 Block
            </span>
          </div>
        </div>

        <div className="flex justify-center"><VArrow /></div>

        {/* 出口 */}
        <div className="flex justify-center">
          <div className="w-full md:w-1/2">
            <Node icon={<Globe2 size={18} />} title="海外出口 IP" sub="经 WG endpoint" state={exitState} prov="live" />
          </div>
        </div>
      </div>
    </section>
  )
}
