/**
 * SettingsPage — 统一设置页（阶段 4 扩充）。
 *
 * 新增：
 * - G10 配置区（调 cookie_get_app_settings / cookie_save_app_settings）。
 * - 英语模块区（嵌入 EnvConfig 组件，管理 customer_id）。
 */

import { useEffect, useState, useCallback } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Save, CheckCircle, XCircle, RefreshCw, Wifi, Globe } from 'lucide-react'
import EnvConfig from '../english/components/EnvConfig'
import LlmSection from './LlmSection'
import { Button } from '../speech/components/ui/Button'

// ── 主题 ─────────────────────────────────────────────────────────────────────

function getStoredTheme(): 'light' | 'dark' {
  return (localStorage.getItem('theme') as 'light' | 'dark') ?? 'light'
}

function applyTheme(theme: 'light' | 'dark') {
  if (theme === 'dark') document.documentElement.classList.add('dark')
  else document.documentElement.classList.remove('dark')
  localStorage.setItem('theme', theme)
}

// ── G10 配置（局域网/外网双地址） ─────────────────────────────────────────────

type NetMode = 'auto' | 'lan' | 'wan'

interface AppSettings {
  schema?: number
  mode: NetMode
  lan_host: string
  wan_host: string
  g10_token?: string | null
}

interface NetStatus {
  mode: NetMode
  picked: NetMode
  g10_base: string
  asr_url: string
  configured: boolean
  reachable: boolean | null
}

const MODE_LABEL: Record<NetMode, string> = {
  auto: '自动（推荐）',
  lan: '强制局域网',
  wan: '强制外网',
}

const inputCls =
  'rounded-md border border-gray-300 bg-white px-3 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100'

function HostField(props: {
  title: string
  icon: React.ReactNode
  hint: string
  value: string
  onChange: (v: string) => void
  placeholder: string
}) {
  const { title, icon, hint, value, onChange, placeholder } = props
  return (
    <div className="flex flex-col gap-2 rounded-md border border-gray-200 p-3 dark:border-gray-700">
      <div className="flex items-center gap-2 text-xs font-medium text-gray-600 dark:text-gray-300">
        {icon}
        {title}
      </div>
      <input
        type="text"
        value={value}
        onChange={e => onChange(e.target.value)}
        placeholder={placeholder}
        className={inputCls}
      />
      <p className="text-xs text-gray-400">{hint}</p>
    </div>
  )
}

function StatusBar({ status, onReprobe, reprobing }: { status: NetStatus | null; onReprobe: () => void; reprobing: boolean }) {
  if (!status) return null
  const isLan = status.picked === 'lan'
  const pickedLabel = isLan ? '局域网' : '外网'
  const dot = status.reachable === true ? '✓ 已连通'
    : status.reachable === false ? '✗ 不可达'
    : status.configured ? '' : '未配置'
  const tone = !status.configured
    ? 'bg-amber-50 text-amber-700 dark:bg-amber-900/20 dark:text-amber-400'
    : status.reachable === false
      ? 'bg-red-50 text-red-600 dark:bg-red-900/20 dark:text-red-400'
      : 'bg-blue-50 text-blue-700 dark:bg-blue-900/20 dark:text-blue-300'
  return (
    <div className={['flex items-center justify-between gap-2 rounded-md px-3 py-2 text-xs', tone].join(' ')}>
      <span className="flex items-center gap-2">
        {isLan ? <Wifi size={13} /> : <Globe size={13} />}
        {status.configured
          ? <>当前生效：<b>{pickedLabel}</b> · {status.g10_base} {dot}</>
          : <>未配置任何地址，请填写下方局域网/外网地址</>}
      </span>
      <button
        onClick={onReprobe}
        disabled={reprobing}
        className="flex items-center gap-1 rounded px-2 py-0.5 hover:bg-black/5 dark:hover:bg-white/10"
        title="清缓存后重新探测当前网络"
      >
        <RefreshCw size={12} className={reprobing ? 'animate-spin' : ''} />
        重新探测
      </button>
    </div>
  )
}

