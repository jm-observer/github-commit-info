/**
 * ShadowService — 跟读采集 + 判分。
 *
 * - 采集：webview `getUserMedia` + `MediaRecorder`；`auto` 模式用 Web Audio 能量 VAD
 *   自动判停，`button` 模式由调用方手动 stop。
 * - 判分：录音字节 + 单元元信息经 Tauri `english_shadow_score` 代理到 toolkit-server。
 * - 统计：`english_shadow_stats` 批量回读。
 *
 * 设计见 docs/english-shadow-design.md §5。
 */

import { invoke } from '@tauri-apps/api/core'
import EnvConfigService from '../services/EnvConfigService'
import type { ShadowScore, ShadowStat, ShadowPhoneResult, ShadowPronStatus } from '../types'
import type { ShadowCaptureMode, ShadowGranularity } from './shadowPrefs'
import { startStreamingCapture, type StreamCaptureHandle } from './streamingCapture'

export interface CapturedAudio {
  bytes: number[]
  mime: string
}

/** 一次采集的句柄：auto 模式直接 await done；button 模式 UI 调 stop()。 */
export interface CaptureHandle {
  /** 手动结束采集（button 模式 / 兜底）。 */
  stop: () => void
  /** 完成后 resolve 录音；用户始终没出声（auto 静默超时）则 resolve null。 */
  done: Promise<CapturedAudio | null>
}

export interface CaptureOptions {
  mode: ShadowCaptureMode
  /** 硬上限：到点强制结束（按句长动态给）。默认 8000ms。 */
  maxMs?: number
  /** auto：检测到说话后，静音多久判停。默认 900ms。 */
  silenceMs?: number
  /** auto：始终没检测到说话则放弃的时长。默认 4000ms。 */
  noSpeechMs?: number
}

const SPEECH_RMS = 0.018 // 经验阈值：高于即视作有人声

function pickMime(): string {
  const candidates = [
    'audio/webm;codecs=opus',
    'audio/webm',
    'audio/ogg;codecs=opus',
    'audio/mp4'
  ]
  for (const c of candidates) {
    if (typeof MediaRecorder !== 'undefined' && MediaRecorder.isTypeSupported(c)) return c
  }
  return ''
}

/**
 * 开始一次采集。返回句柄；auto 模式内部按 VAD/超时自动 stop。
 */
export function captureUtterance(opts: CaptureOptions): CaptureHandle {
  const maxMs = opts.maxMs ?? 8000
  const silenceMs = opts.silenceMs ?? 900
  const noSpeechMs = opts.noSpeechMs ?? 4000

  let stopped = false
  let recorder: MediaRecorder | null = null
  let stream: MediaStream | null = null
  let audioCtx: AudioContext | null = null
  let rafTimer: ReturnType<typeof setInterval> | null = null
  let hardTimer: ReturnType<typeof setTimeout> | null = null
  const chunks: Blob[] = []
  let mime = ''
  let hasSpoken = false
  let lastVoiceAt = 0
  let startedAt = 0

  let resolveDone!: (v: CapturedAudio | null) => void
  let rejectDone!: (e: unknown) => void
  const done = new Promise<CapturedAudio | null>((res, rej) => {
    resolveDone = res
    rejectDone = rej
  })

  const cleanup = () => {
    if (rafTimer) { clearInterval(rafTimer); rafTimer = null }
    if (hardTimer) { clearTimeout(hardTimer); hardTimer = null }
    try { audioCtx?.close() } catch { /* ignore */ }
    try { stream?.getTracks().forEach(t => t.stop()) } catch { /* ignore */ }
  }

  const doStop = () => {
    if (stopped) return
    stopped = true
    if (rafTimer) { clearInterval(rafTimer); rafTimer = null }
    if (hardTimer) { clearTimeout(hardTimer); hardTimer = null }
    try {
      if (recorder && recorder.state !== 'inactive') recorder.stop()
      else finalize(false)
    } catch {
      finalize(false)
    }
  }

  const finalize = async (spoke: boolean) => {
    cleanup()
    if (!spoke && !hasSpoken && chunks.length === 0) {
      resolveDone(null)
      return
    }
    try {
      const blob = new Blob(chunks, { type: mime || 'audio/webm' })
      if (blob.size === 0) { resolveDone(null); return }
      const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()))
      resolveDone({ bytes, mime: blob.type || mime || 'audio/webm' })
    } catch (e) {
      rejectDone(e)
    }
  }

  ;(async () => {
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      mime = pickMime()
      recorder = mime ? new MediaRecorder(stream, { mimeType: mime }) : new MediaRecorder(stream)
      mime = recorder.mimeType || mime
      recorder.ondataavailable = (e) => { if (e.data && e.data.size > 0) chunks.push(e.data) }
      recorder.onstop = () => { void finalize(hasSpoken) }
      recorder.start()
      startedAt = Date.now()
      lastVoiceAt = startedAt

      // 硬上限
      hardTimer = setTimeout(doStop, maxMs)

      // auto 模式：能量 VAD 判停
      if (opts.mode === 'auto') {
        audioCtx = new AudioContext()
        const src = audioCtx.createMediaStreamSource(stream)
        const analyser = audioCtx.createAnalyser()
        analyser.fftSize = 1024
        src.connect(analyser)
        const buf = new Float32Array(analyser.fftSize)
        rafTimer = setInterval(() => {
          analyser.getFloatTimeDomainData(buf)
          let sum = 0
          for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i]
          const rms = Math.sqrt(sum / buf.length)
          const now = Date.now()
          if (rms > SPEECH_RMS) {
            hasSpoken = true
            lastVoiceAt = now
          }
          if (hasSpoken) {
            if (now - lastVoiceAt > silenceMs) doStop()
          } else if (now - startedAt > noSpeechMs) {
            doStop() // 始终没出声 → 放弃
          }
        }, 60)
      }
    } catch (e) {
      cleanup()
      rejectDone(e)
    }
  })()

  return { stop: doStop, done }
}

