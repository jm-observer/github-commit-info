import { useCallback, useState } from 'react'
import {
  Activity,
  RefreshCw,
  Globe2,
  ShieldCheck,
  Network,
  Cpu,
  CheckCircle2,
  XCircle,
  HelpCircle,
  Loader2,
  Search,
} from 'lucide-react'
import {
  NetPolicyAPI,
  type VerifyCase,
  type ProcessCandidate,
} from '../api/tauri-client'
import { useNetPolicyProbe } from '../ProbeContext'

/**
 * 本机现状查询区（只读 · 不改系统）。
 *
 * 数据全部来自 `NetPolicyProbeProvider`（在 App 根挂载，启动即跑 verify + status + conns）。
 * 本组件不再自己跑 verify ——切到此页时数据已就位，不再有 ~1s 等待。
 * 「刷新现状」按钮 → 调 provider.runVerify()（单飞合并，跨页面共享）。
 *
 * 进程候选枚举（略重）仍是本组件按钮触发，不自动跑，也不进 provider（独立 ProcessCandidate UI 状态）。
 */

type CaseTone = 'ok' | 'bad' | 'unknown'

function caseTone(c: VerifyCase | undefined): CaseTone {
  if (!c) return 'unknown'
  if (c.status === 'passed') return 'ok'
  if (c.status === 'failed') return 'bad'
  return 'unknown'
}

function ToneIcon({ tone }: { tone: CaseTone }) {
  if (tone === 'ok') return <CheckCircle2 size={15} className="shrink-0 text-green-500" />
  if (tone === 'bad') return <XCircle size={15} className="shrink-0 text-red-500" />
  return <HelpCircle size={15} className="shrink-0 text-amber-500" />
}

/** 单个现状指标卡。 */
function StatCard({
  icon,
  label,
  value,
  tone,
  hint,
}: {
  icon: React.ReactNode
  label: string
  value: React.ReactNode
  tone?: CaseTone
  hint?: string
}) {
  return (
    <div className="flex flex-col gap-1 rounded-lg border border-gray-200 bg-white px-3 py-2.5 dark:border-gray-800 dark:bg-gray-900" title={hint}>
      <div className="flex items-center gap-1.5 text-[11px] uppercase tracking-wide text-gray-400">
        <span className="text-gray-500 dark:text-gray-400">{icon}</span>
        {label}
      </div>
      <div className="flex items-center gap-1.5 text-sm">
        {tone && <ToneIcon tone={tone} />}
        <span className="min-w-0 truncate font-mono text-[13px]">{value}</span>
      </div>
    </div>
  )
}

