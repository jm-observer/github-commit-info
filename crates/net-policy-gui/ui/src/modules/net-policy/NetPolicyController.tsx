import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import {
  NetPolicyAPI,
  type Settings,
  type RuleSet,
  type Rule,
  type Route,
  type BlockedEntry,
} from './api/tauri-client'
import { useNetPolicyProbe } from './ProbeContext'

/** 清理后端错误串：去掉 `Error:` 前缀 + `[error_kind]` 机器码前缀（如 [operation_conflict]/[internal]），
 *  只留人类可读部分，避免噪音刷在 toast 上。 */
function cleanErr(e: unknown): string {
  return String(e).replace(/^Error:\s*/i, '').replace(/^\[[a-z_]+\]\s*/i, '').trim()
}

/**
 * 网络策略页面级状态 + 交互 controller。
 *
 * 拆多页前，这些状态/handler 都定义在 NetPolicyPage.tsx 单文件里；拆成 7 个菜单页后，
 * 提到这里做成 context，各 page 按需消费，避免状态被拆散到多份、或重复定义。
 * 只负责「设置/规则/busy/flash/wgError」这类跨页共享的可写状态与动作；
 * 「本机现状」（status/conns/blocked/dnsMap/verify/exitIp）仍走 ProbeContext，不重复。
 */

interface NetPolicyControllerValue {
  settings: Settings | null
  setSettings: React.Dispatch<React.SetStateAction<Settings | null>>
  rules: RuleSet
  setRules: React.Dispatch<React.SetStateAction<RuleSet>>
  busy: boolean
  msg: { kind: 'ok' | 'err'; text: string } | null
  wgError: string | null
  newRule: Rule
  setNewRule: React.Dispatch<React.SetStateAction<Rule>>
  showApplyStepper: boolean
  setShowApplyStepper: React.Dispatch<React.SetStateAction<boolean>>
  wgFileRef: React.RefObject<HTMLInputElement>
  flash: (kind: 'ok' | 'err', text: string) => void
  refresh: () => void
  importWgConf: (file: File) => Promise<void>
  saveSettings: () => Promise<boolean>
  emergencyStop: () => Promise<void>
  toggleEnabled: (enabled: boolean) => Promise<void>
  changeDefaultRoute: (route: Route) => Promise<void>
  addRule: () => void | Promise<void>
  deleteRule: (rule: Rule) => Promise<void>
  rerouteRule: (rule: Rule, target: string) => Promise<void>
  allowBlocked: (e: BlockedEntry, route: 'direct' | 'wg') => Promise<void>
  clearBlocked: () => Promise<void>
  runVerify: () => Promise<void>
  resetConnections: () => Promise<void>
  curRoute: Route
  canElevatedAction: boolean
}

const NetPolicyControllerContext = createContext<NetPolicyControllerValue | null>(null)

