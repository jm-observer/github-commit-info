import { useState } from 'react'
import type { LoopRow, StartInput } from '../api/tauri-client'

interface Props {
  /** 已通过设计复核（PASS）的源记录，配置据此预填。 */
  source: LoopRow
  onStart: (input: StartInput) => void
  onClose: () => void
}

/**
 * 实现配置确认窗：design 复核 PASS 后承接进入实现阶段。
 * 默认沿用源记录的会话/目标/各开关（mode 强制为 implementation），用户可改后点「开始实现」。
 * 启动的实现循环通过 parent_loop_id 关联回源设计记录（血缘）。
 */
export function ImplementationModal({ source, onStart, onClose }: Props) {
  const [targetPath, setTargetPath] = useState(source.target_abs)
  const [maxRounds, setMaxRounds] = useState(source.max_rounds)
  const [stepConfirm, setStepConfirm] = useState(true)
  const [useWorktree, setUseWorktree] = useState(source.use_worktree)
  const [waitIdle, setWaitIdle] = useState(false)

  const start = () => {
    onStart({
      claude: { session_id: source.claude_session },
      codex: { session_id: source.codex_session },
      target_path: targetPath.trim(),
      mode: 'implementation',
      max_rounds: maxRounds,
      wait_for_claude_idle: waitIdle,
      step_confirm: stepConfirm,
      use_worktree: useWorktree,
      parent_loop_id: source.id,
    })
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="flex w-[560px] max-w-[92vw] flex-col rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
        <div className="mb-1 text-xs text-gray-400">承接自 #{source.id}（设计复核已 PASS）</div>
        <h2 className="mb-3 text-sm font-medium text-gray-800 dark:text-gray-100">开始实现 · 确认配置</h2>

        <label className="mb-1 text-xs text-gray-500 dark:text-gray-400">实现目标（仓库内文件/目录）</label>
        <input
          type="text"
          value={targetPath}
          onChange={e => setTargetPath(e.target.value)}
          className="mb-3 rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
        />

        <div className="mb-3 flex items-center gap-3">
          <label className="text-xs text-gray-500 dark:text-gray-400">最大轮次</label>
          <input
            type="number"
            min={1}
            max={20}
            value={maxRounds}
            onChange={e => setMaxRounds(Math.max(1, Number(e.target.value) || 1))}
            className="w-20 rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          />
        </div>

        <label className="mb-1.5 flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
          <input type="checkbox" checked={stepConfirm} onChange={e => setStepConfirm(e.target.checked)} />
          逐步确认（每次传递先弹窗）
        </label>
        <label className="mb-1.5 flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
          <input type="checkbox" checked={useWorktree} onChange={e => setUseWorktree(e.target.checked)} />
          worktree 模式（Claude 用 worktree + 子 agent 实现）
        </label>
        <label className="mb-4 flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
          <input type="checkbox" checked={waitIdle} onChange={e => setWaitIdle(e.target.checked)} />
          先等 Claude 当前轮完成
        </label>

        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-md border border-gray-300 px-4 py-1.5 text-sm text-gray-700 hover:bg-gray-100 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          >
            取消
          </button>
          <button
            onClick={start}
            disabled={!targetPath.trim()}
            className="rounded-md bg-emerald-600 px-4 py-1.5 text-sm text-white hover:bg-emerald-700 disabled:opacity-50"
          >
            开始实现
          </button>
        </div>
      </div>
    </div>
  )
}
