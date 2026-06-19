import { useEffect, useRef, useState } from 'react'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  CodeloopAPI,
  onProgress,
  type EntryKind,
  type LoopMessageRow,
  type LoopRow,
  type Progress,
  type Provider,
  type ReviewMode,
  type SessionMessage,
  type SessionSummary,
} from './api/tauri-client'
import { SessionPairPicker } from './components/SessionPairPicker'
import { LoopStatusBar } from './components/LoopStatusBar'
import { AskUserModal } from './components/AskUserModal'
import { ConfirmGateModal } from './components/ConfirmGateModal'
import { LoopList } from './components/LoopList'
import { LoopDetail } from './components/LoopDetail'
import { TrackModal } from './components/TrackModal'
import { PreflightModal } from './components/PreflightModal'
import { PreviewModal } from './components/PreviewModal'
import { ImplementationModal } from './components/ImplementationModal'
import type { StartInput } from './api/tauri-client'

const POLL_MS = 1500

export default function CodeloopPage() {
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [loadingSessions, setLoadingSessions] = useState(false)
  const [sessionsErr, setSessionsErr] = useState<string | null>(null)

  const [claudeId, setClaudeId] = useState('')
  const [codexId, setCodexId] = useState('')
  const [creatingCodex, setCreatingCodex] = useState(false)

  // 双栏消息：cursor 用 ref（不触发渲染），messages 用 state。
  const cursors = useRef<Record<Provider, number>>({ codex: 0, claude: 0 })
  const [messages, setMessages] = useState<Record<Provider, SessionMessage[]>>({ codex: [], claude: [] })

  // 表单
  const [targetPath, setTargetPath] = useState('')
  // 入口种类（多入口设计 §3）：默认 doc_review。ReviewSeed 时有二级 mode 子选；
  // DocReview/Implement 时 mode 由入口决定（design / implementation）。
  const [entryKind, setEntryKind] = useState<EntryKind>('doc_review')
  // 仅 ReviewSeed 时由用户选；DocReview→design / Implement→implementation 由 entry 卡片自动写。
  const [seedMode, setSeedMode] = useState<ReviewMode>('design')
  // ReviewSeed 的 seed 输入：tab 切换文件路径 / 内联文本（两选一，相互清空）。
  const [seedTab, setSeedTab] = useState<'path' | 'inline'>('path')
  const [seedReviewPath, setSeedReviewPath] = useState('')
  const [seedReviewInline, setSeedReviewInline] = useState('')
  // ReviewSeed(mode=implementation) 可选规格依据文档。其它入口忽略。
  const [designDocPath, setDesignDocPath] = useState('')
  const [maxRounds, setMaxRounds] = useState(5)
  const [waitIdle, setWaitIdle] = useState(false)
  const [stepConfirm, setStepConfirm] = useState(true)
  // 多入口设计 §7：Implement 默认 use_worktree=true，其余默认 false。
  const [useWorktree, setUseWorktree] = useState(false)
  // 首轮预热：哪些端已在预览台外部建立（按 provider 分开）。
  const [estCodex, setEstCodex] = useState(false)
  const [estClaude, setEstClaude] = useState(false)

  // 入口选择 → 同步 mode 默认 / use_worktree 默认。
  const handlePickEntry = (next: EntryKind) => {
    setEntryKind(next)
    if (next === 'implement') setUseWorktree(true)
    // DocReview/Implement 时 seed 字段清空，避免后端校验拒绝。
    if (next !== 'review_seed') {
      setSeedReviewPath('')
      setSeedReviewInline('')
    }
    // 切到 review_seed 时不强行重置 seedMode（保留用户上次选择）。
  }

  // 由入口 + ReviewSeed 子选 推出最终 mode。
  const effectiveMode: ReviewMode =
    entryKind === 'doc_review'
      ? 'design'
      : entryKind === 'implement'
        ? 'implementation'
        : seedMode

  // 循环
  const [running, setRunning] = useState(false)
  const [progress, setProgress] = useState<Progress | null>(null)
  const [startErr, setStartErr] = useState<string | null>(null)
  const [answeredSeq, setAnsweredSeq] = useState(0)
  const [decidedSeq, setDecidedSeq] = useState(0)
  // 运行中是否已转全自动（= !step_confirm）；由 status 初始化、确认弹窗/状态条开关翻转。
  const [liveAuto, setLiveAuto] = useState(false)

  // 记录列表
  const [loops, setLoops] = useState<LoopRow[]>([])
  const [loadingLoops, setLoadingLoops] = useState(false)
  const [selectedLoopId, setSelectedLoopId] = useState<number | null>(null)
  // 选中记录的往返消息（详情面板）。
  const [loopMsgs, setLoopMsgs] = useState<LoopMessageRow[]>([])
  const [loadingLoopMsgs, setLoadingLoopMsgs] = useState(false)
  // 跟踪弹窗：展示所选两个会话的消息记录。
  const [showTrack, setShowTrack] = useState(false)
  // 环境自检弹窗。
  const [showPreflight, setShowPreflight] = useState(false)
  // 预览 / 手动驱动台弹窗。
  const [showPreview, setShowPreview] = useState(false)
  // 「开始实现」配置确认窗的源记录（非 null = 弹窗开启）。
  const [implSource, setImplSource] = useState<LoopRow | null>(null)

  // 由表单组装 StartInput（启动 / 自检共用）。多入口字段按 entry_kind 条件性附带：
  // 仅 ReviewSeed 才透传 seed_review_*；仅 ReviewSeed(impl) 才透传 design_doc_path。
  const buildInput = (): StartInput => {
    const base: StartInput = {
      claude: { session_id: claudeId },
      codex: { session_id: codexId },
      target_path: targetPath.trim(),
      mode: effectiveMode,
      max_rounds: maxRounds,
      wait_for_claude_idle: waitIdle,
      step_confirm: stepConfirm,
      use_worktree: useWorktree,
      established: { codex: estCodex, claude: estClaude },
      entry_kind: entryKind,
    }
    if (entryKind === 'review_seed') {
      if (seedTab === 'path' && seedReviewPath.trim()) {
        base.seed_review_path = seedReviewPath.trim()
      } else if (seedTab === 'inline' && seedReviewInline.trim()) {
        base.seed_review_inline = seedReviewInline
      }
      if (effectiveMode === 'implementation' && designDocPath.trim()) {
        base.design_doc_path = designDocPath.trim()
      }
    }
    return base
  }

  // ── 会话清单 ──────────────────────────────────────────────────────────────
  const refreshSessions = () => {
    setLoadingSessions(true)
    setSessionsErr(null)
    CodeloopAPI.listSessions(30)
      .then(setSessions)
      .catch(e => setSessionsErr(String(e)))
      .finally(() => setLoadingSessions(false))
  }
  useEffect(refreshSessions, [])

  // ── 复核记录列表 / 详情 ───────────────────────────────────────────────────
  const refreshLoops = () => {
    setLoadingLoops(true)
    CodeloopAPI.listLoops(50)
      .then(setLoops)
      .catch(() => {})
      .finally(() => setLoadingLoops(false))
  }
  useEffect(refreshLoops, [])

  // 循环结束（done/error）→ 刷新列表，让最新记录终态进列表；并重载选中记录的往返消息。
  useEffect(() => {
    if (progress?.phase === 'done' || progress?.phase === 'error') {
      refreshLoops()
      if (selectedLoopId != null) {
        CodeloopAPI.loopMessages(selectedLoopId).then(setLoopMsgs).catch(() => {})
      }
    }
  }, [progress?.phase])

  const handleDeleteLoop = async (id: number) => {
    try {
      await CodeloopAPI.deleteLoop(id)
    } catch {
      /* ignore */
    }
    if (selectedLoopId === id) setSelectedLoopId(null)
    refreshLoops()
  }

  // 点击记录：选中 + 加载该记录往返消息到右侧只读详情面板。**不回填新建表单**
  // （避免「看历史」污染「配新循环」）。顺便刷新列表，保证详情头部状态是最新的。
  const handleSelectLoop = (id: number) => {
    setSelectedLoopId(id)
    setLoadingLoopMsgs(true)
    setLoopMsgs([])
    refreshLoops()
    CodeloopAPI.loopMessages(id)
      .then(setLoopMsgs)
      .catch(() => {})
      .finally(() => setLoadingLoopMsgs(false))
  }

  // 内联：id → 项目名（cwd 末段名）。空 cwd / 未选 → 空串。用于同项目联动。
  const projectOf = (id: string): string => {
    if (!id) return ''
    const cwd = sessions.find(s => s.id === id)?.cwd || ''
    const parts = cwd.split(/[/\\]+/).filter(Boolean)
    return parts.length ? parts[parts.length - 1] : ''
  }
  const claudeProject = projectOf(claudeId)
  const codexProject = projectOf(codexId)
  // 两侧都选了但不在同一项目时弱提示（不拦截，启动仍由后端三方校验兜底）。
  const projectMismatch =
    !!claudeProject && !!codexProject && claudeProject !== codexProject

  const onPick = (provider: Provider, id: string) => {
    cursors.current[provider] = 0
    setMessages(m => ({ ...m, [provider]: [] }))
    if (provider === 'claude') setClaudeId(id)
    else setCodexId(id)
  }

  // 新建 Codex 会话：复用所选 Claude 会话的仓库目录，建好后刷新清单并自动选中。
  const handleNewCodex = async () => {
    if (!claudeId || creatingCodex) return
    setCreatingCodex(true)
    setSessionsErr(null)
    try {
      const newId = await CodeloopAPI.newCodexSession(claudeId)
      refreshSessions()
      onPick('codex', newId)
    } catch (e) {
      setSessionsErr(String(e))
    } finally {
      setCreatingCodex(false)
    }
  }

  // ── 双栏消息增量轮询 ─────────────────────────────────────────────────────
  useEffect(() => {
    let alive = true
    const pollSide = async (provider: Provider, id: string) => {
      if (!id) return
      try {
        const page = await CodeloopAPI.sessionMessages(provider, id, cursors.current[provider])
        if (!alive) return
        if (page.messages.length) {
          setMessages(m => ({ ...m, [provider]: [...m[provider], ...page.messages] }))
        }
        cursors.current[provider] = page.cursor
      } catch {
        /* 本地读取抖动：静默跳过本轮，不重置 cursor */
      }
    }
    const tick = () => {
      void pollSide('claude', claudeId)
      void pollSide('codex', codexId)
    }
    tick()
    const t = setInterval(tick, POLL_MS)
    return () => {
      alive = false
      clearInterval(t)
    }
  }, [claudeId, codexId])

  // ── 循环进度（event + 初始快照） ─────────────────────────────────────────
  useEffect(() => {
    let un: UnlistenFn | undefined
    onProgress(p => {
      setProgress(p)
      if (p.phase === 'done' || p.phase === 'error') setRunning(false)
    }).then(f => {
      un = f
    })
    CodeloopAPI.status()
      .then(s => {
        setRunning(s.running)
        if (s.progress) setProgress(s.progress)
        setLiveAuto(!s.step_confirm)
      })
      .catch(() => {})
    return () => un?.()
  }, [])

  // ── 启动 / 应答 ──────────────────────────────────────────────────────────
  // ReviewSeed 必须提供 seed_review_path 或 seed_review_inline（按当前 tab 取）。
  const seedReady =
    entryKind !== 'review_seed' ||
    (seedTab === 'path' ? !!seedReviewPath.trim() : !!seedReviewInline.trim())
  const canStart = !!claudeId && !!codexId && !!targetPath.trim() && seedReady && !running
  const startWith = async (input: StartInput) => {
    setStartErr(null)
    try {
      await CodeloopAPI.start(input)
      setRunning(true)
      setProgress({ phase: 'starting' })
      setLiveAuto(!input.step_confirm)
      refreshLoops()
    } catch (e) {
      setStartErr(String(e))
    }
  }
  const handleStart = () => startWith(buildInput())
  // 「开始实现」：弹配置确认窗 → 确认后以 implementation 模式启动并关联血缘。
  const handleStartImplementation = async (input: StartInput) => {
    setImplSource(null)
    await startWith(input)
  }
  const handleStop = async () => {
    try {
      await CodeloopAPI.stop()
    } catch {
      /* ignore */
    }
    setRunning(false)
  }
  const handleAnswer = async (text: string) => {
    const seq = progress?.seq
    if (seq == null) return
    try {
      await CodeloopAPI.answer(seq, text)
      setAnsweredSeq(seq)
    } catch (e) {
      setStartErr(String(e))
    }
  }

  const handleDecide = async (approve: boolean, auto = false) => {
    const seq = progress?.seq
    if (seq == null) return
    setDecidedSeq(seq) // 乐观关窗，避免重复点击
    try {
      await CodeloopAPI.confirm(seq, approve)
      // 勾选「确认后转自动」：放行当前步后关掉逐步确认。
      if (approve && auto) {
        await CodeloopAPI.setAutoConfirm(true)
        setLiveAuto(true)
      }
    } catch (e) {
      setStartErr(String(e))
    }
  }

  // 运行中随时翻转自动确认（状态条入口）。
  const handleToggleAuto = async (enabled: boolean) => {
    setLiveAuto(enabled) // 乐观
    try {
      await CodeloopAPI.setAutoConfirm(enabled)
    } catch (e) {
      setLiveAuto(!enabled) // 回滚
      setStartErr(String(e))
    }
  }

  // ASK_USER 弹窗：进入 awaiting_input 且该 seq 未答过。
  const showAsk =
    progress?.phase === 'awaiting_input' &&
    progress.seq != null &&
    progress.seq > answeredSeq &&
    !!progress.question

  // 逐步确认弹窗：运行中、进入 awaiting_confirm 且该 seq 未拍板过。
  const showConfirm =
    running &&
    progress?.phase === 'awaiting_confirm' &&
    progress.seq != null &&
    progress.seq > decidedSeq

  return (
    <div className="flex h-full flex-col gap-3">
      <div>
        <h1 className="text-xl font-semibold">复核循环</h1>
        <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
          关联一对 Codex / Claude Code 会话，驱动「复核 ↔ 修订」往复。默认逐步确认：每次跨会话传递前弹窗等你拍板（本机直跑，无需额外进程）。
        </p>
      </div>

      <SessionPairPicker
        sessions={sessions}
        claudeId={claudeId}
        codexId={codexId}
        onPick={onPick}
        onRefresh={refreshSessions}
        loading={loadingSessions}
        onNewCodex={handleNewCodex}
        creatingCodex={creatingCodex}
        claudeProject={claudeProject}
        codexProject={codexProject}
      />
      {projectMismatch && (
        <div className="text-xs text-amber-600 dark:text-amber-400">
          Claude（{claudeProject}）与 Codex（{codexProject}）不在同一项目，启动时会校验失败。
        </div>
      )}
      {sessionsErr && (
        <div className="rounded-md bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
          会话清单加载失败：{sessionsErr}（确认本机 codex / claude 已产生过会话）
        </div>
      )}

      {/* 入口选择卡 + ReviewSeed 动态字段（多入口设计 §7）。 */}
      <EntryPicker
        entryKind={entryKind}
        onPick={handlePickEntry}
        seedMode={seedMode}
        setSeedMode={setSeedMode}
        seedTab={seedTab}
        setSeedTab={setSeedTab}
        seedReviewPath={seedReviewPath}
        setSeedReviewPath={setSeedReviewPath}
        seedReviewInline={seedReviewInline}
        setSeedReviewInline={setSeedReviewInline}
        designDocPath={designDocPath}
        setDesignDocPath={setDesignDocPath}
        disabled={running}
      />

      <LoopStatusBar
        targetPath={targetPath}
        setTargetPath={setTargetPath}
        entryKind={entryKind}
        mode={effectiveMode}
        maxRounds={maxRounds}
        setMaxRounds={setMaxRounds}
        waitIdle={waitIdle}
        setWaitIdle={setWaitIdle}
        stepConfirm={stepConfirm}
        setStepConfirm={setStepConfirm}
        useWorktree={useWorktree}
        setUseWorktree={setUseWorktree}
        estCodex={estCodex}
        setEstCodex={setEstCodex}
        estClaude={estClaude}
        setEstClaude={setEstClaude}
        onPreview={() => setShowPreview(true)}
        running={running}
        canStart={canStart}
        onStart={handleStart}
        onStop={handleStop}
        onTrack={() => setShowTrack(true)}
        canTrack={!!claudeId && !!codexId}
        onPreflight={() => setShowPreflight(true)}
        canPreflight={!!claudeId && !!codexId && !!targetPath.trim()}
        progress={progress}
        liveAuto={liveAuto}
        onToggleAuto={handleToggleAuto}
      />
      {startErr && (
        <div className="rounded-md bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
          {startErr}
        </div>
      )}

      {/* 下方左右分栏：左=历史记录列表，右=选中记录只读详情（状态 + 往返消息 + 上下文操作）。 */}
      <div className="flex min-h-0 flex-1 gap-3">
        <div className="w-80 shrink-0">
          <LoopList
            loops={loops}
            selectedId={selectedLoopId}
            onSelect={handleSelectLoop}
            onRefresh={refreshLoops}
            onDelete={handleDeleteLoop}
            loading={loadingLoops}
          />
        </div>
        <div className="min-w-0 flex-1">
          <LoopDetail
            loop={loops.find(l => l.id === selectedLoopId) ?? null}
            messages={loopMsgs}
            loadingMessages={loadingLoopMsgs}
            onStartImplementation={setImplSource}
            running={running}
            liveAuto={liveAuto}
            onToggleAuto={handleToggleAuto}
          />
        </div>
      </div>

      {showAsk && progress?.question && (
        <AskUserModal
          question={progress.question}
          seq={progress.seq!}
          askedBy={progress.asked_by}
          onAnswer={handleAnswer}
        />
      )}

      {showConfirm && (
        <ConfirmGateModal
          seq={progress!.seq!}
          direction={progress!.direction}
          title={progress!.title}
          content={progress!.content}
          onApprove={auto => handleDecide(true, auto)}
          onReject={() => handleDecide(false)}
        />
      )}

      {showTrack && (
        <TrackModal
          claudeId={claudeId}
          codexId={codexId}
          claudeMessages={messages.claude}
          codexMessages={messages.codex}
          onClose={() => setShowTrack(false)}
        />
      )}

      {showPreflight && (
        <PreflightModal input={buildInput()} onClose={() => setShowPreflight(false)} />
      )}

      {implSource && (
        <ImplementationModal
          source={implSource}
          onStart={handleStartImplementation}
          onClose={() => setImplSource(null)}
        />
      )}

      {showPreview && (
        <PreviewModal claudeId={claudeId} codexId={codexId} onClose={() => setShowPreview(false)} />
      )}
    </div>
  )
}

