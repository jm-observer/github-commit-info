/**
 * AudioPlayerService — 平台无关的音频播放服务（从 english/shared 内化）。
 * 无 AntD / AntD-icons 依赖。
 */

import type HtmlAudioAdapter from '../adapters/HtmlAudioAdapter'
import type FileCacheManager from './FileCacheManager'
import type { Sentence, Audio, EnvConfig, AudioPlayerEventType, AudioPlayerEventData, AudioPlayerState } from '../types'

const EVENT_ERROR_LOG_WINDOW_MS = 30_000

export class AudioPlayerService {
  private static instance: AudioPlayerService | null = null

  private audioAdapter: HtmlAudioAdapter
  private fileCacheManager: FileCacheManager
  private envConfig: EnvConfig

  private isPlaying = false
  private currentSentenceIndex = 0
  private currentAudioIndex = 0
  private playCount = 0
  private stopMode: 'halfHour' | 'roundEnd' | null = 'halfHour'
  private maxPlayCount = 2
  private audioSwitchDelay = 1000

  private stopTimer: ReturnType<typeof setTimeout> | null = null
  private delayPlayTimer: ReturnType<typeof setTimeout> | null = null

  private statusText = '准备中...'
  private sentences: Sentence[] = []

  private eventListeners: Record<AudioPlayerEventType, Array<(data: any) => void>> = {
    onPlayStateChange: [], onStatusTextChange: [], onSentenceChange: [],
    onPlayComplete: [], onPlayNext: [], onPlayPrevious: [], onToggleAnnotation: [],
    onToggleReportError: [], onPlayCountChange: [], onTextToggle: [], onAwaitShadow: []
  }

  // 跟读闸门：开启后，一句参考音频自动播完不再自动进下一句，而是 emit onAwaitShadow，
  // 把推进权交给 ShadowController（判分通过 → nextSentence / 失败 → replayCurrent）。
  private shadowGate = false

  private isInitialized = false
  private eventErrorLogState: Record<string, { lastLogAt: number; suppressedCount: number }> = {}

  private constructor(
    audioAdapter: HtmlAudioAdapter,
    fileCacheManager: FileCacheManager,
    envConfig: EnvConfig
  ) {
    this.audioAdapter = audioAdapter
    this.fileCacheManager = fileCacheManager
    this.envConfig = envConfig
  }

  static getInstance(
    audioAdapter?: HtmlAudioAdapter,
    fileCacheManager?: FileCacheManager,
    envConfig?: EnvConfig
  ): AudioPlayerService {
    if (!AudioPlayerService.instance) {
      if (!audioAdapter || !fileCacheManager || !envConfig) {
        throw new Error('首次调用 getInstance 必须提供所有参数')
      }
      AudioPlayerService.instance = new AudioPlayerService(audioAdapter, fileCacheManager, envConfig)
      AudioPlayerService.instance._init()
      AudioPlayerService._notifyInstanceChanged()
    }
    return AudioPlayerService.instance
  }

  static resetInstance(): void {
    AudioPlayerService.instance = null
    AudioPlayerService._notifyInstanceChanged()
  }

  /** 当前是否已有可用单例（供底栏 MiniPlayer 判断是否渲染英语条）。 */
  static hasInstance(): boolean {
    return AudioPlayerService.instance !== null
  }

  /** 单例创建/销毁通知 channel；前端订阅可在英语初始化/重置时同步隐藏/显示底栏控件。 */
  static readonly INSTANCE_CHANGE_EVENT = 'english-audio-instance-changed'
  private static _notifyInstanceChanged(): void {
    if (typeof window !== 'undefined') {
      window.dispatchEvent(new CustomEvent(AudioPlayerService.INSTANCE_CHANGE_EVENT))
    }
  }

  private _init(): void {
    if (!this.isInitialized) this._initAudioContext()
  }

