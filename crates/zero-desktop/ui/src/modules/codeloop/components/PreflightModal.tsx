import { useState } from 'react'
import { CheckCircle2, XCircle, AlertTriangle, MinusCircle, Loader2 } from 'lucide-react'
import { CodeloopAPI, type CheckRow, type StartInput } from '../api/tauri-client'

interface Props {
  /** 当前新建表单组装出的 StartInput（自检据此定位会话/校验三方）。 */
  input: StartInput
  onClose: () => void
}

const TIER_LABEL: Record<string, string> = {
  passive: '被动检查',
  version: '版本探针',
  live: '实发往返',
}

function StatusIcon({ status }: { status: CheckRow['status'] }) {
  if (status === 'pass') return <CheckCircle2 size={15} className="text-green-600" />
  if (status === 'fail') return <XCircle size={15} className="text-red-600" />
  if (status === 'warn') return <AlertTriangle size={15} className="text-amber-500" />
  return <MinusCircle size={15} className="text-gray-400" />
}

/**
 * 环境自检面板：被动检查 + 版本探针 + （可选）实发往返合成探针。
 * 实发往返会真实调用一次 CLI（一次性只读会话，不污染真实会话），故用复选框门控。
 */
export function PreflightModal({ input, onClose }: Props) {
  const [live, setLive] = useState(false)
  const [rows, setRows] = useState<CheckRow[] | null>(null)
  const [running, setRunning] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  const run = async () => {
    setRunning(true)
    setErr(null)
    setRows(null)
    try {
      setRows(await CodeloopAPI.preflight(input, live))
    } catch (e) {
      setErr(String(e))
    } finally {
      setRunning(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="flex max-h-[82vh] w-[640px] max-w-[92vw] flex-col rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-sm font-medium text-gray-800 dark:text-gray-100">环境自检</h2>
          <button onClick={onClose} className="text-xs text-gray-400 hover:text-gray-600">
            关闭
          </button>
        </div>

        <label className="mb-3 flex cursor-pointer items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
          <input type="checkbox" checked={live} onChange={e => setLive(e.target.checked)} className="h-3.5 w-3.5" />
          允许实发探针（会真实调用一次 CLI；一次性只读会话，不污染真实会话）
        </label>

        <div className="mb-3">
          <button
            onClick={run}
            disabled={running}
            className="flex items-center gap-1.5 rounded-md bg-blue-600 px-3 py-1.5 text-sm text-white hover:bg-blue-700 disabled:opacity-50"
          >
            {running && <Loader2 size={14} className="animate-spin" />}
            {running ? '自检中…' : '开始自检'}
          </button>
        </div>

        {err && (
          <div className="mb-3 rounded-md bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
            {err}
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-auto">
          {rows?.map(r => (
            <div key={r.id} className="border-b border-gray-100 py-2 last:border-0 dark:border-gray-800">
              <div className="flex items-center gap-2 text-sm text-gray-700 dark:text-gray-200">
                <StatusIcon status={r.status} />
                <span className="flex-1">{r.label}</span>
                <span className="rounded bg-gray-100 px-1.5 py-0.5 text-[10px] text-gray-500 dark:bg-gray-800 dark:text-gray-400">
                  {TIER_LABEL[r.tier] ?? r.tier}
                </span>
              </div>
              <div className="ml-7 mt-0.5 text-xs text-gray-500 dark:text-gray-400">{r.detail}</div>
              {r.raw_excerpt && (
                <pre className="ml-7 mt-1 max-h-32 overflow-auto whitespace-pre-wrap rounded border border-gray-200 bg-gray-50 p-2 text-[11px] text-gray-600 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-300">
                  {r.raw_excerpt}
                </pre>
              )}
            </div>
          ))}
          {rows && rows.length === 0 && (
            <div className="text-xs text-gray-400">无检查项</div>
          )}
        </div>
      </div>
    </div>
  )
}
