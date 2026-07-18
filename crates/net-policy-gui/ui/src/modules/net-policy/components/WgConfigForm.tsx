import { Download, Upload } from 'lucide-react'
import type { AmneziaConfig, ProxySubscriptions, Settings, WgDialerProxy } from '../api/tauri-client'
import { useNetPolicyProbe } from '../ProbeContext'
import { btn, Section } from '../uiHelpers'

/**
 * 设置页「WireGuard 出口 + 设置」卡：WG 字段表单 + 导入配置 + killswitch/block_ipv6 开关 +
 * DNS bootstrap 只读展示。原「5c. 高级 / 排障」里的第一块，原样搬迁（含 collapsible 默认展开）。
 */
export function WgConfigForm({
  settings,
  setSettings,
  busy,
  saveSettings,
  importWgConf,
  wgFileRef,
}: {
  settings: Settings
  setSettings: (s: Settings) => void
  busy: boolean
  saveSettings: () => void
  importWgConf: (file: File) => void
  wgFileRef: React.RefObject<HTMLInputElement>
}) {
  const { status, conns } = useNetPolicyProbe()
  const amnezia = settings.wg.amnezia ?? null
  const defaultAmnezia: AmneziaConfig = {
    jc: 0, jmin: 0, jmax: 0, s1: 0, s2: 0, s3: 0, s4: 0,
    h1: 5, h2: 6, h3: 7, h4: 8,
  }
  const setAmnezia = (patch: Partial<AmneziaConfig>) => {
    setSettings({ ...settings, wg: { ...settings.wg, amnezia: { ...(amnezia ?? defaultAmnezia), ...patch } } })
  }
  const dialerProxy = settings.wg.dialer_proxy ?? null
  const defaultDialerProxy: WgDialerProxy = {
    type: 'socks5', server: '127.0.0.1', port: 7891, username: '', password: '', udp: true, subscription_slot: null,
  }
  const setDialerProxy = (patch: Partial<WgDialerProxy>) => {
    setSettings({ ...settings, wg: { ...settings.wg, dialer_proxy: { ...(dialerProxy ?? defaultDialerProxy), ...patch } } })
  }
  const subscriptions: ProxySubscriptions = settings.proxy_subscriptions ?? { first: null, second: null, active: null }
  const activeSubscription = subscriptions.active

  const exportWgConf = () => {
    if (!settings.wg.private_key || !settings.wg.public_key || !settings.wg.server || !settings.wg.port) return
    if (!window.confirm('导出的配置包含 WireGuard 私钥。请妥善保管，确认继续导出？')) return
    const amneziaLines = settings.wg.amnezia
      ? [
          `Jc = ${settings.wg.amnezia.jc}`,
          `Jmin = ${settings.wg.amnezia.jmin}`,
          `Jmax = ${settings.wg.amnezia.jmax}`,
          `S1 = ${settings.wg.amnezia.s1}`,
          `S2 = ${settings.wg.amnezia.s2}`,
          `S3 = ${settings.wg.amnezia.s3}`,
          `S4 = ${settings.wg.amnezia.s4}`,
          `H1 = ${settings.wg.amnezia.h1}`,
          `H2 = ${settings.wg.amnezia.h2}`,
          `H3 = ${settings.wg.amnezia.h3}`,
          `H4 = ${settings.wg.amnezia.h4}`,
        ]
      : []
    const lines = [
      '[Interface]',
      `PrivateKey = ${settings.wg.private_key}`,
      `Address = ${settings.wg.ip.includes('/') ? settings.wg.ip : `${settings.wg.ip}/32`}`,
      ...amneziaLines,
      '',
      '[Peer]',
      `PublicKey = ${settings.wg.public_key}`,
      ...(settings.wg.pre_shared_key ? [`PresharedKey = ${settings.wg.pre_shared_key}`] : []),
      'AllowedIPs = 0.0.0.0/0',
      `Endpoint = ${settings.wg.server}:${settings.wg.port}`,
      'PersistentKeepalive = 25',
      '',
    ]
    const blob = new Blob([lines.join('\r\n')], { type: 'text/plain;charset=utf-8' })
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = settings.wg.amnezia ? 'net-policy-amneziawg.conf' : 'net-policy-wireguard.conf'
    anchor.style.display = 'none'
    document.body.appendChild(anchor)
    anchor.click()
    anchor.remove()
    window.setTimeout(() => URL.revokeObjectURL(url), 1000)
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs text-gray-500">配置海外 WireGuard 隧道；保存后，运行中的引擎会自动热加载。</p>
        <div className="flex gap-2">
          <>
          <button className={btn()} onClick={() => wgFileRef.current?.click()} disabled={busy} title="从 WireGuard .conf 文件导入">
            <Upload size={14} /> 导入配置
          </button>
          <button className={btn()} onClick={exportWgConf} disabled={busy || !settings.wg.private_key} title="导出当前 WireGuard/AmneziaWG 配置">
            <Download size={14} /> 导出配置
          </button>
          <button className={btn('primary')} onClick={saveSettings} disabled={busy}>保存</button>
          </>
        </div>
      </div>
      <input
        ref={wgFileRef}
        type="file"
        accept=".conf,text/plain"
        className="hidden"
        onChange={e => {
          const f = e.target.files?.[0]
          if (f) importWgConf(f)
          e.target.value = ''
        }}
      />
      <Section title="隧道端点" description="服务端、隧道地址与密钥">
      <div className={`rounded-md px-3 py-2 text-xs ${status?.wg_configured
        ? 'bg-emerald-50 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300'
        : 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400'}`}>
        {status?.wg_configured
          ? `已配置；引擎${status.mihomo_running ? '运行中' : '未运行'}，保持连接已启用（25 秒）。当前经 WG 的活跃连接：${conns.wg_count}。`
          : '尚未完成有效 WireGuard 配置。保存有效配置后，启用策略时会自动建立并保持隧道；是否作为默认出口由概览中的出口选择决定。'}
      </div>
      <div className="grid grid-cols-1 gap-3 text-sm sm:grid-cols-2">
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
      </Section>
      <Section title="WireGuard 上游代理" description="可选；通过 Clash Verge/Mihomo 本地代理建立 WG endpoint 连接">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={dialerProxy !== null}
            onChange={e => setSettings({
              ...settings,
              wg: { ...settings.wg, dialer_proxy: e.target.checked ? (dialerProxy ?? defaultDialerProxy) : null },
            })}
          />
          通过上游代理连接 WireGuard
        </label>
        {dialerProxy && (
          <>
            <label className="mt-3 flex flex-col gap-1 text-sm">代理来源
              <select className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800" value={dialerProxy.subscription_slot == null ? 'manual' : String(dialerProxy.subscription_slot)}
                onChange={e => setDialerProxy({ subscription_slot: e.target.value === 'manual' ? null : Number(e.target.value) })}>
                <option value="manual">手工填写本地代理</option>
                <option value="0" disabled={!subscriptions.first?.url}>订阅 1（当前 {activeSubscription === 0 ? '激活' : '未激活'}）</option>
                <option value="1" disabled={!subscriptions.second?.url}>订阅 2（当前 {activeSubscription === 1 ? '激活' : '未激活'}）</option>
              </select>
            </label>
            {dialerProxy.subscription_slot != null ? (
              <p className="mt-2 text-xs text-blue-700 dark:text-blue-300">WireGuard 将使用当前激活订阅中的节点组；切换订阅并保存后会热加载。</p>
            ) : (
            <>
            <div className="mt-3 grid grid-cols-1 gap-3 text-sm sm:grid-cols-2">
              <label className="flex flex-col gap-1">代理类型
                <select className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800" value={dialerProxy.type}
                  onChange={e => setDialerProxy({ type: e.target.value as WgDialerProxy['type'], udp: e.target.value === 'socks5' })}>
                  <option value="socks5">SOCKS5（推荐，支持 UDP）</option>
                  <option value="http">HTTP CONNECT（仅 TCP）</option>
                </select>
              </label>
              <label className="flex flex-col gap-1">代理地址
                <input className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800" value={dialerProxy.server}
                  onChange={e => setDialerProxy({ server: e.target.value })} placeholder="127.0.0.1" />
              </label>
              <label className="flex flex-col gap-1">代理端口
                <input type="number" min={1} className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800" value={dialerProxy.port}
                  onChange={e => setDialerProxy({ port: Number(e.target.value) })} />
              </label>
              <label className="flex flex-col gap-1">用户名（可选）
                <input className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800" value={dialerProxy.username}
                  onChange={e => setDialerProxy({ username: e.target.value })} autoComplete="off" />
              </label>
              <label className="flex flex-col gap-1">密码（可选）
                <input type="password" className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800" value={dialerProxy.password}
                  onChange={e => setDialerProxy({ password: e.target.value })} autoComplete="new-password" />
              </label>
            </div>
            {dialerProxy.type === 'socks5' ? (
              <p className="mt-2 text-xs text-blue-700 dark:text-blue-300">
                默认使用 UDP relay。请在 Clash Verge 中启用可用的 SOCKS5 端口（常见为 7891）；关闭 Clash Verge TUN，避免双 TUN 路由环。
              </p>
            ) : (
              <p className="mt-2 text-xs text-amber-700 dark:text-amber-300">
                HTTP CONNECT 通常不能转发 WireGuard UDP 握手；仅在代理链明确支持该模式时使用。
              </p>
            )}
            </>
            )}
          </>
        )}
      </Section>
      <Section title="AmneziaWG 混淆" description="可选的 WireGuard 抗 DPI 参数；客户端与服务端必须完全一致">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={amnezia !== null}
            onChange={e => setSettings({
              ...settings,
              wg: { ...settings.wg, amnezia: e.target.checked ? (amnezia ?? defaultAmnezia) : null },
            })}
          />
          启用 AmneziaWG 混淆
        </label>
        {amnezia && (
          <>
            <div className="mt-3 grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
              {(['jc', 'jmin', 'jmax', 's1', 's2', 's3', 's4', 'h1', 'h2', 'h3', 'h4'] as const).map(key => (
                <label key={key} className="flex flex-col gap-1">
                  {key.toUpperCase()}
                  <input
                    type="number"
                    min={0}
                    className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
                    value={amnezia[key]}
                    onChange={e => setAmnezia({ [key]: Number(e.target.value) })}
                  />
                </label>
              ))}
            </div>
            <p className="mt-2 text-xs text-amber-700 dark:text-amber-300">
              参数不匹配会导致握手失败；H1–H4 需为大于 4 且互不相同的值。
            </p>
          </>
        )}
      </Section>
      <Section title="保护选项" description="控制 fail-closed 与 IPv6 行为">
      <div className="flex flex-wrap items-center gap-4 text-sm">
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
      </Section>
      <Section title="观察增强" description="补全观察数据，不改路由">
      <div className="flex flex-wrap items-center gap-4 text-sm">
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={settings.sniffer_enabled}
            onChange={e => setSettings({ ...settings, sniffer_enabled: e.target.checked })} />
          L2 域名嗅探（从 TLS SNI / HTTP Host / QUIC 补全域名）
        </label>
      </div>
      <div className="mt-2 rounded-md bg-gray-100 px-3 py-1.5 text-xs text-gray-600 dark:bg-gray-800 dark:text-gray-400">
        只增强观察表的域名可见性（<code>override-destination=false</code>，不改路由/目标）；纯 IP、ECH、无 SNI、部分 QUIC 仍可能无域名。保存后引擎热加载生效。
      </div>
      </Section>
    </div>
  )
}
