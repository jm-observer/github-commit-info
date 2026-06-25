/**
 * LlmSection — 设置页「大模型」配置区。
 *
 * 两块：
 * 1. 连接配置：base_url / model / api_key（写 G10 toolkit-server 的 llm_config 表，DB 优先于
 *    环境变量）+ Ping 连通性自测。
 * 2. 可配提示词：列出内置 + DB 覆盖，逐条编辑 / 保存 / 重置为内置默认。
 *
 * 全部经 llm_* Tauri 命令代理到 `{g10_base}/api/web/llm/*`。
 */

import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { invoke } from '@tauri-apps/api/core'
import { Save, CheckCircle, XCircle, Zap, RotateCcw, ChevronDown, ChevronRight, MessagesSquare } from 'lucide-react'

type Feedback = { kind: 'ok' | 'err'; msg: string } | null

interface LlmConfig {
  source: 'db' | 'env' | 'none'
  db_configured: boolean
  base_url: string
  model: string
  has_api_key: boolean
}

interface PromptItem {
  name: string
  description: string
  version: string
  placeholders: string[]
  source: 'db' | 'builtin'
  modified: boolean
  has_builtin: boolean
  text: string
}

function sourceLabel(source: string): string {
  switch (source) {
    case 'db': return '已保存（控制台）'
    case 'env': return '来自环境变量'
    default: return '未配置'
  }
}

// ── 连接配置 ──────────────────────────────────────────────────────────────────

