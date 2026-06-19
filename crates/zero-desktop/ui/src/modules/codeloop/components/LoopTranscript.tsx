import type { LoopMessageRow } from '../api/tauri-client'

interface Props {
  loopId: number | null
  messages: LoopMessageRow[]
  loading: boolean
}

const VERDICT_LABELS: Record<string, string> = {
  pass: 'PASS',
  needs_work: 'NEEDS_WORK',
  parse_failed: '解析失败',
}

function kindMeta(kind: string): { label: string; cls: string } {
  switch (kind) {
    case 'codex_review':
      return { label: 'Codex 复核', cls: 'text-purple-600 dark:text-purple-300' }
    case 'codex_review_seed':
      return { label: 'Codex 复核（外部 seed）', cls: 'text-amber-700 dark:text-amber-300' }
    case 'claude_revise':
      return { label: 'Claude 修订', cls: 'text-blue-600 dark:text-blue-300' }
    default:
      return { label: '系统', cls: 'text-gray-400' }
  }
}

export function LoopTranscript(props: Props) {
  const { loopId, messages, loading } = props

  if (loopId == null) {
    return (
      <div className="flex h-full items-center justify-center rounded-md border border-gray-200 text-xs text-gray-400 dark:border-gray-800">
        选择左侧一条记录查看往返消息
      </div>
    )
  }

  return (
    <div className="flex h-full min-h-0 flex-col rounded-md border border-gray-200 dark:border-gray-800">
      <div className="border-b border-gray-200 px-3 py-1.5 text-xs font-medium text-gray-600 dark:border-gray-800 dark:text-gray-300">
        记录 #{loopId} 往返消息
      </div>
      <div className="min-h-0 flex-1 space-y-2 overflow-auto p-3">
        {loading ? (
          <div className="pt-8 text-center text-xs text-gray-400">加载中…</div>
        ) : messages.length === 0 ? (
          <div className="pt-8 text-center text-xs text-gray-400">该记录暂无消息</div>
        ) : (
          messages.map(m => {
            const meta = kindMeta(m.kind)
            const system = m.kind === 'system'
            const seed = m.kind === 'codex_review_seed'
            const wrapCls = seed
              ? 'border-amber-200 bg-yellow-50 dark:border-amber-700/50 dark:bg-amber-900/20'
              : system
                ? 'border-gray-100 bg-gray-50 dark:border-gray-800/60 dark:bg-gray-900/40'
                : 'border-gray-200 bg-white dark:border-gray-700 dark:bg-gray-800/60'
            return (
              <div key={m.id} className={`rounded-md border px-3 py-2 ${wrapCls}`}>
                <div className="mb-1 flex items-center gap-2 text-xs">
                  <span className={`font-medium ${meta.cls}`}>{meta.label}</span>
                  <span className="text-gray-400">第 {m.round} 轮</span>
                  {seed && (
                    <span className="rounded bg-amber-100 px-1.5 py-0.5 text-[10px] text-amber-700 dark:bg-amber-800/60 dark:text-amber-200">
                      外部 seed
                    </span>
                  )}
                  {m.verdict && (
                    <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] text-gray-500 dark:bg-gray-700 dark:text-gray-300">
                      {VERDICT_LABELS[m.verdict] ?? m.verdict}
                    </span>
                  )}
                </div>
                {seed && (
                  <div className="mb-1.5 text-[11px] italic text-amber-700 dark:text-amber-300">
                    以下内容由用户提供，非 Codex 输出。
                  </div>
                )}
                <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-words text-xs leading-relaxed text-gray-700 dark:text-gray-200">
                  {m.content}
                </pre>
              </div>
            )
          })
        )}
      </div>
    </div>
  )
}
