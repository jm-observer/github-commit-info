import { useEffect, useRef, useState } from 'react'
import { Loader2, Send } from 'lucide-react'
import { CodeloopAPI, type Provider, type SessionMessage } from '../api/tauri-client'
import { MessageColumn } from './MessageColumn'

interface Props {
  claudeId: string
  codexId: string
  onClose: () => void
}

/**
 * 单会话预览 / 手动驱动台：查看某会话内容，并可输入一条消息直接发给它。
 *
 * ⚠️ 「发送」与循环同等权力（Codex workspace-write / Claude acceptEdits），是真发不是只读。
 * 典型用途：首轮预热——手动发establishing消息建立会话上下文，之后在新建表单勾选「此端已预热」，
 * 循环首轮即跳过重复的说明块。
 */
export function PreviewModal({ claudeId, codexId, onClose }: Props) {
  const [provider, setProvider] = useState<Provider>(claudeId ? 'claude' : 'codex')
  const [messages, setMessages] = useState<SessionMessage[]>([])
  const [input, setInput] = useState('')
  const [sending, setSending] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  const cursor = useRef(0)

  const sessionId = provider === 'claude' ? claudeId : codexId

  // 切换会话 → 重置并全量拉取。
  useEffect(() => {
    cursor.current = 0
    setMessages([])
    setErr(null)
    if (!sessionId) return
    let alive = true
    CodeloopAPI.sessionMessages(provider, sessionId, 0)
      .then(page => {
        if (!alive) return
        setMessages(page.messages)
        cursor.current = page.cursor
      })
      .catch(e => alive && setErr(String(e)))
    return () => {
      alive = false
    }
  }, [provider, sessionId])

  const pull = async () => {
    if (!sessionId) return
    try {
      const page = await CodeloopAPI.sessionMessages(provider, sessionId, cursor.current)
      if (page.messages.length) setMessages(m => [...m, ...page.messages])
      cursor.current = page.cursor
    } catch {
      /* 静默 */
    }
  }

  const send = async () => {
    const text = input.trim()
    if (!text || !sessionId || sending) return
    setSending(true)
    setErr(null)
    try {
      await CodeloopAPI.sendOne(provider, sessionId, text)
      setInput('')
      await pull()
    } catch (e) {
      setErr(String(e))
    } finally {
      setSending(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="flex h-[80vh] w-[720px] max-w-[94vw] flex-col rounded-lg bg-white p-4 shadow-xl dark:bg-gray-900">
        <div className="mb-3 flex items-center gap-2">
          <h2 className="flex-1 text-sm font-medium text-gray-800 dark:text-gray-100">预览 / 手动驱动台</h2>
          <div className="flex overflow-hidden rounded-md border border-gray-300 text-xs dark:border-gray-600">
            <button
              onClick={() => setProvider('claude')}
              disabled={!claudeId}
              className={`px-2.5 py-1 ${provider === 'claude' ? 'bg-blue-600 text-white' : 'text-gray-600 disabled:opacity-40 dark:text-gray-300'}`}
            >
              Claude
            </button>
            <button
              onClick={() => setProvider('codex')}
              disabled={!codexId}
              className={`px-2.5 py-1 ${provider === 'codex' ? 'bg-blue-600 text-white' : 'text-gray-600 disabled:opacity-40 dark:text-gray-300'}`}
            >
              Codex
            </button>
          </div>
          <button onClick={onClose} className="text-xs text-gray-400 hover:text-gray-600">
            关闭
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-auto rounded-md border border-gray-200 p-2 dark:border-gray-800">
          {sessionId ? (
            <MessageColumn title={provider} sessionId={sessionId} messages={messages} />
          ) : (
            <div className="flex h-full items-center justify-center text-xs text-gray-400">未选择该端会话</div>
          )}
        </div>

        {err && <div className="mt-2 text-xs text-red-500">{err}</div>}

        <div className="mt-2 text-[11px] text-amber-600 dark:text-amber-400">
          ⚠ 「发送」会真实调用一次 CLI（{provider} 真改文件）；可用于首轮预热后在新建表单勾「此端已预热」。
        </div>
        <div className="mt-1 flex items-end gap-2">
          <textarea
            value={input}
            onChange={e => setInput(e.target.value)}
            placeholder="输入要发给该会话的消息…（首轮预热可粘贴 establishing 提示词）"
            rows={2}
            disabled={!sessionId || sending}
            className="flex-1 resize-none rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 disabled:opacity-60 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          />
          <button
            onClick={send}
            disabled={!input.trim() || !sessionId || sending}
            className="flex items-center gap-1.5 rounded-md bg-blue-600 px-3 py-2 text-sm text-white hover:bg-blue-700 disabled:opacity-50"
          >
            {sending ? <Loader2 size={14} className="animate-spin" /> : <Send size={14} />}
            发送
          </button>
        </div>
      </div>
    </div>
  )
}
