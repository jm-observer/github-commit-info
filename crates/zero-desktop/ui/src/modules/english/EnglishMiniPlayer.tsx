/**
 * EnglishMiniPlayer — ShellLayout 底栏常驻英语听力迷你控件（与 music MiniPlayer 同层级，叠在其上）。
 *
 * - 仅在 AudioPlayerService 单例存在且有句子时渲染（首次进 english 页加载后才出现）；
 *   未进入过 / resetInstance 后自动隐藏，避免空条占位。
 * - 订阅单例事件 + getState() 同步播放状态/当前句/索引，prev/play-pause/next 直接调单例。
 * - 不持有 <audio>，不重建任何东西；和 AnnotationPlayer 共用同一个 service。
 */

import { useEffect, useState } from 'react'
import { Languages, Pause, Play, SkipBack, SkipForward } from 'lucide-react'
import { AudioPlayerService } from './services/AudioPlayerService'
import type { Sentence } from './types'

/** 仅观察"英语 session 是否存在"——BottomPlayerBar 用它决定 tab 启/禁。 */
export function useEnglishPresence(): boolean {
  const [present, setPresent] = useState<boolean>(AudioPlayerService.hasInstance())
  useEffect(() => {
    const onInstanceChanged = () => setPresent(AudioPlayerService.hasInstance())
    window.addEventListener(AudioPlayerService.INSTANCE_CHANGE_EVENT, onInstanceChanged)
    return () => window.removeEventListener(AudioPlayerService.INSTANCE_CHANGE_EVENT, onInstanceChanged)
  }, [])
  return present
}

function useEnglishSession() {
  // 渲染门：单例存在 + 有句子才显示底栏。
  const [present, setPresent] = useState<boolean>(AudioPlayerService.hasInstance())
  // service 内部状态镜像（subscribe 同步）。
  const [isPlaying, setIsPlaying] = useState(false)
  const [sentences, setSentences] = useState<Sentence[]>([])
  const [currentIndex, setCurrentIndex] = useState(0)
  const [currentSentence, setCurrentSentence] = useState<Sentence | null>(null)

  // 单例创建/销毁通知 → 重新评估是否渲染。
  useEffect(() => {
    const onInstanceChanged = () => setPresent(AudioPlayerService.hasInstance())
    window.addEventListener(AudioPlayerService.INSTANCE_CHANGE_EVENT, onInstanceChanged)
    return () => window.removeEventListener(AudioPlayerService.INSTANCE_CHANGE_EVENT, onInstanceChanged)
  }, [])

  // present 翻转后挂事件、同步当前快照；翻 false 时清空。
  useEffect(() => {
    if (!present) {
      setIsPlaying(false)
      setSentences([])
      setCurrentIndex(0)
      setCurrentSentence(null)
      return
    }
    const svc = AudioPlayerService.getInstance()

    // 初始快照。
    const s = svc.getState()
    setIsPlaying(s.isPlaying)
    setSentences(s.sentences)
    setCurrentIndex(s.currentSentenceIndex)
    setCurrentSentence(s.currentSentence)

    const onPlayState = (d: { isPlaying: boolean }) => setIsPlaying(d.isPlaying)
    const onSentenceChange = (d: { sentences: Sentence[]; currentSentenceIndex: number }) => {
      setSentences(d.sentences)
      setCurrentIndex(d.currentSentenceIndex)
      setCurrentSentence(svc.getState().currentSentence)
    }
    const onPlayCountChange = (d: { currentSentenceIndex: number }) => {
      setCurrentIndex(d.currentSentenceIndex)
      setCurrentSentence(svc.getState().currentSentence)
    }

    svc.addEventListener('onPlayStateChange', onPlayState)
    svc.addEventListener('onSentenceChange', onSentenceChange)
    svc.addEventListener('onPlayCountChange', onPlayCountChange)
    return () => {
      // 单例可能在 unmount 前已被 reset；try 避免读已置 null 单例。
      try {
        svc.removeEventListener('onPlayStateChange', onPlayState)
        svc.removeEventListener('onSentenceChange', onSentenceChange)
        svc.removeEventListener('onPlayCountChange', onPlayCountChange)
      } catch { /* ignore */ }
    }
  }, [present])

  return { present, isPlaying, sentences, currentIndex, currentSentence }
}