export function NetPolicyControllerProvider({ children }: { children: React.ReactNode }) {
  const { status, refreshFast, runVerify: runVerifyProbe } = useNetPolicyProbe()

  const [settings, setSettings] = useState<Settings | null>(null)
  const [rules, setRules] = useState<RuleSet>({ rules: [], groups: [] })
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null)
  const [wgError, setWgError] = useState<string | null>(null)
  // React state 要到下一次渲染才会生效；仅靠 busy 无法拦住同一帧内的连续点击。
  // 所有写操作共用这个同步锁，既防出口按钮连点，也避免切出口与保存/规则热载交叉。
  const actionInFlightRef = useRef(false)

  const [newRule, setNewRule] = useState<Rule>({ kind: 'process-name', value: '', route: 'direct' })
  const wgFileRef = useRef<HTMLInputElement>(null)

  // 应用流程分步进度（ApplyStepper）：开始「开始观察」时展开，流程结束保留最终态，用户可手动收起。
  const [showApplyStepper, setShowApplyStepper] = useState(false)

  const flash = useCallback((kind: 'ok' | 'err', text: string) => {
    setMsg({ kind, text })
    setTimeout(() => setMsg(null), 5000)
  }, [])

  const refresh = useCallback(() => {
    refreshFast()
    void NetPolicyAPI.getSettings().then(setSettings).catch(() => {})
    void NetPolicyAPI.listRules().then(setRules).catch(() => {})
  }, [refreshFast])

  useEffect(() => { void refresh() }, [refresh])

  const importWgConf = useCallback(async (file: File) => {
    try {
      const content = await file.text()
      const wg = await NetPolicyAPI.parseWgConf(content)
      setSettings(prev => (prev ? { ...prev, wg } : prev))
      flash('ok', '已导入 WireGuard 配置，请检查各字段后点「保存」')
    } catch (e) {
      flash('err', `导入失败：${cleanErr(e)}`)
    }
  }, [flash])

  const run = useCallback(async (label: string, fn: () => Promise<unknown>): Promise<boolean> => {
    if (actionInFlightRef.current) return false
    actionInFlightRef.current = true
    setBusy(true)
    try {
      await fn()
      flash('ok', `${label}成功`)
      return true
    } catch (e) {
      flash('err', `${label}失败：${cleanErr(e)}`)
      return false
    } finally {
      actionInFlightRef.current = false
      setBusy(false)
      void refresh()
    }
  }, [flash, refresh])

  // 热载失败不能静默：settings/规则已落盘、UI 会显示新状态，但 mihomo 运行配置没变，
  // 配置态与运行态会静默分叉。这里失败时单独 flash 一条可区分于「保存失败」的警告。
  const hotReloadIfApplied = useCallback(async () => {
    // 只在引擎**真在跑**时热载：applied 可能是上次 apply 的残留标志、而 mihomo 已停，
    // 此时改设置只是存盘、下次「开始观察」时生效，不该对着没跑的引擎热载再报「连不上」。
    if (!status?.applied || !status?.mihomo_running) return
    try {
      await NetPolicyAPI.reload()
    } catch (e) {
      throw new Error(`设置已保存，但运行态更新未完整完成：${cleanErr(e)}；请重试热载或重新应用。`)
    }
  }, [status?.applied, status?.mihomo_running])

  const curRoute: Route = settings?.default_route ?? status?.default_route ?? 'direct'

  const saveSettings = useCallback(async (): Promise<boolean> => {
    if (!settings) return false
    return run('保存设置', async () => {
      await NetPolicyAPI.saveSettings(settings)
      await hotReloadIfApplied()
    })
  }, [settings, run, hotReloadIfApplied])

  const emergencyStop = useCallback(async () => {
    await run('紧急停止', () => NetPolicyAPI.emergencyStop())
  }, [run])

  // 主开关：开 → setEnabled(true)（触发 apply，同时后端发 6 步 apply-progress 事件）；关 → setEnabled(false)（触发 stop）。
  const toggleEnabled = useCallback(async (enabled: boolean) => {
    if (enabled) setShowApplyStepper(true)
    await run(enabled ? '开始观察' : '停止观察', () => NetPolicyAPI.setEnabled(enabled))
  }, [run])

  // 默认出口切换（直连 / WireGuard / 代理订阅 / 阻断）。
  const changeDefaultRoute = useCallback(async (route: Route) => {
    if (!settings || route === curRoute || actionInFlightRef.current) return
    if (route === 'wg' && !status?.wg_configured) {
      setWgError('尚未配置 WireGuard——请先到「WireGuard 设置」填写隧道配置后再切换。')
      setTimeout(() => setWgError(null), 6000)
      return
    }
    if (route === 'proxy' && settings.proxy_subscriptions.active == null) {
      setWgError('尚未激活代理订阅——请先到「代理订阅」填写 URL、激活并保存。')
      setTimeout(() => setWgError(null), 6000)
      return
    }
    actionInFlightRef.current = true
    setBusy(true)
    setWgError(null)
    const next: Settings = { ...settings, default_route: route }
    setSettings(next)
    try {
      await NetPolicyAPI.saveSettings(next)
      await hotReloadIfApplied()
      void refreshFast()
    } catch (e) {
      flash('err', `切换出口失败：${cleanErr(e)}`)
      void refresh() // 回滚显示
    } finally {
      actionInFlightRef.current = false
      setBusy(false)
    }
  }, [settings, curRoute, status?.wg_configured, hotReloadIfApplied, refreshFast, flash, refresh])

  const addRule = useCallback(async () => {
    if (!newRule.value.trim()) return
    await run('新增规则', async () => {
      const rs = await NetPolicyAPI.saveRule({ ...newRule, value: newRule.value.trim() })
      setRules(rs)
      setNewRule({ ...newRule, value: '' })
      await hotReloadIfApplied()
    })
  }, [newRule, run, hotReloadIfApplied])

  const deleteRule = useCallback(async (rule: Rule) => {
    await run('删除规则', async () => {
      setRules(await NetPolicyAPI.deleteRule(rule))
      await hotReloadIfApplied()
    })
  }, [run, hotReloadIfApplied])

  const rerouteRule = useCallback(async (rule: Rule, target: string) => {
    await run(`改路 ${target}`, async () => {
      setRules(await NetPolicyAPI.saveRule(rule))
      await hotReloadIfApplied()
    })
  }, [run, hotReloadIfApplied])

  const allowBlocked = useCallback(async (e: BlockedEntry, route: 'direct' | 'wg') => {
    await run('放行', async () => {
      const rule: Rule = e.dest_ip
        ? { kind: 'ip-cidr', value: `${e.dest_ip}/${e.dest_ip.includes(':') ? 128 : 32}`, route }
        : { kind: 'domain-suffix', value: e.host, route }
      setRules(await NetPolicyAPI.saveRule(rule))
      await hotReloadIfApplied()
    })
  }, [run, hotReloadIfApplied])

  const clearBlocked = useCallback(async () => {
    await run('清空被阻断记录', () => NetPolicyAPI.clearBlocked())
  }, [run])

  const runVerify = useCallback(async () => {
    await run('验证', async () => { await runVerifyProbe() })
  }, [run, runVerifyProbe])

  // 手动「重置连接」：与 changeDefaultRoute 里的 best-effort 调用不同，这里是用户主动触发，
  // 走标准 run()（有 busy/flash 反馈）。
  const resetConnections = useCallback(async () => {
    await run('重置连接', () => NetPolicyAPI.resetConnections())
  }, [run])

  // 未提权时改路/放行会触发 reload（落防火墙/TUN），必然失败——未拿到 status 前默认放行交互，
  // 一旦拿到 status 且明确未提权，才禁用（platform_supported 前提下）。
  const canElevatedAction = !status || !status.platform_supported || status.elevated

  const value: NetPolicyControllerValue = {
    settings,
    setSettings,
    rules,
    setRules,
    busy,
    msg,
    wgError,
    newRule,
    setNewRule,
    showApplyStepper,
    setShowApplyStepper,
    wgFileRef,
    flash,
    refresh,
    importWgConf,
    saveSettings,
    emergencyStop,
    toggleEnabled,
    changeDefaultRoute,
    addRule,
    deleteRule,
    rerouteRule,
    allowBlocked,
    clearBlocked,
    runVerify,
    resetConnections,
    curRoute,
    canElevatedAction,
  }

  return <NetPolicyControllerContext.Provider value={value}>{children}</NetPolicyControllerContext.Provider>
}

export function useNetPolicyController(): NetPolicyControllerValue {
  const v = useContext(NetPolicyControllerContext)
  if (!v) throw new Error('useNetPolicyController 必须在 NetPolicyControllerProvider 下使用')
  return v
}
