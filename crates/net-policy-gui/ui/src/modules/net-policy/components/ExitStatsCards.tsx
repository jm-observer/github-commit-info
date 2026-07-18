import type { ConnectionsSnapshot } from '../api/tauri-client'

/**
 * 概览页出口指标卡（直连/VPN/代理订阅/阻断与其他）。
 */
export function ExitStatsCards({
  conns,
  wgConfigured,
}: {
  conns: ConnectionsSnapshot
  wgConfigured?: boolean
}) {
  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
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
          条连接 · WG {wgConfigured ? '已配置' : '未配置'}
        </div>
      </div>
      <div className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-900">
        <div className="text-[11px] uppercase tracking-wide text-gray-400">代理订阅</div>
        <div className="mt-1 text-2xl font-semibold text-violet-600">
          {conns.proxy_count}
        </div>
        <div className="text-xs text-gray-500">条连接</div>
      </div>
      <div className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-900">
        <div className="text-[11px] uppercase tracking-wide text-gray-400">阻断/其他</div>
        <div className="mt-1 text-2xl font-semibold text-red-600">
          {conns.other_count}
        </div>
        <div className="text-xs text-gray-500">命中</div>
      </div>
    </div>
  )
}