  private _initAudioContext(): void {
    this.audioAdapter.onPlay(() => {
      this.isPlaying = true
      this._updateStatusText('播放中...')
      this._triggerEvent('onPlayStateChange', { isPlaying: true })
    })
    this.audioAdapter.onPause(() => {
      this._updateStatusText('已暂停')
      this._triggerEvent('onPlayStateChange', { isPlaying: false })
    })
    this.audioAdapter.onEnded(() => this._onAudioEnded())
    this.audioAdapter.onError((error) => {
      if (error && typeof error === 'object' && 'type' in error && error.type === 'autoplay_blocked') {
        this._updateStatusText('请点击播放按钮开始播放')
        this.isPlaying = false
        this._triggerEvent('onPlayStateChange', { isPlaying: false })
        return
      }
      this._updateStatusText('播放失败')
      this._onAudioError()
    })
    this.audioAdapter.onWaiting(() => { /* loading */ })
    this.audioAdapter.onCanplay(() => { /* ready */ })
    this.isInitialized = true
  }

  private _updateStatusText(text: string): void {
    this.statusText = text
    this._triggerEvent('onStatusTextChange', { statusText: text })
  }

  private _getCurrentSentence(): Sentence | null {
    return this.sentences[this.currentSentenceIndex] || null
  }

  private _getCurrentAudio(): Audio | null {
    const s = this._getCurrentSentence()
    if (!s || !s.audios) return null
    return s.audios[this.currentAudioIndex] || null
  }

  private _buildAudioUrl(audio: Audio): string {
    if (audio.url && audio.url.startsWith('http')) return audio.url
    if (audio.src && audio.src.startsWith('http')) return audio.src
    return `${this.envConfig.apiBaseUrl}/audio/${audio.id}`
  }

  async playCurrentAudio(): Promise<void> {
    let audio = this._getCurrentAudio()
    let sentence = this._getCurrentSentence()

    // 当前句没有可播放音频(audios 为空/缺失)：别静默 return 卡死在"准备中...",
    // 向后跳到第一句有音频的(最多绕整列一圈)。整列都没音频 → 给出明确状态。
    if (sentence && !audio && this.sentences.length > 0) {
      console.warn(`[AudioPlayerService] 句子 #${sentence.id} 无音频(audios 为空)，自动跳过`)
      const total = this.sentences.length
      let found = -1
      for (let step = 1; step <= total; step++) {
        const idx = (this.currentSentenceIndex + step) % total
        const s = this.sentences[idx]
        if (s && Array.isArray(s.audios) && s.audios.length > 0) { found = idx; break }
      }
      if (found < 0) {
        this.isPlaying = false
        this._triggerEvent('onPlayStateChange', { isPlaying: false })
        this._updateStatusText('当前列表无可播放音频')
        return
      }
      this.currentSentenceIndex = found
      this.currentAudioIndex = 0
      this.playCount = 0
      this._triggerEvent('onSentenceChange', { sentences: this.sentences, currentSentenceIndex: found })
      this._triggerEvent('onPlayCountChange', {
        playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
        currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
      })
      audio = this._getCurrentAudio()
      sentence = this._getCurrentSentence()
    }

    if (!audio || !sentence) return

    const cacheKey = `${sentence.id}_${audio.id}`
    try {
      let audioPath = await this.fileCacheManager.getCache(cacheKey)
      if (!audioPath) {
        this._updateStatusText('播放网络音频...')
        audioPath = this._buildAudioUrl(audio)
      } else {
        this._updateStatusText('播放缓存音频...')
      }
      if (this.audioAdapter.setTitle) {
        this.audioAdapter.setTitle(sentence.text ? sentence.text.substring(0, 50) : '英语音频')
      }
      this.audioAdapter.setSrc(audioPath)
      this.audioAdapter.setVolume(1)
      this.audioAdapter.play()
      this._updateStatusText('播放中...')
      if (this.stopMode === 'halfHour' && !this.stopTimer) this._startStopTimer()
    } catch (error) {
      console.error('[AudioPlayerService] 播放失败:', error)
      this._updateStatusText('播放失败')
      this._onAudioError()
    }
  }

