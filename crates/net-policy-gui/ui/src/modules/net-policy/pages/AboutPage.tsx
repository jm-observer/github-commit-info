import { useEffect, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { Section } from '../uiHelpers'

interface BuildInfo {
  version: string
  commit: string
  build_time: string
}

export function AboutPage() {
  const [info, setInfo] = useState<BuildInfo | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    void invoke<BuildInfo>('net_policy_build_info')
      .then(setInfo)
      .catch((reason) => setError(String(reason).replace(/^Error:\s*/i, '')))
  }, [])

  return (
    <div className="space-y-7">
      <Section title="关于 net-policy" description="构建信息用于确认当前运行的安装包版本">
        {error && <p className="text-sm text-red-600 dark:text-red-300">读取构建信息失败：{error}</p>}
        {!error && !info && <p className="text-sm text-gray-500 dark:text-gray-400">正在读取构建信息…</p>}
        {info && (
          <dl className="divide-y divide-gray-200 rounded-lg border border-gray-200 text-sm dark:divide-gray-800 dark:border-gray-800">
            <div className="flex items-center justify-between gap-4 px-4 py-3">
              <dt className="text-gray-500 dark:text-gray-400">版本</dt>
              <dd className="font-mono text-gray-900 dark:text-gray-100">{info.version}</dd>
            </div>
            <div className="flex items-center justify-between gap-4 px-4 py-3">
              <dt className="text-gray-500 dark:text-gray-400">编译 commit</dt>
              <dd className="font-mono text-gray-900 dark:text-gray-100">{info.commit}</dd>
            </div>
            <div className="flex items-center justify-between gap-4 px-4 py-3">
              <dt className="text-gray-500 dark:text-gray-400">编译时间</dt>
              <dd className="font-mono text-gray-900 dark:text-gray-100">{info.build_time}</dd>
            </div>
          </dl>
        )}
      </Section>
    </div>
  )
}