export function CurrentStateSection({
  busy,
}: {
  /** 父级 busy（写动作进行中），让本区按钮也禁用避免并发 PS。 */
  busy: boolean
}) {
  const { status, conns, verify, verifyUpdatedAt, probing, runVerify } = useNetPolicyProbe()
  const [candidates, setCandidates] = useState<ProcessCandidate[] | null>(null)
  const [scanning, setScanning] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  // 「刷新现状」→ provider.runVerify（单飞合并，自带 exit-ip 回填到 ProtectionBanner）。
  const probe = useCallback(async () => {
    setErr(null)
    const rep = await runVerify()
    if (rep == null) setErr('探测失败（见控制台）')
  }, [runVerify])

  // 进程候选枚举（略重）：按钮触发，不自动跑。
  const scanProcesses = useCallback(async () => {
    setScanning(true)
    try {
      setCandidates(await NetPolicyAPI.listProcessCandidates())
    } catch (e) {
      setErr(String(e))
    } finally {
      setScanning(false)
    }
  }, [])

  const exitIp = verify?.cases.find((c) => c.id === 'exit-ip')
  const dns = verify?.cases.find((c) => c.id === 'dns-hijack')
  const engine = verify?.cases.find((c) => c.id === 'engine')

  const fw = status?.firewall
  const fwActive = !!fw?.active
  const ruleCount = fw?.rule_count ?? 0
  const tun = !!status?.tun_ready
  const wgConfigured = !!status?.wg_configured

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="ml-auto flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
          {verifyUpdatedAt ? `最后更新 ${verifyUpdatedAt}` : '查询中…'}
        </span>
        <button
          className="inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm transition-colors hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
          onClick={() => void probe()}
          disabled={probing || busy}
          title="重跑只读探测：当前出口 IP / DNS 劫持 / 控制器可达（不改系统）"
        >
          {probing ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />} 刷新现状
        </button>
      </div>

      <div className="space-y-4">
        {err && (
          <div className="rounded-md bg-red-100 px-3 py-1.5 text-xs text-red-800 dark:bg-red-950/40 dark:text-red-300">
            查询出错：{err}
          </div>
        )}

        {/* 出口可达性（verify：出口 IP / DNS / 控制器） */}
        <div>
          <div className="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-gray-400">出口与 DNS（实时探测）</div>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
            <StatCard
              icon={<Globe2 size={13} />}
              label="当前出口 IP"
              value={exitIp?.observed || (probing ? '探测中…' : '—')}
              tone={caseTone(exitIp)}
              hint="api.ipify.org（10s 超时）。应用 WG 策略后应为海外出口。"
            />
            <StatCard
              icon={<Globe2 size={13} />}
              label="DNS 劫持(fake-ip)"
              value={dns?.observed || (probing ? '探测中…' : '—')}
              tone={caseTone(dns)}
              hint="向 8.8.8.8 显式查询 example.com，返回 198.18.x 表示已被 TUN 劫持（防泄漏）。"
            />
            <StatCard
              icon={<Cpu size={13} />}
              label="mihomo 控制器"
              value={engine ? (engine.status === 'passed' ? '可达' : '不可达') : (probing ? '探测中…' : '—')}
              tone={caseTone(engine)}
              hint="mihomo 外部控制器 /version。未应用策略时通常不可达，属正常。"
            />
          </div>
        </div>

        {/* 本机栈状态（status：防火墙 / TUN / WG） */}
        <div>
          <div className="mb-1.5 text-[11px] font-medium uppercase tracking-wide text-gray-400">本机栈状态（防火墙 / TUN / WG 配置）</div>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
            <StatCard
              icon={<ShieldCheck size={13} />}
              label="防火墙基线"
              value={
                status
                  ? fwActive
                    ? `生效 · ${ruleCount} 条规则`
                    : '未生效'
                  : '查询中…'
              }
              tone={status ? (fwActive && ruleCount > 0 ? 'ok' : 'unknown') : 'unknown'}
              hint="出站默认动作 + NetPolicy-KillSwitch 规则组。active 且 rule_count>0 表示围栏已装。"
            />
            <StatCard
              icon={<Network size={13} />}
              label="TUN (Meta) 起栈"
              value={status ? (tun ? '已起栈' : '未起') : '查询中…'}
              tone={status ? (tun ? 'ok' : 'unknown') : 'unknown'}
              hint="TUN(Meta) 虚拟网卡是否就绪。应用策略后才有。"
            />
            <StatCard
              icon={<Network size={13} />}
              label="WireGuard 配置"
              value={status ? (wgConfigured ? '已配置' : '未配置') : '查询中…'}
              tone={status ? (wgConfigured ? 'ok' : 'unknown') : 'unknown'}
              hint="是否已填写 WG 出口（server/key 等）。这是配置态，非连接态。"
            />
          </div>
        </div>

        {/* 活跃连接（connections：按出口聚合） */}
        <div>
          <div className="mb-1.5 flex items-center gap-2 text-[11px] font-medium uppercase tracking-wide text-gray-400">
            <Activity size={13} /> 活跃连接（按出口聚合）
            {!conns.available && <span className="normal-case text-gray-400">· 连接快照不可用（控制器未起）</span>}
          </div>
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <span className="rounded bg-gray-100 px-2 py-1 dark:bg-gray-800">总计 {conns.total}</span>
            <span className="rounded bg-amber-100 px-2 py-1 text-amber-800 dark:bg-amber-950/50 dark:text-amber-300">
              直连 {conns.direct_count}
            </span>
            <span className="rounded bg-blue-100 px-2 py-1 text-blue-800 dark:bg-blue-950/50 dark:text-blue-300">
              WG {conns.wg_count}
            </span>
            {conns.other_count > 0 && (
              <span className="rounded bg-gray-100 px-2 py-1 dark:bg-gray-800">其它 {conns.other_count}</span>
            )}
          </div>
          {conns.available && Object.keys(conns.by_process).length > 0 && (
            <div className="mt-2 flex flex-wrap gap-1.5 text-[11px]">
              {Object.entries(conns.by_process)
                .sort((a, b) => b[1] - a[1])
                .slice(0, 12)
                .map(([proc, n]) => (
                  <span key={proc} className="rounded bg-gray-50 px-1.5 py-0.5 font-mono text-gray-600 dark:bg-gray-800/60 dark:text-gray-300" title={proc}>
                    {proc} · {n}
                  </span>
                ))}
            </div>
          )}
        </div>

        {/* 进程发现（枚举略重 → 按钮触发，仍是只读查询） */}
        <div>
          <div className="mb-1.5 flex items-center gap-2">
            <span className="text-[11px] font-medium uppercase tracking-wide text-gray-400">近期有公网连接的进程（只读枚举）</span>
            <button
              className="ml-auto inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-2.5 py-1 text-xs transition-colors hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
              onClick={() => void scanProcesses()}
              disabled={scanning || busy}
              title="枚举近期有公网连接的进程（只读，略重，故按需触发）"
            >
              {scanning ? <Loader2 size={13} className="animate-spin" /> : <Search size={13} />} 扫描进程
            </button>
          </div>
          {candidates === null ? (
            <p className="text-xs text-gray-500">点「扫描进程」列出近期有公网连接的进程（纯查询，不改系统）。如需改路，请前往「策略编排」。</p>
          ) : candidates.length === 0 ? (
            <p className="text-xs text-gray-500">未发现近期有公网连接的进程。</p>
          ) : (
            <ul className="divide-y divide-gray-100 text-sm dark:divide-gray-800">
              {candidates.map((c) => (
                <li key={c.pid} className="flex flex-col gap-1 py-1.5">
                  <span className="truncate" title={c.path || c.name}>
                    {c.name || `pid ${c.pid}`}{' '}
                    <span className="text-xs text-gray-400">
                      pid {c.pid} · {c.remotes.length} 个远端
                    </span>
                  </span>
                  {c.remotes.length > 0 && (
                    <div className="flex flex-wrap gap-1 pl-1">
                      {c.remotes.map((r) => (
                        <span
                          key={r}
                          className="rounded bg-gray-100 px-1.5 py-0.5 font-mono text-[11px] text-gray-600 dark:bg-gray-800 dark:text-gray-300"
                        >
                          {r}
                        </span>
                      ))}
                    </div>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  )
}
