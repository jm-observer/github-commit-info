import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

// ── 与后端 agent-session / codeloop 命令对齐的类型 ──────────────────────────────

export type Provider = 'codex' | 'claude'
export type SessionStatus = 'idle' | 'generating' | 'processing' | 'unknown'

export interface SessionSummary {
  provider: Provider
  id: string
  title: string
  /** 首条用户消息前若干字符——比会被改写的 AI 标题更稳定，用于人工对照/筛选。 */
  preview: string
  /** 会话工作目录（项目路径）；前端取末段作项目名。 */
  cwd: string
  status: SessionStatus
  updated_at: string
}

export interface SessionMessage {
  role: string
  text: string
  /** 可展开详情：thinking 正文 / tool_use 入参 / tool_result 返回体（无则缺省）。 */
  detail?: string
  timestamp: string
}

export interface MessagesPage {
  messages: SessionMessage[]
  cursor: number
}

export interface AskUser {
  question: string
  options?: string[]
}

/** 循环进度（codeloop://progress 事件载荷，字段随 phase 变化）。 */
export interface Progress {
  /** 所属循环 id（并发时前端据此路由事件到对应循环）。 */
  loop_id?: number
  phase?: string // starting | implementing | implemented | reviewed | revised | awaiting_input | awaiting_confirm | done | error
  round?: number
  verdict?: string // pass | needs_work | parse_failed
  final_verdict?: string // pass | max_rounds | aborted_timeout | aborted_parse | aborted_by_user | stopped_tracking
  total_rounds?: number
  seq?: number
  asked_by?: Provider
  question?: AskUser
  // 逐步确认门（phase === 'awaiting_confirm'）字段：
  direction?: string // codex_to_claude | claude_to_codex
  title?: string // 确认问句
  content?: string // 即将传递的文本全文
  error?: string
}

/** 一个运行中循环的状态快照（与后端 RunningSnapshot 对齐）。 */
export interface RunningSnapshot {
  loop_id: number
  progress: Progress | null
  /** 当前逐步确认开关（运行时可翻转）；false = 已转全自动。 */
  step_confirm: boolean
}

export type ReviewMode = 'design' | 'implementation'

export interface StartInput {
  claude: { session_id: string; cwd?: string }
  codex: { session_id: string; cwd?: string }
  target_path: string
  target_label?: string
  mode: ReviewMode
  max_rounds?: number
  wait_for_claude_idle?: boolean
  /** 逐步确认（手动）：每次跨会话传递前弹窗等用户拍板；默认 true。 */
  step_confirm?: boolean
  /** worktree 模式：让 Claude 自己用 git worktree + 子 agent 隔离实现，Codex 复核 worktree。 */
  use_worktree?: boolean
  /** 两阶段血缘：本循环承接自哪条记录（design→implementation）。 */
  parent_loop_id?: number
  /** 从已完成 worktree 继续复核：跳过 Claude 实现首步，直接进入 Codex 复核。 */
  resume_worktree_path?: string
  /** 首轮预热：哪些端已在外部（预览台）发过首轮说明块，循环首轮即跳过其 STANDING_BLOCK。 */
  established?: { codex?: boolean; claude?: boolean }
}

// ── 复核循环记录（持久化）──────────────────────────────────────────────────
//   字段 snake_case，与后端 db::LoopRow / LoopMessageRow 一致。

export type LoopStatus = 'running' | 'done' | 'failed' | 'aborted'

/** 一条复核循环记录。 */
export interface LoopRow {
  id: number
  created_at: string
  updated_at: string
  claude_session: string
  codex_session: string
  repo_root: string
  target_repo_rel: string
  target_abs: string
  target_label: string
  mode: ReviewMode
  max_rounds: number
  step_confirm: boolean
  use_worktree: boolean
  status: LoopStatus
  final_verdict?: string | null
  total_rounds: number
  worktree_path?: string | null
  error?: string | null
  /** 承接来源记录 id（design→implementation 血缘）。 */
  parent_loop_id?: number | null
}

/** 一条自检结果（与后端 CheckRow 对齐）。 */
export interface CheckRow {
  id: string
  label: string
  tier: 'passive' | 'version' | 'live'
  status: 'pass' | 'fail' | 'warn' | 'skipped'
  detail: string
  raw_excerpt?: string
}

/** 记录里的一条逐轮消息。 */
export interface LoopMessageRow {
  id: number
  loop_id: number
  ts: string
  round: number
  kind: 'codex_review' | 'claude_implement' | 'claude_revise' | 'system'
  verdict?: string | null
  content: string
}

export const CodeloopAPI = {
  listSessions: (limit = 30) => invoke<SessionSummary[]>('codeloop_list_sessions', { limit }),
  sessionMessages: (provider: Provider, sessionId: string, after: number) =>
    invoke<MessagesPage>('codeloop_session_messages', { provider, sessionId, after }),
  newCodexSession: (claudeSessionId: string) =>
    invoke<string>('codeloop_new_codex_session', { claudeSessionId }),
  /** 向单个会话发一轮（预览驱动台 / 首轮预热）；返回回复文本。真实调用 CLI。 */
  sendOne: (provider: Provider, sessionId: string, text: string) =>
    invoke<string>('codeloop_send_one', { provider, sessionId, text }),
  /** 启动一个复核循环，返回新建记录的 loop_id（并发：可同时跑多个，同一会话除外）。 */
  start: (input: StartInput) => invoke<number>('codeloop_start', { input }),
  /** 启动前自检：live=true 时跑实发往返合成探针（真实调用一次 CLI）。 */
  preflight: (input: StartInput, live: boolean) =>
    invoke<CheckRow[]>('codeloop_preflight', { input, live }),
  /** 所有运行中循环的状态快照（按 loop_id 区分）。 */
  status: () => invoke<RunningSnapshot[]>('codeloop_status'),
  answer: (loopId: number, seq: number, text: string) =>
    invoke<void>('codeloop_answer', { loopId, seq, text }),
  confirm: (loopId: number, seq: number, approve: boolean) =>
    invoke<void>('codeloop_confirm', { loopId, seq, approve }),
  /** 运行时翻转自动确认：enabled=true 转全自动（顺手放行挂起的确认门），false 恢复逐步确认。 */
  setAutoConfirm: (loopId: number, enabled: boolean) =>
    invoke<void>('codeloop_set_auto_confirm', { loopId, enabled }),
  stop: (loopId: number) => invoke<void>('codeloop_stop', { loopId }),
  // 记录列表 / 详情 / 删除
  listLoops: (limit = 50) => invoke<LoopRow[]>('codeloop_list_loops', { limit }),
  loopMessages: (loopId: number) => invoke<LoopMessageRow[]>('codeloop_loop_messages', { loopId }),
  deleteLoop: (loopId: number) => invoke<void>('codeloop_delete_loop', { loopId }),
  /** 把记录关联的 worktree 合并回主仓库当前分支。返回人类可读的状态文案。 */
  mergeWorktree: (loopId: number) => invoke<string>('codeloop_merge_worktree', { loopId }),
}

/** 订阅循环进度事件。返回值用于取消订阅。 */
export function onProgress(cb: (p: Progress) => void): Promise<UnlistenFn> {
  return listen<Progress>('codeloop_progress', e => cb(e.payload))
}
