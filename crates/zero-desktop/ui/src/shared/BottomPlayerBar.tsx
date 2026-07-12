/**
 * BottomPlayerBar — 底栏单条播放器外壳。
 *
 * 左侧是音乐 / 英语两个 tab,右侧渲染当前选中那条的内嵌控件(MiniPlayerBody / EnglishMiniPlayerBody)。
 * 没进过英语听力页 → 英语 tab 禁用 + 自动停在音乐。两者各自的 service 单例都在后台跑,
 * 切 tab 只切换 UI 显示,不影响播放。用户选中持久化到 localStorage。
 */

import { useEffect, useState } from 'react'
import { Languages, Mic, MicOff, Music } from 'lucide-react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import MiniPlayerBody from '../modules/music/MiniPlayer'
import EnglishMiniPlayerBody, { useEnglishPresence } from '../modules/english/EnglishMiniPlayer'

type Active = 'music' | 'english'
const LS_KEY = 'bottom_player_active'

function loadActive(): Active {
  if (typeof window === 'undefined') return 'music'
  const v = window.localStorage.getItem(LS_KEY)
  return v === 'english' ? 'english' : 'music'
}

export default function BottomPlayerBar() {
  const englishPresent = useEnglishPresence()
  const [active, setActive] = useState<Active>(loadActive)

  // 英语没起来时强制停在音乐;英语刚出现时不抢焦点(用户主动切才换)。
  useEffect(() => {
    if (active === 'english' && !englishPresent) setActive('music')
  }, [active, englishPresent])

  useEffect(() => {
    if (typeof window !== 'undefined') window.localStorage.setItem(LS_KEY, active)
  }, [active])

  return (
    <div className="flex h-16 items-stretch border-t border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-950">
      {/* 左侧：识别开关 + 音乐/英语 tab 切换 */}
      <div className="flex flex-shrink-0 items-center gap-1 border-r border-gray-200 px-2 dark:border-gray-800">
        <RecognitionToggle />
        {/* 分隔：识别是动作开关，音乐/英语是视图 tab */}
        <div className="mx-0.5 h-8 w-px bg-gray-200 dark:bg-gray-800" />
        <TabButton
          icon={<Music size={18} />}
          label="音乐"
          active={active === 'music'}
          accent="blue"
          onClick={() => setActive('music')}
        />
        <TabButton
          icon={<Languages size={18} />}
          label="英语"
          active={active === 'english'}
          accent="amber"
          disabled={!englishPresent}
          title={englishPresent ? undefined : '尚未进入英语听力'}
          onClick={() => englishPresent && setActive('english')}
        />
      </div>

      {/* 右侧控件区 */}
      <div className="flex min-w-0 flex-1 items-center">
        {active === 'english' && englishPresent ? <EnglishMiniPlayerBody /> : <MiniPlayerBody />}
      </div>
    </div>
  )
}

interface TabButtonProps {
  icon: React.ReactNode
  label: string
  active: boolean
  accent: 'blue' | 'amber'
  disabled?: boolean
  title?: string
  onClick: () => void
}

function TabButton({ icon, label, active, accent, disabled, title, onClick }: TabButtonProps) {
  const accentCls =
    accent === 'amber'
      ? 'text-amber-600 dark:text-amber-400 bg-amber-50 dark:bg-amber-900/30'
      : 'text-blue-600 dark:text-blue-400 bg-blue-50 dark:bg-blue-900/30'
  const baseCls = 'text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200'
  const cls = [
    'flex h-10 min-w-[3.5rem] flex-col items-center justify-center rounded-md px-2 text-[10px] transition-colors',
    active ? accentCls : baseCls,
    disabled ? 'cursor-not-allowed opacity-40' : 'cursor-pointer',
  ].join(' ')
  return (
    <button type="button" onClick={onClick} disabled={disabled} title={title} className={cls}>
      {icon}
      <span className="mt-0.5">{label}</span>
    </button>
  )
}

// ── 全局识别开关（底栏常驻，与音乐/英语并列；任何页面都能启停） ─────────────────

// 程序生命周期内只自动开启一次（避免组件重挂载时重复触发）。
let autoStarted = false

function RecognitionToggle() {
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

  // 识别中：红色高亮 + 麦克风脉动 + 右上角 ping 点；已停止：灰白静默 + 划掉的麦克风。
  const cls = [
    'relative flex h-10 min-w-[3.5rem] flex-col items-center justify-center rounded-md px-2 text-[10px] transition-colors disabled:opacity-60',
    recording
      ? 'text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/30'
      : 'text-gray-500 hover:text-gray-700 hover:bg-gray-100 dark:text-gray-400 dark:hover:text-gray-200 dark:hover:bg-gray-800',
    busy ? 'cursor-wait' : 'cursor-pointer',
  ].join(' ')

  return (
    <button
      type="button"
      onClick={() => void toggle()}
      disabled={busy}
      title={recording ? '识别中，点击停止' : '已停止，点击开始识别（需先在语音识别页配置远程地址/设备）'}
      className={cls}
    >
      {recording && (
        <span className="absolute right-1 top-1 flex h-1.5 w-1.5">
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-red-500 opacity-75" />
          <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-red-500" />
        </span>
      )}
      {recording ? <Mic size={18} className="animate-pulse" /> : <MicOff size={18} />}
      <span className="mt-0.5">{recording ? '识别中' : '已停止'}</span>
    </button>
  )
}