function ConfigSubsection() {
  const [baseUrl, setBaseUrl] = useState('')
  const [model, setModel] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [hasApiKey, setHasApiKey] = useState(false)
  const [source, setSource] = useState('none')
  const [feedback, setFeedback] = useState<Feedback>(null)
  const [loading, setLoading] = useState(false)
  const [pinging, setPinging] = useState(false)

  const show = (kind: 'ok' | 'err', msg: string) => {
    setFeedback({ kind, msg })
    setTimeout(() => setFeedback(null), 4000)
  }

  const load = async () => {
    try {
      const c = await invoke<LlmConfig>('llm_get_config')
      setBaseUrl(c.base_url ?? '')
      setModel(c.model ?? '')
      setHasApiKey(c.has_api_key)
      setSource(c.source)
    } catch (e: any) {
      show('err', '加载配置失败：' + (typeof e === 'string' ? e : String(e)))
    }
  }
  useEffect(() => { void load() }, [])

  const handleSave = async () => {
    if (!baseUrl.trim() || !model.trim()) {
      show('err', 'base_url 与 model 不能为空')
      return
    }
    setLoading(true)
    try {
      // api_key 语义：留空 = 保持原值（不传）；有值 = 设置。清空请用「清除 Key」。
      const args: Record<string, unknown> = { baseUrl: baseUrl.trim(), model: model.trim() }
      if (apiKey.trim()) args.apiKey = apiKey.trim()
      await invoke('llm_put_config', args)
      setApiKey('')
      await load()
      show('ok', '大模型配置已保存')
    } catch (e: any) {
      show('err', '保存失败：' + (typeof e === 'string' ? e : String(e)))
    } finally {
      setLoading(false)
    }
  }

  const handleClearKey = async () => {
    setLoading(true)
    try {
      await invoke('llm_put_config', { baseUrl: baseUrl.trim(), model: model.trim(), apiKey: '' })
      setApiKey('')
      await load()
      show('ok', 'API Key 已清除')
    } catch (e: any) {
      show('err', '清除失败：' + (typeof e === 'string' ? e : String(e)))
    } finally {
      setLoading(false)
    }
  }

  const handlePing = async () => {
    setPinging(true)
    try {
      const r = await invoke<{ ok: boolean; model: string; reply: string }>('llm_ping')
      show('ok', `连通正常（${r.model}）：${r.reply}`)
    } catch (e: any) {
      show('err', typeof e === 'string' ? e : String(e))
    } finally {
      setPinging(false)
    }
  }

  const inputCls =
    'rounded-md border border-gray-300 bg-white px-3 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100'

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <span className="text-xs text-gray-500 dark:text-gray-400">当前来源：</span>
        <span className="rounded bg-gray-100 px-2 py-0.5 text-xs text-gray-600 dark:bg-gray-800 dark:text-gray-300">
          {sourceLabel(source)}
        </span>
      </div>

      {feedback && (
        <div className={[
          'flex items-start gap-2 rounded-md px-3 py-2 text-xs',
          feedback.kind === 'ok'
            ? 'bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400'
            : 'bg-red-50 text-red-600 dark:bg-red-900/20 dark:text-red-400',
        ].join(' ')}>
          {feedback.kind === 'ok' ? <CheckCircle size={13} className="mt-0.5" /> : <XCircle size={13} className="mt-0.5" />}
          <span className="break-words">{feedback.msg}</span>
        </div>
      )}

      <div className="flex flex-col gap-2">
        <label className="text-xs text-gray-500 dark:text-gray-400">OpenAI 兼容 base URL</label>
        <input type="text" value={baseUrl} onChange={e => setBaseUrl(e.target.value)}
          placeholder="http://127.0.0.1:8000/v1" className={inputCls} />
      </div>
      <div className="flex flex-col gap-2">
        <label className="text-xs text-gray-500 dark:text-gray-400">模型名</label>
        <input type="text" value={model} onChange={e => setModel(e.target.value)}
          placeholder="如 qwen2.5-instruct" className={inputCls} />
      </div>
      <div className="flex flex-col gap-2">
        <label className="text-xs text-gray-500 dark:text-gray-400">
          API Key（可选）{hasApiKey ? ' · 已设置，留空保持不变' : ''}
        </label>
        <input type="password" value={apiKey} onChange={e => setApiKey(e.target.value)}
          placeholder={hasApiKey ? '已设置（留空保持不变）' : '留空表示不鉴权'} className={inputCls} />
      </div>

      <div className="flex items-center gap-2">
        <button type="button" disabled={loading} onClick={() => void handleSave()}
          className="flex items-center gap-2 rounded-md bg-blue-500 px-4 py-1.5 text-sm font-medium text-white hover:bg-blue-600 disabled:opacity-60">
          <Save size={14} />{loading ? '保存中…' : '保存配置'}
        </button>
        <button type="button" disabled={pinging} onClick={() => void handlePing()}
          className="flex items-center gap-2 rounded-md border border-gray-300 px-4 py-1.5 text-sm hover:bg-gray-100 disabled:opacity-60 dark:border-gray-600 dark:hover:bg-gray-800">
          <Zap size={14} />{pinging ? '测试中…' : '连通测试'}
        </button>
        {hasApiKey && (
          <button type="button" disabled={loading} onClick={() => void handleClearKey()}
            className="rounded-md px-3 py-1.5 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800">
            清除 Key
          </button>
        )}
      </div>
    </div>
  )
}

// ── 提示词 ──────────────────────────────────────────────────────────────────

