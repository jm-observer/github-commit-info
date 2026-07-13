import NetPolicyPage from "./modules/net-policy/NetPolicyPage";
import { NetPolicyProbeProvider } from "./modules/net-policy/ProbeContext";

// 独立 app：只有网络策略一个页面，不需要 ShellLayout / react-router（zero-desktop 那套多模块
// 侧边栏在这里没有意义——单页直接渲染）。
export default function App() {
  return (
    <NetPolicyProbeProvider>
      <div className="min-h-screen bg-[var(--bg-app)] p-4">
        <NetPolicyPage />
      </div>
    </NetPolicyProbeProvider>
  );
}
