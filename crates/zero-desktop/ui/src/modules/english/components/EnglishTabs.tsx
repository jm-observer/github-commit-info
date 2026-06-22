/**
 * EnglishTabs — 英语听力模块内的子标签（标注 / 全部 / 听包）。
 * 渲染在各英语页顶部，提供三种播放模式间的切换入口。
 */

import { NavLink } from 'react-router-dom'

const tabs = [
  { to: '/english/annotated', label: '标注' },
  { to: '/english/all', label: '全部' },
  { to: '/english/packages', label: '听包' },
]

export default function EnglishTabs() {
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
