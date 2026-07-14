import { FlaskConical } from 'lucide-react'
import type { Status, VerifyReport } from '../api/tauri-client'

/**
 * 当前实时状态面板：一眼看清「现在」的运行态，区分「实时」与「最近值」。
 *  - 前 4 项（引擎/kill-switch/TUN/默认出口）来自 status，走 3s 快轮询，**真实时**自动更新。
 *  - 后 2 项（出口IP/DNS防泄漏）是重探测（api.ipify.org 10s / DNS 检查），按设计不进快轮询，
 *    只能显示**最近一次自检**的值 + 时间戳，点「自检」手动刷新（见设计 §3.5）。
 * kill-switch 用 firewall.rule_count>0 判断（firewall.active 的 native 读取会误报，见 §13.2）。
 */

type Tone = 'ok' | 'off' | 'warn' | 'block' | 'blue' | 'gray'

const DOT: Record<Tone, string> = {
  ok: 'bg-green-500',
  off: 'bg-gray-400',
  warn: 'bg-amber-400',
  block: 'bg-red-500',
  blue: 'bg-blue-500',
  gray: 'bg-gray-400',
}

function Cell({
  label,
  value,
  sub,
  tone,
  live,
}: {
  label: string
  value: string
  sub?: string
  tone: Tone
  live: boolean
}) {
  return (
    <div className="rounded-lg border border-gray-200 bg-white px-3 py-2 dark:border-gray-800 dark:bg-gray-900">
      <div className="flex items-center gap-1.5 text-[11px] text-gray-400">
        <span className={`inline-block h-2 w-2 shrink-0 rounded-full ${DOT[tone]}`} />
        <span className="truncate">{label}</span>
        <span
          className={`ml-auto shrink-0 rounded px-1 py-px text-[9px] font-medium ${
            live
              ? 'bg-green-100 text-green-700 dark:bg-green-950/50 dark:text-green-300'
              : 'bg-gray-100 text-gray-500 dark:bg-gray-800 dark:text-gray-400'
          }`}
          title={live ? '快轮询实时更新（3s）' : '重探测，非实时——显示最近一次自检值，点「自检」刷新'}
        >
          {live ? '实时' : '最近值'}
        </span>
      </div>
      <div className="mt-0.5 truncate text-sm font-medium" title={value}>{value}</div>
      {sub && <div className="truncate text-[10px] text-gray-400" title={sub}>{sub}</div>}
    </div>
  )
}

export function LiveStatusPanel({
  status,
  verify,
  exitIp,
  exitIpAt,
  onVerify,
  busy,
}: {
  status: Status
  verify: VerifyReport | null
  exitIp: string | null
  exitIpAt: string | null
  onVerify: () => void
  busy: boolean
}) {
  const engine = status.mihomo_running
  const ksRules = status.firewall?.rule_count ?? 0
  const ksOn = ksRules > 0
  const tun = status.tun_ready
  const route = status.default_route
  const routeLabel = route === 'wg' ? '海外 · 全 VPN' : route === 'blackhole' ? '阻断 · 收紧' : '直连 · 观察'
  const routeTone: Tone = route === 'wg' ? 'blue' : route === 'blackhole' ? 'block' : 'gray'

  const dnsCase = verify?.cases?.find((c) => c.id === 'dns-hijack')
  const dnsValue = dnsCase ? (dnsCase.status === 'passed' ? '通过' : dnsCase.status === 'failed' ? '失败' : dnsCase.status) : '未自检'
  const dnsTone: Tone = dnsCase ? (dnsCase.status === 'passed' ? 'ok' : dnsCase.status === 'failed' ? 'block' : 'warn') : 'off'

  return (
    <div className="space-y-2">
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
        <Cell label="引擎" value={engine ? '在线' : '离线'} tone={engine ? 'ok' : 'off'} live />
        <Cell label="kill-switch" value={ksOn ? '已挂载' : '未挂'} sub={ksOn ? `${ksRules} 条放行规则` : undefined} tone={ksOn ? 'ok' : 'off'} live />
        <Cell label="TUN 起栈" value={tun ? '已起栈' : '未起'} tone={tun ? 'ok' : 'off'} live />
        <Cell label="默认出口" value={routeLabel} tone={routeTone} live />
        <Cell label="出口 IP" value={exitIp || '未自检'} sub={exitIp && exitIpAt ? exitIpAt : undefined} tone={exitIp ? 'ok' : 'off'} live={false} />
        <Cell label="DNS 防泄漏" value={dnsValue} sub={dnsCase?.observed || undefined} tone={dnsTone} live={false} />
      </div>
      <div className="flex items-center justify-end gap-2">
        <span className="text-[11px] text-gray-400">出口 IP / DNS 需手动自检刷新</span>
        <button
          className="inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm transition-colors hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
          onClick={onVerify}
          disabled={busy}
          title="跑一次 net_policy_verify（出口 IP / DNS 劫持 / 引擎），刷新上面两个「最近值」"
        >
          <FlaskConical size={14} /> 自检
        </button>
      </div>
    </div>
  )
}
