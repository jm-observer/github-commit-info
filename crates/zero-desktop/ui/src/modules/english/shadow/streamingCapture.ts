/**
 * 流式 PCM 采集(Phase 3:边读边评)。
 *
 * 与 ShadowService 的 `captureUtterance`(MediaRecorder→webm 整段)不同:这里用 Web Audio 取
 * **原始 PCM**,实时降采样到 **16k 单声道 s16le**,逐块回调,供流式 WS 上行。麦克风原生采样率
 * 常是 48k,故内置线性插值重采样(跨块保留小数相位)。
 *
 * 停止:`auto` 模式能量 VAD 判停(检测到说话后静音 silenceMs)、始终没出声 noSpeechMs 放弃、
 * 硬上限 maxMs;`button` 模式由调用方 stop()。
 */

const TARGET_SR = 16000
const SPEECH_RMS = 0.018 // 与 captureUtterance 同阈值

export interface StreamCaptureOptions {
  mode: 'auto' | 'button'
  /** 每块回调的 PCM 时长(ms),默认 256ms(≈4096 样本 @16k)。 */
  chunkMs?: number
  maxMs?: number
  silenceMs?: number
  noSpeechMs?: number
  /** 一块 16k s16le PCM(ArrayBuffer);直接 ws.send。 */
  onChunk: (pcm: ArrayBuffer) => void
}

export interface StreamCaptureHandle {
  stop: () => void
  /** 采集自然结束(VAD/超时/手动)后 resolve;`spoke`=是否检测到过说话。 */
  done: Promise<{ spoke: boolean }>
}

export function startStreamingCapture(opts: StreamCaptureOptions): StreamCaptureHandle {
  const chunkMs = opts.chunkMs ?? 256
  const maxMs = opts.maxMs ?? 12000
  const silenceMs = opts.silenceMs ?? 900
  const noSpeechMs = opts.noSpeechMs ?? 4000
  const chunkSamples = Math.round((TARGET_SR * chunkMs) / 1000)

  let stopped = false
  let stream: MediaStream | null = null
  let ctx: AudioContext | null = null
  let node: ScriptProcessorNode | null = null
  let src: MediaStreamAudioSourceNode | null = null
  let hardTimer: ReturnType<typeof setTimeout> | null = null

  let resamplePos = 0          // 重采样小数相位(跨块保留)
  let prevTail = 0             // 上一块末样本(插值用)
  let out16: number[] = []     // 累积的 16k 样本,满 chunkSamples 就 flush
  let hasSpoken = false
  let lastVoiceAt = 0
  let startedAt = 0

  let resolveDone!: (v: { spoke: boolean }) => void
  let rejectDone!: (e: unknown) => void
  const done = new Promise<{ spoke: boolean }>((res, rej) => { resolveDone = res; rejectDone = rej })

  const cleanup = () => {
    if (hardTimer) { clearTimeout(hardTimer); hardTimer = null }
    try { node?.disconnect() } catch { /* ignore */ }
    try { src?.disconnect() } catch { /* ignore */ }
    try { void ctx?.close() } catch { /* ignore */ }
    try { stream?.getTracks().forEach(t => t.stop()) } catch { /* ignore */ }
  }

  const flushFull = () => {
    while (out16.length >= chunkSamples) {
      const frame = out16.slice(0, chunkSamples)
      out16 = out16.slice(chunkSamples)
      const buf = new ArrayBuffer(frame.length * 2)
      const dv = new DataView(buf)
      for (let i = 0; i < frame.length; i++) {
        const s = Math.max(-1, Math.min(1, frame[i]))
        dv.setInt16(i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true) // s16le
      }
      opts.onChunk(buf)
    }
  }

  const doStop = () => {
    if (stopped) return
    stopped = true
    // flush 尾部不足一块的残量(补足或直接发短块)。
    if (out16.length > 0) {
      const frame = out16; out16 = []
      const buf = new ArrayBuffer(frame.length * 2)
      const dv = new DataView(buf)
      for (let i = 0; i < frame.length; i++) {
        const s = Math.max(-1, Math.min(1, frame[i]))
        dv.setInt16(i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true)
      }
      opts.onChunk(buf)
    }
    cleanup()
    resolveDone({ spoke: hasSpoken })
  }

  ;(async () => {
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      ctx = new AudioContext()
      const inRate = ctx.sampleRate
      const ratio = inRate / TARGET_SR
      src = ctx.createMediaStreamSource(stream)
      node = ctx.createScriptProcessor(4096, 1, 1)
      startedAt = Date.now()
      lastVoiceAt = startedAt

      node.onaudioprocess = (e) => {
        if (stopped) return
        const input = e.inputBuffer.getChannelData(0)
        // 能量 VAD
        let sum = 0
        for (let i = 0; i < input.length; i++) sum += input[i] * input[i]
        const rms = Math.sqrt(sum / input.length)
        const now = Date.now()
        if (rms > SPEECH_RMS) { hasSpoken = true; lastVoiceAt = now }
        // 线性重采样到 16k(跨块保留 resamplePos / prevTail)
        let pos = resamplePos
        while (pos < input.length) {
          const idx = Math.floor(pos)
          const frac = pos - idx
          const a = idx === 0 ? prevTail : input[idx - 1]
          const b = input[idx]
          out16.push(a + (b - a) * frac)
          pos += ratio
        }
        resamplePos = pos - input.length
        prevTail = input[input.length - 1]
        flushFull()
        // 判停
        if (opts.mode === 'auto') {
          if (hasSpoken && now - lastVoiceAt > silenceMs) doStop()
          else if (!hasSpoken && now - startedAt > noSpeechMs) doStop()
        }
      }
      src.connect(node)
      node.connect(ctx.destination) // 某些实现需连到 destination 才触发 onaudioprocess
      hardTimer = setTimeout(doStop, maxMs)
    } catch (e) {
      cleanup()
      rejectDone(e)
    }
  })()

  return { stop: doStop, done }
}
