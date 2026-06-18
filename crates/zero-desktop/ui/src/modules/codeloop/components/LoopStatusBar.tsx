import { Eye, MessageSquare, Play, Square, Stethoscope } from 'lucide-react'
import type { Progress, ReviewMode } from '../api/tauri-client'

interface Props {
  targetPath: string
  setTargetPath: (v: string) => void
  mode: ReviewMode
  setMode: (v: ReviewMode) => void
  maxRounds: number
  setMaxRounds: (v: number) => void
  waitIdle: boolean
  setWaitIdle: (v: boolean) => void
  stepConfirm: boolean
  setStepConfirm: (v: boolean) => void
  useWorktree: boolean
  setUseWorktree: (v: boolean) => void
  /** 首轮预热（按 provider 分开）：该端已在预览台外部建立，循环首轮跳过其说明块。 */
  estCodex: boolean
  setEstCodex: (v: boolean) => void
  estClaude: boolean
  setEstClaude: (v: boolean) => void
  /** 打开预览 / 手动驱动台。 */
  onPreview: () => void
  running: boolean
  canStart: boolean
  onStart: () => void
  onStop: () => void
  /** 跟踪：弹出所选两个会话的消息记录。 */
  onTrack: () => void
  canTrack: boolean
  /** 环境自检：弹出 preflight 面板。 */
  onPreflight: () => void
  canPreflight: boolean
  progress: Progress | null
  /** 运行中是否已转全自动（= !step_confirm）。 */
  liveAuto: boolean
  /** 运行中翻转自动确认（true=转全自动 / false=恢复逐步确认）。 */
  onToggleAuto: (enabled: boolean) => void
}

const VERDICT_LABELS: Record<string, string> = {
  pass: 'PASS',
  needs_work: 'NEEDS_WORK',
  parse_failed: '解析失败',
}

const FINAL_LABELS: Record<string, string> = {
  pass: '通过 ✓',
  max_rounds: '达最大轮次',
  aborted_timeout: '超时中止',
  aborted_parse: '解析失败中止',
  aborted_by_user: '用户中止（可调整后重启）',
}

function statusText(running: boolean, p: Progress | null): { text: string; cls: string } {
  if (p?.phase === 'error') return { text: '基础设施错误', cls: 'text-red-600 dark:text-red-400' }
  if (p?.phase === 'awaiting_confirm')
    return { text: '等待确认传递…', cls: 'text-amber-600 dark:text-amber-400' }
  if (p?.phase === 'awaiting_input')
    return { text: '等待你作答…', cls: 'text-amber-600 dark:text-amber-400' }
  if (p?.phase === 'done') {
    const f = p.final_verdict ?? ''
    const ok = f === 'pass'
    return {
      text: `已结束：${FINAL_LABELS[f] ?? f}`,
      cls: ok ? 'text-green-600 dark:text-green-400' : 'text-amber-600 dark:text-amber-400',
    }
  }
  if (running) return { text: '运行中 ●', cls: 'text-blue-600 dark:text-blue-400' }
  return { text: '空闲', cls: 'text-gray-400' }
}

