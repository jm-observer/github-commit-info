import { GitBranch } from 'lucide-react'
import type { LoopMessageRow, LoopRow } from '../api/tauri-client'
import { LoopTranscript } from './LoopTranscript'

interface Props {
  loop: LoopRow | null
  messages: LoopMessageRow[]
  loadingMessages: boolean
  /** 「开始实现」：仅当该 design 记录最终 PASS 时可点，承接进入实现阶段。 */
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
  interrupted: '中断（应用重启）',
}

/** 短 id：会话 id 取末段，避免占满一行。 */
function shortId(id: string): string {
  return id.length > 12 ? `…${id.slice(-10)}` : id
}

/** 选中记录的只读详情面板：配置 + 状态 + 往返消息 + 上下文操作。 */
export function LoopDetail({ loop, messages, loadingMessages, onStartImplementation }: Props) {
  if (!loop) {
    return (
      <div className="flex h-full items-center justify-center rounded-md border border-gray-200 text-xs text-gray-400 dark:border-gray-800">
        选择左侧一条记录查看详情
      </div>
    )
  }

  const ss = STATUS_STYLE[loop.status] ?? { label: loop.status, cls: 'bg-gray-100 text-gray-600' }
  // 「开始实现」门槛：design 模式且确实通过复核（done + PASS）。
  const passed = loop.status === 'done' && loop.final_verdict === 'pass'
  const showImplement = loop.mode === 'design'

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

        {/* 配置内容（只读，对应上方新建表单的字段）。 */}
        <div className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11px] text-gray-500 dark:text-gray-400">
          <span className="text-gray-400">目标</span>
          <span className="truncate text-gray-600 dark:text-gray-300" title={loop.target_abs}>{loop.target_abs}</span>
          <span className="text-gray-400">仓库</span>
          <span className="truncate text-gray-600 dark:text-gray-300" title={loop.repo_root}>{loop.repo_root}</span>
          <span className="text-gray-400">Claude</span>
          <span className="truncate font-mono text-gray-600 dark:text-gray-300" title={loop.claude_session}>{shortId(loop.claude_session)}</span>
          <span className="text-gray-400">Codex</span>
          <span className="truncate font-mono text-gray-600 dark:text-gray-300" title={loop.codex_session}>{shortId(loop.codex_session)}</span>
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

        {showImplement && (
          <div className="mt-2">
            <button
              onClick={() => passed && onStartImplementation?.(loop)}
              disabled={!passed}
              className="rounded-md bg-emerald-600 px-3 py-1 text-xs text-white hover:bg-emerald-700 disabled:cursor-not-allowed disabled:opacity-50"
              title={passed ? '设计已通过复核，承接进入实现阶段' : '需设计复核通过（PASS）后才能开始实现'}
            >
              开始实现
            </button>
          </div>
        )}
      </div>

      <div className="min-h-0 flex-1">
        <LoopTranscript loopId={loop.id} messages={messages} loading={loadingMessages} />
      </div>
    </div>
  )
}
