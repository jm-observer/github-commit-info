import { RefreshCw, Trash2, GitBranch } from 'lucide-react'
import type { EntryKind, LoopRow } from '../api/tauri-client'

interface Props {
  loops: LoopRow[]
  selectedId: number | null
  onSelect: (id: number) => void
  onRefresh: () => void
  onDelete: (id: number) => void
  loading: boolean
  /** 运行中循环 id（实时态，权威来自运行时；DB status 可能滞后）。 */
  liveIds: Set<number>
  /** 需关注循环 id（等待作答 / 确认）。 */
  attentionIds: Set<number>
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
  aborted_timeout: '超时',
  aborted_parse: '解析失败',
  aborted_by_user: '用户中止',
  interrupted: '中断',
}

/** 入口徽标（旧数据 entry_kind=null 时按 mode 推断；见多入口设计 §5 兼容映射）。 */
const ENTRY_META: Record<EntryKind, { icon: string; label: string; title: string }> = {
  doc_review: { icon: '📄', label: '文档复核', title: '从文档复核开始（DocReview）' },
  implement: { icon: '🛠', label: '实现', title: '从实现开始（Implement）' },
  review_seed: { icon: '📝', label: '外部 seed', title: '从既有复核意见开始（ReviewSeed）' },
}

function inferEntryKind(loop: LoopRow): EntryKind {
  if (loop.entry_kind) return loop.entry_kind
  return loop.mode === 'design' ? 'doc_review' : 'implement'
}

function shortTime(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return `${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')} ${String(
    d.getHours(),
  ).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

export function LoopList(props: Props) {
  const { loops, selectedId, onSelect, onRefresh, onDelete, loading, liveIds, attentionIds } = props
  return (
    <div className="flex h-full w-full flex-col rounded-md border border-gray-200 dark:border-gray-800">
      <div className="flex items-center justify-between border-b border-gray-200 px-2 py-1.5 dark:border-gray-800">
        <span className="text-xs font-medium text-gray-600 dark:text-gray-300">复核记录</span>
        <button
          onClick={onRefresh}
          className="flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800"
          title="刷新"
        >
          <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
        </button>
      </div>
      <ul className="min-h-0 flex-1 overflow-auto">
        {loops.length === 0 ? (
          <li className="px-2 py-6 text-center text-xs text-gray-400">暂无记录</li>
        ) : (
          loops.map(l => {
            const ss = STATUS_STYLE[l.status] ?? { label: l.status, cls: 'bg-gray-100 text-gray-600' }
            const entry = ENTRY_META[inferEntryKind(l)]
            return (
              <li
                key={l.id}
                onClick={() => onSelect(l.id)}
                className={`group cursor-pointer border-b border-gray-100 px-2 py-1.5 dark:border-gray-800/60 ${
                  l.id === selectedId
                    ? 'bg-blue-50 dark:bg-gray-800/70'
                    : 'hover:bg-gray-50 dark:hover:bg-gray-800/40'
                }`}
              >
                <div className="flex items-center gap-1.5">
                  <span className={`rounded px-1.5 py-0.5 text-[10px] ${ss.cls}`}>
                    {liveIds.has(l.id) ? '运行中 ●' : ss.label}
                  </span>
                  {attentionIds.has(l.id) && (
                    <span
                      className="rounded bg-amber-100 px-1 py-0.5 text-[10px] text-amber-700 dark:bg-amber-900/40 dark:text-amber-300"
                      title="待处理：等待作答 / 确认"
                    >
                      !
                    </span>
                  )}
                  <span
                    className="rounded bg-gray-100 px-1 py-0.5 text-[10px] text-gray-600 dark:bg-gray-800 dark:text-gray-300"
                    title={entry.title}
                  >
                    {entry.icon}
                  </span>
                  <span
                    className="min-w-0 flex-1 truncate text-sm text-gray-800 dark:text-gray-100"
                    title={l.target_abs}
                  >
                    {l.target_label || l.target_repo_rel}
                  </span>
                  <button
                    onClick={e => {
                      e.stopPropagation()
                      onDelete(l.id)
                    }}
                    className="opacity-0 transition group-hover:opacity-100"
                    title="删除记录"
                  >
                    <Trash2 size={13} className="text-gray-400 hover:text-red-500" />
                  </button>
                </div>
                <div className="mt-0.5 flex flex-wrap items-center gap-1.5 text-[11px] text-gray-400">
                  <span>{l.mode === 'design' ? '设计' : '实现'}</span>
                  {l.final_verdict && <span>· {FINAL_LABELS[l.final_verdict] ?? l.final_verdict}</span>}
                  <span>· {l.total_rounds} 轮</span>
                  {l.attempts_count > 1 && (
                    <span className="rounded bg-blue-100 px-1 text-[10px] font-medium text-blue-700 dark:bg-blue-900/40 dark:text-blue-300">
                      × {l.attempts_count}
                    </span>
                  )}
                  {l.step_confirm ? <span>· 逐步确认</span> : <span>· 自动</span>}
                  {l.use_worktree && (
                    <span className="flex items-center gap-0.5 text-violet-500">
                      <GitBranch size={10} />
                      worktree
                    </span>
                  )}
                  <span className="ml-auto">{shortTime(l.created_at)}</span>
                </div>
              </li>
            )
          })
        )}
      </ul>
    </div>
  )
}
