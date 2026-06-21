import { useCallback, useEffect, useRef, useState } from 'react'
import { ShieldCheck, RefreshCw, Plus, Trash2, OctagonX, Upload, Settings as SettingsIcon } from 'lucide-react'
import {
  NetPolicyAPI,
  type Status,
  type Settings,
  type RuleSet,
  type Rule,
  type RuleKind,
  type Route,
} from './api/tauri-client'
import { ProtectionBanner } from './components/ProtectionBanner'
import { FlowTopology } from './components/FlowTopology'
import { ApplyStepper } from './components/ApplyStepper'
import { VerifyMatrix } from './components/VerifyMatrix'
import { CurrentStateSection } from './components/CurrentStateSection'
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
}

// ── 小组件（保留原 Panel / btn） ──────────────────────────────────────────────

function Panel({ title, children, right }: { title: string; children: React.ReactNode; right?: React.ReactNode }) {
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

// ── 主页面（编排） ────────────────────────────────────────────────────────────

export default function NetPolicyPage() {
  // status / conns / verify / exitIp 由全局 NetPolicyProbeProvider 持有（App 启动即跑、跨页面持续轮询）。
  // 本页只管 settings / rules / busy / msg / 新规则草稿。
  const {
    status,
    conns,
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

  const [newRule, setNewRule] = useState<Rule>({ kind: 'process-name', value: '', route: 'direct' })
  const wgFileRef = useRef<HTMLInputElement>(null)

  const flash = (kind: 'ok' | 'err', text: string) => {
    setMsg({ kind, text })
    setTimeout(() => setMsg(null), 5000)
  }

  // 「刷新」按钮：除 provider 的快探测外，把本页持有的 settings/rules 也重读一次（动作后兜底刷新）。
  // settings/rules 是读文件，便宜；status/conns 走 provider 的合并请求，不会重复打 PS。
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

  const saveSettings = () => settings && run('保存设置', () => NetPolicyAPI.saveSettings(settings))
  const applyPolicy = () => run('应用策略', () => NetPolicyAPI.apply())
  const emergencyStop = () => run('紧急停止', () => NetPolicyAPI.emergencyStop())
  const addRule = () =>
    newRule.value.trim() &&
    run('新增规则', async () => {
      const rs = await NetPolicyAPI.saveRule({ ...newRule, value: newRule.value.trim() })
      setRules(rs)
      setNewRule({ ...newRule, value: '' })
    })
  const deleteRule = (index: number) =>
    run('删除规则', async () => setRules(await NetPolicyAPI.deleteRule(index)))

  // 验证（含 exit-ip / dns-hijack）：手动触发，重探测不进 3s 快轮询。
  // provider 已在 App 启动时跑过一次 verify；本按钮和 VerifyMatrix 的「一键自检」复用 provider
  // 的 runVerify（自带单飞 + 自动回填出口 IP）。
  const runVerify = () =>
    run('验证', async () => {
      await runVerifyProbe()
    })

  return (
    <div className="mx-auto max-w-4xl space-y-5">
      <div className="flex items-center gap-2">
        <ShieldCheck className="text-blue-600" />
        <h1 className="text-lg font-semibold">网络出口策略</h1>
        <button className={btn() + ' ml-auto'} onClick={() => void refresh()} disabled={busy}>
          <RefreshCw size={14} /> 刷新
        </button>
        <button className={btn('danger')} onClick={emergencyStop} disabled={busy}>
          <OctagonX size={14} /> 紧急停止
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

      {/* ════════════ 现状查询区（只读 · 进页自动查 · 不改系统） ════════════ */}
      <div className="space-y-3 rounded-xl border border-sky-200/70 bg-sky-50/30 p-3 dark:border-sky-900/40 dark:bg-sky-950/10">
        <div className="flex items-center gap-2 px-1 text-xs font-semibold uppercase tracking-wide text-sky-700 dark:text-sky-300">
          <span>① 本机现状查询</span>
          <span className="font-normal normal-case text-sky-600/70 dark:text-sky-400/70">只读 · 不改系统 · 进页自动刷新</span>
        </div>

        {/* 保护状态横幅（汇总当前真实保护态） */}
        {status && <ProtectionBanner status={status} exitIp={exitIp} exitIpAt={exitIpAt} />}

        {/* 本机现状只读查询区：出口 IP / DNS / 控制器 / 防火墙 / TUN / WG / 活跃连接 / 进程候选
            数据全部来自 NetPolicyProbeProvider（App 启动即跑），切到此页时直接展示，不再触发 verify。 */}
        <CurrentStateSection busy={busy} />

        {/* 数据通路全景图：节点标注「现状可查 / 应用后才有」。占位状态立即渲染骨架。 */}
        <FlowTopology status={status ?? LOADING_STATUS} conns={conns} settings={settings} />
      </div>

      {/* ════════════ 应用策略区（独立显式操作 · 有副作用 · 改系统） ════════════ */}
      <div className="space-y-3 rounded-xl border border-violet-200/70 bg-violet-50/30 p-3 dark:border-violet-900/40 dark:bg-violet-950/10">
        <div className="flex items-center gap-2 px-1 text-xs font-semibold uppercase tracking-wide text-violet-700 dark:text-violet-300">
          <SettingsIcon size={13} />
          <span>② 应用策略</span>
          <span className="font-normal normal-case text-violet-600/70 dark:text-violet-400/70">有副作用 · 会改防火墙/路由/起引擎 · 需显式点击</span>
        </div>

      {/* 应用流程分步 stepper */}
      <ApplyStepper onApply={applyPolicy} busy={busy} canApply={!!status?.wg_configured} />

      {/* WG 设置 */}
      {settings && (
        <Panel
          title="WireGuard 出口 + 设置"
          right={
            <div className="flex gap-2">
              <button className={btn()} onClick={() => wgFileRef.current?.click()} disabled={busy} title="从 WireGuard .conf 文件导入">
                <Upload size={14} /> 导入配置
              </button>
              <button className={btn('primary')} onClick={saveSettings} disabled={busy}>保存</button>
            </div>
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

      {/* 分流规则（叠加活跃连接聚合） */}
      <Panel
        title="分流规则（命中走本地直连，未命中默认走 WG）"
        right={
          <span className="text-xs text-gray-500">
            活跃：直连 {conns.direct_count} · WG {conns.wg_count}
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
            <option value="wg">走 WG</option>
          </select>
          <button className={btn('primary')} onClick={addRule} disabled={busy}><Plus size={14} /> 添加</button>
        </div>
        <ul className="divide-y divide-gray-200 text-sm dark:divide-gray-800">
          {rules.rules.length === 0 && <li className="py-2 text-gray-500">暂无规则——未知流量全部走 WG 海外出口。</li>}
          {rules.rules.map((r, i) => (
            <li key={i} className="flex items-center gap-2 py-1.5">
              <span className="w-20 text-gray-500">{KIND_LABELS[r.kind]}</span>
              <span className="flex-1 font-mono text-xs">{r.value}</span>
              <span className={`rounded px-1.5 py-0.5 text-xs ${r.route === 'direct' ? 'bg-amber-100 text-amber-800' : 'bg-blue-100 text-blue-800'}`}>
                {r.route === 'direct' ? '直连' : 'WG'}
              </span>
              <button className="text-gray-400 hover:text-red-600" onClick={() => deleteRule(i)} disabled={busy} title="删除">
                <Trash2 size={14} />
              </button>
            </li>
          ))}
        </ul>
      </Panel>

        <p className="px-1 text-[11px] text-gray-500 dark:text-gray-400">
          提示：要把某个程序设为直连，可在上方表单选「程序名」+ 填可执行名（如 steam.exe）+「本地直连」添加；
          近期有公网连接的进程清单在上方「本机现状 → 扫描进程」里查看。
        </p>
      </div>

      {/* ════════════ 验证证据（只读评估 · 报告历史结论 vs 当前模型 + 一键自检） ════════════ */}
      {status && <VerifyMatrix status={status} verify={verify} onVerify={runVerify} busy={busy} />}
    </div>
  )
}
