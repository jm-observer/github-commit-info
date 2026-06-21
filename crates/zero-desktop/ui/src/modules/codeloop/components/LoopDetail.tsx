import { type ReactNode, useEffect, useState } from 'react'
import { Activity, Bot, GitBranch, GitMerge, Play, Square, Zap } from 'lucide-react'
import type { EntryKind, LoopMessageRow, LoopRow, Progress, StartInput } from '../api/tauri-client'
import { LoopTranscript } from './LoopTranscript'

export interface StageStartOptions {
  freshCodex?: boolean
}

interface Props {
  loop: LoopRow | null
  messages: LoopMessageRow[]
  loadingMessages: boolean
  /** 「开始实现」：design 复核 PASS 后内联配好 StartInput 直接启动（承接血缘）。 */
  onStartImplementation?: (input: StartInput, options?: StageStartOptions) => void
  /** 「继续复核」：implementation 记录已有 worktree 时，从该 worktree 跳过实现阶段继续 Codex 复核。 */
  onContinueReview?: (input: StartInput, options?: StageStartOptions) => void
  /** 「继续」：循环因达最大轮次 / Claude 退出（token 用完等）终止后，沿用同一对会话续跑。 */
  onContinue?: (input: StartInput, options?: StageStartOptions) => void
  /** 该记录是否就是当前正在跑的循环（并发态，权威来自运行时）。 */
  isLive?: boolean
  /** 运行中的实时进度（isLive 时有值）。 */
  liveProgress?: Progress | null
  /** 运行中是否已转全自动（= !step_confirm）。 */
  liveAuto?: boolean
  /** 运行中翻转自动确认：true=转全自动 / false=恢复逐步确认。仅 isLive 可用。 */
  onToggleAuto?: (enabled: boolean) => void
  /** 停止该运行中循环。仅 isLive 可用。 */
  onStop?: () => void
  /** 跟踪：打开 TrackModal，实时查看本记录两端会话的消息记录。 */
  onTrack?: () => void
  /** 把记录关联的 worktree 合并回主仓库当前分支；返回服务端给出的状态文案。 */
  onMergeWorktree?: () => Promise<string>
  /** 本记录是否已派生下一阶段子记录；为 true 时禁用所有"派生新记录"的操作入口。 */
  hasChild?: boolean
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
  stopped_tracking: '已停止跟踪',
  interrupted: '中断（应用重启）',
}

const PHASE_LABELS: Record<string, string> = {
  implementing: '实现中',
  implemented: '实现完成',
  codex_review: 'Codex 复核',
  claude_revise: 'Claude 修订',
  awaiting_user: '等用户回答',
  finalized: '已收尾',
}

const ENTRY_META: Record<EntryKind, { icon: string; label: string; title: string }> = {
  continuation: { icon: '▶', label: '续跑', title: '继续既有讨论（Continuation）' },
  doc_review: { icon: '📄', label: '文档复核', title: '从文档复核开始（DocReview）' },
  implement: { icon: '🛠', label: '从实现开始', title: '从实现开始（Implement）' },
  review_seed: { icon: '📝', label: '外部 seed', title: '从既有复核意见开始（ReviewSeed）' },
}

function inferEntryKind(loop: LoopRow): EntryKind {
  if (loop.entry_kind) return loop.entry_kind
  return loop.mode === 'design' ? 'doc_review' : 'implement'
}

/** 短 id：会话 id 取末段，避免占满一行。 */
function shortId(id: string): string {
  return id.length > 12 ? `…${id.slice(-10)}` : id
}

/** 运行中循环的一行实时状态文案。 */
function liveText(p: Progress | null): string {
  if (!p) return '运行中…'
  if (p.phase === 'awaiting_confirm') return '等待确认传递…'
  if (p.phase === 'awaiting_input') return '等待你作答…'
  if (p.phase === 'starting') return '启动中…'
  if (p.phase === 'implementing') return 'Claude 实现中…'
  if (p.phase === 'implemented') return '实现完成，待复核…'
  const parts: string[] = ['运行中']
  if (p.round != null && p.round > 0) parts.push(`第 ${p.round} 轮`)
  if (p.verdict) parts.push(p.verdict === 'pass' ? 'PASS' : p.verdict === 'needs_work' ? 'NEEDS_WORK' : p.verdict)
  return parts.join(' · ')
}

