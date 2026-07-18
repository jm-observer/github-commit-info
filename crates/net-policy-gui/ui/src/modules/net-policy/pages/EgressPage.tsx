import { useCallback, useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { RefreshCw, ShieldAlert } from 'lucide-react'
import {
  EGRESS_CHANGED_EVENT,
  NetPolicyAPI,
  type EgressFallback,
  type EgressStatus,
} from '../api/tauri-client'
import { btn, Section } from '../uiHelpers'
import { EgressCard } from '../components/EgressCard'
import { usePageHeaderActions } from '../PageHeaderContext'

/** 出口页兜底轮询周期：与本模块其它自持轮询页（TempDirectControl/CapturePage 等）一致。 */
const POLL_MS = 3000

type ConfirmState =
  | { kind: 'stop'; egress: EgressStatus }
  | { kind: 'fallback-direct'; egress: EgressStatus }

/**
 * 出口页：统一出口生命周期管理面（出口设计 §8.8）。核心约束——「出口是否启动/已连接」与
 * 「当前是否有业务流量经过它」是两个独立问题，每张卡片必须**同时**展示这两个状态，绝不能
 * 只给一个笼统的「在线」。
 *
 * 「设为默认出口」等导流策略变更不在本页——那属于「策略编排」/「WireGuard 设置」/「代理订阅」页；
 * 本页的「仅测试连接」只探测一次，不改变生命周期也不改变导流策略。
 *
 * 数据来源：`net-policy://egress-changed` 事件增量更新 + 3s 轮询兜底（事件丢失/断线重连期间兜底）。
 */
export function EgressPage() {
  const [egresses, setEgresses] = useState<EgressStatus[]>([])
  const [loading, setLoading] = useState(false)
  const [busyId, setBusyId] = useState<string | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [confirm, setConfirm] = useState<ConfirmState | null>(null)

  const refresh = useCallback(async () => {
    setLoading(true)
    try {
      setEgresses(await NetPolicyAPI.egressList())
      setErr(null)
    } catch (e) {
      setErr(String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  usePageHeaderActions(
    <button className={btn()} onClick={() => void refresh()} disabled={loading}>
      <RefreshCw size={14} className={loading ? 'animate-spin' : ''} /> 刷新
    </button>,
    [loading, refresh],
  )

  // 兜底轮询：与事件推送并行，事件丢失/agent 重连期间仍能对齐。
  useEffect(() => {
    void refresh()
    const t = setInterval(() => void refresh(), POLL_MS)
    return () => clearInterval(t)
  }, [refresh])

  // 增量更新：订阅 egress-changed，只更新对应那一张卡片。**与 apply-progress 完全独立**，
  // 不据此推断导流策略变化。
  useEffect(() => {
    let unlisten: (() => void) | undefined
    let canceled = false
    void listen<EgressStatus>(EGRESS_CHANGED_EVENT, (event) => {
      const next = event.payload
      setEgresses((prev) => {
        const idx = prev.findIndex((e) => e.id === next.id)
        if (idx < 0) return [...prev, next]
        const copy = prev.slice()
        copy[idx] = next
        return copy
      })
    })
      .then((u) => {
        if (canceled) { u(); return }
        unlisten = u
      })
      .catch((error) => { console.error('subscribe egress-changed failed', error) })
    return () => {
      canceled = true
      unlisten?.()
    }
  }, [])

  const applyResult = (id: string, list: EgressStatus[]) => {
    setEgresses(list)
    setBusyId((cur) => (cur === id ? null : cur))
  }

  const run = async (id: string, op: (id: string) => Promise<EgressStatus[]>) => {
    setBusyId(id)
    setErr(null)
    try {
      applyResult(id, await op(id))
    } catch (e) {
      setErr(String(e))
      setBusyId(null)
    }
  }

  const doStart = (egress: EgressStatus) => void run(egress.id, (id) => NetPolicyAPI.egressStart(id))
  const doReconnect = (egress: EgressStatus) => void run(egress.id, (id) => NetPolicyAPI.egressReconnect(id))
  const doProbe = (egress: EgressStatus) => void run(egress.id, (id) => NetPolicyAPI.egressProbe(id))

  const doStop = (egress: EgressStatus) => {
    // 危险操作二次确认：正被策略使用时才需要弹窗说明影响；未使用则直接停。
    if (egress.selected) {
      setConfirm({ kind: 'stop', egress })
      return
    }
    void run(egress.id, (id) => NetPolicyAPI.egressStop(id))
  }

  const doSetFallback = (egress: EgressStatus, fallback: EgressFallback) => {
    if (fallback === egress.fallback) return
    if (fallback === 'direct') {
      setConfirm({ kind: 'fallback-direct', egress })
      return
    }
    void run(egress.id, (id) => NetPolicyAPI.egressSetFallback(id, 'block'))
  }

  const confirmProceed = () => {
    if (!confirm) return
    const { kind, egress } = confirm
    setConfirm(null)
    if (kind === 'stop') {
      void run(egress.id, (id) => NetPolicyAPI.egressStop(id))
    } else {
      void run(egress.id, (id) => NetPolicyAPI.egressSetFallback(id, 'direct'))
    }
  }

  return (
    <div className="space-y-6">
      <Section title="出口" description="每个出口的生命周期与是否被策略使用是两件独立的事，卡片分别展示。">
        {err && <p className="mb-2 text-[11px] text-red-600 dark:text-red-400">{err}</p>}
        {egresses.length === 0 && loading ? (
          <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">
            正在加载出口清单…
          </div>
        ) : egresses.length === 0 ? (
          <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">
            暂无出口数据，请确认 net-policy-agent 已升级到支持统一出口的版本。
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
            {egresses.map((egress) => (
              <EgressCard
                key={egress.id}
                egress={egress}
                busy={busyId === egress.id}
                onStart={() => doStart(egress)}
                onStop={() => doStop(egress)}
                onReconnect={() => doReconnect(egress)}
                onProbe={() => doProbe(egress)}
                onSetFallback={(fallback) => doSetFallback(egress, fallback)}
              />
            ))}
          </div>
        )}
      </Section>

      {confirm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
          <div className="w-full max-w-md space-y-4 rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
            <h3 className="flex items-center gap-2 text-sm font-semibold">
              <ShieldAlert size={16} className="text-amber-500" />
              {confirm.kind === 'stop' ? '确认停止出口' : '确认回落直连'}
            </h3>
            {confirm.kind === 'stop' ? (
              <p className="text-sm text-gray-600 dark:text-gray-300">
                「{confirm.egress.name}」当前被
                {confirm.egress.usage.is_default ? '默认出口' : ''}
                {confirm.egress.usage.is_default && confirm.egress.usage.rule_count > 0 ? ' + ' : ''}
                {confirm.egress.usage.rule_count > 0 ? `${confirm.egress.usage.rule_count} 条规则` : ''}
                使用，停止它会使这些连接按 fallback 策略（
                {confirm.egress.fallback === 'direct' ? '回落直连' : '阻断'}）处理。是否继续？
              </p>
            ) : (
              <p className="text-sm text-gray-600 dark:text-gray-300">
                把「{confirm.egress.name}」的 fallback 设为「回落直连」后，一旦这个出口不可用，
                原本指向它的流量会<strong>改走本机直连</strong>而不是被阻断——请确认这不会让敏感流量意外裸奔。是否继续？
              </p>
            )}
            <div className="flex justify-end gap-2">
              <button className={btn('ghost')} onClick={() => setConfirm(null)}>取消</button>
              <button className={btn('danger')} onClick={confirmProceed}>确认</button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
