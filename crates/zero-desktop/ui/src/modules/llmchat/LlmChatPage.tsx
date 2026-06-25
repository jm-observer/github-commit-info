/**
 * LlmChatPage — 大模型会话。
 *
 * 两个视图（顶部 tab 切换）：
 * 1. 对话测试（交互）：跟公共大模型多轮聊天；首条消息 → `llm_create_chat`，之后
 *    `llm_chat_send`。每次对话作为一个 chat_test session 落库，可在「会话记录」回看。
 * 2. 会话记录（只读）：列出全部 session（对话测试 + 业务调用 douyin_refine / chat_summary），
 *    点开只读回看完整对话内容。
 *
 * 全部经 llm_* Tauri 命令代理到 G10 toolkit-server 的 `/api/web/llm/*`。
 */

import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Send, MessageCircle, History, Plus, XCircle, RefreshCw } from 'lucide-react'

// ── 类型 ────────────────────────────────────────────────────────────────────

interface ChatMessage {
  id: string
  seq: number
  role: 'system' | 'user' | 'assistant'
  content: string
  meta?: Record<string, unknown>
  created_at: string
}

interface SessionSummary {
  id: string
  kind: string
  title: string
  model: string | null
  prompt_name: string | null
  status: string
  metadata: Record<string, unknown>
  created_at: string
  updated_at: string
}

interface SessionDetail extends SessionSummary {
  messages: ChatMessage[]
}

const KIND_LABEL: Record<string, string> = {
  chat_test: '对话测试',
  douyin_refine: '抖音整理',
  chat_summary: '对话总结',
  agent: 'Agent',
}

function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind
}

function kindBadgeCls(kind: string): string {
  switch (kind) {
    case 'chat_test': return 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-300'
    case 'douyin_refine': return 'bg-pink-100 text-pink-700 dark:bg-pink-900/30 dark:text-pink-300'
    case 'chat_summary': return 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-300'
    default: return 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-300'
  }
}

function errMsg(e: unknown): string {
  return typeof e === 'string' ? e : ((e as any)?.message ?? String(e))
}

// ── 消息气泡 ──────────────────────────────────────────────────────────────────

function MessageBubble({ m }: { m: ChatMessage }) {
  const mine = m.role === 'user'
  return (
    <div className={mine ? 'flex justify-end' : 'flex justify-start'}>
      <div
        className={[
          'max-w-[80%] whitespace-pre-wrap break-words rounded-2xl px-3.5 py-2 text-sm leading-relaxed',
          mine
            ? 'bg-blue-500 text-white'
            : m.role === 'system'
              ? 'bg-amber-50 text-amber-800 dark:bg-amber-900/20 dark:text-amber-300'
              : 'bg-gray-100 text-gray-800 dark:bg-gray-800 dark:text-gray-100',
        ].join(' ')}
      >
        {m.content}
      </div>
    </div>
  )
}

// ── 对话测试（交互） ──────────────────────────────────────────────────────────

