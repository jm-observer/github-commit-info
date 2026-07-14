import { WgConfigForm } from '../components/WgConfigForm'
import { useNetPolicyController } from '../NetPolicyController'

export function SettingsPage() {
  const { settings, setSettings, busy, saveSettings, importWgConf, wgFileRef } = useNetPolicyController()

  if (!settings) {
    return <p className="py-8 text-center text-sm text-gray-500">正在读取 WireGuard 配置…</p>
  }

  return (
    <WgConfigForm
      settings={settings}
      setSettings={setSettings}
      busy={busy}
      saveSettings={() => void saveSettings()}
      importWgConf={(file) => void importWgConf(file)}
      wgFileRef={wgFileRef}
    />
  )
}
