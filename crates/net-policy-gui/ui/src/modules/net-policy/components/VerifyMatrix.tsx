import { CheckCircle2, XCircle, HelpCircle, FlaskConical, Activity, Cpu, AlertTriangle } from 'lucide-react'
import type { Status, VerifyReport, VerifyCase } from '../api/tauri-client'

/**
 * 验证矩阵（设计 §3.4）：把报告 §0.8.2「全部 VP / §9 覆盖状态（唯一权威最终结论表）」搬进 app。
 *
 * 两列证据，**不让当前模型直接继承 §0.8.2 的 ✅**（设计 §3.4 ⚠️）：
 *  - Column A 「报告历史结论」= §0.8.2 原值徽章。fail-closed 相关项是在**旧 RemoteAddress 白名单模型**
 *    上取证的，标「(旧模型)」。
 *  - Column B 「当前代码模型」= 对 fail-closed 相关项（VP-08/09/10 + strict-route 可用性 + §9#2），
 *    当 status.protection_validated === false（镜像后端 FIREWALL_MODEL_VALIDATED）显示「待复测 · 进生产前阻塞」；
 *    翻 true 后显示 ✅。其余模型无关项 B 直接镜像 A。
 *
 * 实时检查：exit-ip → VP-01、dns-hijack → VP-07、engine（verify.mihomo_running）在表头展示。
 * 「一键自检」跑一次 NetPolicyAPI.verify()（一次调用返回全部 case），把结果落到映射行。
 * verify 含 exit-ip（api.ipify.org / 10s 超时），**只手动触发，不进任何快轮询**（设计 §3.5 ⚠️）。
 */

// ── 证据强度分级（§0.8.2） ──────────────────────────────────────────────────────
type Grade = 'full' | 'partial' | 'research' | 'untested' | 'na'

const GRADE_META: Record<Grade, { label: string; cls: string }> = {
  full: {
    label: '✅ 实测·有原始证物',
    cls: 'border-green-300 bg-green-50 text-green-700 dark:border-green-900/60 dark:bg-green-950/40 dark:text-green-300',
  },
  partial: {
    label: '◑ 实测·仅摘要/部分',
    cls: 'border-amber-300 bg-amber-50 text-amber-700 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300',
  },
  research: {
    label: '▢ 研究层',
    cls: 'border-gray-300 bg-gray-50 text-gray-600 dark:border-gray-700 dark:bg-gray-900/40 dark:text-gray-300',
  },
  untested: {
    label: '✗ 未测',
    cls: 'border-gray-300 bg-gray-50 text-gray-500 dark:border-gray-700 dark:bg-gray-900/40 dark:text-gray-400',
  },
  na: {
    label: 'N/A',
    cls: 'border-gray-200 bg-gray-50 text-gray-400 dark:border-gray-800 dark:bg-gray-900/30 dark:text-gray-500',
  },
}

interface MatrixRow {
  id: string
  name: string
  /** §0.8.2 原值证据分级（Column A）。 */
  grade: Grade
  /** §0.8.2 证据/说明，hover 展示。 */
  note: string
  /** fail-closed 相关项：A 标「旧模型」，B 受 protection_validated 驱动。 */
  failClosed?: boolean
  /** 可跑实时检查时，映射到的 verify case id。 */
  liveCaseId?: string
}

