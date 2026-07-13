import { useCallback, useEffect, useRef, useState } from 'react'
import {
  ShieldCheck,
  RefreshCw,
  Plus,
  Trash2,
  OctagonX,
  Upload,
  Settings as SettingsIcon,
  Power,
} from 'lucide-react'
import {
  NetPolicyAPI,
  type Settings,
  type RuleSet,
  type Rule,
  type RuleKind,
  type Route,
  type BlockedEntry,
  type Status,
} from './api/tauri-client'
import { FlowTopology } from './components/FlowTopology'
import { VerifyMatrix } from './components/VerifyMatrix'
import { CurrentStateSection } from './components/CurrentStateSection'
import { ObservatorySection } from './components/ObservatorySection'
import { ObserverTable } from './components/ObserverTable'
import { ApplyStepper } from './components/ApplyStepper'
import { ProtectionBanner } from './components/ProtectionBanner'
import { TempDirectControl } from './components/TempDirectControl'
import { RoutesSection } from './components/RoutesSection'
import { RecordsSection } from './components/RecordsSection'
import { useNetPolicyProbe } from './ProbeContext'

const KIND_LABELS: Record<RuleKind, string> = {
  'process-path': '程序路径',
  'process-name': '程序名',
  'domain-suffix': '域名后缀',
  'ip-cidr': 'IP/CIDR',
}

// 首屏占位状态：让全景图在真实探测（~1s 的 PS 调用）回来前就能立刻渲染出「全灰/未起」骨架，
// 而不是空白等待。platform_supported 设 true 避免闪一下「不支持」横幅。真实 status 一到即覆盖。
const LOADING_STATUS: Status = {
  platform_supported: true,
  wg_configured: false,
  killswitch_enabled: false,
  applied: false,
  mihomo_running: false,
  tun_ready: false,
  protected: false,
  protection_validated: false,
  firewall: null,
  default_route: 'direct',
  enabled: false,
  elevated: true,
}

// ── 小组件（保留原 Panel / btn） ──────────────────────────────────────────────

function Panel({
  title,
  children,
  right,
  collapsible = false,
  defaultOpen = true,
}: {
  title: string
  children: React.ReactNode
  right?: React.ReactNode
  collapsible?: boolean
  defaultOpen?: boolean
}) {
  if (collapsible) {
    return (
      <details className="group rounded-lg border border-gray-200 dark:border-gray-800" open={defaultOpen}>
        <summary className="flex cursor-pointer list-none items-center justify-between border-b border-gray-200 px-4 py-2 select-none hover:bg-gray-50 dark:border-gray-800 dark:hover:bg-gray-800/40">
          <div className="flex items-center gap-2">
            <span className="text-xs text-gray-400 transition-transform group-open:rotate-90">▶</span>
            <h2 className="text-sm font-semibold">{title}</h2>
          </div>
          {right && (
            <div onClick={(e) => e.stopPropagation()} className="flex gap-2">
              {right}
            </div>
          )}
        </summary>
        <div className="p-4">{children}</div>
      </details>
    )
  }
  return (
    <section className="rounded-lg border border-gray-200 dark:border-gray-800">
      <div className="flex items-center justify-between border-b border-gray-200 px-4 py-2 dark:border-gray-800">
        <h2 className="text-sm font-semibold">{title}</h2>
        {right}
      </div>
      <div className="p-4">{children}</div>
    </section>
  )
}

function btn(variant: 'primary' | 'danger' | 'ghost' = 'ghost') {
  const base = 'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition-colors disabled:opacity-50'
  if (variant === 'primary') return `${base} bg-blue-600 text-white hover:bg-blue-700`
  if (variant === 'danger') return `${base} bg-red-600 text-white hover:bg-red-700`
  return `${base} border border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800`
}

// ── 主页面 ────────────────────────────────────────────────────────────────────

