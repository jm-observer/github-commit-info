/**
 * AudioPlayer — 音频播放器 UI（Tailwind 重写，无 AntD）。
 */

import { useState, useEffect, useCallback } from 'react'
import { ChevronLeft, ChevronRight, Play, Pause, BookmarkPlus, BookmarkCheck, AlertTriangle, Replace } from 'lucide-react'
import { AudioPlayerService } from '../services/AudioPlayerService'
import ApiService from '../services/ApiService'
import type { Sentence } from '../types'
import { Button } from '../../speech/components/ui/Button'
import ReplaceAudioModal from './ReplaceAudioModal'
import { readAutoStartPref, writeAutoStartPref } from '../autoStartPref'

interface AudioPlayerProps {
  showAnnotation?: boolean
  showReport?: boolean
  showReplace?: boolean
  showOptions?: boolean
  onPlayComplete?: () => void
}

export default function AudioPlayer({
  showAnnotation = true,
  showReport = true,
  showReplace = true,
  showOptions = true,
  onPlayComplete
}: AudioPlayerProps) {
  const [isPlaying, setIsPlaying] = useState(false)
  const [currentSentenceIndex, setCurrentSentenceIndex] = useState(0)
  const [playCount, setPlayCount] = useState(0)
  const [maxPlayCount, setMaxPlayCount] = useState(2)
  const [stopMode, setStopMode] = useState<'halfHour' | 'roundEnd' | null>('halfHour')
  const [showText, setShowText] = useState(true)
  const [statusText, setStatusText] = useState('准备中...')
  const [sentences, setSentences] = useState<Sentence[]>([])
  const [currentSentence, setCurrentSentence] = useState<Sentence | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const [replaceOpen, setReplaceOpen] = useState(false)
  const [autoStartPref, setAutoStartPref] = useState<boolean>(readAutoStartPref)

  const audioService = AudioPlayerService.getInstance()

  const updateCurrentSentence = useCallback(() => {
    setCurrentSentence(audioService.getState().currentSentence)
  }, [audioService])

  useEffect(() => {
    const onPlayStateChange = (data: { isPlaying: boolean }) => setIsPlaying(data.isPlaying)
    const onStatusTextChange = (data: { statusText: string }) => setStatusText(data.statusText)
    const onSentenceChange = (data: { sentences: Sentence[]; currentSentenceIndex: number }) => {
      setSentences(data.sentences)
      setCurrentSentenceIndex(data.currentSentenceIndex)
      updateCurrentSentence()
    }
    const onPlayCompleteHandler = () => onPlayComplete?.()
    const onPlayCountChange = (data: {
      playCount: number; currentSentenceIndex: number;
      currentAudioIndex: number; maxPlayCount: number
    }) => {
      setPlayCount(data.playCount)
      setCurrentSentenceIndex(data.currentSentenceIndex)
      setMaxPlayCount(data.maxPlayCount)
      updateCurrentSentence()
    }
    const onTextToggle = (data: { showText: boolean }) => setShowText(data.showText)

    audioService.addEventListener('onPlayStateChange', onPlayStateChange)
    audioService.addEventListener('onStatusTextChange', onStatusTextChange)
    audioService.addEventListener('onSentenceChange', onSentenceChange)
    audioService.addEventListener('onPlayComplete', onPlayCompleteHandler)
    audioService.addEventListener('onPlayCountChange', onPlayCountChange)
    audioService.addEventListener('onTextToggle', onTextToggle)

    const state = audioService.getState()
    setIsPlaying(state.isPlaying)
    setCurrentSentenceIndex(state.currentSentenceIndex)
    setPlayCount(state.playCount)
    setMaxPlayCount(state.maxPlayCount)
    setStopMode(state.stopMode || 'halfHour')
    setStatusText(state.statusText)
    setSentences(state.sentences)
    setCurrentSentence(state.currentSentence)

    return () => {
      audioService.removeEventListener('onPlayStateChange', onPlayStateChange)
      audioService.removeEventListener('onStatusTextChange', onStatusTextChange)
      audioService.removeEventListener('onSentenceChange', onSentenceChange)
      audioService.removeEventListener('onPlayComplete', onPlayCompleteHandler)
      audioService.removeEventListener('onPlayCountChange', onPlayCountChange)
      audioService.removeEventListener('onTextToggle', onTextToggle)
    }
  }, [audioService, onPlayComplete, updateCurrentSentence])

  const handleToggleAnnotation = async () => {
    if (!currentSentence?.id) { setActionError('无法获取句子信息'); return }
    const newStatus = !currentSentence.is_annotated
    try {
      if (newStatus) await ApiService.getInstance().annotateSentence(currentSentence.id, currentSentence.text)
      else await ApiService.getInstance().unannotateSentence(currentSentence.id)
      audioService.toggleAnnotation()
      setActionError(null)
    } catch (error: any) {
      setActionError('操作失败: ' + (error?.message || '请重试'))
    }
  }

  const handleToggleReportError = async () => {
    if (!currentSentence?.id) { setActionError('无法获取句子信息'); return }
    const newStatus = !currentSentence.has_error
    try {
      if (newStatus) await ApiService.getInstance().reportError('sentence', currentSentence.id)
      else await ApiService.getInstance().unreportError('sentence', currentSentence.id)
      audioService.toggleReportError()
      setActionError(null)
    } catch (error: any) {
      setActionError('操作失败: ' + (error?.message || '请重试'))
    }
  }

  return (
    <div className="flex flex-col gap-4 p-4">
      {/* 操作反馈 */}
      {actionError && (
        <div className="rounded-md bg-red-50 px-3 py-2 text-sm text-red-600 dark:bg-red-900/30 dark:text-red-400">
          {actionError}
          <button className="ml-2 underline" onClick={() => setActionError(null)}>关闭</button>
        </div>
      )}

      {/* 状态(播放时左侧补一组频谱条做"跳动"指示,直观判断 service 状态有没有同步) */}
      <div className="flex items-center gap-2 text-sm text-gray-500 dark:text-gray-400">
        {isPlaying && (
          <span className="flex items-end gap-0.5 text-amber-500 dark:text-amber-400" aria-label="正在播放">
            <span className="eq-bar" />
            <span className="eq-bar" />
            <span className="eq-bar" />
          </span>
        )}
        <span>{statusText}</span>
      </div>

      {/* 当前句子 */}
      <div className="min-h-12 rounded-lg bg-gray-50 px-4 py-3 dark:bg-gray-800">
        <div className="flex items-start gap-2">
          {currentSentence?.is_annotated && (
            <span className="mt-0.5 text-blue-500" title="已标注">
              <BookmarkCheck size={16} />
            </span>
          )}
          {currentSentence?.has_error && (
            <span className="mt-0.5 text-yellow-500" title="已报错">
              <AlertTriangle size={16} />
            </span>
          )}
          {showText && (
            <span className="text-sm leading-relaxed text-gray-800 dark:text-gray-200">
              {currentSentence?.text ?? ''}
            </span>
          )}
        </div>
      </div>

      {/* 控制按钮(播放时给主按钮外圈一圈 ping 涟漪,跟左上"跳动"指示对应,
          失同步时一眼看出来) */}
      <div className="flex items-center justify-center gap-3">
        <Button variant="outline" size="sm" onClick={() => audioService.previousSentence()} title="上一句">
          <ChevronLeft size={16} />
          上一个
        </Button>
        <span className="relative inline-flex">
          {isPlaying && (
            <span className="absolute inset-0 -m-1 inline-flex animate-ping rounded-md bg-blue-400 opacity-30" />
          )}
          <Button variant="primary" size="md" onClick={() => audioService.togglePlayPause()} title={isPlaying ? '暂停' : '播放'}>
            {isPlaying ? <Pause size={16} /> : <Play size={16} />}
            {isPlaying ? '暂停' : '播放'}
          </Button>
        </span>
        <Button variant="outline" size="sm" onClick={() => audioService.nextSentence()} title="下一句">
          下一个
          <ChevronRight size={16} />
        </Button>
      </div>

      {/* 标注 / 报错 / 替换 */}
      {(showAnnotation || showReport || showReplace) && (
        <div className="flex items-center gap-2">
          {showAnnotation && (
            <Button
              variant={currentSentence?.is_annotated ? 'primary' : 'outline'}
              size="sm"
              onClick={handleToggleAnnotation}
            >
              {currentSentence?.is_annotated ? <BookmarkCheck size={14} /> : <BookmarkPlus size={14} />}
              {currentSentence?.is_annotated ? '已标注' : '标注'}
            </Button>
          )}
          {showReport && (
            <Button
              variant={currentSentence?.has_error ? 'danger' : 'outline'}
              size="sm"
              onClick={handleToggleReportError}
            >
              <AlertTriangle size={14} />
              {currentSentence?.has_error ? '已报错' : '报错'}
            </Button>
          )}
          {showReplace && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => { audioService.stopAudio(); setActionError(null); setReplaceOpen(true) }}
              disabled={!currentSentence?.id}
              title="编辑文本并用 TTS 重新生成、替换当前音频"
            >
              <Replace size={14} />
              替换
            </Button>
          )}
        </div>
      )}

      {/* 选项 */}
      {showOptions && (
        <div className="flex flex-col gap-3 rounded-lg border border-gray-200 p-3 dark:border-gray-700">
          {/* 播放次数 */}
          <div className="flex items-center gap-2">
            <span className="text-xs text-gray-500 dark:text-gray-400">播放次数：</span>
            {[1, 2, 4].map(n => (
              <button
                key={n}
                onClick={() => audioService.setMaxPlayCount(n)}
                className={[
                  'h-7 w-7 rounded-md text-xs font-medium transition-colors',
                  maxPlayCount === n
                    ? 'bg-blue-600 text-white'
                    : 'border border-gray-300 text-gray-700 hover:bg-gray-100 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800'
                ].join(' ')}
              >
                {n}
              </button>
            ))}
          </div>

          {/* 半小时后停止 */}
          <label className="flex items-center gap-2 text-xs text-gray-700 dark:text-gray-300">
            <input
              type="checkbox"
              checked={stopMode === 'halfHour'}
              onChange={e => {
                const mode = e.target.checked ? 'halfHour' as const : null
                setStopMode(mode)
                audioService.setStopMode(mode)
              }}
              className="rounded"
            />
            半小时后停止
          </label>

          {/* 启动 / 首次进入页面时自动播放(写 localStorage,跨重启保留)。
              下次软件启动 / 切换 annotated <-> all 触发的全套 init 才会读这个值。 */}
          <label className="flex items-center gap-2 text-xs text-gray-700 dark:text-gray-300">
            <input
              type="checkbox"
              checked={autoStartPref}
              onChange={e => {
                const v = e.target.checked
                setAutoStartPref(v)
                writeAutoStartPref(v)
              }}
              className="rounded"
            />
            启动时自动播放
          </label>
        </div>
      )}

      {/* 进度 */}
      <div className="flex items-center justify-between text-xs text-gray-400 dark:text-gray-500">
        <span>句子 {currentSentenceIndex + 1} / {sentences.length}</span>
        <span>播放次数 {playCount + 1} / {maxPlayCount}</span>
      </div>

      {/* 替换弹窗 */}
      {replaceOpen && currentSentence?.id != null && (
        <ReplaceAudioModal
          open={replaceOpen}
          sentenceId={currentSentence.id}
          audioId={audioService.getState().currentAudio?.id ?? null}
          initialText={currentSentence.text ?? ''}
          onClose={() => setReplaceOpen(false)}
          onReplaced={(newText) => {
            void audioService.applyReplacedCurrentSentence(newText).then(updateCurrentSentence)
          }}
        />
      )}
    </div>
  )
}
