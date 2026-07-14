import { CurrentStateSection } from '../components/CurrentStateSection'
import { VerifyMatrix } from '../components/VerifyMatrix'
import { LiveStatusPanel } from '../components/LiveStatusPanel'
import { useNetPolicyProbe } from '../ProbeContext'
import { useNetPolicyController } from '../NetPolicyController'
import { Section } from '../uiHelpers'

export function DiagnosticsPage() {
  const { status, verify, exitIp, exitIpAt } = useNetPolicyProbe()
  const { busy, runVerify } = useNetPolicyController()

  return (
    <div className="space-y-7">
      {status && (
        <Section
          title="当前实时状态"
          description="引擎 / kill-switch / TUN / 默认出口走 3s 快轮询实时更新；出口 IP、DNS 为最近一次自检值"
        >
          <LiveStatusPanel
            status={status}
            verify={verify}
            exitIp={exitIp}
            exitIpAt={exitIpAt}
            onVerify={() => void runVerify()}
            busy={busy}
          />
        </Section>
      )}

      <Section title="本机现状" description="只读探测，不修改系统配置">
        <CurrentStateSection busy={busy} />
      </Section>

      {status && (
        <Section
          title="历史验证参考"
          description="报告 §0.8.2 的历史验证结论（非实时，供查阅）；仅出口 IP / DNS / 引擎 3 项可点「一键自检」实测"
        >
          <VerifyMatrix status={status} verify={verify} onVerify={() => void runVerify()} busy={busy} />
        </Section>
      )}
    </div>
  )
}
