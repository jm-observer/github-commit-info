/**
 * HtmlAudioAdapter — HTML5 Audio 桌面端适配器（直接复用，无 AntD 依赖）。
 */

type VoidFn = () => void

/**
 * 所有活着的 adapter。每个 adapter 持一个 `new Audio()`——脱离 DOM、不受 React 卸载影响，
 * 只要还在内存里就会继续出声。而 AudioPlayerService.stopAudio() 只停得了「当前单例持有的
 * 那一个」：一旦某个 adapter 在播放中被 resetInstance 从单例上摘掉（标注/听包两个入口交错
 * 初始化、Strict Mode 双 mount 都会造成），它就再没有任何引用能停它——表现为按钮和文案都
 * 正常响应、声音却停不下来。注册表存在的意义就是让这种失联的 adapter 仍然可达。
 */
const LIVE_ADAPTERS = new Set<HtmlAudioAdapter>()

export default class HtmlAudioAdapter {
  private audio: HTMLAudioElement
  private isEndedFired: boolean = false
  private destroyed = false

  private playCallbacks: VoidFn[] = []
  private pauseCallbacks: VoidFn[] = []
  private endedCallbacks: VoidFn[] = []
  private errorCallbacks: Array<(error: any) => void> = []
  private waitingCallbacks: VoidFn[] = []
  private canplayCallbacks: VoidFn[] = []

  /**
   * 销毁所有活着的 adapter（含已从单例上摘掉、再也没人引用得到的那些）。
   * 调用点：任何「即将创建新 adapter / 新单例」的地方——换歌单即换播放会话，
   * 旧会话的声音一律不该活过这一刻。
   */
  static destroyAll(): void {
    for (const a of Array.from(LIVE_ADAPTERS)) {
      try { a.destroy() } catch { /* ignore */ }
    }
  }

  constructor() {
    LIVE_ADAPTERS.add(this)
    this.audio = new Audio()
    this.audio.preload = 'auto'
    this.audio.volume = 1.0

    this.audio.addEventListener('play', () => {
      this.isEndedFired = false
      this.playCallbacks.forEach(cb => cb())
    })

    this.audio.addEventListener('pause', () => {
      if (!this.isEndedFired) {
        this.pauseCallbacks.forEach(cb => cb())
      }
    })

    this.audio.addEventListener('ended', () => {
      this.isEndedFired = true
      this.endedCallbacks.forEach(cb => cb())
    })

    this.audio.addEventListener('waiting', () => {
      this.waitingCallbacks.forEach(cb => cb())
    })

    this.audio.addEventListener('canplay', () => {
      this.canplayCallbacks.forEach(cb => cb())
    })

    this.audio.addEventListener('error', (e) => {
      const err: any = (this.audio as any).error || e
      this.errorCallbacks.forEach(cb => cb(err))
    })
  }

  play(): void {
    // 已弃用的 adapter 拒绝再出声：它的持有者可能是一个仍在飞行中的旧 playCurrentAudio()。
    if (this.destroyed) return
    void this.audio.play().catch(err => {
      if (err instanceof DOMException && err.name === 'NotAllowedError') {
        this.errorCallbacks.forEach(cb => cb({
          type: 'autoplay_blocked',
          message: '自动播放被阻止，请点击播放按钮',
          originalError: err
        }))
      } else {
        this.errorCallbacks.forEach(cb => cb(err))
      }
    })
  }

  pause(): void { this.audio.pause() }

  stop(): void {
    this.audio.pause()
    try { this.audio.currentTime = 0 } catch { /* ignore */ }
  }

  /**
   * 彻底停音并弃用本 adapter：光 pause() 不够——已缓冲的数据 + 未决的 play() promise 仍可能
   * 让它再响一声，故清空 src 并 load() 把媒体元素打回空载。之后所有回调不再触发。
   */
  destroy(): void {
    if (this.destroyed) return
    this.destroyed = true
    LIVE_ADAPTERS.delete(this)
    try {
      this.audio.pause()
      this.audio.src = ''
      this.audio.load()
    } catch { /* ignore */ }
    this.playCallbacks = []
    this.pauseCallbacks = []
    this.endedCallbacks = []
    this.errorCallbacks = []
    this.waitingCallbacks = []
    this.canplayCallbacks = []
  }

  setSrc(url: string): void {
    if (this.destroyed) return
    this.isEndedFired = false
    this.audio.src = url
  }

  setVolume(volume: number): void {
    this.audio.volume = Math.max(0, Math.min(1, volume))
  }

  setTitle(_title: string): void {}
  setCover(_coverUrl: string): void {}

  onPlay(callback: VoidFn): void { this.playCallbacks.push(callback) }
  onPause(callback: VoidFn): void { this.pauseCallbacks.push(callback) }
  onEnded(callback: VoidFn): void { this.endedCallbacks.push(callback) }
  onError(callback: (error: any) => void): void { this.errorCallbacks.push(callback) }
  onWaiting(callback: VoidFn): void { this.waitingCallbacks.push(callback) }
  onCanplay(callback: VoidFn): void { this.canplayCallbacks.push(callback) }
}