export function LoopStatusBar(props: Props) {
  const { running, canStart, onStart, onStop, progress: p } = props
  const st = statusText(running, p)

  return (
    <div className="flex flex-col gap-2 rounded-md border border-gray-200 bg-gray-50 p-3 dark:border-gray-800 dark:bg-gray-900">
      <div className="flex flex-wrap items-end gap-3">
        <div className="flex flex-1 flex-col gap-1" style={{ minWidth: 220 }}>
          <label className="text-xs text-gray-500 dark:text-gray-400">复核目标（仓库内文件/目录路径）</label>
          <input
            type="text"
            value={props.targetPath}
            onChange={e => props.setTargetPath(e.target.value)}
            disabled={running}
            placeholder="docs/foo.md"
            className="rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 disabled:opacity-60 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          />
        </div>

        <div className="flex flex-col gap-1">
          <label className="text-xs text-gray-500 dark:text-gray-400">模式</label>
          <select
            value={props.mode}
            onChange={e => props.setMode(e.target.value as ReviewMode)}
            disabled={running}
            className="rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 disabled:opacity-60 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          >
            <option value="design">设计复核</option>
            <option value="implementation">实现复核</option>
          </select>
        </div>

        <div className="flex flex-col gap-1">
          <label className="text-xs text-gray-500 dark:text-gray-400">最大轮次</label>
          <input
            type="number"
            min={1}
            max={20}
            value={props.maxRounds}
            onChange={e => props.setMaxRounds(Math.max(1, Number(e.target.value) || 1))}
            disabled={running}
            className="w-20 rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 disabled:opacity-60 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          />
        </div>

        <label className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300">
          <input
            type="checkbox"
            checked={props.waitIdle}
            onChange={e => props.setWaitIdle(e.target.checked)}
            disabled={running}
          />
          先等 Claude 当前轮完成
        </label>

        <label className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300">
          <input
            type="checkbox"
            checked={props.stepConfirm}
            onChange={e => props.setStepConfirm(e.target.checked)}
            disabled={running}
          />
          逐步确认（每次传递先弹窗）
        </label>

        <label
          className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300"
          title="勾选后让 Claude 自己用 git worktree + 子 agent 隔离实现，再把 Codex 复核重定位到该 worktree"
        >
          <input
            type="checkbox"
            checked={props.useWorktree}
            onChange={e => props.setUseWorktree(e.target.checked)}
            disabled={running}
          />
          worktree 模式（Claude 用 worktree+子 agent 实现）
        </label>

        <label
          className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300"
          title="若已在预览台对该端发过 establishing 首轮，循环首轮跳过其说明块"
        >
          <input
            type="checkbox"
            checked={props.estCodex}
            onChange={e => props.setEstCodex(e.target.checked)}
            disabled={running}
          />
          Codex 已预热
        </label>
        <label
          className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300"
          title="若已在预览台对该端发过 establishing 首轮，循环首轮跳过其说明块"
        >
          <input
            type="checkbox"
            checked={props.estClaude}
            onChange={e => props.setEstClaude(e.target.checked)}
            disabled={running}
          />
          Claude 已预热
        </label>

        <button
          onClick={props.onPreview}
          disabled={!props.canTrack}
          className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          title="预览单个会话内容，并可手动发一条（首轮预热 / 调试）"
        >
          <MessageSquare size={14} />
          驱动台
        </button>

        <button
          onClick={props.onPreflight}
          disabled={!props.canPreflight}
          className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          title="启动前自检：会话可定位 / 三方一致 / CLI 版本 / 可选实发往返"
        >
          <Stethoscope size={14} />
          环境自检
        </button>

        <button
          onClick={props.onTrack}
          disabled={!props.canTrack}
          className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm text-gray-700 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          title="弹出所选两个会话的消息记录"
        >
          <Eye size={14} />
          跟踪
        </button>

        {running ? (
          <button
            onClick={onStop}
            className="flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-1.5 text-sm text-white hover:bg-red-700"
          >
            <Square size={14} />
            停止
          </button>
        ) : (
          <button
            onClick={onStart}
            disabled={!canStart}
            className="flex items-center gap-1.5 rounded-md bg-blue-600 px-3 py-1.5 text-sm text-white hover:bg-blue-700 disabled:opacity-50"
          >
            <Play size={14} />
            启动复核循环
          </button>
        )}
      </div>

      <div className="flex items-center gap-4 text-xs">
        <span className={st.cls}>循环：{st.text}</span>
        {p?.round != null && <span className="text-gray-500 dark:text-gray-400">轮次 {p.round}</span>}
        {p?.verdict && (
          <span className="text-gray-500 dark:text-gray-400">
            判定 {VERDICT_LABELS[p.verdict] ?? p.verdict}
          </span>
        )}
        {p?.phase === 'error' && p.error && (
          <span className="truncate text-red-500" title={p.error}>
            {p.error}
          </span>
        )}
        {running && (
          <label
            className="flex cursor-pointer items-center gap-1.5 text-gray-600 dark:text-gray-300"
            title="转全自动后不再逐步确认；撞到 ASK_USER 仍会停下问你"
          >
            <input
              type="checkbox"
              checked={props.liveAuto}
              onChange={e => props.onToggleAuto(e.target.checked)}
            />
            {props.liveAuto ? '全自动（不再逐步确认）' : '逐步确认中'}
          </label>
        )}
        {running && (
          <span className="text-amber-600 dark:text-amber-400">
            ⚠ 循环期间请勿在桌面端操作这两个会话
          </span>
        )}
      </div>
    </div>
  )
}