function PromptRow({ p, onChanged }: { p: PromptItem; onChanged: () => void }) {
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState(p.text)
  const [busy, setBusy] = useState(false)
  const [feedback, setFeedback] = useState<Feedback>(null)

  useEffect(() => { setDraft(p.text) }, [p.text])

  const show = (kind: 'ok' | 'err', msg: string) => {
    setFeedback({ kind, msg })
    setTimeout(() => setFeedback(null), 3000)
  }

  const save = async () => {
    if (!draft.trim()) { show('err', '提示词不能为空'); return }
    setBusy(true)
    try {
      await invoke('llm_put_prompt', { name: p.name, text: draft })
      show('ok', '已保存')
      onChanged()
    } catch (e: any) {
      show('err', typeof e === 'string' ? e : String(e))
    } finally { setBusy(false) }
  }

  const reset = async () => {
    setBusy(true)
    try {
      await invoke('llm_reset_prompt', { name: p.name })
      show('ok', '已重置为内置默认')
      onChanged()
    } catch (e: any) {
      show('err', typeof e === 'string' ? e : String(e))
    } finally { setBusy(false) }
  }

  return (
    <div className="rounded-md border border-gray-200 dark:border-gray-700">
      <button type="button" onClick={() => setOpen(o => !o)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm hover:bg-gray-50 dark:hover:bg-gray-800/50">
        {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        <span className="font-mono text-xs">{p.name}</span>
        {p.modified && (
          <span className="rounded bg-amber-100 px-1.5 py-0.5 text-[10px] text-amber-700 dark:bg-amber-900/30 dark:text-amber-400">
            已修改
          </span>
        )}
        <span className="ml-auto truncate text-xs text-gray-400">{p.description}</span>
      </button>

      {open && (
        <div className="flex flex-col gap-2 border-t border-gray-200 p-3 dark:border-gray-700">
          {p.placeholders.length > 0 && (
            <div className="text-xs text-gray-500 dark:text-gray-400">
              占位符：{p.placeholders.map(ph => <code key={ph} className="mx-0.5 rounded bg-gray-100 px-1 dark:bg-gray-800">{ph}</code>)}
            </div>
          )}
          <textarea value={draft} onChange={e => setDraft(e.target.value)} rows={8}
            className="w-full resize-y rounded-md border border-gray-300 bg-white px-3 py-2 font-mono text-xs outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100" />
          {feedback && (
            <div className={feedback.kind === 'ok' ? 'text-xs text-green-600 dark:text-green-400' : 'text-xs text-red-600 dark:text-red-400'}>
              {feedback.msg}
            </div>
          )}
          <div className="flex items-center gap-2">
            <button type="button" disabled={busy} onClick={() => void save()}
              className="flex items-center gap-1.5 rounded-md bg-blue-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-600 disabled:opacity-60">
              <Save size={12} />保存
            </button>
            {p.source === 'db' && (
              <button type="button" disabled={busy} onClick={() => void reset()}
                className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-xs hover:bg-gray-100 disabled:opacity-60 dark:border-gray-600 dark:hover:bg-gray-800">
                <RotateCcw size={12} />重置为内置默认
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

function PromptsSubsection() {
  const [prompts, setPrompts] = useState<PromptItem[]>([])
  const [error, setError] = useState<string | null>(null)

  const load = async () => {
    try {
      const r = await invoke<{ prompts: PromptItem[] }>('llm_list_prompts')
      setPrompts(r.prompts ?? [])
      setError(null)
    } catch (e: any) {
      setError(typeof e === 'string' ? e : String(e))
    }
  }
  useEffect(() => { void load() }, [])

  return (
    <div className="flex flex-col gap-2">
      <label className="text-xs text-gray-500 dark:text-gray-400">
        可配提示词（DB 覆盖优先于内置默认；删除覆盖即恢复内置）
      </label>
      {error && <div className="text-xs text-red-600 dark:text-red-400">{error}</div>}
      {prompts.map(p => <PromptRow key={p.name} p={p} onChanged={() => void load()} />)}
      {!error && prompts.length === 0 && (
        <div className="text-xs text-gray-400">加载中…（需先在上方配置好 G10 地址）</div>
      )}
    </div>
  )
}

// ── 主区 ──────────────────────────────────────────────────────────────────

export default function LlmSection() {
  const navigate = useNavigate()
  return (
    <section className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-medium text-gray-600 dark:text-gray-400">大模型</h2>
        <button
          type="button"
          onClick={() => navigate('/llmchat')}
          className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-xs hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800"
        >
          <MessagesSquare size={13} />对话测试 / 会话记录
        </button>
      </div>
      <ConfigSubsection />
      <PromptsSubsection />
    </section>
  )
}
