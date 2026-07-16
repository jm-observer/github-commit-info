import type { RuleKind, Status } from './api/tauri-client'

/**
 * 各页面/组件共用的展示态小工具（原先都定义在 NetPolicyPage.tsx 内，拆多页后提到这里，
 * 避免每个 page/组件各自重复一份）。纯展示逻辑，不含状态。
 */

export const KIND_LABELS: Record<RuleKind, string> = {
  'process-path': '程序路径',
  'process-name': '程序名',
  'domain-suffix': '域名后缀',
  'domain-keyword': '域名关键词',
  'ip-cidr': 'IP/CIDR',
}

// 首屏占位状态：让全景图在真实探测（~1s 的 PS 调用）回来前就能立刻渲染出「全灰/未起」骨架，
// 而不是空白等待。platform_supported 设 true 避免闪一下「不支持」横幅。真实 status 一到即覆盖。
export const LOADING_STATUS: Status = {
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

/**
 * 页面级标题栏：页名 + 一句话说明。放在每个 page 组件顶部，与 NetPolicyShell 的全局大标题
 * （「网络出口策略」）区分开——这是二级标题，告诉用户「这页看什么/干什么」。
 */
/**
 * 轻量分区标题：小标题 + 可选说明/右侧操作 + 细分隔线，不带独立卡片边框。
 * 用于把页面内多个相关内容块归组，用轻标题和分隔线代替重边框容器。
 */
export function Section({
  title,
  description,
  right,
  children,
}: {
  title: string
  description?: string
  right?: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <section className="space-y-3">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 border-b border-gray-200 pb-1.5 dark:border-gray-800">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">{title}</h3>
        {description && (
          <span className="text-[11px] font-normal normal-case text-gray-400 dark:text-gray-500">{description}</span>
        )}
        {right && <div className="ml-auto flex items-center gap-2 normal-case">{right}</div>}
      </div>
      <div className="space-y-4">{children}</div>
    </section>
  )
}

export function btn(variant: 'primary' | 'danger' | 'ghost' = 'ghost') {
  const base = 'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition-colors disabled:opacity-50'
  if (variant === 'primary') return `${base} bg-blue-600 text-white hover:bg-blue-700`
  if (variant === 'danger') return `${base} bg-red-600 text-white hover:bg-red-700`
  return `${base} border border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800`
}

// 三路按钮样式辅助
export function segCls(active: boolean, danger = false) {
  const base = 'px-3 py-1.5 text-sm rounded-md border transition-colors disabled:opacity-50'
  if (active && danger) return `${base} bg-red-600 text-white border-red-600`
  if (active) return `${base} bg-blue-600 text-white border-blue-600`
  return `${base} border-gray-300 hover:bg-gray-100 dark:border-gray-700 dark:hover:bg-gray-800`
}
