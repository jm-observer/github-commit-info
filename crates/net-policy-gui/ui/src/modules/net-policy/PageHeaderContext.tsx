import { createContext, useContext, useEffect, useMemo, useState, type DependencyList, type ReactNode } from 'react'

/**
 * 页头操作插槽：让每个页面把自己专属的头部操作（刷新 / 自动刷新开关 / 导出…）注入到
 * NetPolicyShell 顶部页头的右侧，而不是各页在正文里再摆一套，避免与全局页头重复、错位。
 *
 * - Shell 侧用 {@link usePageHeaderSlot} 持有插槽状态并把 provider 包在页面外层。
 * - 页面侧调 {@link usePageHeaderActions} 注入本页操作；不调用的页面沿用 Shell 的默认全局刷新。
 */

interface HeaderSlot {
  node: ReactNode
}

interface PageHeaderContextValue {
  setActions: (slot: HeaderSlot | null) => void
}

const PageHeaderContext = createContext<PageHeaderContextValue>({ setActions: () => {} })

export const PageHeaderProvider = PageHeaderContext.Provider

/** Shell 侧：页头操作插槽的容器状态。`slot` 为 null 表示当前页未注入 → 显示默认全局刷新。 */
export function usePageHeaderSlot() {
  const [slot, setSlot] = useState<HeaderSlot | null>(null)
  const ctx = useMemo<PageHeaderContextValue>(() => ({ setActions: setSlot }), [])
  return { slot, setSlot, ctx }
}

/**
 * 页面侧：注入本页专属的页头操作。
 * @param node 要渲染到页头右侧的操作节点；传 `null` 表示本页页头不放任何操作（覆盖默认全局刷新）。
 * @param deps 依赖数组，随本页状态（如 loading）变化时重新注入，保证按钮态实时。
 */
export function usePageHeaderActions(node: ReactNode, deps: DependencyList) {
  const { setActions } = useContext(PageHeaderContext)
  useEffect(() => {
    setActions({ node })
    return () => setActions(null)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)
}