export interface ScoreUnit {
  kind: ShadowGranularity
  sentenceId: number
  /** word 模式：句内词序号；sentence 模式：undefined。 */
  wordIndex?: number
  refText: string
  threshold?: number
}

/** 调判分：录音字节 + 单元 → toolkit-server。customer_id 由 EnvConfigService 提供。 */
export async function scoreShadow(audio: CapturedAudio, unit: ScoreUnit): Promise<ShadowScore> {
  const customerId = (await EnvConfigService.getInstance().getCustomerId()) ?? 1
  return invoke<ShadowScore>('english_shadow_score', {
    audio: audio.bytes,
    mime: audio.mime,
    customerId,
    kind: unit.kind,
    sentenceId: unit.sentenceId,
    wordIndex: unit.wordIndex ?? null,
    refText: unit.refText,
    threshold: unit.threshold ?? null
  })
}

/** 流式 partial 事件:某词落定的临时分(committed,落定后稳)。无 v1 `status`(纯发音维度)。 */
export interface ShadowPartial {
  word_index: number
  ref: string
  score?: number
  pron_status?: ShadowPronStatus
  phones?: ShadowPhoneResult[]
}

export interface StreamHandlers {
  onReady?: () => void
  /** 某词落定 → 渐进点亮(tentative 渲染)。 */
  onPartial?: (p: ShadowPartial) => void
  /** 整句权威分(批量 finalizer)。 */
  onFinal?: (score: ShadowScore) => void
  onError?: (message: string) => void
  /** 本次录音(16k WAV blob),供「听我的录音」回放。 */
  onRecorded?: (wav: Blob) => void
}

/** 把流式采集的 16k s16le PCM 块拼成可播放的 WAV blob(给回放用)。 */
export function pcmChunksToWav(chunks: ArrayBuffer[], sampleRate = 16000): Blob {
  let total = 0
  for (const c of chunks) total += c.byteLength
  const buf = new ArrayBuffer(44 + total)
  const dv = new DataView(buf)
  const writeStr = (off: number, s: string) => { for (let i = 0; i < s.length; i++) dv.setUint8(off + i, s.charCodeAt(i)) }
  writeStr(0, 'RIFF'); dv.setUint32(4, 36 + total, true); writeStr(8, 'WAVE')
  writeStr(12, 'fmt '); dv.setUint32(16, 16, true); dv.setUint16(20, 1, true); dv.setUint16(22, 1, true)
  dv.setUint32(24, sampleRate, true); dv.setUint32(28, sampleRate * 2, true); dv.setUint16(32, 2, true); dv.setUint16(34, 16, true)
  writeStr(36, 'data'); dv.setUint32(40, total, true)
  let off = 44
  for (const c of chunks) { new Uint8Array(buf, off, c.byteLength).set(new Uint8Array(c)); off += c.byteLength }
  return new Blob([buf], { type: 'audio/wav' })
}

