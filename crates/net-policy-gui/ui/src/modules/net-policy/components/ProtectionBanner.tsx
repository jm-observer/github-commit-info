import { ShieldCheck, ShieldAlert, ShieldOff, ShieldQuestion, FlaskConical, AlertTriangle } from 'lucide-react'
import type { Status } from '../api/tauri-client'

/**
 * 保护状态横幅：把 4 个布尔位（applied / protected / protection_validated / firewall.active+rule_count）
 * 收敛成单一有名字的状态（设计 §3.2）。
 *
 * 判定**按下表自上而下、先命中先生效**（顺序很重要——危险态优先于「未应用」）：
 *  1. 防火墙仍生效·引擎未受管  !applied && firewall.active && rule_count>0
 *  2. 未应用                  !applied && !(firewall.active && rule_count>0)
 *  3. 不受保护预览            applied && !firewall.active
 *  4. 已阻断·隧道未连通       applied && firewall.active && !(mihomo_running && tun_ready)
 *  5. 实验保护                protected && !protection_validated
 *  6. 受保护·fail-closed     protected && protection_validated
 */

type Tone = 'red' | 'gray' | 'amber' | 'green'

interface BannerState {
  name: string
  tone: Tone
  meaning: string
  next: string
  Icon: typeof ShieldCheck
}

function resolveState(s: Status): BannerState {
  const fw = s.firewall
  const fwActive = !!fw?.active
  const ruleCount = fw?.rule_count ?? 0
  // 危险态守卫：必须叠加 rule_count>0，否则 firewall.active 单独只读 Domain DefaultOutboundAction，
  // 会把系统/企业本来的默认 Block 误报成 net-policy 残留（设计 §3.2 ⚠️）。
  const fwResidual = fwActive && ruleCount > 0
  const connected = s.mihomo_running && s.tun_ready

  // 1. 危险态：防火墙仍生效但引擎未受管。
  if (!s.applied && fwResidual) {
    return {
      name: '防火墙仍生效 · 引擎未受管',
      tone: 'red',
      meaning: `检测到 NetPolicy-KillSwitch 规则组仍在（${ruleCount} 条）+ 出站默认 Block，但运行态未恢复「已应用」。你实际被防火墙卡住却以为「未应用」（可能是 net-policy 残留，也可能叠加了系统策略）。`,
      next: '重新「应用」恢复引擎，或「紧急停止」撤防火墙回基线。',
      Icon: ShieldAlert,
    }
  }

  // 2. 未应用。
  if (!s.applied) {
    return {
      name: '未应用',
      tone: 'gray',
      meaning: '策略未生效，且无 net-policy 残留规则。',
      next: '配置 WireGuard 出口后点「应用」启用策略。',
      Icon: ShieldQuestion,
    }
  }

  // 3. 不受保护预览。
  if (!fwActive) {
    return {
      name: '不受保护预览',
      tone: 'amber',
      meaning: 'mihomo 在跑但 kill-switch 未开 → 引擎/隧道异常时流量可能泄漏到本地出口。',
      next: '生产环境请在设置里开启 kill-switch 并重新「应用」。',
      Icon: ShieldOff,
    }
  }

  // 4. 已阻断·隧道未连通。
  if (!connected) {
    return {
      name: '已阻断 · 隧道未连通',
      tone: 'red',
      meaning: 'fail-closed 成立（未知流量不会泄漏），但 mihomo 控制器 / TUN(Meta) 未就绪，当前无法联网。',
      next: '检查 WG 配置 / mihomo 引擎，或重新「应用」。',
      Icon: ShieldAlert,
    }
  }

  // 5. 实验保护（当前后端 FIREWALL_MODEL_VALIDATED=false，实际恒命中此态）。
  if (!s.protection_validated) {
    return {
      name: '实验保护 (待 VP-08/09/10 复测)',
      tone: 'amber',
      meaning: 'kill-switch 已生效，但新防火墙「程序放行」模型（Program=mihomo.exe）尚未真机复测 fail-closed。',
      next: '进生产前需通过 VP-08/09/10 复测；当前不可视为已坐实的 fail-closed。',
      Icon: FlaskConical,
    }
  }

  // 6. 受保护·fail-closed。
  return {
    name: '受保护 · fail-closed',
    tone: 'green',
    meaning: '未知流量默认走海外；引擎/隧道断开则物理网卡全阻断，不泄漏。',
    next: '正常运行中。可用「一键验证」抽查出口 IP / DNS 劫持。',
    Icon: ShieldCheck,
  }
}

const TONE: Record<Tone, { box: string; icon: string; name: string }> = {
  red: {
    box: 'border-red-300 bg-red-50 dark:border-red-900/60 dark:bg-red-950/40',
    icon: 'text-red-600 dark:text-red-400',
    name: 'text-red-700 dark:text-red-300',
  },
  amber: {
    box: 'border-amber-300 bg-amber-50 dark:border-amber-900/60 dark:bg-amber-950/40',
    icon: 'text-amber-600 dark:text-amber-400',
    name: 'text-amber-700 dark:text-amber-300',
  },
  gray: {
    box: 'border-gray-300 bg-gray-50 dark:border-gray-700 dark:bg-gray-900/40',
    icon: 'text-gray-500 dark:text-gray-400',
    name: 'text-gray-700 dark:text-gray-200',
  },
  green: {
    box: 'border-green-300 bg-green-50 dark:border-green-900/60 dark:bg-green-950/40',
    icon: 'text-green-600 dark:text-green-400',
    name: 'text-green-700 dark:text-green-300',
  },
}

export function ProtectionBanner({
  status,
  exitIp,
  exitIpAt,
}: {
  status: Status
  exitIp?: string | null
  exitIpAt?: string | null
}) {
  const st = resolveState(status)
  const tone = TONE[st.tone]
  const { Icon } = st
  return (
    <section className={`rounded-lg border p-4 ${tone.box}`}>
      <div className="flex items-start gap-3">
        <Icon className={`mt-0.5 shrink-0 ${tone.icon}`} size={28} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <h2 className={`text-base font-semibold ${tone.name}`}>{st.name}</h2>
            <span className="ml-auto flex items-center gap-1.5 text-xs text-gray-500 dark:text-gray-400">
              <span>出口 IP</span>
              <span className="font-mono text-gray-700 dark:text-gray-200">{exitIp || '—'}</span>
              {exitIpAt && <span className="text-gray-400">· {exitIpAt}</span>}
            </span>
          </div>
          <p className="mt-1 text-sm text-gray-700 dark:text-gray-300">{st.meaning}</p>
          <p className="mt-1 flex items-center gap-1.5 text-xs text-gray-600 dark:text-gray-400">
            <AlertTriangle size={12} className="shrink-0" />
            <span>下一步：{st.next}</span>
          </p>
        </div>
      </div>
    </section>
  )
}
