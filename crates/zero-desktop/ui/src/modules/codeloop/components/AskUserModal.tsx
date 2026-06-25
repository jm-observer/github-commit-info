import { useState } from 'react'
import type { AskUser, Provider } from '../api/tauri-client'

interface Props {
  question: AskUser
  seq: number
  askedBy?: Provider
  onAnswer: (text: string) => void
}

export function AskUserModal({ question, seq, askedBy, onAnswer }: Props) {
  const [text, setText] = useState('')

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="w-[480px] max-w-[90vw] rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
        <div className="mb-1 text-xs text-gray-400">
          {askedBy === 'codex' ? 'Codex' : askedBy === 'claude' ? 'Claude' : '循环'} 需要你拍板（seq {seq}）
        </div>
        <div className="mb-4 whitespace-pre-wrap text-sm font-medium text-gray-800 dark:text-gray-100">
          {question.question?.trim() || (
            <span className="italic text-gray-400">（未能解析出问题文本，可自由作答）</span>
          )}
        </div>

        {question.options && question.options.length > 0 && (
          <div className="mb-4 flex flex-col gap-2">
            {question.options.map((opt, i) => (
              <button
                key={i}
                onClick={() => onAnswer(opt)}
                className="rounded-md border border-gray-300 px-3 py-2 text-left text-sm hover:bg-blue-50 hover:border-blue-400 dark:border-gray-600 dark:hover:bg-blue-900/20"
              >
                {opt}
              </button>
            ))}
          </div>
        )}

        <div className="flex flex-col gap-2">
          <label className="text-xs text-gray-500 dark:text-gray-400">或自由作答</label>
          <textarea
            value={text}
            onChange={e => setText(e.target.value)}
            rows={3}
            className="rounded-md border border-gray-300 bg-white px-2 py-1.5 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
            placeholder="输入你的答复…"
          />
          <button
            onClick={() => text.trim() && onAnswer(text.trim())}
            disabled={!text.trim()}
            className="self-end rounded-md bg-blue-600 px-4 py-1.5 text-sm text-white hover:bg-blue-700 disabled:opacity-50"
          >
            发送
          </button>
        </div>
      </div>
    </div>
  )
}