  private _onAudioEnded(): void {
    if (!this.isPlaying) return
    if (this.playCount < this.maxPlayCount - 1) {
      this.playCount++
      this._triggerEvent('onPlayCountChange', {
        playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
        currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
      })
      if (this.delayPlayTimer) clearTimeout(this.delayPlayTimer)
      this.delayPlayTimer = setTimeout(() => {
        this.delayPlayTimer = null
        if (this.isPlaying) void this.playCurrentAudio()
      }, this.audioSwitchDelay)
    } else {
      this._nextAudio()
    }
  }

  private _onAudioError(): void { this._nextAudio() }

  private _nextAudio(): void {
    if (!this.isPlaying) return
    const sentence = this._getCurrentSentence()
    if (!sentence || !sentence.audios) return
    const nextIdx = this.currentAudioIndex + 1
    if (nextIdx < sentence.audios.length) {
      try { this.audioAdapter.pause() } catch { /* ignore */ }
      this.currentAudioIndex = nextIdx
      this.playCount = 0
      this._triggerEvent('onPlayCountChange', {
        playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
        currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
      })
      this._triggerEvent('onTextToggle', { showText: true })
      if (this.delayPlayTimer) clearTimeout(this.delayPlayTimer)
      this.delayPlayTimer = setTimeout(() => {
        this.delayPlayTimer = null
        if (this.isPlaying) void this.playCurrentAudio()
      }, this.audioSwitchDelay)
    } else {
      // 本句参考音频全部播完。开了跟读闸门则在此停住、等用户跟读；否则照常进下一句。
      if (this.shadowGate) {
        this._enterShadowWait()
      } else {
        this._nextSentence()
      }
    }
  }

  /** 进入跟读等待态：暂停、停掉播放标志，emit onAwaitShadow 把推进权交给 ShadowController。 */
  private _enterShadowWait(): void {
    try { this.audioAdapter.pause() } catch { /* ignore */ }
    this._clearTimers()
    this.isPlaying = false
    this._triggerEvent('onPlayStateChange', { isPlaying: false })
    this._updateStatusText('请跟读…')
    const sentence = this._getCurrentSentence()
    if (sentence) {
      this._triggerEvent('onAwaitShadow', {
        sentence,
        sentenceIndex: this.currentSentenceIndex
      })
    }
  }

  private _nextSentence(): void {
    try { this.audioAdapter.pause() } catch { /* ignore */ }
    this.isPlaying = false
    const isLast = this.currentSentenceIndex === this.sentences.length - 1
    if (this.stopMode === 'roundEnd' && isLast) {
      this.stopAudio()
      this._updateStatusText('本轮播放结束')
      this._triggerEvent('onPlayComplete', {})
      return
    }
    this.currentSentenceIndex = isLast ? 0 : this.currentSentenceIndex + 1
    this.currentAudioIndex = 0
    this.playCount = 0
    this._triggerEvent('onSentenceChange', { sentences: this.sentences, currentSentenceIndex: this.currentSentenceIndex })
    this._triggerEvent('onPlayCountChange', {
      playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
      currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
    })
    this._triggerEvent('onPlayNext', { sentenceIndex: this.currentSentenceIndex, sentence: this.sentences[this.currentSentenceIndex] })
    this._triggerEvent('onTextToggle', { showText: false })
    if (this.delayPlayTimer) clearTimeout(this.delayPlayTimer)
    this.delayPlayTimer = setTimeout(() => {
      this.delayPlayTimer = null
      void this.playCurrentAudio()
    }, this.audioSwitchDelay)
  }

  private _previousSentence(): void {
    try { this.audioAdapter.pause() } catch { /* ignore */ }
    this.isPlaying = false
    this.currentSentenceIndex = this.currentSentenceIndex > 0
      ? this.currentSentenceIndex - 1
      : this.sentences.length - 1
    this.currentAudioIndex = 0
    this.playCount = 0
    this._triggerEvent('onSentenceChange', { sentences: this.sentences, currentSentenceIndex: this.currentSentenceIndex })
    this._triggerEvent('onPlayCountChange', {
      playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
      currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
    })
    this._triggerEvent('onPlayPrevious', { sentenceIndex: this.currentSentenceIndex, sentence: this._getCurrentSentence()! })
    this._triggerEvent('onTextToggle', { showText: false })
    if (this.delayPlayTimer) clearTimeout(this.delayPlayTimer)
    this.delayPlayTimer = setTimeout(() => {
      this.delayPlayTimer = null
      void this.playCurrentAudio()
    }, this.audioSwitchDelay)
  }