export default function NetPolicyPage() {
  const {
    status,
    conns,
    blocked,
    dnsMap,
    verify,
    exitIp,
    exitIpAt,
    refreshFast,
    runVerify: runVerifyProbe,
  } = useNetPolicyProbe()

  const [settings, setSettings] = useState<Settings | null>(null)
  const [rules, setRules] = useState<RuleSet>({ rules: [], groups: [] })
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null)
  const [wgError, setWgError] = useState<string | null>(null)

  const [newRule, setNewRule] = useState<Rule>({ kind: 'process-name', value: '', route: 'direct' })
  const wgFileRef = useRef<HTMLInputElement>(null)

  // 应用流程分步进度（ApplyStepper）：开始「开始观察」时展开，流程结束保留最终态，用户可手动收起。
  const [showApplyStepper, setShowApplyStepper] = useState(false)

  const flash = (kind: 'ok' | 'err', text: string) => {
    setMsg({ kind, text })
    setTimeout(() => setMsg(null), 5000)
  }

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
      flash('err', `导入失败: ${String(e)}`)
    }
  }, [])

  const run = async (label: string, fn: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await fn()
      flash('ok', `${label}成功`)
    } catch (e) {
      flash('err', `${label}失败: ${String(e)}`)
    } finally {
      setBusy(false)
      void refresh()
    }
  }

  // 热载失败不能静默：settings/规则已落盘、UI 会显示新状态，但 mihomo 运行配置没变，
  // 配置态与运行态会静默分叉。这里失败时单独 flash 一条可区分于「保存失败」的警告。
  const hotReloadIfApplied = useCallback(async () => {
    if (!status?.applied) return
    try {
      await NetPolicyAPI.reload()
    } catch (e) {
      throw new Error(`设置已保存，但引擎热载失败：${String(e)}——规则尚未生效，请重试或重新应用。`)
    }
  }, [status?.applied])

  const saveSettings = () =>
    settings &&
    run('保存设置', async () => {
      await NetPolicyAPI.saveSettings(settings)
      await hotReloadIfApplied()
    })

  const emergencyStop = () => run('紧急停止', () => NetPolicyAPI.emergencyStop())

  // 主开关：开 → setEnabled(true)（触发 apply，同时后端发 6 步 apply-progress 事件）；关 → setEnabled(false)（触发 stop）。
  const toggleEnabled = (enabled: boolean) => {
    if (enabled) setShowApplyStepper(true)
    return run(enabled ? '开始观察' : '停止观察', () => NetPolicyAPI.setEnabled(enabled))
  }

  // 默认出口三路切换
  const changeDefaultRoute = async (route: Route) => {
    if (!settings) return
    if (route === 'wg' && !status?.wg_configured) {
      setWgError('尚未配置 WireGuard——请先在下方「高级 / 排障」填写 WG 出口后再切换。')
      setTimeout(() => setWgError(null), 6000)
      return
    }
    setWgError(null)
    const next: Settings = { ...settings, default_route: route }
    setSettings(next)
    try {
      await NetPolicyAPI.saveSettings(next)
      await hotReloadIfApplied()
      void refreshFast()
    } catch (e) {
      flash('err', `切换出口失败: ${String(e)}`)
      void refresh() // 回滚显示
    }
  }

  const addRule = () =>
    newRule.value.trim() &&
    run('新增规则', async () => {
      const rs = await NetPolicyAPI.saveRule({ ...newRule, value: newRule.value.trim() })
      setRules(rs)
      setNewRule({ ...newRule, value: '' })
      await hotReloadIfApplied()
    })

  const deleteRule = (rule: Rule) =>
    run('删除规则', async () => {
      setRules(await NetPolicyAPI.deleteRule(rule))
      await hotReloadIfApplied()
    })

  const allowBlocked = (e: BlockedEntry, route: 'direct' | 'wg') =>
    run('放行', async () => {
      const rule: Rule = e.dest_ip
        ? { kind: 'ip-cidr', value: `${e.dest_ip}/${e.dest_ip.includes(':') ? 128 : 32}`, route }
        : { kind: 'domain-suffix', value: e.host, route }
      setRules(await NetPolicyAPI.saveRule(rule))
      await hotReloadIfApplied()
    })

  const clearBlocked = () => run('清空被阻断记录', () => NetPolicyAPI.clearBlocked())

  const runVerify = () =>
    run('验证', async () => { await runVerifyProbe() })

  const currentSettings = settings

  // 三路按钮样式辅助
  const segCls = (active: boolean, danger = false) => {
    const base = 'px-3 py-1.5 text-sm rounded-md border transition-colors disabled:opacity-50'
    if (active && danger) return `${base} bg-red-600 text-white border-red-600`
    if (active) return `${base} bg-blue-600 text-white border-blue-600`
    return `${base} border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800`
  }

  const curRoute: Route = currentSettings?.default_route ?? status?.default_route ?? 'direct'

  // 未提权时改路/放行会触发 reload（落防火墙/TUN），必然失败——未拿到 status 前默认放行交互，
  // 一旦拿到 status 且明确未提权，才禁用（platform_supported 前提下）。
  const canElevatedAction = !status || !status.platform_supported || status.elevated

  return (
    <div className="mx-auto max-w-4xl space-y-5">
      {/* ════════════ 1. 标题行 ════════════ */}
      <div className="flex items-center gap-2">
        <ShieldCheck className="text-blue-600" />
        <h1 className="text-lg font-semibold">网络出口策略</h1>
        <button className={btn()} onClick={() => void refresh()} disabled={busy}>
          <RefreshCw size={14} /> 刷新
        </button>
      </div>

      {msg && (
        <div className={`rounded-md px-4 py-2 text-sm ${msg.kind === 'ok' ? 'bg-green-100 text-green-800' : 'bg-red-100 text-red-800'}`}>
          {msg.text}
        </div>
      )}

      {status && !status.platform_supported && (
        <div className="rounded-md bg-yellow-100 px-4 py-2 text-sm text-yellow-800">
          net-policy 仅支持 Windows，当前平台不可用。
        </div>
      )}

      {/* ════════════ 2. 保护状态横幅（权威态：applied/protected/protection_validated/firewall 收敛） ════════════ */}
      {status && status.platform_supported && (
        <ProtectionBanner status={status} exitIp={exitIp} exitIpAt={exitIpAt} />
      )}

      {/* ════════════ 2b. 应用流程分步（主开关开启过程中显示 6 步进度，流程结束保留最终态可手动收起） ════════════ */}
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

      {/* ════════════ 2c. 控制栏（单卡片） ════════════ */}
      <section className="rounded-xl border border-gray-200 bg-white p-4 dark:border-gray-800 dark:bg-gray-900 space-y-4">
        {/* 三路默认出口选择器 */}
        <div className="flex flex-col gap-1.5">
          <div className="text-xs font-medium text-gray-500 uppercase tracking-wide">默认出口</div>
          <div className="flex flex-wrap gap-2">
            <button
              className={segCls(curRoute === 'direct')}
              disabled={busy}
              onClick={() => void changeDefaultRoute('direct')}
              title="原样直连——不改流量，只观察谁在连"
            >
              直连·观察
            </button>
            <button
              className={segCls(curRoute === 'wg')}
              disabled={busy}
              onClick={() => void changeDefaultRoute('wg')}
              title="未命中规则的流量全走 WireGuard 海外出口（需先配好 WG）"
            >
              海外·全VPN
            </button>
            <button
              className={segCls(curRoute === 'blackhole', true)}
              disabled={busy}
              onClick={() => void changeDefaultRoute('blackhole')}
              title="未命中规则的流量全部阻断（空出口）"
            >
              阻断·收紧
            </button>
          </div>
          {wgError && (
            <div className="rounded-md bg-amber-100 px-3 py-1.5 text-xs text-amber-800 dark:bg-amber-950/50 dark:text-amber-300">
              {wgError}
            </div>
          )}
          <p className="text-[11px] text-gray-400 dark:text-gray-500">
            {curRoute === 'direct' && '不改流量，只看谁在连、连到哪、走哪个出口。'}
            {curRoute === 'wg' && '未命中规则的流量走 WireGuard 海外出口，需配好 WG。'}
            {curRoute === 'blackhole' && '未命中规则的流量全阻断——在下方观察主表逐行放行。'}
          </p>
        </div>

        {/* 未提权提示：网络策略要改防火墙/建 TUN,必须管理员 */}
        {status && status.platform_supported && !status.elevated && (
          <div className="rounded-md bg-amber-100 px-3 py-2 text-xs text-amber-800 dark:bg-amber-950/50 dark:text-amber-300">
            ⚠ 未以管理员身份运行——网络策略要改全局防火墙、建 TUN 网卡,均需管理员权限。
            请右键 Zero Desktop「以管理员身份运行」重启后再「开始观察」。
          </div>
        )}

        {/* 主开关 + 紧急停止 */}
        <div className="flex flex-wrap items-center gap-3">
          <button
            className={status?.enabled ? btn('danger') : btn('primary')}
            disabled={busy || (!status?.enabled && !!status && status.platform_supported && !status.elevated)}
            onClick={() => toggleEnabled(!status?.enabled)}
            title={status?.enabled ? '停止引擎（撤防火墙+停 mihomo）' : '开始观察（启动引擎，需管理员）'}
          >
            <Power size={14} />
            {status?.enabled ? '停止' : '开始观察'}
          </button>
          <button className={btn('danger')} onClick={emergencyStop} disabled={busy} title="紧急撤销：停引擎 + 撤防火墙">
            <OctagonX size={14} /> 紧急停止
          </button>
          <span className="text-xs text-gray-500">
            {status?.enabled
              ? '引擎运行中（启动即自动恢复）'
              : '引擎未启动'}
          </span>
        </div>
      </section>

      {/* ════════════ 3. 出口概览（3 个指标卡） ════════════ */}
      <div className="grid grid-cols-3 gap-3">
        <div className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-900">
          <div className="text-[11px] uppercase tracking-wide text-gray-400">直连(本地)</div>
          <div className="mt-1 text-2xl font-semibold text-gray-700 dark:text-gray-200">
            {conns.direct_count}
          </div>
          <div className="text-xs text-gray-500">条连接</div>
        </div>
        <div className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-900">
          <div className="text-[11px] uppercase tracking-wide text-gray-400">VPN 海外</div>
          <div className="mt-1 text-2xl font-semibold text-blue-600">
            {conns.wg_count}
          </div>
          <div className="text-xs text-gray-500">
            条连接 · WG {status?.wg_configured ? '已配置' : '未配置'}
          </div>
        </div>
        <div className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-900">
          <div className="text-[11px] uppercase tracking-wide text-gray-400">阻断</div>
          <div className="mt-1 text-2xl font-semibold text-red-600">
            {conns.other_count}
          </div>
          <div className="text-xs text-gray-500">命中</div>
        </div>
      </div>

      {/* ════════════ 4. 观察主表 ════════════ */}
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

      {/* ════════════ 5. 折叠区 ════════════ */}

      {/* 5a. 观测细节（CurrentStateSection） */}
      <details className="group rounded-xl border border-gray-200 dark:border-gray-800">
        <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-sm font-semibold select-none hover:bg-gray-50 dark:hover:bg-gray-800/40">
          <span className="text-gray-600 group-open:rotate-90 transition-transform">▶</span>
          <SettingsIcon size={14} className="text-gray-500" />
          观测细节
          <span className="ml-auto text-xs font-normal text-gray-400">扫描进程 · 连接明细</span>
        </summary>
        <div className="p-4 pt-0">
          <CurrentStateSection busy={busy} />
        </div>
      </details>

      {/* 5b. 收紧后才用的（被阻断 feed） */}
      <details className="group rounded-xl border border-gray-200 dark:border-gray-800">
        <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-sm font-semibold select-none hover:bg-gray-50 dark:hover:bg-gray-800/40">
          <span className="text-gray-600 group-open:rotate-90 transition-transform">▶</span>
          <OctagonX size={14} className="text-gray-500" />
          收紧后才用的
          <span className="ml-auto text-xs font-normal text-gray-400">
            被阻断 feed（仅 default=blackhole 时有数据）
          </span>
        </summary>
        <div className="p-4 pt-0">
          <ObservatorySection
            blocked={blocked}
            dnsMap={dnsMap}
            busy={busy}
            canAllow={canElevatedAction}
            onAllow={allowBlocked}
            onClear={clearBlocked}
          />
        </div>
      </details>

      {/* 5b2. 临时直连（限时应急） */}
      <TempDirectControl />

      {/* 5b3. 生效路由（优先级 + 删除） */}
      <RoutesSection busy={busy} />

      {/* 5b4. 记录（请求 / 事件 / 进程树） */}
      <RecordsSection />

      {/* 5c. 高级 / 排障 */}
      <details className="group rounded-xl border border-gray-200 dark:border-gray-800">
        <summary className="flex cursor-pointer list-none items-center gap-2 px-4 py-3 text-sm font-semibold select-none hover:bg-gray-50 dark:hover:bg-gray-800/40">
          <span className="text-gray-600 group-open:rotate-90 transition-transform">▶</span>
          <ShieldCheck size={14} className="text-gray-500" />
          高级 / 排障
          <span className="ml-auto text-xs font-normal text-gray-400">WG 配置 · 全景图 · 验证</span>
        </summary>
        <div className="space-y-4 p-4 pt-0">

          {/* WG 配置 + 高级开关 */}
          {settings && (
            <Panel
              title="WireGuard 出口 + 设置"
              collapsible
              right={
                <>
                  <button className={btn()} onClick={() => wgFileRef.current?.click()} disabled={busy} title="从 WireGuard .conf 文件导入">
                    <Upload size={14} /> 导入配置
                  </button>
                  <button className={btn('primary')} onClick={saveSettings} disabled={busy}>保存</button>
                </>
              }
            >
              <input
                ref={wgFileRef}
                type="file"
                accept=".conf,text/plain"
                className="hidden"
                onChange={e => {
                  const f = e.target.files?.[0]
                  if (f) void importWgConf(f)
                  e.target.value = ''
                }}
              />
              <div className="grid grid-cols-2 gap-3 text-sm">
                <label className="flex flex-col gap-1">服务端 IP
                  <input className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={settings.wg.server}
                    onChange={e => setSettings({ ...settings, wg: { ...settings.wg, server: e.target.value } })} placeholder="38.x.x.x（必须是 IP）" />
                </label>
                <label className="flex flex-col gap-1">端口
                  <input type="number" className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={settings.wg.port}
                    onChange={e => setSettings({ ...settings, wg: { ...settings.wg, port: Number(e.target.value) } })} />
                </label>
                <label className="flex flex-col gap-1">隧道内本机 IP
                  <input className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={settings.wg.ip}
                    onChange={e => setSettings({ ...settings, wg: { ...settings.wg, ip: e.target.value } })} placeholder="10.66.66.x" />
                </label>
                <label className="flex flex-col gap-1">MTU
                  <input type="number" className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={settings.wg.mtu}
                    onChange={e => setSettings({ ...settings, wg: { ...settings.wg, mtu: Number(e.target.value) } })} />
                </label>
                <label className="col-span-2 flex flex-col gap-1">本机私钥
                  <input className="rounded border px-2 py-1 font-mono text-xs dark:bg-gray-800 dark:border-gray-700" value={settings.wg.private_key}
                    onChange={e => setSettings({ ...settings, wg: { ...settings.wg, private_key: e.target.value } })} />
                </label>
                <label className="col-span-2 flex flex-col gap-1">服务端公钥
                  <input className="rounded border px-2 py-1 font-mono text-xs dark:bg-gray-800 dark:border-gray-700" value={settings.wg.public_key}
                    onChange={e => setSettings({ ...settings, wg: { ...settings.wg, public_key: e.target.value } })} />
                </label>
                <label className="col-span-2 flex flex-col gap-1">预共享密钥（可选）
                  <input className="rounded border px-2 py-1 font-mono text-xs dark:bg-gray-800 dark:border-gray-700" value={settings.wg.pre_shared_key}
                    onChange={e => setSettings({ ...settings, wg: { ...settings.wg, pre_shared_key: e.target.value } })} />
                </label>
              </div>
              <div className="mt-3 flex flex-wrap items-center gap-4 text-sm">
                <label className="flex items-center gap-2">
                  <input type="checkbox" checked={settings.killswitch_enabled}
                    onChange={e => setSettings({ ...settings, killswitch_enabled: e.target.checked })} />
                  防火墙 kill-switch（fail-closed，<b>建议保持开启</b>）
                </label>
                <label className="flex items-center gap-2">
                  <input type="checkbox" checked={settings.block_ipv6}
                    onChange={e => setSettings({ ...settings, block_ipv6: e.target.checked })} />
                  阻断 IPv6 公网（kill-switch 生效时）
                </label>
                <span className="text-xs text-gray-500">DNS bootstrap: {settings.dns_bootstrap.join(', ')}</span>
              </div>
              {!settings.killswitch_enabled && (
                <div className="mt-2 rounded-md bg-amber-100 px-3 py-1.5 text-xs text-amber-800">
                  ⚠ 关闭 kill-switch = <b>不受保护预览</b>模式，失去 fail-closed 兜底；阻断 IPv6 也不会生效。
                </div>
              )}
            </Panel>
          )}

          {/* 手动规则列表（精细管控） */}
          <Panel
            title={`分流规则（命中=指定出口，未命中=默认${
              curRoute === 'wg' ? '走海外' : curRoute === 'blackhole' ? '阻断' : '直连'
            }）`}
            collapsible
            right={
              <span className="text-xs text-gray-500">
                活跃：直连 {conns.direct_count} · 海外 {conns.wg_count}
              </span>
            }
          >
            <div className="mb-3 flex flex-wrap items-end gap-2 text-sm">
              <select className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={newRule.kind}
                onChange={e => setNewRule({ ...newRule, kind: e.target.value as RuleKind })}>
                {Object.entries(KIND_LABELS).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
              </select>
              <input className="flex-1 rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" placeholder="值（如 steam.exe / example.cn / 1.2.3.0/24）"
                value={newRule.value} onChange={e => setNewRule({ ...newRule, value: e.target.value })}
                onKeyDown={e => e.key === 'Enter' && addRule()} />
              <select className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={newRule.route}
                onChange={e => setNewRule({ ...newRule, route: e.target.value as Route })}>
                <option value="direct">本地直连</option>
                <option value="wg">走 VPN</option>
                <option value="blackhole">阻断</option>
              </select>
              <button className={btn('primary')} onClick={addRule} disabled={busy}><Plus size={14} /> 添加</button>
            </div>
            <ul className="divide-y divide-gray-200 text-sm dark:divide-gray-800">
              {rules.rules.length === 0 && (
                <li className="py-2 text-gray-500">
                  暂无规则——未命中流量全部{curRoute === 'wg' ? '走海外 VPN' : curRoute === 'blackhole' ? '被阻断（空出口）' : '原样直连（观察）'}。
                </li>
              )}
              {rules.rules.map((r, i) => (
                <li key={i} className="flex items-center gap-2 py-1.5">
                  <span className="w-20 text-gray-500">{KIND_LABELS[r.kind]}</span>
                  <span className="flex-1 font-mono text-xs">{r.value}</span>
                  <span className={`rounded px-1.5 py-0.5 text-xs ${
                    r.route === 'direct' ? 'bg-gray-100 text-gray-700' :
                    r.route === 'wg' ? 'bg-blue-100 text-blue-800' :
                    'bg-red-100 text-red-700'
                  }`}>
                    {r.route === 'direct' ? '直连' : r.route === 'wg' ? 'VPN' : '阻断'}
                  </span>
                  <button className="text-gray-400 hover:text-red-600" onClick={() => deleteRule(r)} disabled={busy} title="删除">
                    <Trash2 size={14} />
                  </button>
                </li>
              ))}
            </ul>
            <p className="mt-2 px-1 text-[11px] text-gray-500 dark:text-gray-400">
              提示：把某个程序设为直连，选「程序名」填可执行名（如 steam.exe）+「本地直连」；
              近期有公网连接的进程可在「观测细节 → 扫描进程」里查看。
            </p>
          </Panel>

          {/* 数据通路全景图 */}
          <Panel title="数据通路全景图" collapsible defaultOpen={false}>
            <FlowTopology status={status ?? LOADING_STATUS} conns={conns} settings={settings} />
          </Panel>

          {/* 验证矩阵 */}
          {status && (
            <Panel title="验证矩阵" collapsible defaultOpen={false}>
              <VerifyMatrix status={status} verify={verify} onVerify={runVerify} busy={busy} />
            </Panel>
          )}

          {/* 出口 IP 显示（来自 exitIp probe） */}
          {(exitIp || exitIpAt) && (
            <p className="text-xs text-gray-500">
              出口 IP：<span className="font-mono text-gray-700 dark:text-gray-200">{exitIp || '—'}</span>
              {exitIpAt && <span className="ml-2 text-gray-400">· {exitIpAt}</span>}
            </p>
          )}
        </div>
      </details>
    </div>
  )
}
