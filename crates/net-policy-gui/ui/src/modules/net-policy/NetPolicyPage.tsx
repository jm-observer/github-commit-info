import { NetPolicyControllerProvider } from './NetPolicyController'
import { NetPolicyShell } from './NetPolicyShell'

/**
 * 网络策略模块的入口组件（保留原文件名/导出，App.tsx 的引用路径不用改）。
 * 实际布局/状态已拆到 NetPolicyShell（左侧栏 + 右侧内容区）+ NetPolicyController（跨页共享状态）。
 * NetPolicyProbeProvider 仍由 App.tsx 提供，这里只加一层 controller provider（不重复 probe）。
 */
export default function NetPolicyPage() {
  return (
    <NetPolicyControllerProvider>
      <NetPolicyShell />
    </NetPolicyControllerProvider>
  )
}
