import { OctagonX, Power, Zap } from 'lucide-react'
import type { Route, Status } from '../api/tauri-client'
import { btn, segCls } from '../uiHelpers'

/**
 * 概览页控制卡：三路默认出口选择器 + wgError 提示 + 未提权警告 + 主开关 + 紧急停止。
 * 从原 NetPolicyPage 的「2c. 控制栏」原样搬迁，交互/文案/disabled 条件均不变。
 */
export function ControlBar({
  status,
  busy,
  wgError,
  curRoute,
  onChangeRoute,
  onToggleEnabled,
  onEmergencyStop,
  onResetConnections,
}: {
  status: Status | null
  busy: boolean
  wgError: string | null
  curRoute: Route
  onChangeRoute: (route: Route) => void
  onToggleEnabled: (enabled: boolean) => void
  onEmergencyStop: () => void
  onResetConnections?: () => void
}) {
  return (
    <section className="rounded-xl border border-gray-200 bg-white p-4 dark:border-gray-800 dark:bg-gray-900 space-y-4">
      {/* 三路默认出口选择器 */}
      <div className="flex flex-col gap-1.5">
        <div className="text-xs font-medium text-gray-500 uppercase tracking-wide">默认出口</div>
        <div className="flex flex-wrap gap-2">
          <button
            className={segCls(curRoute === 'direct')}
            disabled={busy}
            onClick={() => onChangeRoute('direct')}
            title="原样直连——不改流量，只观察谁在连"
          >
            直连·观察
          </button>
          <button
            className={segCls(curRoute === 'wg')}
            disabled={busy}
            onClick={() => onChangeRoute('wg')}
            title="未命中规则的流量全走 WireGuard 海外出口（需先配好 WG）"
          >
            海外·全VPN
          </button>
          <button
            className={segCls(curRoute === 'blackhole', true)}
            disabled={busy}
            onClick={() => onChangeRoute('blackhole')}
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
          onClick={() => onToggleEnabled(!status?.enabled)}
          title={status?.enabled ? '停止引擎（撤防火墙+停 mihomo）' : '开始观察（启动引擎，需管理员）'}
        >
          <Power size={14} />
          {status?.enabled ? '停止' : '开始观察'}
        </button>
        <button className={btn('danger')} onClick={onEmergencyStop} disabled={busy} title="紧急撤销：停引擎 + 撤防火墙">
          <OctagonX size={14} /> 紧急停止
        </button>
        {onResetConnections && (
          <button
            className={btn()}
            onClick={onResetConnections}
            disabled={busy || !status?.mihomo_running}
            title="关闭 mihomo 所有活跃连接，逼流量立即用当前出口重新建连（切出口后自动做一次，这里是手动补触发）"
          >
            <Zap size={14} /> 重置连接
          </button>
        )}
        <span className="text-xs text-gray-500">
          {status?.enabled
            ? '引擎运行中（启动即自动恢复）'
            : '引擎未启动'}
        </span>
      </div>
    </section>
  )
}
