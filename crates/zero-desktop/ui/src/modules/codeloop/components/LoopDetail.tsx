import { GitBranch, Copy } from 'lucide-react'
import type { LoopMessageRow, LoopRow } from '../api/tauri-client'
import { LoopTranscript } from './LoopTranscript'

interface Props {
  loop: LoopRow | null
  messages: LoopMessageRow[]
  loadingMessages: boolean
  /** 把该记录的配置复制进上方新建表单（显式动作，避免选中即污染）。 */
  onCopyConfig: (loop: LoopRow) => void
  /** 「开始实现」：design 且 PASS 的记录可承接进入实现阶段（Task #6 接入）。 */
  onStartImplementation?: (loop: LoopRow) => void
}

const STATUS_STYLE: Record<string, { label: string; cls: string }> = {
  running: { label: '运行中', cls: 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300' },
  done: { label: '完成', cls: 'bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-300' },
  failed: { label: '失败', cls: 'bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-300' },
  aborted: { label: '中止', cls: 'bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300' },
}

const FINAL_LABELS: Record<string, string> = {
  pass: 'PASS',
  max_rounds: '达最大轮次',
  aborted_timeout: '超时中止',
  aborted_parse: '解析失败中止',
  aborted_by_user: '用户中止',
}

/** 选中记录的只读详情面板：元信息 + 往返消息 + 上下文操作。 */
export function LoopDetail({ loop, messages, loadingMessages, onCopyConfig, onStartImplementation }: Props) {
  if (!loop) {
    return (
      <div className="flex h-full items-center justify-center rounded-md border border-gray-200 text-xs text-gray-400 dark:border-gray-800">
        选择左侧一条记录查看详情
      </div>
    )
  }

  const ss = STATUS_STYLE[loop.status] ?? { label: loop.status, cls: 'bg-gray-100 text-gray-600' }
  // design 且最终 PASS → 可承接进入实现阶段。
  const canImplement = loop.mode === 'design' && loop.final_verdict === 'pass'

  return (
    <div className="flex h-full min-h-0 flex-col gap-2">
      <div className="rounded-md border border-gray-200 p-3 dark:border-gray-800">
        <div className="flex items-center gap-2">
          <span className={`rounded px-1.5 py-0.5 text-[10px] ${ss.cls}`}>{ss.label}</span>
          <span className="min-w-0 flex-1 truncate text-sm font-medium text-gray-800 dark:text-gray-100" title={loop.target_abs}>
            #{loop.id} {loop.target_label || loop.target_repo_rel}
          </span>
          {loop.parent_loop_id != null && (
            <span className="rounded bg-emerald-50 px-1.5 py-0.5 text-[10px] text-emerald-600 dark:bg-emerald-900/30 dark:text-emerald-300">
              承接自 #{loop.parent_loop_id}
            </span>
          )}
        </div>
        <div className="mt-1.5 flex flex-wrap items-center gap-2 text-[11px] text-gray-500 dark:text-gray-400">
          <span>{loop.mode === 'design' ? '设计复核' : '实现复核'}</span>
          {loop.final_verdict && <span>· {FINAL_LABELS[loop.final_verdict] ?? loop.final_verdict}</span>}
          <span>· {loop.total_rounds}/{loop.max_rounds} 轮</span>
          {loop.step_confirm ? <span>· 逐步确认</span> : <span>· 自动</span>}
          {loop.use_worktree && (
            <span className="flex items-center gap-0.5 text-violet-500">
              <GitBranch size={10} />
              worktree
            </span>
          )}
        </div>
        {loop.worktree_path && (
          <div className="mt-1 truncate text-[11px] text-violet-500" title={loop.worktree_path}>
            worktree: {loop.worktree_path}
          </div>
        )}
        {loop.error && (
          <div className="mt-1 rounded bg-red-50 px-2 py-1 text-[11px] text-red-600 dark:bg-red-900/20 dark:text-red-400">
            {loop.error}
          </div>
        )}

        <div className="mt-2 flex items-center gap-2">
          <button
            onClick={() => onCopyConfig(loop)}
            className="flex items-center gap-1 rounded-md border border-gray-300 px-2.5 py-1 text-xs text-gray-700 hover:bg-gray-100 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
            title="把此记录的会话/目标/配置复制进上方新建表单"
          >
            <Copy size={12} />
            用此配置新建
          </button>
          {canImplement && (
            <button
              onClick={() => onStartImplementation?.(loop)}
              className="rounded-md bg-emerald-600 px-2.5 py-1 text-xs text-white hover:bg-emerald-700"
              title="设计已通过复核，承接进入实现阶段"
            >
              开始实现
            </button>
          )}
        </div>
      </div>

      <div className="min-h-0 flex-1">
        <LoopTranscript loopId={loop.id} messages={messages} loading={loadingMessages} />
      </div>
    </div>
  )
}
