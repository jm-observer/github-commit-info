import { CurrentStateSection } from '../components/CurrentStateSection'
import { VerifyMatrix } from '../components/VerifyMatrix'
import { useNetPolicyProbe } from '../ProbeContext'
import { useNetPolicyController } from '../NetPolicyController'
import { Section } from '../uiHelpers'

export function DiagnosticsPage() {
  const { status, verify } = useNetPolicyProbe()
  const { busy, runVerify } = useNetPolicyController()

  return (
    <div className="space-y-7">
      <Section title="本机现状" description="只读探测，不修改系统配置">
        <CurrentStateSection busy={busy} />
      </Section>
      {status && (
        <Section title="验证依据" description="历史验证结论与当前代码模型的实时检查结果">
          <VerifyMatrix status={status} verify={verify} onVerify={() => void runVerify()} busy={busy} />
        </Section>
      )}
    </div>
  )
}
