import { useState } from 'react'
import {
  Activity,
  FileClock,
  Gauge,
  Radar,
  RefreshCw,
  ScrollText,
  SlidersHorizontal,
  Settings,
  ShieldCheck,
  Stethoscope,
  Timer,
} from 'lucide-react'
import { useNetPolicyProbe } from './ProbeContext'
import { useNetPolicyController } from './NetPolicyController'
import { btn } from './uiHelpers'
import { OverviewPage } from './pages/OverviewPage'
import { TrafficPage } from './pages/TrafficPage'
import { PolicyPage } from './pages/PolicyPage'
import { RecordsPage } from './pages/RecordsPage'
import { TempDirectPage } from './pages/TempDirectPage'
import { DiagnosticsPage } from './pages/DiagnosticsPage'
import { SettingsPage } from './pages/SettingsPage'
import { LogsPage } from './pages/LogsPage'
import { CapturePage } from './pages/CapturePage'

type MenuKey = 'overview' | 'traffic' | 'policy' | 'tempdirect' | 'records' | 'capture' | 'logs' | 'diagnostics' | 'settings'

interface MenuItem {
  key: MenuKey
  label: string
  description: string
  icon: React.ReactNode
}

const MENU_GROUPS: { label: string; items: MenuItem[] }[] = [
  {
    label: '核心',
    items: [
      { key: 'overview', label: '概览', description: '保护姿态、默认出口与数据通路。', icon: <Gauge size={16} /> },
      { key: 'traffic', label: '流量', description: '观察活跃与被阻断流量，并直接调整出口。', icon: <Activity size={16} /> },
      { key: 'policy', label: '策略编排', description: '维护用户规则，并核对最终生效顺序。', icon: <SlidersHorizontal size={16} /> },
    ],
  },
  {
    label: '工具',
    items: [
      { key: 'tempdirect', label: '临时直连', description: '海外出口故障时限时切换直连。', icon: <Timer size={16} /> },
      { key: 'records', label: '操作记录', description: '查看请求、事件与进程树。', icon: <FileClock size={16} /> },
      { key: 'capture', label: '抓包', description: '抓取 TUN 数据包，导出 Wireshark pcapng。', icon: <Radar size={16} /> },
      { key: 'logs', label: '日志', description: 'mihomo / WireGuard 隧道的实时运行日志。', icon: <ScrollText size={16} /> },
    ],
  },
  {
    label: '系统',
    items: [
      { key: 'diagnostics', label: '诊断', description: '检查本机网络栈并运行验证。', icon: <Stethoscope size={16} /> },
      { key: 'settings', label: 'WireGuard 设置', description: '配置海外隧道与 kill-switch。', icon: <Settings size={16} /> },
    ],
  },
]

const MENU_ITEMS = MENU_GROUPS.flatMap((group) => group.items)

function renderPage(key: MenuKey) {
  switch (key) {
    case 'overview': return <OverviewPage />
    case 'traffic': return <TrafficPage />
    case 'policy': return <PolicyPage />
    case 'records': return <RecordsPage />
    case 'capture': return <CapturePage />
    case 'logs': return <LogsPage />
    case 'tempdirect': return <TempDirectPage />
    case 'diagnostics': return <DiagnosticsPage />
    case 'settings': return <SettingsPage />
  }
}

export function NetPolicyShell() {
  const { status, probeError } = useNetPolicyProbe()
  const { busy, msg, refresh } = useNetPolicyController()
  const [active, setActive] = useState<MenuKey>('overview')
  const current = MENU_ITEMS.find((item) => item.key === active) ?? MENU_ITEMS[0]

  return (
    <div className="mx-auto flex max-w-7xl gap-6">
      <aside className="sticky top-4 h-[calc(100vh-2rem)] w-52 shrink-0 overflow-y-auto border-r border-gray-200 pr-4 dark:border-gray-800">
        <div className="mb-6 flex items-center gap-2 px-2">
          <span className="grid h-8 w-8 place-items-center rounded-lg bg-blue-600 text-white">
            <ShieldCheck size={18} />
          </span>
          <div>
            <div className="text-sm font-semibold text-gray-900 dark:text-gray-100">网络出口策略</div>
            <div className="text-[11px] text-gray-400">net-policy</div>
          </div>
        </div>

        <nav className="space-y-5">
          {MENU_GROUPS.map((group) => (
            <div key={group.label}>
              <div className="mb-1 px-2 text-[10px] font-semibold uppercase tracking-[0.16em] text-gray-400">
                {group.label}
              </div>
              <div className="space-y-0.5">
                {group.items.map((item) => (
                  <button
                    key={item.key}
                    onClick={() => setActive(item.key)}
                    aria-current={active === item.key ? 'page' : undefined}
                    className={`flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-left text-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-400 ${
                      active === item.key
                        ? 'bg-blue-50 font-medium text-blue-700 dark:bg-blue-950/40 dark:text-blue-300'
                        : 'text-gray-600 hover:bg-gray-100 hover:text-gray-900 dark:text-gray-400 dark:hover:bg-gray-800 dark:hover:text-gray-100'
                    }`}
                  >
                    {item.icon}
                    {item.label}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </nav>
      </aside>

      <main className="min-w-0 flex-1 pb-8">
        <header className="mb-5 flex items-start justify-between gap-4 border-b border-gray-200 pb-4 dark:border-gray-800">
          <div>
            <h1 className="text-lg font-semibold text-gray-900 dark:text-gray-100">{current.label}</h1>
            <p className="mt-0.5 text-xs text-gray-500 dark:text-gray-400">{current.description}</p>
          </div>
          <button className={btn()} onClick={() => void refresh()} disabled={busy}>
            <RefreshCw size={14} className={busy ? 'animate-spin' : ''} /> 刷新
          </button>
        </header>

        {status && !status.platform_supported && (
          <div className="mb-4 rounded-md bg-yellow-100 px-4 py-2 text-sm text-yellow-800">
            net-policy 仅支持 Windows，当前平台不可用。
          </div>
        )}

        {probeError && (
          <div className="mb-4 rounded-md bg-red-100 px-4 py-2 text-sm text-red-800 dark:bg-red-950/40 dark:text-red-300">
            实时状态探测失败，当前数据可能已过期：{probeError}
          </div>
        )}

        {renderPage(active)}
      </main>

      {msg && (
        <div className={`fixed bottom-4 right-4 z-50 max-w-sm rounded-lg px-4 py-3 text-sm text-white shadow-lg ${
          msg.kind === 'ok' ? 'bg-green-600' : 'bg-red-600'
        }`}>
          {msg.text}
        </div>
      )}
    </div>
  )
}
