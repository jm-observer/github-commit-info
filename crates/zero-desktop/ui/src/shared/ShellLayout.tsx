import { NavLink, Outlet, useLocation } from 'react-router-dom'
import { useEffect, useState } from 'react'
import { BookOpen, Mic, Wand2, Cookie, ShieldCheck, GitCompareArrows, MessageSquareText, MessagesSquare, Music, Rocket, Settings, Crop } from 'lucide-react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import EnvConfigService from '../modules/english/services/EnvConfigService'
import { readLastEnglishRoute } from '../modules/english/englishRoute'
import BottomPlayerBar from './BottomPlayerBar'

// `match` 设了的项：`to` 动态取(英语听力跳回上次子标签),且 path 以 match 前缀打头即高亮。
const navItems = [
  { to: '/english/annotated', icon: BookOpen, label: '英语听力', match: '/english' },
  { to: '/speech', icon: Mic, label: '语音识别' },
  { to: '/audio-clean', icon: Wand2, label: '音频清洗' },
  { to: '/cookie', icon: Cookie, label: 'Cookie 采集' },
  { to: '/net-policy', icon: ShieldCheck, label: '网络策略' },
  { to: '/codeloop', icon: GitCompareArrows, label: '复核循环' },
  { to: '/chat-summary', icon: MessageSquareText, label: '对话总结' },
  { to: '/llmchat', icon: MessagesSquare, label: '大模型会话' },
  { to: '/music', icon: Music, label: '音乐' },
  { to: '/screenshot', icon: Crop, label: '截图' },
  { to: '/g10-deploy', icon: Rocket, label: 'G10 部署' },
  { to: '/settings', icon: Settings, label: '设置' },
]

// ── 指示灯颜色 ──────────────────────────────────────────────────────────────────

type LightStatus = 'ok' | 'checking' | 'error' | 'unknown'

function lightClass(status: LightStatus): string {
  switch (status) {
    case 'ok':       return 'bg-green-500'
    case 'checking': return 'bg-yellow-400'
    case 'error':    return 'bg-red-500'
    default:         return 'bg-gray-400'
  }
}

function StatusLight({
  status,
  label,
  title,
}: {
  status: LightStatus
  label: string
  title: string
}) {
  return (
    <span className="flex items-center gap-1.5" title={title}>
      <span className={`inline-block h-2 w-2 rounded-full ${lightClass(status)}`} />
      <span>{label}</span>
    </span>
  )
}

// ── G10 指示灯（每 30s ping） ──────────────────────────────────────────────────

function G10Indicator() {
  const [status, setStatus] = useState<LightStatus>('unknown')
  const [detail, setDetail] = useState('G10 连接状态（检测中）')

  useEffect(() => {
    let cancelled = false

    async function check() {
      if (cancelled) return
      setStatus('checking')
      try {
        const r = await invoke<{ state: string; latency_ms?: number; error?: string }>(
          'cookie_ping_server',
        )
        if (cancelled) return
        if (r.state === 'ok') {
          setStatus('ok')
          setDetail(`G10 已连接 ${r.latency_ms ?? ''}ms`)
        } else if (r.state === 'unconfigured') {
          setStatus('error')
          setDetail('G10 未配置，请到设置页填 base URL')
        } else {
          setStatus('error')
          setDetail(`G10 不可达: ${r.error ?? r.state}`)
        }
      } catch (e) {
        if (!cancelled) {
          setStatus('error')
          setDetail(`G10 ping 失败: ${String(e)}`)
        }
      }
    }

    check()
    const id = setInterval(check, 30_000)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])

  return <StatusLight status={status} label="G10" title={detail} />
}

// ── Cookie 指示灯（每 60s 检查；抖音+同花顺） ────────────────────────────────

