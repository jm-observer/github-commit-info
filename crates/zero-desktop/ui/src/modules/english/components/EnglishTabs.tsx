/**
 * EnglishTabs — 英语听力模块内的子标签（标注 / 全部 / 听包）。
 * 渲染在各英语页顶部，提供各歌单间的切换入口；切换时记住当前子标签，供侧栏回跳。
 *
 * 「跟读判分」不是独立标签——它是叠加在当前歌单上的一个模式开关（见各页内 ShadowPanel）。
 */

import { useEffect } from 'react'
import { NavLink, useLocation } from 'react-router-dom'
import { writeLastEnglishRoute } from '../englishRoute'

const tabs = [
  { to: '/english/annotated', label: '标注' },
  { to: '/english/all', label: '全部' },
  { to: '/english/packages', label: '听包' },
]

export default function EnglishTabs() {
  const location = useLocation()
  // 记住当前子标签：切到别的功能再点回「英语听力」时回到这里，而非写死「标注」。
  useEffect(() => { writeLastEnglishRoute(location.pathname) }, [location.pathname])

  return (
    <div className="mb-4 flex gap-1 border-b border-gray-200 dark:border-gray-800">
      {tabs.map((t) => (
        <NavLink
          key={t.to}
          to={t.to}
          end
          className={({ isActive }) =>
            [
              '-mb-px border-b-2 px-4 py-2 text-sm transition-colors',
              isActive
                ? 'border-blue-500 font-medium text-blue-600 dark:text-blue-400'
                : 'border-transparent text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200',
            ].join(' ')
          }
        >
          {t.label}
        </NavLink>
      ))}
    </div>
  )
}
