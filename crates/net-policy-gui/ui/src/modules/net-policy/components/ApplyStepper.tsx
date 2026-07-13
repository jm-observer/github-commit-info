import { useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { CheckCircle2, XCircle, Loader2, Circle, Play } from 'lucide-react'
import { APPLY_PROGRESS_EVENT, APPLY_STEPS, type ApplyProgress } from '../api/tauri-client'

type StepStatus = 'pending' | 'running' | 'ok' | 'fail'

interface StepState {
  status: StepStatus
  detail?: string
}

function initialSteps(): StepState[] {
  return APPLY_STEPS.map(() => ({ status: 'pending' as StepStatus }))
}

const HINTS: Record<number, string> = {
  0: '检查 WireGuard / DNS / LAN 配置是否合法（保存设置时也会校验）。',
  1: '装防火墙基线需管理员权限，且至少有一块处于 Up 的物理网卡。',
  2: '确认 mihomo 可执行文件存在（设 MIHOMO_BIN 或放到 net-policy/mihomo/）。',
  3: 'TUN(Meta) 未在超时内起栈：检查 WG endpoint 可达性 / 管理员权限 / wintun 驱动。',
  4: '补 KS-TUN 放行规则失败：检查防火墙服务与权限。',
  5: '引擎/隧道未就绪，连通验证未通过。',
}

/**
 * 应用流程分步 stepper（设计 §3.3）：订阅 net-policy://apply-progress 事件，渲染 6 步状态。
 * 失败时高亮失败步 + 错误原文 + 修复提示。「应用」按钮触发 onApply（实际 invoke 在父组件）。
 */
export function ApplyStepper({
  onApply,
  busy,
  canApply,
}: {
  onApply: () => Promise<void>
  busy: boolean
  canApply: boolean
}) {
  const [steps, setSteps] = useState<StepState[]>(initialSteps)
  const [active, setActive] = useState(false)

  useEffect(() => {
    let unlisten: (() => void) | undefined
    let canceled = false
    void listen<ApplyProgress>(APPLY_PROGRESS_EVENT, (event) => {
      const p = event.payload
      setSteps((prev) => {
        const next = prev.slice()
        if (p.step >= 0 && p.step < next.length) {
          next[p.step] = { status: p.status, detail: p.detail ?? undefined }
        }
        return next
      })
    })
      .then((u) => {
        if (canceled) { u(); return }
        unlisten = u
      })
      .catch((err) => { console.error('subscribe apply-progress failed', err) })
    return () => {
      canceled = true
      unlisten?.()
    }
  }, [])

  const start = async () => {
    setSteps(initialSteps())
    setActive(true)
    await onApply()
    // 保持最终态显示（成功全 ok / 失败高亮）。
  }

  const failedIdx = steps.findIndex((s) => s.status === 'fail')

  return (
    <section className="rounded-lg border border-gray-200 dark:border-gray-800">
      <div className="flex items-center justify-between border-b border-gray-200 px-4 py-2 dark:border-gray-800">
        <h2 className="text-sm font-semibold">应用流程</h2>
        <button
          className="inline-flex items-center gap-1.5 rounded-md bg-blue-600 px-3 py-1.5 text-sm text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
          onClick={() => void start()}
          disabled={busy || !canApply}
          title={!canApply ? '请先配置 WireGuard 出口' : undefined}
        >
          {busy ? <Loader2 size={14} className="animate-spin" /> : <Play size={14} />} 应用
        </button>
      </div>
      <ol className="divide-y divide-gray-100 dark:divide-gray-800">
        {steps.map((s, i) => {
          const isFail = s.status === 'fail'
          return (
            <li key={i} className={`flex items-start gap-2.5 px-4 py-2 ${isFail ? 'bg-red-50 dark:bg-red-950/30' : ''}`}>
              <span className="mt-0.5 shrink-0">
                {s.status === 'ok' && <CheckCircle2 size={16} className="text-green-500" />}
                {s.status === 'running' && <Loader2 size={16} className="animate-spin text-blue-500" />}
                {s.status === 'fail' && <XCircle size={16} className="text-red-500" />}
                {s.status === 'pending' && <Circle size={16} className="text-gray-300 dark:text-gray-600" />}
              </span>
              <div className="min-w-0 flex-1">
                <span className={`text-sm ${
                  s.status === 'pending'
                    ? 'text-gray-400 dark:text-gray-500'
                    : isFail
                      ? 'font-medium text-red-700 dark:text-red-300'
                      : ''
                }`}>
                  {i + 1}. {APPLY_STEPS[i]}
                </span>
                {s.detail && (
                  <span className={`ml-2 text-xs ${isFail ? 'text-red-600 dark:text-red-400' : 'text-gray-500'}`}>
                    {s.detail}
                  </span>
                )}
              </div>
            </li>
          )
        })}
      </ol>
      {active && failedIdx >= 0 && (
        <div className="border-t border-red-200 bg-red-50 px-4 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          第 {failedIdx + 1} 步「{APPLY_STEPS[failedIdx]}」失败。提示：{HINTS[failedIdx] ?? '查看上方错误原文，修复后重试。'}
        </div>
      )}
    </section>
  )
}
