import { useState } from 'react'
import { Activity, Ban } from 'lucide-react'
import { ObserverTable } from '../components/ObserverTable'
import { BlockedFeed, ProcessMap } from '../components/ObservatorySection'
import { useNetPolicyProbe } from '../ProbeContext'
import { useNetPolicyController } from '../NetPolicyController'
import { Section } from '../uiHelpers'

type TrafficView = 'active' | 'blocked'

export function TrafficPage() {
  const [view, setView] = useState<TrafficView>('active')
  const { status, conns, blocked, dnsMap } = useNetPolicyProbe()
  const {
    rules,
    setRules,
    busy,
    flash,
    canElevatedAction,
    allowBlocked,
    clearBlocked,
  } = useNetPolicyController()

  return (
    <div className="space-y-5">
      <div className="inline-flex rounded-lg bg-gray-100 p-1 dark:bg-gray-800">
        <button
          className={`inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition-colors ${
            view === 'active' ? 'bg-white font-medium text-gray-900 shadow-sm dark:bg-gray-700 dark:text-white' : 'text-gray-500'
          }`}
          onClick={() => setView('active')}
        >
          <Activity size={14} /> 活跃连接
          <span className="text-xs text-gray-400">{conns.total}</span>
        </button>
        <button
          className={`inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition-colors ${
            view === 'blocked' ? 'bg-white font-medium text-gray-900 shadow-sm dark:bg-gray-700 dark:text-white' : 'text-gray-500'
          }`}
          onClick={() => setView('blocked')}
        >
          <Ban size={14} /> 被阻断
          <span className="text-xs text-gray-400">{blocked.length}</span>
        </button>
      </div>

      {view === 'active' ? (
        <>
          <ObserverTable
            connections={conns.connections}
            rules={rules}
            applied={!!status?.applied}
            busy={busy}
            dnsMap={dnsMap}
            canReroute={canElevatedAction}
            onRulesChange={setRules}
            onFlash={flash}
          />
          <Section title="进程关联" description="按进程聚合：每个进程的连接数与它访问过的所有域名">
            <ProcessMap dnsMap={dnsMap} />
          </Section>
        </>
      ) : (
        <BlockedFeed
          blocked={blocked}
          busy={busy}
          canAllow={canElevatedAction}
          onAllow={allowBlocked}
          onClear={clearBlocked}
        />
      )}
    </div>
  )
}