// 硬编码自报告 §0.8.2（id + 名称 + 证据分级 + 说明）。
const ROWS: MatrixRow[] = [
  { id: 'VP-01', name: '未知流量 → 海外', grade: 'full', note: '§0.7：mihomo MATCH,wg-out → 出口 38.209.122.38(US)', liveCaseId: 'exit-ip' },
  { id: 'VP-02', name: 'IP/CIDR 直连', grade: 'partial', note: '与 VP-03/04 同一规则引擎；未单独取证' },
  { id: 'VP-03', name: '域名直连', grade: 'full', note: '§0.8：DOMAIN-SUFFIX,3322.net,DIRECT → 国内出口 58.23.139.139，api.ipify 仍 US' },
  { id: 'VP-04', name: '程序直连', grade: 'full', note: '§0.7：curl.exe → 国内 / powershell → US，同刻双出口' },
  { id: 'VP-05', name: '子进程发现', grade: 'partial', note: '核心 claim 已证（按连接自身进程匹配，不继承父进程）；UI「加入程序组」未测' },
  { id: 'VP-06', name: '浏览器风险提示', grade: 'na', note: '纯 UI 交互，无后端可测' },
  { id: 'VP-07', name: 'DNS 防泄漏', grade: 'partial', note: '系统 DNS hijack 无泄漏 ✅（§0.6，8.8.8.8 → fake-ip 198.18.x）；DoH-443 绕过 ▢ 研究层；逐包抓包未做', liveCaseId: 'dns-hijack' },
  { id: 'strict-route', name: 'strict-route=true 可用性', grade: 'full', note: '§0.9.2：strict-route 下 LAN ping=True、DNS@8.8.8.8 → fake-ip、出口 38.209.122.38；防火墙快照+路由表实录', failClosed: true },
  { id: 'VP-08', name: '引擎崩溃 → 不泄漏', grade: 'full', note: '§0.9.2：停引擎后 Meta 消失、9.9.9.9:443=False、LAN ping=True', failClosed: true },
  { id: 'VP-09', name: 'WG 断开 → 不泄漏', grade: 'full', note: '§0.9.2 同上（停引擎=断 wg-out）；§0.4 官方 WG 旁证', failClosed: true },
  { id: 'VP-10', name: '路由被改 → 防火墙仍拦', grade: 'full', note: '§0.9.2：路由表 9.9.9.9 → 物理网关 metric1，9.9.9.9:443 仍 False', failClosed: true },
  { id: 'VP-11', name: '重启恢复', grade: 'partial', note: '§0.9.1：重启后 OutAction=Block、规则(3)存活、9.9.9.9 仍 False（vp11_results.txt 摘要，无完整快照）' },
  { id: 'VP-12', name: 'IPv6 泄漏', grade: 'na', note: '0.228 无全局 IPv6（地址数 0）→ 无 v6 可泄漏' },
  { id: '§9#1', name: '路由环', grade: 'full', note: '§0.7：WG userspace outbound 握手包经 auto-detect 绕 TUN，无环' },
  { id: '§9#2', name: 'kill-switch 独立性', grade: 'full', note: '§0.4：PersistentStore 规则独立于引擎进程', failClosed: true },
  { id: '§9#3', name: 'InterfaceType', grade: 'full', note: '§0.2/0.3：IfType=53，Wired 规则不命中隧道' },
  { id: '§9#5', name: 'DNS 泄漏（全）', grade: 'partial', note: 'hijack 层 ✅；mihomo 上游 DNS 泄漏已定位（§0.8.1）；strict-route 完整防护未测' },
  { id: '§9#6', name: 'mihomo 上游 DNS 路径', grade: 'full', note: '§0.8.1：走物理 NIC，需白名单或 remote-dns-resolve' },
  { id: '#1800', name: 'UDP 进程匹配', grade: 'full', note: '§0.8：UDP → DIRECT 按 ProcessName 匹配 → #1800 在 v1.19.27 已修复' },
]

// ── 徽章子组件 ────────────────────────────────────────────────────────────────
function GradeBadge({ grade, suffix }: { grade: Grade; suffix?: string }) {
  const m = GRADE_META[grade]
  return (
    <span className={`inline-flex items-center whitespace-nowrap rounded border px-1.5 py-0.5 text-[11px] leading-tight ${m.cls}`}>
      {m.label}{suffix ? ` ${suffix}` : ''}
    </span>
  )
}

function BlockedBadge() {
  return (
    <span className="inline-flex items-center gap-1 whitespace-nowrap rounded border border-red-300 bg-red-50 px-1.5 py-0.5 text-[11px] leading-tight text-red-700 dark:border-red-900/60 dark:bg-red-950/40 dark:text-red-300">
      <AlertTriangle size={11} className="shrink-0" /> 待复测 · 进生产前阻塞
    </span>
  )
}

// 实时检查结果（per-row）。
function LiveResult({ c }: { c: VerifyCase }) {
  const passed = c.status === 'passed'
  const failed = c.status === 'failed'
  return (
    <span className="inline-flex items-center gap-1 whitespace-nowrap text-[11px]">
      {passed && <CheckCircle2 size={12} className="shrink-0 text-green-500" />}
      {failed && <XCircle size={12} className="shrink-0 text-red-500" />}
      {!passed && !failed && <HelpCircle size={12} className="shrink-0 text-amber-500" />}
      <span className="font-mono text-gray-500 dark:text-gray-400" title={c.observed}>{c.observed || c.status}</span>
    </span>
  )
}

