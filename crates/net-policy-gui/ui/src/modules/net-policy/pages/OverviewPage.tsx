import { ApplyStepper } from '../components/ApplyStepper'
import { ProtectionBanner } from '../components/ProtectionBanner'
import { ControlBar } from '../components/ControlBar'
import { ExitStatsCards } from '../components/ExitStatsCards'
import { FlowTopology } from '../components/FlowTopology'
import { useNetPolicyProbe } from '../ProbeContext'
import { useNetPolicyController } from '../NetPolicyController'
import { LOADING_STATUS, Section } from '../uiHelpers'

/**
 * 概览页：保护状态横幅 + 应用流程分步（条件展示） + 控制卡（三路出口/主开关/紧急停止） + 出口指标卡。
 * 对应原 NetPolicyPage 的「2. 保护状态横幅」～「3. 出口概览」。
 */
export function OverviewPage() {
  const { status, conns, exitIp, exitIpAt } = useNetPolicyProbe()
  const {
    busy,
    wgError,
    showApplyStepper,
    setShowApplyStepper,
    curRoute,
    changeDefaultRoute,
    toggleEnabled,
    emergencyStop,
    resetConnections,
    settings,
  } = useNetPolicyController()

  return (
    <div className="space-y-6">
      {status && status.platform_supported && (
        <ProtectionBanner status={status} exitIp={exitIp} exitIpAt={exitIpAt} />
      )}

      {showApplyStepper && (
        <div className="space-y-1.5">
          <ApplyStepper
            busy={busy}
            canApply={!!status?.wg_configured || curRoute !== 'wg'}
            onApply={() => toggleEnabled(true)}
          />
          <div className="flex justify-end">
            <button className="text-xs text-gray-400 hover:text-gray-600 dark:hover:text-gray-300" onClick={() => setShowApplyStepper(false)}>
              收起应用流程
            </button>
          </div>
        </div>
      )}

      <ControlBar
        status={status}
        busy={busy}
        wgError={wgError}
        curRoute={curRoute}
        onChangeRoute={(r) => void changeDefaultRoute(r)}
        onToggleEnabled={(e) => void toggleEnabled(e)}
        onEmergencyStop={() => void emergencyStop()}
        onResetConnections={() => void resetConnections()}
      />

      <ExitStatsCards conns={conns} wgConfigured={status?.wg_configured} />

      <Section title="流量路径" description="当前连接从本机应用到物理出口的完整链路">
        <FlowTopology status={status ?? LOADING_STATUS} conns={conns} settings={settings} exitIp={exitIp} exitIpAt={exitIpAt} />
      </Section>
    </div>
  )
}