  private _startStopTimer(): void {
    this._clearStopTimer()
    this.stopTimer = setTimeout(() => {
      this.stopAudio()
      this._updateStatusText('半小时后自动停止')
    }, 30 * 60 * 1000)
  }

  private _clearStopTimer(): void {
    if (this.stopTimer) { clearTimeout(this.stopTimer); this.stopTimer = null }
  }

  private _clearTimers(): void {
    this._clearStopTimer()
    if (this.delayPlayTimer) { clearTimeout(this.delayPlayTimer); this.delayPlayTimer = null }
  }

  private _logEventListenerError(eventName: string, error: unknown): void {
    const now = Date.now()
    const state = this.eventErrorLogState[eventName] || { lastLogAt: 0, suppressedCount: 0 }
    if (now - state.lastLogAt < EVENT_ERROR_LOG_WINDOW_MS) {
      state.suppressedCount += 1
      this.eventErrorLogState[eventName] = state
      return
    }
    if (state.suppressedCount > 0) {
      console.warn(`[AudioPlayerService] 事件 ${eventName} 节流期间额外抛错 ${state.suppressedCount} 次`)
    }
    console.warn(`[AudioPlayerService] 事件回调失败 ${eventName}:`, error)
    state.lastLogAt = now
    state.suppressedCount = 0
    this.eventErrorLogState[eventName] = state
  }

  private _triggerEvent<T extends AudioPlayerEventType>(
    eventName: T,
    data: AudioPlayerEventData[T]
  ): void {
    this.eventListeners[eventName]?.forEach(cb => {
      try { cb(data) } catch (e) { this._logEventListenerError(eventName, e) }
    })
  }

  // ── Public API ──────────────────────────────────────────────────────────────

  setSentences(sentences: Sentence[]): void {
    this.sentences = sentences
    this.currentSentenceIndex = 0
    this.currentAudioIndex = 0
    this.playCount = 0
    this._triggerEvent('onSentenceChange', { sentences, currentSentenceIndex: 0 })
    this._triggerEvent('onPlayCountChange', {
      playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
      currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
    })
  }

  togglePlayPause(): void {
    if (this.isPlaying) {
      this.audioAdapter.pause()
      this.isPlaying = false
      this._updateStatusText('已暂停')
      this._triggerEvent('onPlayStateChange', { isPlaying: false })
    } else {
      this.isPlaying = true
      this._triggerEvent('onPlayStateChange', { isPlaying: true })
      void this.playCurrentAudio()
    }
  }

  stopAudio(): void {
    this._clearTimers()
    try {
      if (this.audioAdapter.stop) this.audioAdapter.stop()
      else this.audioAdapter.pause()
    } catch { /* ignore */ }
    this.isPlaying = false
    this.playCount = 0
    this._updateStatusText('已停止')
  }

  previousSentence(): void { this._previousSentence() }
  nextSentence(): void { this._nextSentence() }

  /** 开/关跟读闸门。开启时一句参考音频自动播完会停在 onAwaitShadow 等跟读，不自动进下一句。 */
  setShadowGate(enabled: boolean): void { this.shadowGate = enabled }

  /** 跟读判分不通过时重读：重播当前句参考音频一遍，播完再次进入跟读等待。 */
  replayCurrent(): void {
    this._clearTimers()
    this.currentAudioIndex = 0
    // 置 playCount = max-1：播一遍后 _onAudioEnded 即走 _nextAudio → 末段 → 再次进闸门。
    this.playCount = Math.max(0, this.maxPlayCount - 1)
    this.isPlaying = true
    this._triggerEvent('onPlayStateChange', { isPlaying: true })
    void this.playCurrentAudio()
  }

