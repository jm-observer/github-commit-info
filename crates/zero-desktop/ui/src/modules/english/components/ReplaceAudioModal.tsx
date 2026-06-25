/**
 * ReplaceAudioModal — 「替换」弹窗：编辑文本 + 语速 → **一次性为所有音色生成 TTS 试听** →
 * 用户逐个试听后选中某个音色 → 确认替换当前句子的文本与音频。
 *
 * 设计取舍：不再让用户「先选音色再生成」，而是把全部音色都生成好并列试听、横向对比后再选。
 * 改文本/语速会作废已生成的全部预览，需重新生成（保证「听到的 == 存下的」）。逐个音色顺序
 * 生成（上游 TTS 单机串行），生成一个就立即出现在列表里，用户可边等边听。
 */

import { useEffect, useState, useCallback } from 'react'
import { convertFileSrc } from '@tauri-apps/api/core'
import { X, Loader2, Sparkles } from 'lucide-react'
import ApiService from '../services/ApiService'
import { Button } from '../../speech/components/ui/Button'

interface VoiceOption {
  id: string
  label: string
}

/** 单个音色的预览结果：成功带 src，失败带 error。 */
interface PreviewItem {
  voiceId: string
  label: string
  src?: string
  path?: string
  error?: string
}

interface ReplaceAudioModalProps {
  open: boolean
  sentenceId: number
  audioId: number | null
  initialText: string
  onClose: () => void
  /** 替换成功回调，参数为新文本（父组件据此刷新播放器状态）。 */
  onReplaced: (newText: string) => void
}

/** 把 /voices 各种可能形态归一化成 {id,label} 列表。 */
function normalizeVoices(raw: any): VoiceOption[] {
  const toOpt = (v: any): VoiceOption | null => {
    if (typeof v === 'string') return { id: v, label: v }
    if (v && typeof v === 'object') {
      const id = v.id ?? v.voice_id ?? v.name ?? v.value
      if (id != null) return { id: String(id), label: String(v.name ?? v.label ?? id) }
    }
    return null
  }
  let list: any[] = []
  if (Array.isArray(raw)) list = raw
  else if (raw && typeof raw === 'object') {
    if (Array.isArray(raw.voices)) list = raw.voices
    else if (Array.isArray(raw.data)) list = raw.data
    else if (Array.isArray(raw.spk)) list = raw.spk
  }
  return list.map(toOpt).filter((x): x is VoiceOption => x !== null)
}