export function VerifyMatrix({
  status,
  verify,
  onVerify,
  busy,
}: {
  status: Status
  verify: VerifyReport | null
  onVerify: () => void
  busy: boolean
}) {
  const modelValidated = status.protection_validated
  const liveById = new Map<string, VerifyCase>()
  for (const c of verify?.cases ?? []) liveById.set(c.id, c)

  const engineOnline = verify?.mihomo_running ?? status.mihomo_running

  return (
    <section className="rounded-lg border border-gray-200 dark:border-gray-800">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-gray-200 px-4 py-2 dark:border-gray-800">
        <h2 className="text-sm font-semibold">验证矩阵（VP / §9，源自报告 §0.8.2）</h2>
        <span className="flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400" title="verify.mihomo_running（一键自检后更新）/ 否则 status.mihomo_running">
          <Cpu size={13} className={engineOnline ? 'text-green-500' : 'text-gray-400'} />
          引擎 {engineOnline ? '在线' : '离线'}
        </span>
        <button
          className="ml-auto inline-flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm transition-colors hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
          onClick={onVerify}
          disabled={busy}
          title="跑一次 net_policy_verify（出口 IP / DNS 劫持 / 引擎），结果落到对应行。重探测，不在快轮询内。"
        >
          <FlaskConical size={14} /> 一键自检
        </button>
      </div>

      {/* 列说明 + 旧模型告警 */}
      <div className="border-b border-gray-100 px-4 py-2 text-[11px] text-gray-500 dark:border-gray-800 dark:text-gray-400">
        <p>
          <b>报告历史结论</b>取自 §0.8.2 原值；其中 fail-closed 相关项标「(旧模型)」——在旧 RemoteAddress 白名单模型上取证。
          <b className="ml-1">当前代码模型</b>已换成「程序放行」（Program=mihomo.exe），
          {modelValidated
            ? ' 已通过真机复测（FIREWALL_MODEL_VALIDATED=true），继承 ✅。'
            : ' 尚未真机复测（FIREWALL_MODEL_VALIDATED=false），fail-closed 相关项标记为「待复测 · 进生产前阻塞」，不可视为已坐实。'}
        </p>
      </div>

      {/* 矩阵表（窄页响应式） */}
      <div className="overflow-x-auto">
        <table className="w-full table-fixed text-sm">
          <colgroup>
            <col className="w-[34%]" />
            <col className="w-[30%]" />
            <col className="w-[24%]" />
            <col className="w-[12%]" />
          </colgroup>
          <thead>
            <tr className="border-b border-gray-100 text-left text-[11px] uppercase tracking-wide text-gray-400 dark:border-gray-800">
              <th className="px-4 py-1.5 font-medium">项</th>
              <th className="px-2 py-1.5 font-medium">报告历史结论</th>
              <th className="px-2 py-1.5 font-medium">当前代码模型</th>
              <th className="px-2 py-1.5 font-medium">实时检查</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-100 dark:divide-gray-800">
            {ROWS.map((row) => {
              const live = row.liveCaseId ? liveById.get(row.liveCaseId) : undefined
              const fcWaiting = row.failClosed && !modelValidated
              return (
                <tr key={row.id} className={fcWaiting ? 'bg-red-50/40 dark:bg-red-950/20' : undefined}>
                  <td className="px-4 py-2 align-top">
                    <div className="font-mono text-xs text-gray-500 dark:text-gray-400">{row.id}</div>
                    <div className="text-[13px]" title={row.note}>{row.name}</div>
                  </td>
                  <td className="px-2 py-2 align-top">
                    <span title={row.note}>
                      <GradeBadge grade={row.grade} suffix={row.failClosed ? '(旧模型)' : undefined} />
                    </span>
                  </td>
                  <td className="px-2 py-2 align-top">
                    {row.failClosed
                      ? (modelValidated ? <GradeBadge grade="full" /> : <BlockedBadge />)
                      : <GradeBadge grade={row.grade} />}
                  </td>
                  <td className="px-2 py-2 align-top">
                    {row.liveCaseId
                      ? (live
                          ? <LiveResult c={live} />
                          : <span className="inline-flex items-center gap-1 text-[11px] text-gray-400"><Activity size={11} /> 待自检</span>)
                      : <span className="text-[11px] text-gray-300 dark:text-gray-600">—</span>}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
    </section>
  )
}
