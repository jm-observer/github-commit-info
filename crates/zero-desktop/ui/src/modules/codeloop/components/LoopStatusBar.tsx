import { Eye, MessageSquare, Play, Stethoscope } from 'lucide-react'
import type { EntryKind, ReviewMode } from '../api/tauri-client'

interface Props {
  targetPath: string
  setTargetPath: (v: string) => void
  /** 入口种类：决定 target_path 控件的 label 文案（多入口设计 §7）。 */
  entryKind: EntryKind
  /** ReviewSeed 时的二级 mode 子选；DocReview/Implement 时只读、由入口决定。 */
  mode: ReviewMode
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
  /** 评估方案最优性：仅 Design 系入口（非 Continuation、mode=design）显示。 */
  evaluateAlternatives: boolean
  setEvaluateAlternatives: (v: boolean) => void
  /** 打开预览 / 手动驱动台。 */
  onPreview: () => void
  canStart: boolean
  onStart: () => void
  /** 跟踪：弹出所选两个会话的消息记录。 */
  onTrack: () => void
  canTrack: boolean
  /** 环境自检：弹出 preflight 面板。 */
  onPreflight: () => void
  canPreflight: boolean
  /** 当前并发运行中的循环数（并发模型：本栏只配置 + 启动，运行态在下方记录里逐个管理）。 */
  liveCount: number
}

/** target_path 控件的 label 文案随入口 + mode 变化（§7 角色矩阵）。 */
function targetPathLabel(entryKind: EntryKind, mode: ReviewMode): string {
  switch (entryKind) {
    case 'continuation':
      return ''
    case 'doc_review':
      return '待复核 / 修订文档（仓库内路径）'
    case 'implement':
      return '设计/规格文档（仓库内路径）'
    case 'review_seed':
      return mode === 'implementation'
        ? '待修订代码根（仓库内相对路径，文件或目录均可）'
        : '待修订文档（仓库内路径）'
  }
}

/**
 * 新建循环的配置 + 启动栏（并发模型）：本栏只负责「配一个新循环并启动」，启动后表单清空、
 * 可继续配下一个；运行中循环的停止 / 逐步确认翻转在下方记录的详情面板里逐个进行。
 */
export function LoopStatusBar(props: Props) {
  const { canStart, onStart, entryKind, mode, liveCount } = props

  return (
    <div className="flex flex-col gap-2 rounded-md border border-gray-200 bg-gray-50 p-3 dark:border-gray-800 dark:bg-gray-900">
      <div className="flex flex-wrap items-end gap-3">
        {entryKind === 'continuation' ? (
          <div className="flex flex-1 flex-col gap-1 pb-2" style={{ minWidth: 220 }}>
            <div className="text-xs text-gray-500 dark:text-gray-400">复核范围</div>
            <div className="rounded-md border border-dashed border-gray-300 bg-gray-50 px-2 py-1.5 text-xs text-gray-600 dark:border-gray-700 dark:bg-gray-800/60 dark:text-gray-300">
              既有会话已携带上下文 —— 无需指定 target，循环只发「继续审核 ↔ 继续修订」。
            </div>
          </div>
        ) : (
          <div className="flex flex-1 flex-col gap-1" style={{ minWidth: 220 }}>
            <label className="text-xs text-gray-500 dark:text-gray-400">
              {targetPathLabel(entryKind, mode)}
            </label>
            <input
              type="text"
              value={props.targetPath}
              onChange={e => props.setTargetPath(e.target.value)}
              placeholder={
                entryKind === 'review_seed' && mode === 'implementation'
                  ? 'crates/zero-desktop（文件或目录均可）'
                  : 'docs/foo.md'
              }
              className="rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
            />
          </div>
        )}

        <div className="flex flex-col gap-1">
          <label className="text-xs text-gray-500 dark:text-gray-400">最大轮次</label>
          <input
            type="number"
            min={1}
            max={20}
            value={props.maxRounds}
            onChange={e => props.setMaxRounds(Math.max(1, Number(e.target.value) || 1))}
            className="w-20 rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          />
        </div>

        <label className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300">
          <input
            type="checkbox"
            checked={props.waitIdle}
            onChange={e => props.setWaitIdle(e.target.checked)}
          />
          先等 Claude 当前轮完成
        </label>

        <label
          className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300"
          title="新循环首轮起按此设置；运行中可在记录详情里逐个翻转"
        >
          <input
            type="checkbox"
            checked={props.stepConfirm}
            onChange={e => props.setStepConfirm(e.target.checked)}
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
          />
          Claude 已预热
        </label>

        {/* 仅 Design 系入口（非 Continuation、mode=design）显示——Implementation 阶段评估方案
            最优性 = 返工；Continuation 直接走会话历史，无 SCOPE。 */}
        {entryKind !== 'continuation' && mode === 'design' && (
          <label
            className="flex items-center gap-1.5 pb-2 text-xs text-gray-600 dark:text-gray-300"
            title="多一条「评估所选方案 vs 替代方案合理性」维度。慢、易发散，仅对方案未定稿的文档有用；定稿落地阶段勿开。"
          >
            <input
              type="checkbox"
              checked={props.evaluateAlternatives}
              onChange={e => props.setEvaluateAlternatives(e.target.checked)}
            />
            评估方案合理性
          </label>
        )}

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

        <button
          onClick={onStart}
          disabled={!canStart}
          className="flex items-center gap-1.5 rounded-md bg-blue-600 px-3 py-1.5 text-sm text-white hover:bg-blue-700 disabled:opacity-50"
        >
          <Play size={14} />
          启动复核循环
        </button>
      </div>

      <div className="flex items-center gap-4 text-xs">
        {liveCount > 0 ? (
          <span className="text-blue-600 dark:text-blue-400">● {liveCount} 个循环运行中（在下方记录里逐个管理）</span>
        ) : (
          <span className="text-gray-400">空闲：配置上方表单后启动；可同时跑多个循环（同一会话除外）</span>
        )}
      </div>
    </div>
  )
}