export default function ReplaceAudioModal({
  open, sentenceId, audioId, initialText, onClose, onReplaced
}: ReplaceAudioModalProps) {
  const [text, setText] = useState(initialText)
  const [voices, setVoices] = useState<VoiceOption[]>([])
  const [speed, setSpeed] = useState(1.0)
  const [previews, setPreviews] = useState<PreviewItem[]>([])
  const [selectedVoiceId, setSelectedVoiceId] = useState<string | null>(null)
  const [generating, setGenerating] = useState(false)
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null)
  const [replacing, setReplacing] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // 打开时重置 + 拉音色库。
  useEffect(() => {
    if (!open) return
    setText(initialText)
    setSpeed(1.0)
    setPreviews([])
    setSelectedVoiceId(null)
    setProgress(null)
    setError(null)
    let cancelled = false
    ApiService.getInstance().getVoices()
      .then(raw => {
        if (cancelled) return
        setVoices(normalizeVoices(raw))
      })
      .catch(e => { if (!cancelled) setError('加载音色失败：' + (e?.message || e)) })
    return () => { cancelled = true }
  }, [open, initialText])

  // 任何会改变音频内容的参数变化 → 作废已生成的全部预览。
  const invalidatePreviews = useCallback(() => {
    setPreviews([])
    setSelectedVoiceId(null)
    setProgress(null)
  }, [])

  // 为全部音色逐个生成试听；生成一个就追加进列表（用户可边等边听）。
  const handleGenerateAll = async () => {
    const trimmed = text.trim()
    if (!trimmed) { setError('文本不能为空'); return }
    if (voices.length === 0) { setError('没有可用音色'); return }
    setGenerating(true)
    setError(null)
    setPreviews([])
    setSelectedVoiceId(null)
    const stamp = Date.now()
    const api = ApiService.getInstance()
    for (let i = 0; i < voices.length; i++) {
      const v = voices[i]
      setProgress({ done: i, total: voices.length })
      try {
        const path = await api.previewTts(trimmed, v.id, speed)
        // 带时间戳击穿 webview 资源缓存，确保试听的是这一轮新生成的。
        const src = convertFileSrc(path) + '?t=' + stamp
        setPreviews(prev => [...prev, { voiceId: v.id, label: v.label, path, src }])
        // 默认选中第一个成功的音色。
        setSelectedVoiceId(prev => prev ?? v.id)
      } catch (e: any) {
        setPreviews(prev => [...prev, { voiceId: v.id, label: v.label, error: e?.message || String(e) }])
      }
    }
    setProgress({ done: voices.length, total: voices.length })
    setGenerating(false)
  }

  const handleConfirm = async () => {
    if (audioId == null) { setError('当前句子没有可替换的音频'); return }
    const chosen = previews.find(p => p.voiceId === selectedVoiceId && p.path)
    if (!chosen?.path) { setError('请先生成并选择一个音色'); return }
    setReplacing(true)
    setError(null)
    try {
      await ApiService.getInstance().replaceSentenceAudio(sentenceId, audioId, text.trim(), chosen.path)
      onReplaced(text.trim())
      onClose()
    } catch (e: any) {
      setError(e?.message || String(e))
    } finally {
      setReplacing(false)
    }
  }

  if (!open) return null

  const hasSelectable = previews.some(p => p.path)

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
      onClick={onClose}
    >
      <div
        className="flex max-h-[90vh] w-full max-w-lg flex-col rounded-xl bg-white p-5 shadow-xl dark:bg-gray-900"
        onClick={e => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h3 className="text-base font-semibold text-gray-800 dark:text-gray-100">替换句子音频</h3>
          <button
            className="rounded-md p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800"
            onClick={onClose}
            title="关闭"
          >
            <X size={18} />
          </button>
        </div>

        {error && (
          <div className="mb-3 rounded-md bg-red-50 px-3 py-2 text-sm text-red-600 dark:bg-red-900/30 dark:text-red-400">
            {error}
          </div>
        )}

        {/* 文本 */}
        <label className="mb-1 block text-xs text-gray-500 dark:text-gray-400">文本</label>
        <textarea
          value={text}
          onChange={e => { setText(e.target.value); invalidatePreviews() }}
          rows={3}
          className="mb-3 w-full resize-y rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-800 focus:border-blue-500 focus:outline-none dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
          placeholder="输入要朗读的英文文本"
        />

        {/* 语速 + 一键生成全部试听 */}
        <div className="mb-3 flex items-end gap-3">
          <div className="w-40">
            <label className="mb-1 block text-xs text-gray-500 dark:text-gray-400">语速 {speed.toFixed(1)}x</label>
            <input
              type="range"
              min={0.5}
              max={2.0}
              step={0.1}
              value={speed}
              onChange={e => { setSpeed(parseFloat(e.target.value)); invalidatePreviews() }}
              className="w-full"
            />
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={handleGenerateAll}
            disabled={generating || !text.trim() || voices.length === 0}
          >
            {generating ? <Loader2 size={14} className="animate-spin" /> : <Sparkles size={14} />}
            {generating
              ? `生成中 ${progress?.done ?? 0}/${progress?.total ?? voices.length}`
              : `生成全部试听（${voices.length} 个音色）`}
          </Button>
        </div>

        {/* 全部音色试听列表：逐个出现，选中一个用于替换 */}
        <div className="mb-4 min-h-0 flex-1 overflow-y-auto rounded-lg border border-gray-200 dark:border-gray-700">
          {previews.length === 0 ? (
            <div className="px-3 py-6 text-center text-sm text-gray-400 dark:text-gray-500">
              点击「生成全部试听」为每个音色生成预览，听完后选择一个替换。
            </div>
          ) : (
            <ul className="divide-y divide-gray-100 dark:divide-gray-800">
              {previews.map(p => (
                <li
                  key={p.voiceId}
                  className={`flex items-center gap-3 px-3 py-2 ${
                    p.path && selectedVoiceId === p.voiceId ? 'bg-blue-50 dark:bg-blue-900/20' : ''
                  }`}
                >
                  <input
                    type="radio"
                    name="voice-pick"
                    className="shrink-0"
                    disabled={!p.path}
                    checked={selectedVoiceId === p.voiceId}
                    onChange={() => setSelectedVoiceId(p.voiceId)}
                  />
                  <span className="w-32 shrink-0 truncate text-sm text-gray-700 dark:text-gray-200" title={p.label}>
                    {p.label}
                  </span>
                  {p.error ? (
                    <span className="flex-1 truncate text-xs text-red-500" title={p.error}>生成失败：{p.error}</span>
                  ) : (
                    <audio key={p.src} src={p.src} controls preload="none" className="h-8 flex-1" />
                  )}
                </li>
              ))}
              {generating && (
                <li className="flex items-center gap-2 px-3 py-2 text-xs text-gray-400">
                  <Loader2 size={12} className="animate-spin" /> 正在生成剩余音色…
                </li>
              )}
            </ul>
          )}
        </div>

        {/* 操作 */}
        <div className="flex justify-end gap-2">
          <Button variant="outline" size="sm" onClick={onClose} disabled={replacing}>取消</Button>
          <Button
            variant="primary"
            size="sm"
            onClick={handleConfirm}
            disabled={replacing || !hasSelectable || !selectedVoiceId || audioId == null}
            title={!hasSelectable ? '请先生成并选择一个音色' : undefined}
          >
            {replacing ? <Loader2 size={14} className="animate-spin" /> : null}
            {replacing ? '替换中...' : '确认替换'}
          </Button>
        </div>
      </div>
    </div>
  )
}