// ── 入口选择卡 sub-section（多入口设计 §7）─────────────────────────────────────
// 三选一卡片 + ReviewSeed 时的二级 mode 子选 / seed tab / 可选规格依据文档。
// 选中即更新 page 顶部状态，buildInput 据此组装请求。

interface EntryPickerProps {
  entryKind: EntryKind
  onPick: (next: EntryKind) => void
  seedMode: ReviewMode
  setSeedMode: (v: ReviewMode) => void
  seedTab: 'path' | 'inline'
  setSeedTab: (v: 'path' | 'inline') => void
  seedReviewPath: string
  setSeedReviewPath: (v: string) => void
  seedReviewInline: string
  setSeedReviewInline: (v: string) => void
  designDocPath: string
  setDesignDocPath: (v: string) => void
  disabled: boolean
}

const ENTRY_CARDS: { kind: EntryKind; icon: string; title: string; hint: string }[] = [
  { kind: 'doc_review', icon: '📄', title: '从文档复核开始', hint: '现有默认：Codex 复核文档 ↔ Claude 修订。' },
  { kind: 'implement', icon: '🛠', title: '从实现开始', hint: '文档已定稿：Claude 按文档实现 → 复核环。' },
  { kind: 'review_seed', icon: '📝', title: '从既有复核意见开始', hint: '已有 review 产物：跳过 Codex 首轮直接修订。' },
]

