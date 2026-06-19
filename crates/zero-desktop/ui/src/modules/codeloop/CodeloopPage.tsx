import { useEffect, useRef, useState } from 'react'
import type { UnlistenFn } from '@tauri-apps/api/event'
import {
  CodeloopAPI,
  onProgress,
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
import { LoopDetail, type StageStartOptions } from './components/LoopDetail'
import { TrackModal } from './components/TrackModal'
import { PreflightModal } from './components/PreflightModal'
import { PreviewModal } from './components/PreviewModal'
import type { StartInput } from './api/tauri-client'

const POLL_MS = 1500

/** 一个运行中循环的前端态：最近进度 + 是否已转全自动。 */
interface LiveLoop {
  progress: Progress
  liveAuto: boolean
}

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

  // 新建表单
  const [targetPath, setTargetPath] = useState('')
  const [mode, setMode] = useState<ReviewMode>('design')
  const [maxRounds, setMaxRounds] = useState(10)
  const [waitIdle, setWaitIdle] = useState(false)
  const [stepConfirm, setStepConfirm] = useState(true)
  const [useWorktree, setUseWorktree] = useState(false)
  // 首轮预热：哪些端已在预览台外部建立（按 provider 分开）。
  const [estCodex, setEstCodex] = useState(false)
  const [estClaude, setEstClaude] = useState(false)

  // 并发循环：按 loop_id 索引每个运行中循环的进度 + 是否已转全自动。
  const [runningLoops, setRunningLoops] = useState<Record<number, LiveLoop>>({})
  const [startErr, setStartErr] = useState<string | null>(null)
  // 各循环已应答 / 已拍板的 seq（避免弹窗重复触发；seq 在循环内单调）。
  const [answered, setAnswered] = useState<Record<number, number>>({})
  const [decided, setDecided] = useState<Record<number, number>>({})

  // 记录列表
  const [loops, setLoops] = useState<LoopRow[]>([])
  const [loadingLoops, setLoadingLoops] = useState(false)
  const [selectedLoopId, setSelectedLoopId] = useState<number | null>(null)
  // onProgress 闭包在 mount 时定型，用 ref 读最新选中 id（避免 stale closure）。
  const selectedRef = useRef<number | null>(null)
  useEffect(() => {
    selectedRef.current = selectedLoopId
  }, [selectedLoopId])
  // 选中记录的往返消息（详情面板）。
  const [loopMsgs, setLoopMsgs] = useState<LoopMessageRow[]>([])
  const [loadingLoopMsgs, setLoadingLoopMsgs] = useState(false)
  // 跟踪弹窗：展示所选两个会话的消息记录。
  const [showTrack, setShowTrack] = useState(false)
  // 详情面板的跟踪弹窗：按选中记录的两端 session 拉消息，与新建表单互不污染。
  const [detailTrack, setDetailTrack] = useState<{ claude: string; codex: string } | null>(null)
  const detailCursors = useRef<Record<Provider, number>>({ codex: 0, claude: 0 })
  const [detailTrackMessages, setDetailTrackMessages] = useState<Record<Provider, SessionMessage[]>>({ codex: [], claude: [] })
  // 环境自检弹窗。
  const [showPreflight, setShowPreflight] = useState(false)
  // 预览 / 手动驱动台弹窗。
  const [showPreview, setShowPreview] = useState(false)

  // 由表单组装 StartInput（启动 / 自检共用）。
  const buildInput = (): StartInput => ({
    claude: { session_id: claudeId },
    codex: { session_id: codexId },
    target_path: targetPath.trim(),
    mode,
    max_rounds: maxRounds,
    wait_for_claude_idle: waitIdle,
    step_confirm: stepConfirm,
    use_worktree: useWorktree,
    established: { codex: estCodex, claude: estClaude },
  })

  // 启动后清空新建表单，让用户立即配下一个循环（含会话选择——同一会话不可并发占用）。
  const resetForm = () => {
    setTargetPath('')
    setMode('design')
    setMaxRounds(10)
    setWaitIdle(false)
    setStepConfirm(true)
    setUseWorktree(false)
    setEstCodex(false)
    setEstClaude(false)
    setClaudeId('')
    setCodexId('')
    cursors.current = { codex: 0, claude: 0 }
    setMessages({ codex: [], claude: [] })
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

  const handleDeleteLoop = async (id: number) => {
    try {
      await CodeloopAPI.deleteLoop(id)
    } catch {
      /* ignore */
    }
    if (selectedLoopId === id) setSelectedLoopId(null)
    refreshLoops()
  }

  // 点击记录：选中 + 加载该记录往返消息到右侧详情面板。**不回填新建表单**
  // （避免「看历史」污染「配新循环」）。顺便刷新列表，保证详情头部状态是最新的。
  const handleSelectLoop = (id: number) => {
    setSelectedLoopId(id)
    setLoadingLoopMsgs(true)
    setLoopMsgs([])
    refreshLoops()
    CodeloopAPI.loopMessages(id)
      .then(msgs => {
        setLoopMsgs(msgs)
        refreshLoops()
      })
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

  // ── 详情跟踪：按 detailTrack 拉两端会话消息（与上方新建表单的轮询独立） ──
  useEffect(() => {
    if (!detailTrack) return
    let alive = true
    detailCursors.current = { codex: 0, claude: 0 }
    setDetailTrackMessages({ codex: [], claude: [] })
    const pollSide = async (provider: Provider, id: string) => {
      if (!id) return
      try {
        const page = await CodeloopAPI.sessionMessages(provider, id, detailCursors.current[provider])
        if (!alive) return
        if (page.messages.length) {
          setDetailTrackMessages(m => ({ ...m, [provider]: [...m[provider], ...page.messages] }))
        }
        detailCursors.current[provider] = page.cursor
      } catch {
        /* 抖动：跳过本轮 */
      }
    }
    const tick = () => {
      void pollSide('claude', detailTrack.claude)
      void pollSide('codex', detailTrack.codex)
    }
    tick()
    const t = setInterval(tick, POLL_MS)
    return () => {
      alive = false
      clearInterval(t)
    }
  }, [detailTrack?.claude, detailTrack?.codex])

  // ── 并发循环进度（event + 初始快照） ─────────────────────────────────────
  useEffect(() => {
    let un: UnlistenFn | undefined
    onProgress(p => {
      const id = p.loop_id
      if (id == null) return
      const ended = p.phase === 'done' || p.phase === 'error'
      setRunningLoops(prev => {
        if (ended) {
          const next = { ...prev }
          delete next[id]
          return next
        }
        return { ...prev, [id]: { progress: p, liveAuto: prev[id]?.liveAuto ?? false } }
      })
      if (id === selectedRef.current) {
        CodeloopAPI.loopMessages(id).then(setLoopMsgs).catch(() => {})
      }
      if (ended) {
        refreshLoops()
      }
    }).then(f => {
      un = f
    })
    // mount 时重建并发态（应用重开 / 切页回来）。
    CodeloopAPI.status()
      .then(list => {
        const map: Record<number, LiveLoop> = {}
        for (const s of list) map[s.loop_id] = { progress: s.progress ?? {}, liveAuto: !s.step_confirm }
        setRunningLoops(map)
      })
      .catch(() => {})
    return () => un?.()
  }, [])

  // ── 启动 / 应答 ──────────────────────────────────────────────────────────
  const canStart = !!claudeId && !!codexId && !!targetPath.trim()
  const startWith = async (input: StartInput) => {
    setStartErr(null)
    try {
      const id = await CodeloopAPI.start(input)
      setRunningLoops(prev => ({
        ...prev,
        [id]: { progress: { phase: 'starting' }, liveAuto: !input.step_confirm },
      }))
      resetForm()
      setSelectedLoopId(id)
      setLoopMsgs([])
      refreshLoops()
    } catch (e) {
      setStartErr(String(e))
    }
  }
  const handleStart = () => startWith(buildInput())
  // 阶段动作：可选择先新建 Codex Agent/session，再启动派生记录。
  const startStageWith = async (input: StartInput, options?: StageStartOptions) => {
    let next = input
    if (options?.freshCodex) {
      setCreatingCodex(true)
      try {
        const newId = await CodeloopAPI.newCodexSession(input.claude.session_id)
        next = { ...input, codex: { session_id: newId } }
        refreshSessions()
      } catch (e) {
        setStartErr(String(e))
        return
      } finally {
        setCreatingCodex(false)
      }
    }
    await startWith(next)
  }
  // 「开始实现」：LoopDetail 内联配好 StartInput（承接血缘已带 parent_loop_id）后直接启动。
  const handleStartImplementation = (input: StartInput, options?: StageStartOptions) =>
    startStageWith(input, options)
  const handleContinueReview = (input: StartInput, options?: StageStartOptions) =>
    startStageWith(input, options)
  const handleContinue = (input: StartInput, options?: StageStartOptions) =>
    startStageWith(input, options)

  const handleStop = async (loopId: number) => {
    try {
      await CodeloopAPI.stop(loopId)
    } catch {
      /* ignore */
    }
    setRunningLoops(prev => {
      const next = { ...prev }
      delete next[loopId]
      return next
    })
    if (loopId === selectedRef.current) {
      CodeloopAPI.loopMessages(loopId).then(setLoopMsgs).catch(() => {})
    }
    refreshLoops()
  }

  const handleAnswer = async (loopId: number, text: string) => {
    const seq = runningLoops[loopId]?.progress?.seq
    if (seq == null) return
    try {
      await CodeloopAPI.answer(loopId, seq, text)
      setAnswered(prev => ({ ...prev, [loopId]: seq }))
    } catch (e) {
      setStartErr(String(e))
    }
  }

  const handleDecide = async (loopId: number, approve: boolean, auto = false) => {
    const seq = runningLoops[loopId]?.progress?.seq
    if (seq == null) return
    setDecided(prev => ({ ...prev, [loopId]: seq })) // 乐观关窗，避免重复点击
    try {
      await CodeloopAPI.confirm(loopId, seq, approve)
      // 勾选「确认后转自动」：放行当前步后关掉逐步确认。
      if (approve && auto) {
        await CodeloopAPI.setAutoConfirm(loopId, true)
        setRunningLoops(prev =>
          prev[loopId] ? { ...prev, [loopId]: { ...prev[loopId], liveAuto: true } } : prev,
        )
      }
    } catch (e) {
      setStartErr(String(e))
    }
  }

  // 运行中随时翻转自动确认（详情面板入口）。
  const handleToggleAuto = async (loopId: number, enabled: boolean) => {
    setRunningLoops(prev =>
      prev[loopId] ? { ...prev, [loopId]: { ...prev[loopId], liveAuto: enabled } } : prev,
    ) // 乐观
    try {
      await CodeloopAPI.setAutoConfirm(loopId, enabled)
    } catch (e) {
      setRunningLoops(prev =>
        prev[loopId] ? { ...prev, [loopId]: { ...prev[loopId], liveAuto: !enabled } } : prev,
      ) // 回滚
      setStartErr(String(e))
    }
  }

  // 详情面板对应的记录 + 其运行态（若在跑）。
  const detailLoop = loops.find(l => l.id === selectedLoopId) ?? null
  const detailLive = detailLoop ? runningLoops[detailLoop.id] : undefined
  // 是否已派生下一阶段子记录（用于禁用详情面板的派生入口）。
  const detailHasChild = detailLoop
    ? loops.some(l => l.parent_loop_id === detailLoop.id)
    : false

  // 列表标记：运行中 id / 需关注（等待作答或确认）id。
  const liveIds = new Set(Object.keys(runningLoops).map(Number))
  const attentionIds = new Set(
    Object.entries(runningLoops)
      .filter(([, v]) => v.progress?.phase === 'awaiting_input' || v.progress?.phase === 'awaiting_confirm')
      .map(([k]) => Number(k)),
  )

  // 弹窗按「选中循环」路由：只展示当前选中循环的待答 / 待确认。其余需关注的循环在列表里有标记。
  const selId = selectedLoopId
  const selProg = selId != null ? runningLoops[selId]?.progress : undefined
  const showAsk =
    selId != null &&
    selProg?.phase === 'awaiting_input' &&
    selProg.seq != null &&
    selProg.seq > (answered[selId] ?? 0) &&
    !!selProg.question

  const showConfirm =
    selId != null &&
    selProg?.phase === 'awaiting_confirm' &&
    selProg.seq != null &&
    selProg.seq > (decided[selId] ?? 0)

  return (
    <div className="flex h-full flex-col gap-3">
      <div>
        <h1 className="text-xl font-semibold">复核循环</h1>
        <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
          关联一对 Codex / Claude Code 会话，驱动「复核 ↔ 修订」往复。启动后表单即清空，可继续配下一个循环（多个循环可同时跑，同一会话除外）。运行中循环在下方记录里管理（停止 / 逐步确认）。
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

      <LoopStatusBar
        targetPath={targetPath}
        setTargetPath={setTargetPath}
        mode={mode}
        setMode={setMode}
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
        canStart={canStart}
        onStart={handleStart}
        onTrack={() => setShowTrack(true)}
        canTrack={!!claudeId && !!codexId}
        onPreflight={() => setShowPreflight(true)}
        canPreflight={!!claudeId && !!codexId && !!targetPath.trim()}
        liveCount={liveIds.size}
      />
      {startErr && (
        <div className="rounded-md bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
          {startErr}
        </div>
      )}

      {/* 下方左右分栏：左=历史记录列表，右=选中记录详情（状态 + 往返消息 + 上下文操作）。 */}
      <div className="flex min-h-0 flex-1 gap-3">
        <div className="w-80 shrink-0">
          <LoopList
            loops={loops}
            selectedId={selectedLoopId}
            onSelect={handleSelectLoop}
            onRefresh={refreshLoops}
            onDelete={handleDeleteLoop}
            loading={loadingLoops}
            liveIds={liveIds}
            attentionIds={attentionIds}
          />
        </div>
        <div className="min-w-0 flex-1">
          <LoopDetail
            loop={detailLoop}
            messages={loopMsgs}
            loadingMessages={loadingLoopMsgs}
            onStartImplementation={handleStartImplementation}
            onContinueReview={handleContinueReview}
            onContinue={handleContinue}
            isLive={!!detailLive}
            liveProgress={detailLive?.progress ?? null}
            liveAuto={detailLive?.liveAuto ?? false}
            onToggleAuto={enabled => detailLoop && handleToggleAuto(detailLoop.id, enabled)}
            onStop={() => detailLoop && handleStop(detailLoop.id)}
            onTrack={
              detailLoop
                ? () =>
                    setDetailTrack({
                      claude: detailLoop.claude_session,
                      codex: detailLoop.codex_session,
                    })
                : undefined
            }
            onMergeWorktree={
              detailLoop
                ? async () => {
                    const msg = await CodeloopAPI.mergeWorktree(detailLoop.id)
                    refreshLoops()
                    return msg
                  }
                : undefined
            }
            hasChild={detailHasChild}
          />
        </div>
      </div>

      {showAsk && selProg?.question && (
        <AskUserModal
          question={selProg.question}
          seq={selProg.seq!}
          askedBy={selProg.asked_by}
          onAnswer={text => handleAnswer(selId!, text)}
        />
      )}

      {showConfirm && (
        <ConfirmGateModal
          seq={selProg!.seq!}
          direction={selProg!.direction}
          title={selProg!.title}
          content={selProg!.content}
          onApprove={auto => handleDecide(selId!, true, auto)}
          onReject={() => handleDecide(selId!, false)}
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

      {detailTrack && (
        <TrackModal
          claudeId={detailTrack.claude}
          codexId={detailTrack.codex}
          claudeMessages={detailTrackMessages.claude}
          codexMessages={detailTrackMessages.codex}
          onClose={() => setDetailTrack(null)}
        />
      )}

      {showPreflight && (
        <PreflightModal input={buildInput()} onClose={() => setShowPreflight(false)} />
      )}

      {showPreview && (
        <PreviewModal claudeId={claudeId} codexId={codexId} onClose={() => setShowPreview(false)} />
      )}
    </div>
  )
}
