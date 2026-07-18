import {
  AlertTriangle,
  Cable,
  CheckCircle2,
  Cloud,
  Loader2,
  Lock,
  Play,
  RefreshCw,
  Server,
  Square,
  XCircle,
  Zap,
} from 'lucide-react'
import {
  egressDependsOnEngine,
  egressSelectedButUnusable,
  EGRESS_LIFECYCLE_LABELS,
  EGRESS_MANAGEMENT_LABELS,
  type EgressFallback,
  type EgressKind,
  type EgressLifecycle,
  type EgressManagement,
  type EgressStatus,
  type HealthState,
} from '../api/tauri-client'
import { btn } from '../uiHelpers'

/**
 * 单个出口卡片：**同时**展示「生命周期」（是否连得上）与「策略选中」（是否真的在承载流量）
 * 两个互相独立的标签——绝不能只显示一个「在线」，那会让用户误以为所有在线出口都在导流。
 *
 * 还有第三个必须讲明白的维度：**数据面归属**（`management`）。第一阶段 WG/代理的隧道由
 * mihomo 承载，引擎停了出口就没了。如果只显示「已就绪」，用户会以为这是个能独立存活的资源
 * ——那是产品语义欺骗。故 `mihomo-managed` 出口恒显示一条说明其依赖关系的提示。
 */

function fmtTime(ms: number): string {
  return ms ? new Date(ms).toLocaleString() : '从未'
}

const KIND_META: Record<EgressKind, { label: string; icon: React.ReactNode }> = {
  direct: { label: '直连', icon: <Cable size={16} /> },
  wire_guard: { label: 'WireGuard', icon: <Lock size={16} /> },
  proxy: { label: '代理订阅', icon: <Cloud size={16} /> },
}

const LIFECYCLE_STYLE: Record<EgressLifecycle, string> = {
  stopped: 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-300',
  starting: 'bg-blue-100 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300',
  connecting: 'bg-blue-100 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300',
  ready: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300',
  degraded: 'bg-amber-100 text-amber-700 dark:bg-amber-950/40 dark:text-amber-300',
  reconnecting: 'bg-amber-100 text-amber-700 dark:bg-amber-950/40 dark:text-amber-300',
  failed: 'bg-red-100 text-red-700 dark:bg-red-950/40 dark:text-red-300',
}

function lifecycleIcon(l: EgressLifecycle) {
  if (l === 'ready' || l === 'degraded') return <CheckCircle2 size={13} />
  if (l === 'starting' || l === 'connecting' || l === 'reconnecting') {
    return <Loader2 size={13} className="animate-spin" />
  }
  if (l === 'failed') return <XCircle size={13} />
  return null
}

function LifecycleBadge({ lifecycle }: { lifecycle: EgressLifecycle }) {
  return (
    <span className={`inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs font-medium ${LIFECYCLE_STYLE[lifecycle]}`}>
      {lifecycleIcon(lifecycle)} 生命周期：{EGRESS_LIFECYCLE_LABELS[lifecycle]}
    </span>
  )
}

function UsageBadge({ egress }: { egress: EgressStatus }) {
  if (!egress.selected) {
    return (
      <span className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 text-xs text-gray-500 dark:border-gray-700 dark:text-gray-400">
        当前策略：未使用
      </span>
    )
  }
  const parts: string[] = []
  if (egress.usage.is_default) parts.push('默认出口')
  if (egress.usage.rule_count > 0) parts.push(`${egress.usage.rule_count} 条规则`)
  return (
    <span className="inline-flex items-center gap-1 rounded bg-sky-100 px-2 py-0.5 text-xs font-medium text-sky-700 dark:bg-sky-950/40 dark:text-sky-300">
      当前策略：使用中（{parts.join(' · ')}）
    </span>
  )
}

/** 数据面归属徽章：`mihomo-managed` 用中性灰而非绿色，避免读成「另一种健康状态」。 */
function ManagementBadge({ management }: { management: EgressManagement }) {
  return (
    <span
      className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-0.5 text-[11px] text-gray-500 dark:border-gray-700 dark:text-gray-400"
      title={
        egressDependsOnEngine(management)
          ? '该出口的隧道由 mihomo 进程承载：停止管控或重载配置都会重建它，它无法脱离引擎独立在线。'
          : '该出口不依赖 mihomo 进程。'
      }
    >
      <Server size={11} /> {EGRESS_MANAGEMENT_LABELS[management]}
    </span>
  )
}

const HEALTH_STYLE: Record<HealthState, string> = {
  unknown: 'text-gray-400',
  healthy: 'text-emerald-600 dark:text-emerald-400',
  degraded: 'text-amber-600 dark:text-amber-400',
  unhealthy: 'text-red-600 dark:text-red-400',
}

const HEALTH_LABEL: Record<HealthState, string> = {
  unknown: '未探测',
  healthy: '健康',
  degraded: '降级',
  unhealthy: '不健康',
}

