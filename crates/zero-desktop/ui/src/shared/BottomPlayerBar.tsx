/**
 * BottomPlayerBar — 底栏单条播放器外壳。
 *
 * 左侧是音乐 / 英语两个 tab,右侧渲染当前选中那条的内嵌控件(MiniPlayerBody / EnglishMiniPlayerBody)。
 * 没进过英语听力页 → 英语 tab 禁用 + 自动停在音乐。两者各自的 service 单例都在后台跑,
 * 切 tab 只切换 UI 显示,不影响播放。用户选中持久化到 localStorage。
 */

import { useEffect, useState } from 'react'
import { Languages, Music } from 'lucide-react'
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
      {/* 左侧 tab 切换 */}
      <div className="flex flex-shrink-0 items-center gap-1 border-r border-gray-200 px-2 dark:border-gray-800">
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
