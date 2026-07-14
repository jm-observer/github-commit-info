import { useCallback, useEffect, useState } from 'react'
import { RefreshCw } from 'lucide-react'
import { NetPolicyAPI } from '../api/tauri-client'

/**
 * 日志页：mihomo / WireGuard 隧道运行日志（stdout/stderr 落 `mihomo.log`，agent 读取最近 N 行）。
 * 引擎未运行过或日志文件不存在时后端返回空列表，展示空态而非报错。
 */
export function LogsPage() {
  const [lines, setLines] = useState<string[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [autoRefresh, setAutoRefresh] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      setLines(await NetPolicyAPI.getMihomoLog(500))
      setError(null)
    } catch (e) {
      setError(String(e).replace(/^Error:\s*/i, ''))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => { void load() }, [load])

  useEffect(() => {
    if (!autoRefresh) return
    const id = setInterval(() => void load(), 4000)
    return () => clearInterval(id)
  }, [autoRefresh, load])

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs text-gray-500 dark:text-gray-400">
          mihomo / WireGuard 隧道的实时运行日志，最近 {lines.length} 行。
        </p>
        <div className="flex items-center gap-2 text-xs text-gray-500">
          <label className="inline-flex cursor-pointer items-center gap-1.5 select-none">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(e) => setAutoRefresh(e.target.checked)}
              className="h-3.5 w-3.5"
            />
            自动刷新（4s）
          </label>
          <button
            className="inline-flex items-center gap-1 rounded border border-gray-300 px-2 py-1 hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800"
            onClick={() => void load()}
            disabled={loading}
          >
            <RefreshCw size={12} className={loading ? 'animate-spin' : ''} /> 刷新
          </button>
        </div>
      </div>

      {error && (
        <div className="rounded-md bg-red-100 px-3 py-2 text-xs text-red-800 dark:bg-red-950/50 dark:text-red-300">
          读取日志失败：{error}
        </div>
      )}

      {!error && lines.length === 0 ? (
        <div className="rounded-lg border border-dashed border-gray-300 px-4 py-10 text-center text-sm text-gray-500 dark:border-gray-700">
          引擎未运行或暂无日志。
        </div>
      ) : (
        <div className="max-h-[36rem] overflow-auto rounded-lg border border-gray-800 bg-gray-950 p-3">
          <pre className="whitespace-pre-wrap break-all font-mono text-[11px] leading-relaxed text-gray-200">
            {lines.join('\n')}
          </pre>
        </div>
      )}
    </div>
  )
}