export function EgressCard({
  egress,
  busy,
  onStart,
  onStop,
  onReconnect,
  onProbe,
  onSetFallback,
}: {
  egress: EgressStatus
  /** 本卡片是否有操作在途（disable 所有按钮）。 */
  busy: boolean
  onStart: () => void
  onStop: () => void
  onReconnect: () => void
  onProbe: () => void
  onSetFallback: (fallback: EgressFallback) => void
}) {
  const meta = KIND_META[egress.kind]
  const canStart = egress.configured && (egress.lifecycle === 'stopped' || egress.lifecycle === 'failed')
  const canStop = egress.lifecycle !== 'stopped'
  const canReconnect = egress.configured && egress.lifecycle !== 'stopped'
  const canProbe = egress.configured

  return (
    <div className="space-y-3 rounded-lg border border-gray-200 bg-white p-4 dark:border-gray-800 dark:bg-gray-900">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <div className="flex items-center gap-1.5 text-sm font-semibold text-gray-900 dark:text-gray-100">
            {meta.icon} {egress.name}
            <span className="text-xs font-normal text-gray-400">{meta.label}</span>
          </div>
          <div className="mt-1"><ManagementBadge management={egress.management} /></div>
        </div>
        {/* 两个互相独立的标签：生命周期 ≠ 是否在承载流量。 */}
        <div className="flex flex-wrap items-center gap-1.5">
          <LifecycleBadge lifecycle={egress.lifecycle} />
          <UsageBadge egress={egress} />
        </div>
      </div>

      {egressSelectedButUnusable(egress) && (
        <div className="flex items-start gap-1.5 rounded-md border border-red-300 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          策略正把流量导向这个出口，但它当前不接受流量（{EGRESS_LIFECYCLE_LABELS[egress.lifecycle]}）——
          按 fallback 策略「{egress.fallback === 'direct' ? '回落直连' : '阻断'}」处理，请尽快处理或切换出口。
        </div>
      )}

      {!egress.configured && (
        <div className="rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-900/50 dark:bg-amber-950/20 dark:text-amber-300">
          未配置：{egress.unconfigured_reason ?? '缺少必要配置'}。请先到「WireGuard 设置」或「代理订阅」页完成配置。
        </div>
      )}

      {/* 第一阶段的真实约束，不藏在 tooltip 里：出口不能脱离引擎存活。 */}
      {egressDependsOnEngine(egress.management) && (
        <p className="rounded-md bg-gray-50 px-3 py-2 text-[11px] leading-relaxed text-gray-500 dark:bg-gray-800/40 dark:text-gray-400">
          这个出口的隧道由 mihomo 承载：<strong>停止管控或重载配置会一并重建它</strong>，它无法脱离引擎独立在线。
          这里的「已就绪」只表示当前引擎内这条出口探测得通。
        </p>
      )}

      <dl className="grid grid-cols-2 gap-x-4 gap-y-1.5 text-xs sm:grid-cols-3">
        <div>
          <dt className="text-gray-400">健康状态</dt>
          <dd className={`font-medium ${HEALTH_STYLE[egress.health.state]}`}>
            {HEALTH_LABEL[egress.health.state]}
            {egress.health.latency_ms != null && <span className="ml-1 font-normal text-gray-500">{egress.health.latency_ms} ms</span>}
          </dd>
        </div>
        <div>
          <dt className="text-gray-400">最近探测</dt>
          <dd className="text-gray-700 dark:text-gray-300">{fmtTime(egress.health.checked_at_ms)}</dd>
        </div>
        <div>
          <dt className="text-gray-400">探测目标</dt>
          <dd className="truncate text-gray-700 dark:text-gray-300" title={egress.health.target ?? undefined}>{egress.health.target || '—'}</dd>
        </div>
        <div>
          {/* 决议 §6.2：只能说这是「探测驱动的」计数，不能说成隧道真的断连了几次。 */}
          <dt className="text-gray-400" title="主动探测从「通」跌到「不通」的次数；不代表隧道真实断连次数，mihomo 重载引起的重建不计入">
            探测跌落次数
          </dt>
          <dd className="text-gray-700 dark:text-gray-300">{egress.reconnect_count}</dd>
        </div>
        <div>
          <dt className="text-gray-400">状态变更</dt>
          <dd className="text-gray-700 dark:text-gray-300">{fmtTime(egress.changed_at_ms)}</dd>
        </div>
        <div>
          <dt className="text-gray-400">实际活跃连接</dt>
          <dd className="text-gray-700 dark:text-gray-300">{egress.active_connections}</dd>
        </div>
      </dl>

      {egress.health.error && (
        <p className="rounded bg-red-50 px-2 py-1 text-[11px] text-red-600 dark:bg-red-950/20 dark:text-red-400">
          探测错误：{egress.health.error}
        </p>
      )}
      {egress.last_error && (
        <p className="rounded bg-red-50 px-2 py-1 text-[11px] text-red-600 dark:bg-red-950/20 dark:text-red-400">
          最近错误：{egress.last_error}
        </p>
      )}

      {/* 类型相关连接信息 */}
      {egress.detail.wireguard && (
        <dl className="grid grid-cols-2 gap-x-4 gap-y-1.5 rounded-md bg-gray-50 px-3 py-2 text-xs dark:bg-gray-800/40 sm:grid-cols-3">
          <div><dt className="text-gray-400">endpoint</dt><dd className="font-mono text-gray-700 dark:text-gray-300">{egress.detail.wireguard.endpoint || '—'}</dd></div>
          <div><dt className="text-gray-400">本地地址</dt><dd className="font-mono text-gray-700 dark:text-gray-300">{egress.detail.wireguard.local_ip || '—'}</dd></div>
          <div><dt className="text-gray-400">MTU</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.wireguard.mtu}</dd></div>
          <div><dt className="text-gray-400">混淆</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.wireguard.obfuscation ? '已启用' : '未启用'}</dd></div>
          <div><dt className="text-gray-400">经代理拨号</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.wireguard.via_dialer_proxy ? '是' : '否'}</dd></div>
          <div><dt className="text-gray-400">最近探测成功</dt><dd className="text-gray-700 dark:text-gray-300">{fmtTime(egress.detail.wireguard.last_probe_ok_at_ms)}</dd></div>
        </dl>
      )}
      {egress.detail.proxy && (
        <dl className="grid grid-cols-2 gap-x-4 gap-y-1.5 rounded-md bg-gray-50 px-3 py-2 text-xs dark:bg-gray-800/40 sm:grid-cols-3">
          <div><dt className="text-gray-400">订阅</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.proxy.subscription || '—'}</dd></div>
          <div><dt className="text-gray-400">节点</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.proxy.node || '（未选定）'}</dd></div>
          <div><dt className="text-gray-400">节点状态</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.proxy.node_alive ? '可用' : '未验证/不可用'}</dd></div>
          <div><dt className="text-gray-400">节点延迟</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.proxy.node_delay_ms != null ? `${egress.detail.proxy.node_delay_ms} ms` : '—'}</dd></div>
          <div><dt className="text-gray-400">最近刷新</dt><dd className="text-gray-700 dark:text-gray-300">{fmtTime(egress.detail.proxy.refreshed_at_ms)}</dd></div>
        </dl>
      )}
      {egress.detail.direct && (
        <dl className="grid grid-cols-2 gap-x-4 gap-y-1.5 rounded-md bg-gray-50 px-3 py-2 text-xs dark:bg-gray-800/40 sm:grid-cols-3">
          <div><dt className="text-gray-400">出口网卡</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.direct.interface || '（未知）'}</dd></div>
          <div><dt className="text-gray-400">默认网关</dt><dd className="text-gray-700 dark:text-gray-300">{egress.detail.direct.gateway || '（未知）'}</dd></div>
        </dl>
      )}

      {/* fallback：出口不可用时如何处理指向它的规则 */}
      <div className="flex flex-wrap items-center gap-2 border-t border-gray-100 pt-2.5 dark:border-gray-800">
        <span className="text-xs text-gray-500">不可用时</span>
        <div className="inline-flex gap-1.5">
          <button
            className={`rounded-md border px-2 py-0.5 text-[11px] transition-colors disabled:opacity-50 ${
              egress.fallback === 'block'
                ? 'border-gray-600 bg-gray-600 text-white'
                : 'border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800'
            }`}
            disabled={busy}
            onClick={() => onSetFallback('block')}
          >
            阻断
          </button>
          <button
            className={`rounded-md border px-2 py-0.5 text-[11px] transition-colors disabled:opacity-50 ${
              egress.fallback === 'direct'
                ? 'border-amber-500 bg-amber-500 text-white'
                : 'border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800'
            }`}
            disabled={busy}
            onClick={() => onSetFallback('direct')}
          >
            回落直连
          </button>
        </div>
      </div>

      {/* 操作：启动/停止/重连是三个不同操作，测试连接不改变导流策略。 */}
      <div className="flex flex-wrap items-center gap-2 pt-1">
        <button className={btn('primary')} disabled={busy || !canStart} onClick={onStart} title={!egress.configured ? '未配置，无法启动' : undefined}>
          {busy ? <Loader2 size={13} className="animate-spin" /> : <Play size={13} />} 启动
        </button>
        <button className={btn('danger')} disabled={busy || !canStop} onClick={onStop}>
          <Square size={13} /> 停止
        </button>
        {/* mihomo-managed 出口没有「隧道」可重建：这个动作是关掉该 outbound 上的存量连接
            并重新探测，逼后续连接重新建立。文案不能读成「重建隧道」（决议 §7.3）。 */}
        <button
          className={btn()}
          disabled={busy || !canReconnect}
          onClick={onReconnect}
          title="关闭该出口上的现有连接并重新探测，使后续连接重新建立；不影响其它出口，也不改变导流策略"
        >
          <RefreshCw size={13} /> 重置连接
        </button>
        <button className={btn()} disabled={busy || !canProbe} onClick={onProbe} title="只探测一次，不改变当前流量策略">
          <Zap size={13} /> 仅测试连接
        </button>
      </div>
      <p className="text-[11px] text-gray-400">
        「测试连接」只探测一次，不启动出口、不改变当前流量策略；「设为默认出口」等导流策略变更请到「策略编排」页处理。
      </p>
    </div>
  )
}