function truncate(s: string, n: number): string {
  if (!s) return ''
  return s.length > n ? `${s.slice(0, n)}…` : s
}

export default function EnglishMiniPlayer() {
  const { present, isPlaying, sentences, currentIndex, currentSentence } = useEnglishSession()
  // 单例还未建立时直接 null,BottomPlayerBar 会回退到音乐。
  if (!present) return null

  const total = Math.max(sentences.length, 1)
  const display = currentSentence?.text ? truncate(currentSentence.text, 60) : '准备中…'

  const onToggle = () => { try { AudioPlayerService.getInstance().togglePlayPause() } catch { /* ignore */ } }
  const onPrev = () => { try { AudioPlayerService.getInstance().previousSentence() } catch { /* ignore */ } }
  const onNext = () => { try { AudioPlayerService.getInstance().nextSentence() } catch { /* ignore */ } }

  // 「内嵌 body」形态: 不带外层 h-16/border-t/bg/padding,由 BottomPlayerBar 统一包裹。
  // 列宽与音乐 MiniPlayerBody 一致(w-56 / flex-1 / w-32)。主色用 amber 与音乐 blue 做区分。
  return (
    <div className="flex h-full w-full items-center gap-3 px-4">
      {/* 图标 + 标题 + 索引（对齐音乐封面块） */}
      <div className="flex w-56 min-w-0 flex-shrink-0 items-center gap-3">
        <div className="flex h-11 w-11 flex-shrink-0 items-center justify-center rounded bg-amber-50 text-amber-600 dark:bg-amber-900/30 dark:text-amber-400">
          <Languages size={20} />
        </div>
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 truncate text-sm font-medium text-gray-900 dark:text-gray-100">
            英语听力
            {isPlaying && (
              <span className="flex items-end gap-0.5 text-amber-500 dark:text-amber-400" aria-label="正在播放">
                <span className="eq-bar" style={{ height: 10 }} />
                <span className="eq-bar" style={{ height: 10 }} />
                <span className="eq-bar" style={{ height: 10 }} />
              </span>
            )}
          </div>
          <div className="truncate text-xs text-gray-500 dark:text-gray-400">
            第 {currentIndex + 1} / {total} 句
          </div>
        </div>
      </div>

      {/* 传输 + 句子文本（对齐音乐"传输+进度"两行结构） */}
      <div className="flex flex-1 flex-col items-center gap-1">
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={onPrev}
            title="上一句"
            className="rounded p-1 text-gray-600 transition-colors hover:text-gray-900 dark:text-gray-300 dark:hover:text-white"
          >
            <SkipBack size={18} />
          </button>
          <button
            type="button"
            onClick={onToggle}
            title={isPlaying ? '暂停' : '播放'}
            className="flex h-9 w-9 items-center justify-center rounded-full bg-amber-500 text-white transition-colors hover:bg-amber-600"
          >
            {isPlaying ? <Pause size={18} /> : <Play size={18} className="ml-0.5" />}
          </button>
          <button
            type="button"
            onClick={onNext}
            title="下一句"
            className="rounded p-1 text-gray-600 transition-colors hover:text-gray-900 dark:text-gray-300 dark:hover:text-white"
          >
            <SkipForward size={18} />
          </button>
        </div>
        <div
          className="w-full max-w-xl truncate text-center text-xs text-gray-500 dark:text-gray-400"
          title={currentSentence?.text || ''}
        >
          {display}
        </div>
      </div>

      {/* 右侧占位：与音乐 MiniPlayer 的 volume 列对齐，避免两条按钮列错位。 */}
      <div className="w-32 flex-shrink-0" aria-hidden />
    </div>
  )
}
