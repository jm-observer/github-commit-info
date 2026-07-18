import { useCallback, useEffect, useState } from 'react'
import { ShieldAlert } from 'lucide-react'
import {
  NetPolicyAPI,
  egressAcceptsTraffic,
  EGRESS_LIFECYCLE_LABELS,
  type EgressKind,
  type EgressStatus,
  type Route,
} from '../api/tauri-client'
import { btn } from '../uiHelpers'

/**
 * 出口选择组件（出口设计 §6.5）：策略页/规则/默认出口三处「选出口」的入口，统一收拢到这里，
 * 避免各处各写一份四选一 `<select>` 却对出口生命周期一无所知。
 *
 * 核心约束：`Route` 有四个字面量，但只有三个对应真正的「出口」（direct/wg/proxy 分别对应
 * agent 的 EgressKind `direct`/`wire_guard`/`proxy`）；`blackhole` 不是出口，没有生命周期，
 * 恒可选。选择一个当前 `failed`/`stopped` 的出口时，**不允许隐式回落直连**——必须让用户在
 * 「阻断该流量 / 等待出口恢复 / 为该出口设置 fallback=直连」三个选项里明确选一个，见
 * `useGuardedRouteChange`。
 */

/** Route 字面量 → 出口 kind；`blackhole` 没有对应出口。 */
const ROUTE_TO_KIND: Partial<Record<Route, EgressKind>> = {
  direct: 'direct',
  wg: 'wire_guard',
  proxy: 'proxy',
}

const ROUTE_KIND_LABEL: Record<Route, string> = {
  direct: '直连',
  wg: 'WireGuard',
  proxy: '代理订阅',
  blackhole: '阻断',
}

const ROUTE_ORDER: Route[] = ['direct', 'wg', 'proxy', 'blackhole']

/** 下拉里不可用出口的视觉标记（部分 WebView 会应用 `<option>` 的行内样式，忽略时仍有 ⚠ 前缀兜底）。 */
const WARN_OPTION_STYLE: React.CSSProperties = { color: '#b45309' }

/** 按 Route 在出口清单里找对应出口；`blackhole` 或清单未加载时返回 undefined。 */
export function egressForRoute(route: Route, egresses: EgressStatus[]): EgressStatus | undefined {
  const kind = ROUTE_TO_KIND[route]
  if (!kind) return undefined
  return egresses.find((e) => e.kind === kind)
}

/** 决议 §6.5 的确认门槛：只有 `failed`/`stopped` 才强制要求用户明确决策（其余生命周期正常放行）。 */
function isBlockedLifecycle(egress: EgressStatus): boolean {
  return egress.lifecycle === 'failed' || egress.lifecycle === 'stopped'
}

/** 轮询出口全量清单（与 EgressPage 同步机制一致，独立轮询，互不干扰）。 */
export function useEgressList(pollMs = 5000): EgressStatus[] {
  const [egresses, setEgresses] = useState<EgressStatus[]>([])
  useEffect(() => {
    let alive = true
    const load = () => {
      void NetPolicyAPI.egressList()
        .then((list) => { if (alive) setEgresses(list) })
        .catch(() => { /* 沿用现有值；调用方页面自身的错误提示已经覆盖 agent 连不上的情况 */ })
    }
    load()
    const t = setInterval(load, pollMs)
    return () => { alive = false; clearInterval(t) }
  }, [pollMs])
  return egresses
}

type PendingRoute = { route: Route; egress: EgressStatus }
export type RouteConfirmChoice = 'block' | 'wait' | 'fallback-direct'

/**
 * 把「选择出口」这个动作套上决议 §6.5 的确认门槛：调用方拿到 `requestChange` 替代直接
 * `onChange`，未命中 failed/stopped 时透明直通；命中时收起 `onChange`，弹出 `modal`，
 * 等用户三选一后才继续。三个选项：
 * - `block`　　　　  这条选择改成「阻断」（不使用这个出口）；
 * - `wait`　　　　　 仍按原选择继续（保持规则指向该出口），用户已知晓当前会被 fail-closed；
 * - `fallback-direct` 先把该出口的 fallback 设为「直连」，再按原选择继续。
 */