function G10ConfigSection() {
  const [mode, setMode] = useState<NetMode>('auto')
  const [lanHost, setLanHost] = useState('')
  const [wanHost, setWanHost] = useState('')
  const [g10Token, setG10Token] = useState('')
  const [feedback, setFeedback] = useState<{ kind: 'ok' | 'err'; msg: string } | null>(null)
  const [loading, setLoading] = useState(false)
  const [status, setStatus] = useState<NetStatus | null>(null)
  const [reprobing, setReprobing] = useState(false)

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await invoke<NetStatus>('net_resolve_status'))
    } catch (err) {
      console.error('[SettingsPage] 读取网络状态失败:', err)
    }
  }, [])

  useEffect(() => {
    void invoke<AppSettings>('cookie_get_app_settings').then(s => {
      setMode(s.mode ?? 'auto')
      setLanHost(s.lan_host ?? '')
      setWanHost(s.wan_host ?? '')
      setG10Token(s.g10_token ?? '')
    }).catch(err => console.error('[SettingsPage] 加载 G10 配置失败:', err))
    void refreshStatus()
  }, [refreshStatus])

  const showFeedback = (kind: 'ok' | 'err', msg: string) => {
    setFeedback({ kind, msg })
    setTimeout(() => setFeedback(null), 3000)
  }

  const handleSave = async () => {
    setLoading(true)
    try {
      const settingsData: AppSettings = {
        schema: 3,
        mode,
        lan_host: lanHost.trim(),
        wan_host: wanHost.trim(),
        g10_token: g10Token.trim() || null,
      }
      await invoke('cookie_save_app_settings', { settingsData })
      showFeedback('ok', 'G10 配置已保存')
      await refreshStatus()
    } catch (err: any) {
      showFeedback('err', '保存失败: ' + (err?.message ?? String(err)))
    } finally {
      setLoading(false)
    }
  }

  const handleReprobe = async () => {
    setReprobing(true)
    try {
      setStatus(await invoke<NetStatus>('net_reprobe'))
    } catch (err) {
      console.error('[SettingsPage] 重新探测失败:', err)
    } finally {
      setReprobing(false)
    }
  }

  return (
    <section className="flex flex-col gap-3">
      <h2 className="text-sm font-medium text-gray-600 dark:text-gray-400">G10 配置（局域网 / 外网）</h2>

      {feedback && (
        <div className={[
          'flex items-center gap-2 rounded-md px-3 py-2 text-xs',
          feedback.kind === 'ok'
            ? 'bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400'
            : 'bg-red-50 text-red-600 dark:bg-red-900/20 dark:text-red-400'
        ].join(' ')}>
          {feedback.kind === 'ok' ? <CheckCircle size={13} /> : <XCircle size={13} />}
          {feedback.msg}
        </div>
      )}

      <StatusBar status={status} onReprobe={handleReprobe} reprobing={reprobing} />

      <div className="flex flex-col gap-2">
        <label className="text-xs text-gray-500 dark:text-gray-400">网络模式</label>
        <div className="flex gap-2">
          {(['auto', 'lan', 'wan'] as NetMode[]).map(m => (
            <button
              key={m}
              onClick={() => setMode(m)}
              className={[
                'rounded-md border px-3 py-1.5 text-sm',
                mode === m
                  ? 'border-blue-400 bg-blue-50 text-blue-700 dark:bg-blue-900/30 dark:text-blue-300'
                  : 'border-gray-300 hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800',
              ].join(' ')}
            >
              {MODE_LABEL[m]}
            </button>
          ))}
        </div>
        <p className="text-xs text-gray-400">
          自动：在局域网内自动直连局域网地址，否则走外网域名；强制档用于调试。
        </p>
      </div>

      <HostField
        title="局域网地址（在家直连）"
        icon={<Wifi size={13} />}
        value={lanHost}
        onChange={setLanHost}
        placeholder="192.168.0.68"
        hint="只填局域网 IP；端口/协议（http :8788、ASR ws）由各服务自动决定。需要时可写 IP:端口。"
      />
      <HostField
        title="外网地址（在外经域名）"
        icon={<Globe size={13} />}
        value={wanHost}
        onChange={setWanHost}
        placeholder="www.for-memory.cloud:28080"
        hint="只填外网域名（含反代端口）；https + wss、ASR 路径全部自动派生。"
      />

      <div className="flex flex-col gap-2">
        <label className="text-xs text-gray-500 dark:text-gray-400">G10 Bearer Token（可选，内外网共用）</label>
        <input
          type="password"
          value={g10Token}
          onChange={e => setG10Token(e.target.value)}
          placeholder="留空表示不鉴权"
          className={inputCls}
        />
      </div>

      <Button variant="primary" size="sm" disabled={loading} onClick={handleSave} className="self-start">
        <Save size={14} />
        {loading ? '保存中...' : '保存 G10 配置'}
      </Button>
    </section>
  )
}

// ── 主组件 ────────────────────────────────────────────────────────────────────

export default function SettingsPage() {
  const [theme, setTheme] = useState<'light' | 'dark'>(getStoredTheme)

  useEffect(() => { applyTheme(theme) }, [theme])

  return (
    <div className="flex flex-col gap-8 max-w-xl">
      <h1 className="text-xl font-semibold">设置</h1>

      {/* 外观 */}
      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-medium text-gray-600 dark:text-gray-400">外观</h2>
        <div className="flex items-center gap-3">
          <span className="text-sm">主题</span>
          <button
            onClick={() => setTheme(prev => prev === 'light' ? 'dark' : 'light')}
            className="rounded-md border border-gray-300 px-4 py-1.5 text-sm hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800"
          >
            {theme === 'light' ? '切换深色' : '切换浅色'}
          </button>
          <span className="text-xs text-gray-400">当前：{theme === 'light' ? '浅色' : '深色'}</span>
        </div>
      </section>

      {/* G10 配置 */}
      <G10ConfigSection />

      {/* 大模型（公共 LLM 层：配置 + 提示词） */}
      <LlmSection />

      {/* 英语模块 */}
      <section className="flex flex-col gap-3">
        <h2 className="text-sm font-medium text-gray-600 dark:text-gray-400">英语模块</h2>
        <EnvConfig />
      </section>
    </div>
  )
}