/** 选中记录的详情面板：配置 + 状态 + 往返消息 + 上下文操作。 */
export function LoopDetail({
  loop,
  messages,
  loadingMessages,
  onStartImplementation,
  onContinueReview,
  onContinue,
  isLive = false,
  liveProgress = null,
  liveAuto = false,
  onToggleAuto,
  onStop,
  onTrack,
  onMergeWorktree,
  hasChild = false,
}: Props) {
  // 下一阶段动作配置。它只影响从当前记录派生出的新记录，不回写当前记录。
  const [implRounds, setImplRounds] = useState(5)
  const [freshAgent, setFreshAgent] = useState(true)
  const [useStageWorktree, setUseStageWorktree] = useState(true)
  const [autoGo, setAutoGo] = useState(false)
  // 合并 worktree 的状态：进行中 + 服务端文案（成功/失败）。
  const [merging, setMerging] = useState(false)
  const [mergeMsg, setMergeMsg] = useState<{ ok: boolean; text: string } | null>(null)
  useEffect(() => {
    if (!loop) return
    setImplRounds(loop.max_rounds)
    setFreshAgent(true)
    setUseStageWorktree(loop.mode === 'design' ? true : loop.use_worktree)
    setAutoGo(false)
    setMergeMsg(null)
  }, [loop?.id])

  if (!loop) {
    return (
      <div className="flex h-full items-center justify-center rounded-md border border-gray-200 text-xs text-gray-400 dark:border-gray-800">
        选择左侧一条记录查看详情
      </div>
    )
  }

  const ss = isLive
    ? STATUS_STYLE.running
    : STATUS_STYLE[loop.status] ?? { label: loop.status, cls: 'bg-gray-100 text-gray-600' }
  const entry = ENTRY_META[inferEntryKind(loop)]
  // 「开始实现」门槛：design 模式且确实通过复核（done + PASS），且当前未在跑。
  const passed = loop.status === 'done' && loop.final_verdict === 'pass'
  // 已派生下一阶段 → 本条记录的所有"派生新记录"入口都禁用（避免血缘分叉）。
  const showImplement = loop.mode === 'design' && !isLive && !hasChild
  const canContinueReview =
    loop.mode === 'implementation' && !isLive && !hasChild && loop.final_verdict !== 'pass' && !!loop.worktree_path
  // 「继续」可用条件：未在跑 + 未派生子记录 + 终态非 PASS（失败 / 达最大轮次）。沿用同一对会话续跑。
  const canContinue =
    !isLive && !hasChild &&
    (loop.status === 'failed' ||
      (loop.status === 'done' && loop.final_verdict === 'max_rounds') ||
      loop.status === 'aborted' && loop.final_verdict !== 'aborted_by_user' && loop.final_verdict !== 'stopped_tracking')
  // 合并入口：有 worktree、未在跑（避免动它）；hasChild 仍允许（合代码与派生新记录不冲突）。
  const canMerge = !!loop.worktree_path && !isLive && !!onMergeWorktree

  const startImpl = () => {
    onStartImplementation?.({
      claude: { session_id: loop.claude_session },
      codex: { session_id: loop.codex_session },
      target_path: loop.target_abs,
      target_label: loop.target_label,
      mode: 'implementation',
      max_rounds: implRounds,
      wait_for_claude_idle: false,
      step_confirm: !autoGo,
      use_worktree: useStageWorktree,
      parent_loop_id: loop.id,
    }, { freshCodex: freshAgent })
  }

  const continueReview = () => {
    if (!loop.worktree_path) return
    onContinueReview?.({
      claude: { session_id: loop.claude_session },
      codex: { session_id: loop.codex_session },
      target_path: loop.target_abs,
      target_label: loop.target_label,
      mode: 'implementation',
      max_rounds: loop.max_rounds,
      wait_for_claude_idle: false,
      step_confirm: !autoGo,
      use_worktree: true,
      parent_loop_id: loop.id,
      resume_worktree_path: loop.worktree_path,
    }, { freshCodex: freshAgent })
  }

  // 继续这条循环：沿用旧 session（不新建 Codex），established 两端置 true 跳过 STANDING_BLOCK 重发。
  const continueLoop = () => {
    onContinue?.({
      claude: { session_id: loop.claude_session },
      codex: { session_id: loop.codex_session },
      target_path: loop.target_abs,
      target_label: loop.target_label,
      mode: loop.mode,
      max_rounds: implRounds,
      wait_for_claude_idle: false,
      step_confirm: !autoGo,
      use_worktree: loop.use_worktree,
      parent_loop_id: loop.id,
      resume_worktree_path:
        loop.mode === 'implementation' && loop.worktree_path ? loop.worktree_path : undefined,
      established: { codex: true, claude: true },
    }, { freshCodex: false })
  }

  const StageToggle = ({
    checked,
    onChange,
    icon,
    label,
    title,
    disabled = false,
  }: {
    checked: boolean
    onChange: (checked: boolean) => void
    icon: ReactNode
    label: string
    title: string
    disabled?: boolean
  }) => (
    <label
      className={`flex h-8 items-center gap-1.5 rounded-md border px-2 text-xs ${
        checked
          ? 'border-blue-200 bg-blue-50 text-blue-700 dark:border-blue-900/60 dark:bg-blue-950/30 dark:text-blue-300'
          : 'border-gray-200 bg-white text-gray-500 dark:border-gray-700 dark:bg-gray-900/50 dark:text-gray-400'
      } ${disabled ? 'opacity-50' : ''}`}
      title={title}
    >
      <input
        type="checkbox"
        className="h-3.5 w-3.5"
        checked={checked}
        disabled={disabled}
        onChange={e => onChange(e.target.checked)}
      />
      {icon}
      {label}
    </label>
  )

  return (
    <div className="flex h-full min-h-0 flex-col gap-2">
      <div className="rounded-md border border-gray-200 p-3 dark:border-gray-800">
        <div className="flex items-center gap-2">
          <span className={`rounded px-1.5 py-0.5 text-[10px] ${ss.cls}`}>{ss.label}</span>
          <span
            className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] text-gray-700 dark:bg-gray-800 dark:text-gray-200"
            title={entry.title}
          >
            {entry.icon} {entry.label}
          </span>
          <span className="min-w-0 flex-1 truncate text-sm font-medium text-gray-800 dark:text-gray-100" title={loop.target_abs}>
            #{loop.id} {loop.target_label || loop.target_repo_rel}
          </span>
          {loop.parent_loop_id != null && (
            <span className="rounded bg-emerald-50 px-1.5 py-0.5 text-[10px] text-emerald-600 dark:bg-emerald-900/30 dark:text-emerald-300">
              承接自 #{loop.parent_loop_id}
            </span>
          )}
          {hasChild && (
            <span
              className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] text-gray-500 dark:bg-gray-800 dark:text-gray-400"
              title="本记录已派生下一阶段子记录，派生入口已禁用以避免血缘分叉"
            >
              已派生
            </span>
          )}
          {onTrack && (
            <button
              onClick={onTrack}
              className="flex items-center gap-1 rounded border border-gray-200 px-1.5 py-0.5 text-[11px] text-gray-600 hover:bg-gray-50 dark:border-gray-700 dark:text-gray-300 dark:hover:bg-gray-800"
              title="打开跟踪窗口，实时查看本记录两端会话（Claude / Codex）的消息"
            >
              <Activity size={11} />
              跟踪
            </button>
          )}
          {canMerge && (
            <button
              onClick={async () => {
                if (!onMergeWorktree || merging) return
                setMerging(true)
                setMergeMsg(null)
                try {
                  const text = await onMergeWorktree()
                  setMergeMsg({ ok: true, text })
                } catch (e) {
                  setMergeMsg({ ok: false, text: String(e) })
                } finally {
                  setMerging(false)
                }
              }}
              disabled={merging}
              className="flex items-center gap-1 rounded border border-violet-300 px-1.5 py-0.5 text-[11px] text-violet-700 hover:bg-violet-50 disabled:opacity-50 dark:border-violet-500/60 dark:text-violet-300 dark:hover:bg-violet-900/30"
              title="把本记录的 worktree（HEAD 提交）合并回主仓库当前分支；两侧工作树需干净"
            >
              <GitMerge size={11} />
              {merging ? '合并中…' : '合并 worktree'}
            </button>
          )}
        </div>
        <div className="mt-1.5 flex flex-wrap items-center gap-2 text-[11px] text-gray-500 dark:text-gray-400">
          <span>{loop.mode === 'design' ? '设计复核' : '实现复核'}</span>
          {loop.final_verdict && <span>· {FINAL_LABELS[loop.final_verdict] ?? loop.final_verdict}</span>}
          <span>· {loop.total_rounds}/{loop.max_rounds} 轮</span>
          {loop.attempts_count > 1 && (
            <span className="rounded bg-blue-100 px-1 text-[11px] font-medium text-blue-700 dark:bg-blue-900/40 dark:text-blue-300">
              × {loop.attempts_count}
            </span>
          )}
          {loop.last_phase && !isLive && (
            <span className="text-gray-400">· 上次停在 {PHASE_LABELS[loop.last_phase] ?? loop.last_phase}</span>
          )}
          {isLive && <span className="text-blue-600 dark:text-blue-400">· {liveText(liveProgress)}</span>}
        </div>

        {/* 配置内容（只读，对应上方新建表单的字段）。 */}
        <div className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[11px] text-gray-500 dark:text-gray-400">
          <span className="text-gray-400">目标</span>
          {loop.entry_kind === 'continuation' || !loop.target_repo_rel ? (
            <span className="text-gray-500 dark:text-gray-400 italic">（继续既有讨论，无指定 target）</span>
          ) : (
            <span className="truncate text-gray-600 dark:text-gray-300" title={loop.target_abs}>{loop.target_abs}</span>
          )}
          <span className="text-gray-400">仓库</span>
          <span className="truncate text-gray-600 dark:text-gray-300" title={loop.repo_root}>{loop.repo_root}</span>
          <span className="text-gray-400">Claude</span>
          <span className="truncate font-mono text-gray-600 dark:text-gray-300" title={loop.claude_session}>{shortId(loop.claude_session)}</span>
          <span className="text-gray-400">Codex</span>
          <span className="truncate font-mono text-gray-600 dark:text-gray-300" title={loop.codex_session}>{shortId(loop.codex_session)}</span>
          {loop.design_doc_path && (
            <>
              <span className="text-gray-400">规格依据</span>
              <span className="truncate text-gray-600 dark:text-gray-300" title={loop.design_doc_path}>
                {loop.design_doc_path}
              </span>
            </>
          )}
          {loop.seed_review_path && (
            <>
              <span className="text-gray-400">seed 文件</span>
              <span className="truncate text-gray-600 dark:text-gray-300" title={loop.seed_review_path}>
                {loop.seed_review_path}
              </span>
            </>
          )}
          {!loop.seed_review_path && loop.seed_review_inline_hash && (
            <>
              <span className="text-gray-400">seed 内联</span>
              <span className="truncate font-mono text-gray-600 dark:text-gray-300" title={loop.seed_review_inline_hash}>
                sm3:{loop.seed_review_inline_hash}
              </span>
            </>
          )}
        </div>

        {loop.worktree_path && (
          <div className="mt-1 truncate text-[11px] text-violet-500" title={loop.worktree_path}>
            worktree: {loop.worktree_path}
          </div>
        )}
        {mergeMsg && (
          <div
            className={`mt-1 rounded px-2 py-1 text-[11px] ${
              mergeMsg.ok
                ? 'bg-emerald-50 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-300'
                : 'bg-red-50 text-red-600 dark:bg-red-900/20 dark:text-red-400'
            }`}
          >
            {mergeMsg.text}
          </div>
        )}
        {loop.error && (
          <div className="mt-1 rounded bg-red-50 px-2 py-1 text-[11px] text-red-600 dark:bg-red-900/20 dark:text-red-400">
            {loop.error}
          </div>
        )}

        {/* ── 上下文操作区 ── */}
        {isLive ? (
          // 运行中：实时控制（逐步确认翻转 + 停止）。worktree 启动时确定，运行中只读。
          <div className="mt-2 flex flex-wrap items-center gap-3">
            <button
              onClick={onStop}
              className="flex items-center gap-1.5 rounded-md bg-red-600 px-3 py-1 text-xs text-white hover:bg-red-700"
            >
              <Square size={12} />
              停止
            </button>
            <label
              className="flex items-center gap-1.5 text-[11px] text-gray-600 dark:text-gray-300"
              title="运行中：切换会立即应用到当前循环（取消勾选=转全自动；撞到 ASK_USER 仍会停下问你）"
            >
              <input
                type="checkbox"
                checked={!liveAuto}
                onChange={e => onToggleAuto?.(!e.target.checked)}
              />
              逐步确认 {liveAuto ? '· 当前：全自动' : '· 当前：逐步'}
            </label>
            <label className="flex items-center gap-1.5 text-[11px] text-gray-400" title="worktree 模式在循环启动时确定，运行中不可更改">
              <input type="checkbox" checked={loop.use_worktree} disabled readOnly />
              <span className="flex items-center gap-0.5">
                <GitBranch size={10} className={loop.use_worktree ? 'text-violet-500' : 'text-gray-400'} />
                worktree 模式
              </span>
            </label>
          </div>
        ) : showImplement ? (
          // design 历史：阶段动作面板。当前记录只读，点击后派生出新的记录（实现 / 继续 design）。
          <div className="mt-2 flex flex-wrap items-center gap-2 border-t border-gray-100 pt-2 dark:border-gray-800">
            <span className="mr-1 text-xs font-medium text-gray-700 dark:text-gray-200">
              下一步：{passed ? '代码实现' : '继续 / 跳过文档复核直接开发'}
            </span>
            <StageToggle
              checked={freshAgent}
              onChange={setFreshAgent}
              icon={<Bot size={13} />}
              label="新 Agent"
              title="新建一个 Codex 会话承接下一阶段；不勾选则沿用当前 Codex 会话（继续按钮始终沿用旧会话）"
            />
            <StageToggle
              checked={useStageWorktree}
              onChange={setUseStageWorktree}
              icon={<GitBranch size={13} />}
              label="worktree"
              title="让 Claude 在独立 worktree 中实现，完成后 Codex 复核该 worktree"
            />
            <StageToggle
              checked={autoGo}
              onChange={setAutoGo}
              icon={<Zap size={13} />}
              label="GO"
              title="跳过逐步确认，自动在 Claude/Codex 间传递；遇到 ASK_USER 仍会停下"
            />
            <label className="flex h-8 items-center gap-1 rounded-md border border-gray-200 bg-white px-2 text-xs text-gray-500 dark:border-gray-700 dark:bg-gray-900/50 dark:text-gray-400">
              轮次
              <input
                type="number"
                min={1}
                max={20}
                value={implRounds}
                onChange={e => setImplRounds(Math.max(1, Number(e.target.value) || 1))}
                className="w-12 bg-transparent text-gray-700 outline-none disabled:opacity-50 dark:text-gray-200"
              />
            </label>
            {canContinue && (
              <button
                onClick={continueLoop}
                className="ml-auto flex h-8 items-center gap-1.5 rounded-md border border-blue-500 bg-white px-3 text-xs text-blue-600 hover:bg-blue-50 dark:border-blue-400 dark:bg-transparent dark:text-blue-300 dark:hover:bg-blue-900/30"
                title="沿用同一对 Claude / Codex 会话续跑（适用于 token 用完或达最大轮次后继续）"
              >
                <Play size={13} />
                继续 design 复核
              </button>
            )}
            <button
              onClick={startImpl}
              className={`${canContinue ? '' : 'ml-auto'} flex h-8 items-center gap-1.5 rounded-md px-3 text-xs text-white ${
                passed
                  ? 'bg-emerald-600 hover:bg-emerald-700'
                  : 'bg-amber-600 hover:bg-amber-700'
              }`}
              title={passed ? '基于通过复核的设计稿开始代码实现' : '跳过文档复核，直接进入代码实现阶段'}
            >
              <Play size={13} />
              {passed ? '开始代码实现' : '跳过、直接开发'}
            </button>
          </div>
        ) : canContinueReview ? (
          <div className="mt-2 flex flex-wrap items-center gap-2 border-t border-gray-100 pt-2 dark:border-gray-800">
            <span className="mr-1 text-xs font-medium text-gray-700 dark:text-gray-200">
              下一步：代码审核
            </span>
            <StageToggle
              checked={freshAgent}
              onChange={setFreshAgent}
              icon={<Bot size={13} />}
              label="新 Agent"
              title="新建一个 Codex 会话承接代码审核；不勾选则沿用当前 Codex 会话"
            />
            <StageToggle
              checked
              onChange={() => {}}
              icon={<GitBranch size={13} />}
              label="worktree"
              title="已检测到实现 worktree，代码审核将在该工作树中进行"
              disabled
            />
            <StageToggle
              checked={autoGo}
              onChange={setAutoGo}
              icon={<Zap size={13} />}
              label="GO"
              title="跳过逐步确认，自动在 Codex/Claude 间传递；遇到 ASK_USER 仍会停下"
            />
            <label className="flex h-8 items-center gap-1 rounded-md border border-gray-200 bg-white px-2 text-xs text-gray-500 dark:border-gray-700 dark:bg-gray-900/50 dark:text-gray-400">
              轮次
              <input
                type="number"
                min={1}
                max={20}
                value={implRounds}
                onChange={e => setImplRounds(Math.max(1, Number(e.target.value) || 1))}
                className="w-12 bg-transparent text-gray-700 outline-none dark:text-gray-200"
              />
            </label>
            {canContinue && (
              <button
                onClick={continueLoop}
                className="ml-auto flex h-8 items-center gap-1.5 rounded-md border border-blue-500 bg-white px-3 text-xs text-blue-600 hover:bg-blue-50 dark:border-blue-400 dark:bg-transparent dark:text-blue-300 dark:hover:bg-blue-900/30"
                title="沿用同一对 Claude / Codex 会话续跑（适用于 token 用完或达最大轮次后继续）"
              >
                <Play size={13} />
                继续
              </button>
            )}
            <button
              onClick={continueReview}
              className={`${canContinue ? '' : 'ml-auto'} flex h-8 items-center gap-1.5 rounded-md bg-emerald-600 px-3 text-xs text-white hover:bg-emerald-700`}
            >
              <Play size={13} />
              开始代码审核
            </button>
            <div className="basis-full truncate text-[11px] text-violet-500" title={loop.worktree_path ?? undefined}>
              worktree: {loop.worktree_path}
            </div>
          </div>
        ) : (
          // 其它历史记录（如实现复核）：只读展示本记录设置。
          <div className="mt-2 flex flex-wrap items-center gap-3">
            <label className="flex items-center gap-1.5 text-[11px] text-gray-400">
              <input type="checkbox" checked={loop.step_confirm} disabled readOnly />
              逐步确认
            </label>
            <label className="flex items-center gap-1.5 text-[11px] text-gray-400">
              <input type="checkbox" checked={loop.use_worktree} disabled readOnly />
              <span className="flex items-center gap-0.5">
                <GitBranch size={10} className={loop.use_worktree ? 'text-violet-500' : 'text-gray-400'} />
                worktree 模式
              </span>
            </label>
            {canContinue && (
              <>
                <label className="flex h-8 items-center gap-1 rounded-md border border-gray-200 bg-white px-2 text-xs text-gray-500 dark:border-gray-700 dark:bg-gray-900/50 dark:text-gray-400">
                  轮次
                  <input
                    type="number"
                    min={1}
                    max={20}
                    value={implRounds}
                    onChange={e => setImplRounds(Math.max(1, Number(e.target.value) || 1))}
                    className="w-12 bg-transparent text-gray-700 outline-none dark:text-gray-200"
                  />
                </label>
                <StageToggle
                  checked={autoGo}
                  onChange={setAutoGo}
                  icon={<Zap size={13} />}
                  label="GO"
                  title="跳过逐步确认；遇到 ASK_USER 仍会停下"
                />
                <button
                  onClick={continueLoop}
                  className="ml-auto flex h-8 items-center gap-1.5 rounded-md border border-blue-500 bg-white px-3 text-xs text-blue-600 hover:bg-blue-50 dark:border-blue-400 dark:bg-transparent dark:text-blue-300 dark:hover:bg-blue-900/30"
                  title="沿用同一对 Claude / Codex 会话续跑（适用于 token 用完或达最大轮次后继续）"
                >
                  <Play size={13} />
                  继续
                </button>
              </>
            )}
          </div>
        )}
      </div>

      <div className="min-h-0 flex-1">
        <LoopTranscript loopId={loop.id} messages={messages} loading={loadingMessages} />
      </div>
    </div>
  )
}