function CookieIndicator() {
  const [status, setStatus] = useState<LightStatus>('unknown')
  const [detail, setDetail] = useState('Cookie 状态（检测中）')

  useEffect(() => {
    let cancelled = false

    async function check() {
      if (cancelled) return
      try {
        // 检查本地登录窗 cookie（抖音）
        const r = await invoke<{ state: string; count?: number; has_ms_token_any?: boolean }>(
          'cookie_inspect_cookies',
        )
        if (cancelled) return
        if (r.state === 'ok') {
          const count = r.count ?? 0
          setStatus(count > 0 ? 'ok' : 'error')
          setDetail(`抖音 Cookie: ${count} 条${r.has_ms_token_any ? '（msToken 已就绪）' : ''}`)
        } else {
          // no_login_window = 未打开登录窗，不算失败，用 unknown
          setStatus('unknown')
          setDetail('抖音登录窗未打开，Cookie 未检测')
        }
      } catch (e) {
        if (!cancelled) {
          setStatus('error')
          setDetail(`Cookie 检查失败: ${String(e)}`)
        }
      }
    }

    check()
    const id = setInterval(check, 60_000)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])

  return <StatusLight status={status} label="Cookie" title={detail} />
}

// ── 录音状态指示灯（监听 Tauri 事件） ───────────────────────────────────────

function RecordingIndicator() {
  const [recording, setRecording] = useState(false)

  useEffect(() => {
    let unlisten: (() => void) | null = null

    // 首次加载拉一次当前状态
    invoke<{ recording: boolean }>('speech_get_recording_state')
      .then(r => setRecording(r.recording))
      .catch(() => {/* 忽略初始拉取失败 */})

    // 监听录音状态变更事件
    listen<{ recording: boolean }>('speech_recording_state_changed', event => {
      setRecording(event.payload.recording)
    }).then(fn => {
      unlisten = fn
    })

    // 周期轮询后端真值，自愈漏接事件 / 启动竞态。
    const poll = setInterval(() => {
      invoke<{ recording: boolean }>('speech_get_recording_state')
        .then(r => setRecording(r.recording))
        .catch(() => {/* 忽略抖动 */})
    }, 2000)

    return () => {
      unlisten?.()
      clearInterval(poll)
    }
  }, [])

  return (
    <StatusLight
      status={recording ? 'ok' : 'unknown'}
      label="识别"
      title={recording ? '识别中' : '未识别'}
    />
  )
}

// ── 全局识别开关（侧栏常驻，不必切到语音识别页即可启停） ─────────────────────

// 程序生命周期内只自动开启一次（避免组件重挂载时重复触发）。
let autoStarted = false