  toggleAnnotation(): void {
    const s = this._getCurrentSentence()
    if (!s) return
    const newStatus = !s.is_annotated
    this.sentences[this.currentSentenceIndex].is_annotated = newStatus
    this._triggerEvent('onToggleAnnotation', { sentenceId: s.id, isAnnotated: newStatus, sentence: s })
  }

  toggleReportError(): void {
    const s = this._getCurrentSentence()
    if (!s) return
    const newStatus = !s.has_error
    this.sentences[this.currentSentenceIndex].has_error = newStatus
    this._triggerEvent('onToggleReportError', { sentenceId: s.id, hasError: newStatus, sentence: s })
  }

  /**
   * 替换成功后刷新当前句子：更新文本 + 让当前音频重新拉取（清本地缓存 + 给音频对象挂带
   * 时间戳的网络 URL，击穿 webview/HTTP 缓存，保证下次播放到新音频）。正在播放则先停。
   */
  async applyReplacedCurrentSentence(text: string): Promise<void> {
    const s = this._getCurrentSentence()
    const a = this._getCurrentAudio()
    if (!s) return
    if (this.isPlaying) this.stopAudio()
    this.sentences[this.currentSentenceIndex].text = text
    if (a) {
      try { await this.fileCacheManager.removeCache(`${s.id}_${a.id}`) } catch { /* ignore */ }
      // 带时间戳网络 URL：_buildAudioUrl 命中 http 分支 → 绕过文件缓存、击穿 HTTP 缓存。
      a.url = `${this.envConfig.apiBaseUrl}/audio/${a.id}?v=${Date.now()}`
    }
    this._triggerEvent('onSentenceChange', {
      sentences: this.sentences,
      currentSentenceIndex: this.currentSentenceIndex
    })
  }

  setStopMode(stopMode: 'halfHour' | 'roundEnd' | null): void {
    this.stopMode = stopMode
    if (stopMode === 'halfHour') {
      if (!this.stopTimer) this._startStopTimer()
    } else {
      this._clearStopTimer()
    }
  }

  setMaxPlayCount(count: number): void {
    if (typeof count !== 'number' || count < 1) return
    this.maxPlayCount = count
    this._triggerEvent('onPlayCountChange', {
      playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
      currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
    })
  }

  resetPlayer(): void {
    this.stopAudio()
    this.currentSentenceIndex = 0
    this.currentAudioIndex = 0
    this.playCount = 0
    this.stopMode = 'halfHour'
    this._clearTimers()
    this._updateStatusText('准备中...')
    this._triggerEvent('onSentenceChange', { sentences: this.sentences, currentSentenceIndex: 0 })
    this._triggerEvent('onPlayCountChange', {
      playCount: this.playCount, currentSentenceIndex: this.currentSentenceIndex,
      currentAudioIndex: this.currentAudioIndex, maxPlayCount: this.maxPlayCount
    })
  }

  getState(): AudioPlayerState {
    return {
      isPlaying: this.isPlaying, currentSentenceIndex: this.currentSentenceIndex,
      currentAudioIndex: this.currentAudioIndex, playCount: this.playCount,
      maxPlayCount: this.maxPlayCount, stopMode: this.stopMode,
      statusText: this.statusText, sentences: this.sentences,
      currentSentence: this._getCurrentSentence(), currentAudio: this._getCurrentAudio()
    }
  }

  addEventListener<T extends AudioPlayerEventType>(
    eventName: T,
    callback: (data: AudioPlayerEventData[T]) => void
  ): void {
    this.eventListeners[eventName].push(callback)
  }

  removeEventListener<T extends AudioPlayerEventType>(
    eventName: T,
    callback: (data: AudioPlayerEventData[T]) => void
  ): void {
    const idx = this.eventListeners[eventName].indexOf(callback)
    if (idx > -1) this.eventListeners[eventName].splice(idx, 1)
  }
}
