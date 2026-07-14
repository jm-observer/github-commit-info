import { Upload } from 'lucide-react'
import type { Settings } from '../api/tauri-client'
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
  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs text-gray-500">配置海外 WireGuard 隧道；保存后，运行中的引擎会自动热加载。</p>
        <div className="flex gap-2">
          <>
          <button className={btn()} onClick={() => wgFileRef.current?.click()} disabled={busy} title="从 WireGuard .conf 文件导入">
            <Upload size={14} /> 导入配置
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
    </div>
  )
}