function ChatTestView() {
  const [sessionId, setSessionId] = useState<string | null>(null)
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
  const [model, setModel] = useState('')
  const [error, setError] = useState<string | null>(null)
  const scrollRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: 'smooth' })
  }, [messages, loading])

  const send = async () => {
    const text = input.trim()
    if (!text || loading) return
    setError(null)
    setLoading(true)
    // 乐观插入用户消息（后端落库后会以权威结果覆盖）。
    const optimistic: ChatMessage = {
      id: `tmp-${Date.now()}`, seq: messages.length, role: 'user', content: text, created_at: '',
    }
    setMessages(prev => [...prev, optimistic])
    setInput('')
    try {
      if (!sessionId) {
        const r = await invoke<{ id: string; model: string; messages: ChatMessage[] }>(
          'llm_create_chat', { message: text },
        )
        setSessionId(r.id)
        setModel(r.model ?? '')
        setMessages(r.messages ?? [])
      } else {
        const r = await invoke<{ message: ChatMessage; model: string }>(
          'llm_chat_send', { id: sessionId, message: text },
        )
        setModel(r.model ?? model)
        // 把乐观消息替换为权威（user 已落库）+ 追加 assistant 回复。
        setMessages(prev => {
          const withoutTmp = prev.filter(m => !m.id.startsWith('tmp-'))
          const userMsg: ChatMessage = {
            ...optimistic, id: `${sessionId}-u-${withoutTmp.length}`, created_at: r.message.created_at,
          }
          return [...withoutTmp, userMsg, r.message]
        })
      }
    } catch (e) {
      setError(errMsg(e))
      // 失败回滚乐观消息。
      setMessages(prev => prev.filter(m => !m.id.startsWith('tmp-')))
      setInput(text)
    } finally {
      setLoading(false)
    }
  }

  const reset = () => {
    setSessionId(null)
    setMessages([])
    setInput('')
    setError(null)
    setModel('')
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter 发送，Shift+Enter 换行。
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void send()
    }
  }

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between pb-3">
        <span className="text-xs text-gray-500 dark:text-gray-400">
          与公共大模型多轮对话{model ? ` · ${model}` : ''}
        </span>
        <button
          type="button"
          onClick={reset}
          disabled={loading || (!sessionId && messages.length === 0)}
          className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-xs hover:bg-gray-100 disabled:opacity-60 dark:border-gray-600 dark:hover:bg-gray-800"
        >
          <Plus size={13} />新对话
        </button>
      </div>

      <div
        ref={scrollRef}
        className="flex-1 space-y-3 overflow-auto rounded-md border border-gray-200 bg-gray-50/50 p-4 dark:border-gray-700 dark:bg-gray-900/40"
      >
        {messages.length === 0 && !loading && (
          <div className="flex h-full items-center justify-center text-sm text-gray-400">
            输入消息开始对话
          </div>
        )}
        {messages.map(m => <MessageBubble key={m.id} m={m} />)}
        {loading && (
          <div className="flex justify-start">
            <div className="rounded-2xl bg-gray-100 px-3.5 py-2 text-sm text-gray-400 dark:bg-gray-800">
              回复中…
            </div>
          </div>
        )}
      </div>

      {error && (
        <div className="mt-2 flex items-start gap-2 rounded-md bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
          <XCircle size={13} className="mt-0.5 flex-shrink-0" />
          <span className="break-words">{error}</span>
        </div>
      )}

      <div className="mt-3 flex items-end gap-2">
        <textarea
          value={input}
          onChange={e => setInput(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="输入消息（Enter 发送，Shift+Enter 换行）…"
          rows={2}
          className="flex-1 resize-y rounded-md border border-gray-300 bg-white px-3 py-2 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
        />
        <button
          type="button"
          onClick={() => void send()}
          disabled={loading || !input.trim()}
          className="flex items-center gap-2 rounded-md bg-blue-500 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-600 disabled:opacity-60"
        >
          <Send size={15} />{loading ? '发送中…' : '发送'}
        </button>
      </div>
    </div>
  )
}

// ── 会话记录（只读） ──────────────────────────────────────────────────────────

const FILTERS: { label: string; origin: string | null }[] = [
  { label: '全部', origin: null },
  { label: '对话测试', origin: 'chat_test' },
  { label: '抖音整理', origin: 'douyin_refine' },
  { label: '对话总结', origin: 'chat_summary' },
]

