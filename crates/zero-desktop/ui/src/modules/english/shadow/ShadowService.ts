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
import type { ShadowScore, ShadowStat } from '../types'
import type { ShadowCaptureMode, ShadowGranularity } from './shadowPrefs'

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