function RecordingToggle() {
  const [recording, setRecording] = useState(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    let unlisten: (() => void) | null = null

    invoke<{ recording: boolean }>('speech_get_recording_state')
      .then(r => setRecording(r.recording))
      .catch(() => {/* 忽略初始拉取失败 */})

    listen<{ recording: boolean }>('speech_recording_state_changed', event => {
      setRecording(event.payload.recording)
    }).then(fn => {
      unlisten = fn
    })

    // 周期轮询后端真值，自愈漏接事件 / 启动竞态（与语音识别页同一真相源）。
    const poll = setInterval(() => {
      invoke<{ recording: boolean }>('speech_get_recording_state')
        .then(r => setRecording(r.recording))
        .catch(() => {/* 忽略抖动 */})
    }, 2000)

    // 启动自动开启识别：仅当设置里「启动时自动识别」打开时触发。
    // best-effort：地址/设备未配置时静默失败，后端有重复启动保护。
    if (!autoStarted) {
      autoStarted = true
      invoke<{ auto_start?: boolean }>('speech_get_settings')
        .then(s => {
          if (s?.auto_start) {
            return invoke('speech_start_recording').catch(() => {/* 未配置则忽略 */})
          }
        })
        .catch(() => {/* 读设置失败则不自动开启 */})
    }

    return () => {
      unlisten?.()
      clearInterval(poll)
    }
  }, [])

  const toggle = async () => {
    setBusy(true)
    try {
      // 启停命令无参，配置（远程地址/输入设备）由后端从已保存状态读取。
      if (recording) await invoke('speech_stop_recording')
      else await invoke('speech_start_recording')
    } catch (e) {
      // 例如远程识别地址未配置：到语音识别页设置后再试。
      window.alert(`录音操作失败：${String(e)}`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <button
      type="button"
      onClick={() => void toggle()}
      disabled={busy}
      title={recording ? '点击停止识别' : '点击开始识别（需先在语音识别页配置远程地址/设备）'}
      className={[
        'flex w-full items-center justify-center gap-2 rounded-md px-3 py-2 text-sm font-medium transition-colors disabled:opacity-60',
        recording
          ? 'bg-red-500 text-white hover:bg-red-600'
          : 'bg-blue-500 text-white hover:bg-blue-600',
      ].join(' ')}
    >
      <Mic size={16} />
      {recording ? '停止识别' : '开始识别'}
    </button>
  )
}

// ── customer_id 指示灯（mount-once + 监听设置保存事件） ─────────────────────

function CustomerIdIndicator() {
  const [status, setStatus] = useState<LightStatus>('unknown')

  useEffect(() => {
    let cancelled = false

    async function check() {
      if (cancelled) return
      try {
        const cid = await EnvConfigService.getInstance().getCustomerId()
        if (!cancelled) {
          setStatus(cid ? 'ok' : 'error')
        }
      } catch {
        if (!cancelled) setStatus('error')
      }
    }

    check()

    // 设置页保存 customer_id 后 dispatch 'customer-id-changed' 事件
    const handler = () => { void check() }
    window.addEventListener('customer-id-changed', handler)
    return () => {
      cancelled = true
      window.removeEventListener('customer-id-changed', handler)
    }
  }, [])

  const title =
    status === 'ok'
      ? 'customer_id 已配置'
      : 'customer_id 未配置，请到设置页配置'

  return <StatusLight status={status} label="CID" title={title} />
}

// ── Shell 主布局 ─────────────────────────────────────────────────────────────

export default function ShellLayout() {
  const location = useLocation()
  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-white text-gray-900 dark:bg-gray-950 dark:text-gray-100">
      <div className="flex min-h-0 flex-1">
      {/* 左侧导航 */}
      <aside className="flex w-44 flex-shrink-0 flex-col border-r border-gray-200 bg-gray-50 dark:border-gray-800 dark:bg-gray-900">
        <div className="flex h-12 items-center px-4 text-sm font-semibold tracking-wide text-gray-500 dark:text-gray-400">
          zero-desktop
        </div>
        <nav className="flex-1 space-y-1 px-2 py-2">
          {navItems.map(({ to, icon: Icon, label, match }) => {
            // 英语听力(match='/english')：to 取上次子标签，前缀命中即高亮；其余项走精确匹配。
            const resolvedTo = match ? readLastEnglishRoute() : to
            const active = match
              ? location.pathname.startsWith(match)
              : location.pathname === to
            return (
              <NavLink
                key={label}
                to={resolvedTo}
                className={[
                  'flex items-center gap-2 rounded-md px-3 py-2 text-sm transition-colors',
                  active
                    ? 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300'
                    : 'text-gray-700 hover:bg-gray-200 dark:text-gray-300 dark:hover:bg-gray-800',
                ].join(' ')}
              >
                <Icon size={16} />
                {label}
              </NavLink>
            )
          })}
        </nav>

        {/* 侧栏底部：全局录音开关，任何页面都能启停 */}
        <div className="border-t border-gray-200 p-2 dark:border-gray-800">
          <RecordingToggle />
        </div>
      </aside>

      {/* 右侧主区域 */}
      <div className="flex flex-1 flex-col overflow-hidden">
        {/* 顶部状态栏 */}
        <header className="flex h-10 items-center justify-end gap-3 border-b border-gray-200 px-4 text-xs text-gray-500 dark:border-gray-800 dark:text-gray-400">
          <G10Indicator />
          <CookieIndicator />
          <RecordingIndicator />
          <CustomerIdIndicator />
        </header>

        {/* 内容区 */}
        <main className="flex-1 overflow-auto p-6">
          <Outlet />
        </main>
      </div>
      </div>

      {/* 底栏常驻播放器:左侧 tab 切换音乐/英语,右侧渲染所选条目控件;两个 service
          单例都在后台跑,切 tab 只切 UI。 */}
      <BottomPlayerBar />
    </div>
  )
}
