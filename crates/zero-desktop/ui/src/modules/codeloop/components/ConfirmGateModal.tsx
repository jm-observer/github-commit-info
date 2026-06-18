import { useState } from 'react'
import { ArrowRight } from 'lucide-react'

interface Props {
  seq: number
  /** codex_to_claude | claude_to_codex */
  direction?: string
  title?: string
  /** 即将传递的文本全文。 */
  content?: string
  /** 确认放行；`auto=true` 表示用户勾选了「确认后转全自动」，后续不再逐步确认。 */
  onApprove: (auto: boolean) => void
  onReject: () => void
}

/** 方向 → 「从 → 到」标签。 */
function flow(direction?: string): { from: string; to: string } {
  if (direction === 'claude_to_codex') return { from: 'Claude Code', to: 'Codex' }
  return { from: 'Codex', to: 'Claude Code' } // 默认 codex_to_claude
}

/**
 * 逐步确认门弹窗：每次跨会话传递前，展示即将发送的文本，等用户拍板。
 * 「确认发送」继续流程；「不同意（停止）」否决，循环主动中止，用户自行调整后重启。
 */
export function ConfirmGateModal({ seq, direction, title, content, onApprove, onReject }: Props) {
  const { from, to } = flow(direction)
  const [auto, setAuto] = useState(false)
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="flex max-h-[80vh] w-[640px] max-w-[92vw] flex-col rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
        <div className="mb-1 flex items-center gap-2 text-xs text-gray-400">
          <span>待确认传递（seq {seq}）</span>
        </div>
        <div className="mb-3 flex items-center gap-2 text-sm font-medium text-gray-800 dark:text-gray-100">
          <span className="rounded bg-gray-100 px-2 py-0.5 dark:bg-gray-800">{from}</span>
          <ArrowRight size={14} className="text-gray-400" />
          <span className="rounded bg-gray-100 px-2 py-0.5 dark:bg-gray-800">{to}</span>
        </div>
        {title && (
          <div className="mb-2 text-sm text-gray-700 dark:text-gray-200">{title}</div>
        )}

        <div className="mb-1 text-xs text-gray-400">即将发送的内容</div>
        <pre className="mb-4 min-h-[80px] flex-1 overflow-auto whitespace-pre-wrap rounded-md border border-gray-200 bg-gray-50 p-3 text-xs leading-relaxed text-gray-700 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-200">
          {content || '（无内容）'}
        </pre>

        <label className="mb-3 flex cursor-pointer items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
          <input
            type="checkbox"
            checked={auto}
            onChange={e => setAuto(e.target.checked)}
            className="h-3.5 w-3.5"
          />
          确认后自动继续，不再逐步确认（撞到 ASK_USER 仍会停下问你）
        </label>

        <div className="flex justify-end gap-2">
          <button
            onClick={onReject}
            className="rounded-md border border-gray-300 px-4 py-1.5 text-sm text-gray-700 hover:bg-gray-100 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          >
            不同意（停止）
          </button>
          <button
            onClick={() => onApprove(auto)}
            className="rounded-md bg-blue-600 px-4 py-1.5 text-sm text-white hover:bg-blue-700"
          >
            {auto ? '确认并转自动' : '确认发送'}
          </button>
        </div>
      </div>
    </div>
  )
}