function RecordsView() {
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [origin, setOrigin] = useState<string | null>(null)
  const [selected, setSelected] = useState<SessionDetail | null>(null)
  const [loadingList, setLoadingList] = useState(false)
  const [loadingDetail, setLoadingDetail] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const loadList = async (o: string | null) => {
    setLoadingList(true)
    setError(null)
    try {
      const r = await invoke<{ sessions: SessionSummary[] }>('llm_list_sessions', {
        origin: o ?? undefined, limit: 100,
      })
      setSessions(r.sessions ?? [])
    } catch (e) {
      setError(errMsg(e))
    } finally {
      setLoadingList(false)
    }
  }
  useEffect(() => { void loadList(origin) }, [origin])

  const openSession = async (id: string) => {
    setLoadingDetail(true)
    try {
      const r = await invoke<SessionDetail>('llm_get_session', { id })
      setSelected(r)
    } catch (e) {
      setError(errMsg(e))
    } finally {
      setLoadingDetail(false)
    }
  }

  return (
    <div className="flex h-full gap-4">
      {/* 左：列表 */}
      <div className="flex w-72 flex-shrink-0 flex-col">
        <div className="flex flex-wrap items-center gap-1.5 pb-2">
          {FILTERS.map(f => (
            <button
              key={f.label}
              type="button"
              onClick={() => setOrigin(f.origin)}
              className={[
                'rounded-full px-2.5 py-1 text-xs transition-colors',
                origin === f.origin
                  ? 'bg-blue-500 text-white'
                  : 'bg-gray-100 text-gray-600 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-300 dark:hover:bg-gray-700',
              ].join(' ')}
            >
              {f.label}
            </button>
          ))}
          <button
            type="button"
            onClick={() => void loadList(origin)}
            title="刷新"
            className="ml-auto rounded-md p-1 text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800"
          >
            <RefreshCw size={13} />
          </button>
        </div>

        <div className="flex-1 space-y-1 overflow-auto">
          {loadingList && <div className="px-2 py-4 text-xs text-gray-400">加载中…</div>}
          {!loadingList && sessions.length === 0 && (
            <div className="px-2 py-4 text-xs text-gray-400">暂无会话记录</div>
          )}
          {sessions.map(s => (
            <button
              key={s.id}
              type="button"
              onClick={() => void openSession(s.id)}
              className={[
                'flex w-full flex-col gap-1 rounded-md border px-2.5 py-2 text-left transition-colors',
                selected?.id === s.id
                  ? 'border-blue-300 bg-blue-50 dark:border-blue-700 dark:bg-blue-900/20'
                  : 'border-gray-200 hover:bg-gray-50 dark:border-gray-700 dark:hover:bg-gray-800/50',
              ].join(' ')}
            >
              <div className="flex items-center gap-1.5">
                <span className={`rounded px-1.5 py-0.5 text-[10px] ${kindBadgeCls(s.kind)}`}>
                  {kindLabel(s.kind)}
                </span>
                {s.status === 'error' && (
                  <span className="rounded bg-red-100 px-1.5 py-0.5 text-[10px] text-red-600 dark:bg-red-900/30 dark:text-red-400">
                    失败
                  </span>
                )}
              </div>
              <span className="truncate text-xs text-gray-700 dark:text-gray-200">
                {s.title || '(无标题)'}
              </span>
              <span className="text-[10px] text-gray-400">{s.created_at}</span>
            </button>
          ))}
        </div>
      </div>

      {/* 右：详情（只读） */}
      <div className="flex flex-1 flex-col rounded-md border border-gray-200 dark:border-gray-700">
        {error && (
          <div className="m-3 flex items-start gap-2 rounded-md bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
            <XCircle size={13} className="mt-0.5 flex-shrink-0" />
            <span className="break-words">{error}</span>
          </div>
        )}
        {!selected && !loadingDetail && (
          <div className="flex h-full items-center justify-center text-sm text-gray-400">
            选择左侧一条会话查看完整对话
          </div>
        )}
        {loadingDetail && (
          <div className="flex h-full items-center justify-center text-sm text-gray-400">加载中…</div>
        )}
        {selected && !loadingDetail && (
          <>
            <div className="border-b border-gray-200 px-4 py-3 dark:border-gray-700">
              <div className="flex items-center gap-2">
                <span className={`rounded px-1.5 py-0.5 text-[10px] ${kindBadgeCls(selected.kind)}`}>
                  {kindLabel(selected.kind)}
                </span>
                <h2 className="truncate text-sm font-medium text-gray-700 dark:text-gray-200">
                  {selected.title || '(无标题)'}
                </h2>
              </div>
              <div className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 text-[10px] text-gray-400">
                {selected.model && <span>模型 {selected.model}</span>}
                {typeof selected.metadata?.aweme_id === 'string' && (
                  <span>aweme {selected.metadata.aweme_id}</span>
                )}
                <span>{selected.created_at}</span>
              </div>
            </div>
            <div className="flex-1 space-y-3 overflow-auto p-4">
              {selected.messages.map(m => <MessageBubble key={m.id} m={m} />)}
            </div>
          </>
        )}
      </div>
    </div>
  )
}

// ── 主页面 ────────────────────────────────────────────────────────────────────

type Tab = 'chat' | 'records'

export default function LlmChatPage() {
  const [tab, setTab] = useState<Tab>('chat')

  const tabBtn = (key: Tab, label: string, Icon: typeof MessageCircle) => (
    <button
      type="button"
      onClick={() => setTab(key)}
      className={[
        'flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition-colors',
        tab === key
          ? 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300'
          : 'text-gray-600 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-800',
      ].join(' ')}
    >
      <Icon size={15} />{label}
    </button>
  )

  return (
    // 占满内容区高度，便于聊天/记录区内部滚动。
    <div className="flex h-[calc(100vh-7rem)] flex-col">
      <div className="flex items-center justify-between pb-3">
        <div>
          <h1 className="text-xl font-semibold">大模型会话</h1>
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
            与公共大模型对话测试，并回看各功能产生的会话记录。连接配置在「设置 → 大模型」。
          </p>
        </div>
        <div className="flex items-center gap-1.5">
          {tabBtn('chat', '对话测试', MessageCircle)}
          {tabBtn('records', '会话记录', History)}
        </div>
      </div>

      <div className="min-h-0 flex-1">
        {tab === 'chat' ? <ChatTestView /> : <RecordsView />}
      </div>
    </div>
  )
}