export function useGuardedRouteChange(egresses: EgressStatus[], onChange: (route: Route) => void) {
  const [pending, setPending] = useState<PendingRoute | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const requestChange = useCallback((route: Route) => {
    const egress = egressForRoute(route, egresses)
    if (egress && isBlockedLifecycle(egress)) {
      setError(null)
      setPending({ route, egress })
      return
    }
    onChange(route)
  }, [egresses, onChange])

  const cancel = useCallback(() => {
    if (busy) return
    setPending(null)
    setError(null)
  }, [busy])

  const resolve = useCallback(async (choice: RouteConfirmChoice) => {
    if (!pending) return
    const { route, egress } = pending
    if (choice === 'block') {
      setPending(null)
      onChange('blackhole')
      return
    }
    if (choice === 'wait') {
      setPending(null)
      onChange(route)
      return
    }
    // fallback-direct：先落该出口的 fallback，再按原选择继续；两步都成功才关弹窗。
    setBusy(true)
    setError(null)
    try {
      await NetPolicyAPI.egressSetFallback(egress.id, 'direct')
      setPending(null)
      onChange(route)
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }, [pending, onChange])

  const modal = pending ? (
    <EgressRouteConfirmModal pending={pending} busy={busy} error={error} onResolve={resolve} onCancel={cancel} />
  ) : null

  return { requestChange, modal }
}

function EgressRouteConfirmModal({
  pending,
  busy,
  error,
  onResolve,
  onCancel,
}: {
  pending: PendingRoute
  busy: boolean
  error: string | null
  onResolve: (choice: RouteConfirmChoice) => void
  onCancel: () => void
}) {
  const { egress } = pending
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
      <div className="w-full max-w-md space-y-4 rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
        <h3 className="flex items-center gap-2 text-sm font-semibold">
          <ShieldAlert size={16} className="text-amber-500" />
          出口「{egress.name}」当前不可用
        </h3>
        <p className="text-sm text-gray-600 dark:text-gray-300">
          它的生命周期是「{EGRESS_LIFECYCLE_LABELS[egress.lifecycle]}」，当前不会承载流量。
          按决议要求，不能因此静默回落直连——请明确选择下面一项：
        </p>
        <div className="space-y-2 text-sm">
          <button className={`${btn()} w-full justify-start text-left`} disabled={busy} onClick={() => onResolve('block')}>
            阻断该流量——不用这个出口，改成「阻断」，直到你手动改路
          </button>
          <button className={`${btn()} w-full justify-start text-left`} disabled={busy} onClick={() => onResolve('wait')}>
            等待出口恢复——仍保留这条选择指向它，当前按它的 fallback（
            {egress.fallback === 'direct' ? '回落直连' : '阻断'}）处理，恢复后自动生效
          </button>
          <button className={`${btn()} w-full justify-start text-left`} disabled={busy} onClick={() => onResolve('fallback-direct')}>
            为该出口设置 fallback＝直连——出口不可用期间，<strong>所有</strong>指向它的流量都会改走本机直连（不只是这一处）
          </button>
        </div>
        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}
        <div className="flex justify-end">
          <button className={btn('ghost')} disabled={busy} onClick={onCancel}>取消</button>
        </div>
      </div>
    </div>
  )
}

export function EgressSelect({
  value,
  onChange,
  disabled,
  className,
  placeholder,
  includeBlackhole = true,
  title,
}: {
  /** 受控值；传 `''` 表示「未选定」占位态（如观察表的一次性「改路…」下拉）。 */
  value: Route | ''
  onChange: (route: Route) => void
  disabled?: boolean
  className?: string
  /** 值为 `''` 时首个占位项文案（如「改路…」）；不传则不显示占位项。 */
  placeholder?: string
  /** 是否包含「阻断」项。`blackhole` 不是出口，恒可选，无需确认。默认 true。 */
  includeBlackhole?: boolean
  /** 透传给 `<select>` 的 title（如未提权/fake-ip 提示）。 */
  title?: string
}) {
  const egresses = useEgressList()
  const { requestChange, modal } = useGuardedRouteChange(egresses, onChange)

  const options = ROUTE_ORDER
    .filter((route) => route !== 'blackhole' || includeBlackhole)
    .map((route) => {
      const kindLabel = ROUTE_KIND_LABEL[route]
      const egress = egressForRoute(route, egresses)
      if (!egress) return { route, label: kindLabel, warn: false }
      const warn = !egressAcceptsTraffic(egress.lifecycle)
      return { route, label: `${egress.name} · ${kindLabel} · ${EGRESS_LIFECYCLE_LABELS[egress.lifecycle]}`, warn }
    })

  return (
    <>
      <select
        className={className}
        disabled={disabled}
        title={title}
        value={value}
        onChange={(e) => {
          const v = e.target.value as Route | ''
          if (!v) return
          requestChange(v)
        }}
      >
        {placeholder != null && <option value="">{placeholder}</option>}
        {options.map((o) => (
          <option key={o.route} value={o.route} style={o.warn ? WARN_OPTION_STYLE : undefined}>
            {o.warn ? '⚠ ' : ''}{o.label}
          </option>
        ))}
      </select>
      {modal}
    </>
  )
}
