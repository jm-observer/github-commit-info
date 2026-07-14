import NetPolicyPage from "./modules/net-policy/NetPolicyPage";
import { NetPolicyProbeProvider } from "./modules/net-policy/ProbeContext";

// 独立 app：只有网络策略一个模块，不需要 react-router；模块内部侧栏由 NetPolicyShell 管理。
export default function App() {
  return (
    <NetPolicyProbeProvider>
      <div className="min-h-screen bg-[var(--bg-app)] p-4">
        <NetPolicyPage />
      </div>
    </NetPolicyProbeProvider>
  );
}
