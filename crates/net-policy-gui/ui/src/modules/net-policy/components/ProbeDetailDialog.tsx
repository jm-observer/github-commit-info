import { useEffect } from 'react'
import { X } from 'lucide-react'

export interface ProbeDetail {
  title: string
  status: string
  observed?: string
  description?: string
  source?: string
}

export function ProbeDetailDialog({ detail, onClose }: { detail: ProbeDetail | null; onClose: () => void }) {
  useEffect(() => {
    if (!detail) return
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', closeOnEscape)
    return () => window.removeEventListener('keydown', closeOnEscape)
  }, [detail, onClose])

  if (!detail) return null

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/45 p-4"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose()
      }}
    >
      <div
        className="w-full max-w-lg rounded-xl border border-gray-200 bg-white p-4 shadow-xl dark:border-gray-700 dark:bg-gray-900"
        role="dialog"
        aria-modal="true"
        aria-labelledby="probe-detail-title"
      >
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h3 id="probe-detail-title" className="text-base font-semibold">{detail.title}</h3>
            <p className="mt-0.5 text-xs text-gray-500 dark:text-gray-400">状态：{detail.status}</p>
          </div>
          <button
            type="button"
            className="rounded-md p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-700 dark:hover:bg-gray-800 dark:hover:text-gray-200"
            onClick={onClose}
            aria-label="关闭详情"
          >
            <X size={18} />
          </button>
        </div>

        {detail.observed && (
          <div className="mt-4">
            <div className="mb-1 text-xs font-medium text-gray-500 dark:text-gray-400">完整探测结果</div>
            <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-lg bg-gray-50 p-3 font-mono text-xs text-gray-800 dark:bg-gray-950 dark:text-gray-200">
              {detail.observed}
            </pre>
          </div>
        )}
        {detail.description && <p className="mt-3 text-sm leading-6 text-gray-600 dark:text-gray-300">{detail.description}</p>}
        {detail.source && (
          <div className="mt-3 break-all text-xs text-gray-400">探测目标：<span className="font-mono">{detail.source}</span></div>
        )}
      </div>
    </div>
  )
}