export interface StreamHandle {
  /** 手动结束(button 模式 / 兜底):停采集并发 end。 */
  stop: () => void
  /** 收到 final(或出错/无声)后 resolve。 */
  done: Promise<void>
}

/**
 * 流式跟读评测:开 WS → hello → 边采 16k PCM 边推 → 收 partial/final。
 * 返回 `null` 表示流式不可用(未配 / 拿不到 URL),调用方应回退批量 `scoreShadow`。
 */
export async function streamScore(unit: ScoreUnit, handlers: StreamHandlers, maxMs?: number): Promise<StreamHandle | null> {
  const customerId = (await EnvConfigService.getInstance().getCustomerId()) ?? 1
  const url = await invoke<string>('english_shadow_stream_url', {
    customerId,
    kind: unit.kind,
    sentenceId: unit.sentenceId,
    wordIndex: unit.wordIndex ?? null,
    threshold: unit.threshold ?? null
  })
  if (!url) return null // 未配置 → 调用方回退批量

  let capture: StreamCaptureHandle | null = null
  let ended = false
  const recorded: ArrayBuffer[] = [] // 累积本次录音 PCM,供回放
  let resolveDone!: () => void
  const done = new Promise<void>((res) => { resolveDone = res })
  const ws = new WebSocket(url)
  ws.binaryType = 'arraybuffer'

  const finish = () => {
    if (ended) return
    ended = true
    try { capture?.stop() } catch { /* ignore */ }
    try { if (ws.readyState === WebSocket.OPEN) ws.close() } catch { /* ignore */ }
    if (recorded.length > 0) { try { handlers.onRecorded?.(pcmChunksToWav(recorded)) } catch { /* ignore */ } }
    resolveDone()
  }

  const sendEnd = () => {
    try { if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'end' })) } catch { /* ignore */ }
  }

  ws.onopen = () => {
    ws.send(JSON.stringify({ type: 'hello', ref_text: unit.refText, granularity: unit.kind }))
    capture = startStreamingCapture({
      mode: 'auto',
      maxMs,
      onChunk: (pcm) => {
        recorded.push(pcm.slice(0)) // 留一份副本用于回放(ws.send 不消费 buffer,但 slice 防意外复用)
        try { if (ws.readyState === WebSocket.OPEN) ws.send(pcm) } catch { /* ignore */ }
      }
    })
    // 采集自然结束(VAD/超时)→ 发 end 让服务端 finalize。
    void capture.done.then(({ spoke }) => {
      if (!spoke) { handlers.onError?.('未检测到朗读'); finish(); return }
      sendEnd()
    }).catch((e) => { handlers.onError?.('录音失败：' + (e?.message || '检查麦克风')); finish() })
  }

  ws.onmessage = (ev) => {
    if (typeof ev.data !== 'string') return
    let d: any
    try { d = JSON.parse(ev.data) } catch { return }
    switch (d.type) {
      case 'ready': handlers.onReady?.(); break
      case 'partial': handlers.onPartial?.(d as ShadowPartial); break
      case 'final': handlers.onFinal?.(d as ShadowScore); finish(); break
      case 'error': handlers.onError?.(d.message || '流式评测错误'); finish(); break
    }
  }
  ws.onerror = () => { handlers.onError?.('流式连接错误'); finish() }
  ws.onclose = () => finish()

  return { stop: () => { capture?.stop(); sendEnd() }, done }
}

/** 批量回读统计。 */
export async function fetchShadowStats(sentenceIds: number[]): Promise<ShadowStat[]> {
  if (sentenceIds.length === 0) return []
  const customerId = (await EnvConfigService.getInstance().getCustomerId()) ?? 1
  const resp = await invoke<{ stats: ShadowStat[] }>('english_shadow_stats', {
    customerId,
    sentenceIds
  })
  return resp?.stats ?? []
}

/**
 * 把句子文本切成「词」单元（与后端 normalize 同源：非字母数字作分隔，保留原始词形）。
 * 返回每个词的原文，序号即数组下标，用作 word 模式的 word_index。
 */
export function splitWords(text: string): string[] {
  return text
    .replace(/[^\p{L}\p{N}]+/gu, ' ')
    .split(/\s+/)
    .filter(Boolean)
}