function EntryPicker(props: EntryPickerProps) {
  const {
    entryKind,
    onPick,
    seedMode,
    setSeedMode,
    seedTab,
    setSeedTab,
    seedReviewPath,
    setSeedReviewPath,
    seedReviewInline,
    setSeedReviewInline,
    designDocPath,
    setDesignDocPath,
    disabled,
  } = props
  const seedActive = entryKind === 'review_seed'

  return (
    <div className="flex flex-col gap-2 rounded-md border border-gray-200 bg-white p-3 dark:border-gray-800 dark:bg-gray-900">
      <div className="text-xs font-medium text-gray-600 dark:text-gray-300">起点（入口）</div>
      <div className="grid grid-cols-1 gap-2 md:grid-cols-3">
        {ENTRY_CARDS.map(c => {
          const selected = c.kind === entryKind
          return (
            <button
              key={c.kind}
              type="button"
              disabled={disabled}
              onClick={() => onPick(c.kind)}
              className={`flex flex-col items-start gap-1 rounded-md border px-3 py-2 text-left transition disabled:cursor-not-allowed disabled:opacity-60 ${
                selected
                  ? 'border-blue-400 bg-blue-50 dark:border-blue-500 dark:bg-blue-900/30'
                  : 'border-gray-200 hover:bg-gray-50 dark:border-gray-700 dark:hover:bg-gray-800/50'
              }`}
            >
              <span className="text-sm">
                <span className="mr-1">{c.icon}</span>
                <span className="font-medium text-gray-800 dark:text-gray-100">{c.title}</span>
              </span>
              <span className="text-[11px] text-gray-500 dark:text-gray-400">{c.hint}</span>
            </button>
          )
        })}
      </div>

      {seedActive && (
        <div className="mt-1 flex flex-col gap-2 rounded-md border border-amber-200 bg-yellow-50 p-3 dark:border-amber-700/50 dark:bg-amber-900/20">
          {/* ReviewSeed 二级 mode 子选 */}
          <div className="flex items-center gap-3 text-xs">
            <span className="text-gray-600 dark:text-gray-300">修订对象</span>
            <label className="flex items-center gap-1 text-gray-700 dark:text-gray-200">
              <input
                type="radio"
                name="seed-mode"
                value="design"
                checked={seedMode === 'design'}
                disabled={disabled}
                onChange={() => setSeedMode('design')}
              />
              文档（design）
            </label>
            <label className="flex items-center gap-1 text-gray-700 dark:text-gray-200">
              <input
                type="radio"
                name="seed-mode"
                value="implementation"
                checked={seedMode === 'implementation'}
                disabled={disabled}
                onChange={() => setSeedMode('implementation')}
              />
              代码（implementation）
            </label>
          </div>

          {/* seed tab：文件路径 / 直接粘贴文本（二选一，相互清空） */}
          <div className="flex items-center gap-2 text-xs">
            <button
              type="button"
              disabled={disabled}
              onClick={() => {
                setSeedTab('path')
                setSeedReviewInline('')
              }}
              className={`rounded px-2 py-0.5 ${
                seedTab === 'path'
                  ? 'bg-amber-200 text-amber-800 dark:bg-amber-800/60 dark:text-amber-100'
                  : 'text-gray-600 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-800'
              }`}
            >
              文件路径
            </button>
            <button
              type="button"
              disabled={disabled}
              onClick={() => {
                setSeedTab('inline')
                setSeedReviewPath('')
              }}
              className={`rounded px-2 py-0.5 ${
                seedTab === 'inline'
                  ? 'bg-amber-200 text-amber-800 dark:bg-amber-800/60 dark:text-amber-100'
                  : 'text-gray-600 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-800'
              }`}
            >
              直接粘贴文本
            </button>
          </div>

          {seedTab === 'path' ? (
            <input
              type="text"
              value={seedReviewPath}
              onChange={e => setSeedReviewPath(e.target.value)}
              disabled={disabled}
              placeholder="docs/review-2026-06-19.md（待 Claude 修订的 seed 复核意见文件）"
              className="rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-amber-400 disabled:opacity-60 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
            />
          ) : (
            <textarea
              value={seedReviewInline}
              onChange={e => setSeedReviewInline(e.target.value)}
              disabled={disabled}
              rows={5}
              placeholder="直接粘贴 review 文本（非 Codex 输出；将作为 round 1 喂给 Claude 修订）"
              className="rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-amber-400 disabled:opacity-60 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
            />
          )}

          {seedMode === 'implementation' && (
            <div className="flex flex-col gap-1">
              <label className="text-xs text-gray-600 dark:text-gray-300">
                规格依据文档（可选；同仓内相对/绝对路径，仅 implementation 子模式可用）
              </label>
              <input
                type="text"
                value={designDocPath}
                onChange={e => setDesignDocPath(e.target.value)}
                disabled={disabled}
                placeholder="docs/spec.md"
                className="rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-amber-400 disabled:opacity-60 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
              />
            </div>
          )}
        </div>
      )}
    </div>
  )
}
