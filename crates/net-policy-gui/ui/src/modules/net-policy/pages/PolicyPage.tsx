import { RuleList } from '../components/RuleList'
import { RoutesSection } from '../components/RoutesSection'
import { useNetPolicyProbe } from '../ProbeContext'
import { useNetPolicyController } from '../NetPolicyController'
import { Section } from '../uiHelpers'

export function PolicyPage() {
  const { conns } = useNetPolicyProbe()
  const { rules, newRule, setNewRule, addRule, deleteRule, busy, curRoute } = useNetPolicyController()

  return (
    <div className="space-y-7">
      <Section title="用户规则" description="新增规则后立即保存；运行中会自动热加载">
        <RuleList
          rules={rules}
          newRule={newRule}
          setNewRule={setNewRule}
          addRule={() => void addRule()}
          deleteRule={(rule) => void deleteRule(rule)}
          busy={busy}
          curRoute={curRoute}
          conns={conns}
        />
      </Section>
      <Section title="最终生效顺序" description="内置规则、临时例外、程序组、用户规则与兜底出口的合并结果">
        <RoutesSection busy={busy} />
      </Section>
    </div>
  )
}
