import { useCallback, useEffect, useState } from 'react'
import { Zap, X, Plus, Timer, ShieldAlert } from 'lucide-react'
import { NetPolicyAPI, type ProcessRef, type TempDirectStatus } from '../api/tauri-client'

/**
 * 临时直连（限时应急）：海外出口出问题时，一键把未命中规则的流量临时改为本地直连；`except`
 * 进程仍被强制 Blackhole（防敏感流量在隧道故障时泄漏到直连）。到期自动还原（后端定时器）。
 *
 * 自持状态：3s 轮询 tempStatus 以显示倒计时。
 */

function fmt(sec: number): string {
  const m = Math.floor(sec / 60)
  const s = sec % 60
  return `${m}:${String(s).padStart(2, '0')}`
}

const PRESETS: { label: string; secs: number }[] = [
  { label: '5 分钟', secs: 300 },
  { label: '15 分钟', secs: 900 },
  { label: '1 小时', secs: 3600 },
]

export function TempDirectControl() {
  const [status, setStatus] = useState<TempDirectStatus | null>(null)
  const [secs, setSecs] = useState(300)
  const [exceptInput, setExceptInput] = useState('')
  const [except, setExcept] = useState<string[]>([])
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  const refresh = useCallback(() => {
    void NetPolicyAPI.tempStatus()
      .then(setStatus)
      .catch(() => {})
  }, [])

  useEffect(() => {
    refresh()
    const t = setInterval(refresh, 3000)
    return () => clearInterval(t)
  }, [refresh])

  const turnOn = async () => {
    setBusy(true)
    setErr(null)
    try {
      const refs: ProcessRef[] = except.map((v) => ({ kind: 'process_name', value: v }))
      setStatus(await NetPolicyAPI.tempDirectOn(secs, refs))
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  const turnOff = async () => {
    setBusy(true)
    setErr(null)
    try {
      setStatus(await NetPolicyAPI.tempDirectOff())
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  const addExcept = () => {
    const v = exceptInput.trim()
    if (v && !except.includes(v)) setExcept([...except, v])
    setExceptInput('')
  }

  const active = status?.active ?? false

  return (
    <div className="space-y-5">
      <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-900/50 dark:bg-amber-950/20 dark:text-amber-300">
        这是应急操作：默认出口会在限定时间内改为直连；列入例外的敏感进程仍保持阻断。
      </div>
      {active ? (
        <div className="flex flex-wrap items-center gap-3">
          <span className="inline-flex items-center gap-1.5 rounded-md bg-amber-100 px-3 py-1.5 text-sm font-medium text-amber-800 dark:bg-amber-900/40 dark:text-amber-200">
            <Timer size={15} /> 生效中 · 剩余 {fmt(status?.remaining_secs ?? 0)}
          </span>
          {status && status.except.length > 0 && (
            <span className="text-xs text-gray-600 dark:text-gray-400">
              例外（阻断）：{status.except.map((e) => e.value).join('、')}
            </span>
          )}
          <button
            className="inline-flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-1.5 text-sm text-white hover:bg-red-700 disabled:opacity-50"
            onClick={turnOff}
            disabled={busy}
          >
            <X size={15} /> 立即解除
          </button>
        </div>
      ) : (
        <div className="space-y-2.5">
          {/* 时长 */}
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs text-gray-500">时长</span>
            {PRESETS.map((p) => (
              <button
                key={p.secs}
                className={`rounded-md px-2.5 py-1 text-xs transition-colors ${
                  secs === p.secs
                    ? 'bg-amber-500 text-white'
                    : 'border border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800'
                }`}
                onClick={() => setSecs(p.secs)}
              >
                {p.label}
              </button>
            ))}
            <input
              type="number"
              min={10}
              className="w-20 rounded-md border border-gray-300 px-2 py-1 text-xs dark:border-gray-700 dark:bg-gray-900"
              value={secs}
              onChange={(e) => setSecs(Math.max(10, Number(e.target.value) || 0))}
            />
            <span className="text-xs text-gray-400">秒</span>
          </div>

          {/* 例外进程 */}
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs text-gray-500">例外进程（不走直连、强制阻断）</span>
            <input
              className="w-40 rounded-md border border-gray-300 px-2 py-1 text-xs dark:border-gray-700 dark:bg-gray-900"
              placeholder="如 secret.exe"
              value={exceptInput}
              onChange={(e) => setExceptInput(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && addExcept()}
            />
            <button
              className="inline-flex items-center gap-1 rounded-md border border-gray-300 px-2 py-1 text-xs hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800"
              onClick={addExcept}
            >
              <Plus size={12} /> 加
            </button>
            {except.map((v) => (
              <span
                key={v}
                className="inline-flex items-center gap-1 rounded bg-gray-200 px-2 py-0.5 text-xs dark:bg-gray-700"
              >
                {v}
                <button onClick={() => setExcept(except.filter((x) => x !== v))} className="text-gray-500 hover:text-red-500">
                  <X size={11} />
                </button>
              </span>
            ))}
          </div>

          <button
            className="inline-flex items-center gap-1.5 rounded-md bg-amber-500 px-3 py-1.5 text-sm text-white hover:bg-amber-600 disabled:opacity-50"
            onClick={turnOn}
            disabled={busy}
          >
            <Zap size={15} /> 开启临时直连（{fmt(secs)}）
          </button>
        </div>
      )}

      {err && (
        <p className="flex items-start gap-1 px-1 text-[11px] text-red-600 dark:text-red-400">
          <ShieldAlert size={12} className="mt-0.5 shrink-0" /> {err}
        </p>
      )}
      <p className="px-1 text-[11px] text-gray-500 dark:text-gray-400">
        原理：临时把 mihomo 兜底 MATCH 改 DIRECT（例外进程 Blackhole）；到期后端定时器自动还原。
        kill-switch 仍按姿态挂——DIRECT 流量经 mihomo 拨号出物理网卡放行。
      </p>
    </div>
  )
}
