/**
 * ShadowPanel — 跟读判分面板。
 *
 * 挂在播放器下方，订阅 AudioPlayerService 的 onAwaitShadow 闸门事件：一句参考音频播完后
 * 接管推进——采集用户跟读 → 判分 → 通过(可选)自动跳下一个 / 不通过留在原地重读。支持
 * 整句 / 逐词两种粒度，逐词标色，展示成功/失败累计，提供 重读 / 跳过 / 标注重点。
 *
 * 设计见 docs/english-shadow-design.md。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import { Mic, Square, SkipForward, RotateCcw, Star, Loader2, CheckCircle2, XCircle, Volume2 } from 'lucide-react'
import { AudioPlayerService } from '../services/AudioPlayerService'
import ApiService from '../services/ApiService'
import type { Sentence, ShadowScore, ShadowStat, ShadowWordResult, ShadowPhoneResult, ShadowPronStatus } from '../types'
import { Button } from '../../speech/components/ui/Button'
import {
  captureUtterance,
  scoreShadow,
  streamScore,
  fetchShadowStats,
  splitWords,
  type CaptureHandle,
  type StreamHandle,
  type ShadowPartial
} from './ShadowService'
import {
  readShadowPrefs,
  writeShadowPrefs,
  type ShadowPrefs,
  type ShadowGranularity,
  type ShadowCaptureMode
} from './shadowPrefs'

type Phase = 'idle' | 'awaiting' | 'recording' | 'scoring' | 'result'

/**
 * 逐词配色：GOP 后端有 `pron_status`（发音三档 ok/warn/bad）则优先按发音上色；
 * 否则回退 v1 的内容维度 `status`（ok/wrong/missing）——GOP 未启用时零回归。
 * 见 docs/english-shadow-gop-design.md §6。
 */
function wordColorClass(w: ShadowWordResult): string {
  if (w.pron_status) {
    switch (w.pron_status) {
      case 'ok': return 'text-green-600 dark:text-green-400'
      case 'warn': return 'text-amber-600 underline decoration-dotted dark:text-amber-400'
      case 'bad': return 'text-red-500 underline decoration-wavy dark:text-red-400'
      case 'uncertain': return 'text-gray-400 decoration-dotted dark:text-gray-500'
    }
  }
  return w.status === 'ok'
    ? 'text-green-600 dark:text-green-400'
    : w.status === 'wrong'
      ? 'text-red-500 underline decoration-wavy dark:text-red-400'
      : 'text-gray-400 line-through dark:text-gray-500'
}

/** 发音四档 → 明细表的色/标(含中文标签)。 */
function pronStyle(s?: ShadowPronStatus): { cls: string, label: string } {
  switch (s) {
    case 'ok': return { cls: 'text-green-600 dark:text-green-400', label: '达标' }
    case 'warn': return { cls: 'text-amber-600 dark:text-amber-400', label: '偏弱' }
    case 'bad': return { cls: 'text-red-500 dark:text-red-400', label: '错读' }
    case 'uncertain': return { cls: 'text-gray-400 dark:text-gray-500', label: '存疑' }
    default: return { cls: 'text-gray-500', label: '—' }
  }
}

/** 音素的 IPA(若已知 ARPAbet→IPA 映射,可扩;暂直接显示 ARPAbet)。 */
function phoneLabel(p: ShadowPhoneResult): string {
  return p.ph
}

interface ActiveUnit {
  sentence: Sentence
  /** word 模式的词序列；sentence 模式为空。 */
  words: string[]
  /** word 模式当前词序号。 */
  wordPos: number
}

function dynamicMaxMs(refText: string): number {
  const n = splitWords(refText).length
  return Math.min(15000, Math.max(4000, n * 700 + 2000))
}

export default function ShadowPanel() {
  const audioService = AudioPlayerService.getInstance()

  const [prefs, setPrefs] = useState<ShadowPrefs>(readShadowPrefs)
  const [phase, setPhase] = useState<Phase>('idle')
  const [score, setScore] = useState<ShadowScore | null>(null)
  const [stat, setStat] = useState<ShadowStat | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [info, setInfo] = useState<string | null>(null)
  // 流式 partial:逐词落定的临时分(committed,渲染为 tentative);final 到达即清空改用 score。
  const [partials, setPartials] = useState<Map<number, ShadowPartial>>(new Map())
  // 明细表:当前展开的词(看其逐音素);评分细则说明折叠。
  const [expandedWord, setExpandedWord] = useState<number | null>(null)
  const [showRules, setShowRules] = useState(false)
  // 上一次「自己读的」录音,供回放(批量=webm blob;流式=拼成的 16k wav blob)。
  const [myAudioUrl, setMyAudioUrl] = useState<string | null>(null)
  const myAudioUrlRef = useRef<string | null>(null)
  const myAudioElRef = useRef<HTMLAudioElement | null>(null)

  // 异步流程里读到的「当前态」走 ref，避开事件回调闭包过期。
  const prefsRef = useRef(prefs)
  const unitRef = useRef<ActiveUnit | null>(null)
  const captureRef = useRef<CaptureHandle | null>(null)
  const streamRef = useRef<StreamHandle | null>(null)
  prefsRef.current = prefs

  // 分数变化(新结果 / 清空)→ 收起音素明细,避免指向过期的词。
  useEffect(() => { setExpandedWord(null) }, [score])

  const persist = useCallback((next: ShadowPrefs) => {
    setPrefs(next)
    writeShadowPrefs(next)
    audioService.setShadowGate(next.enabled)
  }, [audioService])

  // 记下「自己读的」录音(撤销旧 URL,建新 object URL),供回放。
  const setMyRecording = useCallback((blob: Blob) => {
    if (myAudioUrlRef.current) URL.revokeObjectURL(myAudioUrlRef.current)
    const u = URL.createObjectURL(blob)
    myAudioUrlRef.current = u
    setMyAudioUrl(u)
  }, [])

  // 回放自己的录音。用独立一次性 Audio,不走参考音频的统一播放器(避免扰动闸门/歌单)。
  const playMyRecording = useCallback(() => {
    const u = myAudioUrlRef.current
    if (!u) return
    if (!myAudioElRef.current) myAudioElRef.current = new Audio()
    const el = myAudioElRef.current
    try { el.pause(); el.currentTime = 0; el.src = u; void el.play() } catch { /* ignore */ }
  }, [])

  // 当前跟读单元的参考文本（整句 or 当前词）。
  const currentRefText = useCallback((u: ActiveUnit): string => {
    if (prefsRef.current.granularity === 'word') return u.words[u.wordPos] ?? ''
    return u.sentence.text
  }, [])

  // runCapture / advanceUnit 互相调用：用 ref 打破环，避免 use-before-define 与闭包过期。
  const runCaptureRef = useRef<() => Promise<void>>()

  // 推进到下一个单元。word 模式先走词，词读完再进下一句；sentence 模式直接进下一句。
  const advanceUnit = useCallback((passed: boolean) => {
    const u = unitRef.current
    const p = prefsRef.current
    setScore(null)
    setInfo(null)
    if (u && p.granularity === 'word' && u.wordPos + 1 < u.words.length) {
      u.wordPos += 1
      unitRef.current = u
      setPhase('awaiting')
      // 词模式不重播参考音频，直接进入下一个词的采集（auto 模式自动开录）。
      if (p.captureMode === 'auto') void runCaptureRef.current?.()
      return
    }
    // 整句完成 / 词序列读完 → 交回播放器放下一句（其参考音频播完会再次触发闸门）。
    setPhase('idle')
    unitRef.current = null
    audioService.nextSentence()
    void passed
  }, [audioService])

  // 流式路径:边读边逐词点亮 + 整句结束权威分落定。返回 true=已处理;null/false=流式不可用须回退批量。
  const runStream = useCallback(async (u: ActiveUnit, p: ShadowPrefs, refText: string): Promise<boolean> => {
    setPhase('recording')
    let finalArrived = false
    const handle = await streamScore(
      {
        kind: p.granularity,
        sentenceId: u.sentence.id,
        wordIndex: p.granularity === 'word' ? u.wordPos : undefined,
        refText,
        threshold: p.passThreshold
      },
      {
        onReady: () => setInfo('评估中…'),
        onPartial: (pt: ShadowPartial) => {
          setPartials(prev => new Map(prev).set(pt.word_index, pt))
        },
        onFinal: (result: ShadowScore) => {
          finalArrived = true
          setPartials(new Map())
          setScore(result)
          setInfo(null)
          setPhase('result')
          // final 不带 stat(中继直透上游),刷新一次计数。
          void fetchShadowStats([u.sentence.id]).then(stats => {
            const match = stats.find(s => s.kind === p.granularity &&
              (p.granularity === 'word' ? s.word_index === u.wordPos : true))
            setStat(match ?? null)
          }).catch(() => { /* 忽略 */ })
          if (result.passed && p.autoAdvanceOnPass) {
            window.setTimeout(() => advanceUnit(true), 600)
          }
        },
        onError: (msg: string) => {
          if (!finalArrived) { setInfo(null); setError('流式评测：' + msg) }
        },
        onRecorded: (wav: Blob) => setMyRecording(wav)
      },
      dynamicMaxMs(refText)
    )
    if (!handle) return false // 流式不可用 → 回退批量
    streamRef.current = handle
    await handle.done
    streamRef.current = null
    // 没等到 final(出错/无声/中途关)→ 回 awaiting 等手动;final 已把 phase 设为 result。
    if (!finalArrived) setPhase('awaiting')
    return true
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [advanceUnit])
  const runStreamRef = useRef(runStream)
  runStreamRef.current = runStream

  // 跑一次采集 + 判分。done=null（没出声）→ 回到 awaiting 等手动。
  const runCapture = useCallback(async () => {
    const u = unitRef.current
    if (!u) return
    const p = prefsRef.current
    const refText = currentRefText(u)
    if (!refText.trim()) { advanceUnit(false); return }

    setError(null)
    setInfo(null)
    setScore(null)
    setPartials(new Map())

    // 流式路径(opt-in):边读边逐词点亮 + 整句结束权威分落定。WS 不可用 → 返回 null,回退批量。
    if (p.streaming) {
      const streamed = await runStream(u, p, refText)
      if (streamed) return
      // null → 流式不可用,继续走下面批量。
    }

    setPhase('recording')

    const handle = captureUtterance({ mode: p.captureMode, maxMs: dynamicMaxMs(refText) })
    captureRef.current = handle
    let captured
    try {
      captured = await handle.done
    } catch (e: any) {
      captureRef.current = null
      setPhase('awaiting')
      setError('录音失败：' + (e?.message || '请检查麦克风权限'))
      return
    }
    captureRef.current = null
    if (!captured) {
      setPhase('awaiting')
      setInfo('未检测到朗读，点「开始」重试或跳过。')
      return
    }
    // 留一份录音供「听我的录音」回放。
    setMyRecording(new Blob([new Uint8Array(captured.bytes)], { type: captured.mime || 'audio/webm' }))

    setPhase('scoring')
    try {
      const result = await scoreShadow(captured, {
        kind: p.granularity,
        sentenceId: u.sentence.id,
        wordIndex: p.granularity === 'word' ? u.wordPos : undefined,
        refText,
        threshold: p.passThreshold
      })
      setScore(result)
      setStat(result.stat ?? null)
      setPhase('result')
      // 通过 + 自动跳 → 推进；否则停在结果态等用户操作。
      if (result.passed && p.autoAdvanceOnPass) {
        window.setTimeout(() => advanceUnit(true), 600)
      }
    } catch (e: any) {
      setPhase('awaiting')
      setError('判分失败：' + (e?.message || '请重试'))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentRefText, advanceUnit])
  runCaptureRef.current = runCapture

  // 闸门事件：参考音频播完，进入跟读。
  useEffect(() => {
    const onAwait = (data: { sentence: Sentence }) => {
      if (!prefsRef.current.enabled) return
      const sentence = data.sentence
      unitRef.current = {
        sentence,
        words: prefsRef.current.granularity === 'word' ? splitWords(sentence.text) : [],
        wordPos: 0
      }
      setScore(null)
      setError(null)
      setInfo(null)
      // 回填该句已有统计。
      void fetchShadowStats([sentence.id]).then(stats => {
        const match = stats.find(s =>
          s.kind === prefsRef.current.granularity &&
          (prefsRef.current.granularity === 'word' ? s.word_index === 0 : true))
        setStat(match ?? null)
      }).catch(() => { /* 统计失败不影响跟读 */ })

      setPhase('awaiting')
      if (prefsRef.current.captureMode === 'auto') void runCapture()
    }

    // 用户手动切句 → 取消当前跟读态。
    const onSentenceChange = () => {
      captureRef.current?.stop()
      streamRef.current?.stop()
      unitRef.current = null
      setPhase('idle')
      setScore(null)
      setPartials(new Map())
    }

    audioService.addEventListener('onAwaitShadow', onAwait)
    audioService.addEventListener('onSentenceChange', onSentenceChange)
    audioService.setShadowGate(prefsRef.current.enabled)
    return () => {
      audioService.removeEventListener('onAwaitShadow', onAwait)
      audioService.removeEventListener('onSentenceChange', onSentenceChange)
      captureRef.current?.stop()
      streamRef.current?.stop()
      try { myAudioElRef.current?.pause() } catch { /* ignore */ }
      if (myAudioUrlRef.current) { URL.revokeObjectURL(myAudioUrlRef.current); myAudioUrlRef.current = null }
      // 离开跟读页 → 关掉闸门，否则共享的播放单例会停在「请跟读…」等待态，
      // 切回标注/全部页时无人推进 → 看起来"没法播放"。
      audioService.setShadowGate(false)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [audioService])

  const handleAnnotate = useCallback(async () => {
    const u = unitRef.current
    const sentence = u?.sentence ?? audioService.getState().currentSentence
    if (!sentence?.id) { setError('无法获取句子信息'); return }
    try {
      await ApiService.getInstance().annotateSentence(sentence.id, sentence.text)
      audioService.toggleAnnotation()
      setInfo('已标注重点')
    } catch (e: any) {
      setError('标注失败：' + (e?.message || '请重试'))
    }
  }, [audioService])

  const handleManualStartStop = useCallback(() => {
    if (phase === 'recording') {
      captureRef.current?.stop()
    } else {
      void runCapture()
    }
  }, [phase, runCapture])

  const retry = useCallback(() => {
    setScore(null)
    void runCapture()
  }, [runCapture])

  const replayThenShadow = useCallback(() => {
    setScore(null)
    setPhase('idle')
    audioService.replayCurrent() // 重播参考音频，播完再次触发闸门
  }, [audioService])

  // ── 渲染 ───────────────────────────────────────────────────────────────────
  const refText = unitRef.current ? currentRefText(unitRef.current) : ''
  const isWord = prefs.granularity === 'word'
  const wordProgress = unitRef.current && isWord
    ? `第 ${unitRef.current.wordPos + 1} / ${unitRef.current.words.length} 词`
    : null

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-gray-200 p-3 dark:border-gray-700">
      {/* 标题 + 总开关 */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-sm font-medium text-gray-700 dark:text-gray-200">
          <Mic size={16} /> 跟读判分
        </div>
        <label className="flex items-center gap-2 text-xs text-gray-700 dark:text-gray-300">
          <input
            type="checkbox"
            checked={prefs.enabled}
            onChange={e => persist({ ...prefs, enabled: e.target.checked })}
            className="rounded"
          />
          开启
        </label>
      </div>

      {prefs.enabled && (
        <>
          {/* 设置行 */}
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-xs text-gray-600 dark:text-gray-400">
            <div className="flex items-center gap-1">
              <span>粒度：</span>
              {(['sentence', 'word'] as ShadowGranularity[]).map(g => (
                <button
                  key={g}
                  onClick={() => persist({ ...prefs, granularity: g })}
                  className={[
                    'rounded px-2 py-0.5',
                    prefs.granularity === g
                      ? 'bg-blue-600 text-white'
                      : 'border border-gray-300 hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800'
                  ].join(' ')}
                >
                  {g === 'sentence' ? '整句' : '逐词'}
                </button>
              ))}
            </div>
            <label className="flex items-center gap-1">
              <input
                type="checkbox"
                checked={prefs.autoAdvanceOnPass}
                onChange={e => persist({ ...prefs, autoAdvanceOnPass: e.target.checked })}
                className="rounded"
              />
              通过即自动跳
            </label>
            <label className="flex items-center gap-1" title="边读边逐词点亮发音分（需 GOP 流式后端，不可用时自动回退）">
              <input
                type="checkbox"
                checked={prefs.streaming}
                onChange={e => persist({ ...prefs, streaming: e.target.checked })}
                className="rounded"
              />
              流式评测
            </label>
            <div className="flex items-center gap-1">
              <span>采集：</span>
              {(['auto', 'button'] as ShadowCaptureMode[]).map(m => (
                <button
                  key={m}
                  onClick={() => persist({ ...prefs, captureMode: m })}
                  className={[
                    'rounded px-2 py-0.5',
                    prefs.captureMode === m
                      ? 'bg-blue-600 text-white'
                      : 'border border-gray-300 hover:bg-gray-100 dark:border-gray-600 dark:hover:bg-gray-800'
                  ].join(' ')}
                >
                  {m === 'auto' ? '自动' : '按钮'}
                </button>
              ))}
            </div>
            <div className="flex items-center gap-1">
              <span>通过阈值 {Math.round(prefs.passThreshold * 100)}%</span>
              <input
                type="range" min={50} max={100} step={5}
                value={Math.round(prefs.passThreshold * 100)}
                onChange={e => persist({ ...prefs, passThreshold: Number(e.target.value) / 100 })}
              />
            </div>
          </div>

          {/* 当前单元 + 统计 */}
          <div className="flex items-center justify-between text-xs text-gray-500 dark:text-gray-400">
            <span>{wordProgress ?? (refText ? '整句跟读' : '等待播放…')}</span>
            {stat && (
              <span>
                成功 <b className="text-green-600 dark:text-green-400">{stat.success_count}</b>
                {' / '}失败 <b className="text-red-500 dark:text-red-400">{stat.fail_count}</b>
              </span>
            )}
          </div>

          {/* 参考文本 / 逐词标色。final 用权威分;否则流式 partial 渐进点亮(tentative 半透明);都没有则纯文本。 */}
          <div className="min-h-10 rounded-md bg-gray-50 px-3 py-2 text-sm dark:bg-gray-800">
            {score
              ? (
                <span className="leading-relaxed">
                  {score.words.map((w, i) => {
                    const hasPhones = (w.phones?.length ?? 0) > 0
                    return (
                      <span
                        key={i}
                        onClick={hasPhones ? () => setExpandedWord(expandedWord === i ? null : i) : undefined}
                        className={`${wordColorClass(w)} ${hasPhones ? 'cursor-pointer' : ''} ${expandedWord === i ? 'bg-blue-100 dark:bg-blue-900/40 rounded' : ''}`}
                        title={w.pron_status && w.score != null ? `发音 ${Math.round(w.score * 100)}%${hasPhones ? ' · 点击看音素' : ''}` : undefined}
                      >
                        {w.ref}{' '}
                      </span>
                    )
                  })}
                </span>
              )
              : partials.size > 0
                ? (
                  <span className="leading-relaxed">
                    {splitWords(refText).map((w, i) => {
                      const pt = partials.get(i)
                      return (
                        <span
                          key={i}
                          className={pt
                            ? `${wordColorClass({ ref: w, status: 'ok', pron_status: pt.pron_status, score: pt.score })} opacity-70`
                            : 'text-gray-400 dark:text-gray-500'}
                          title={pt?.score != null ? `评估中 ${Math.round(pt.score * 100)}%` : '评估中…'}
                        >
                          {w}{' '}
                        </span>
                      )
                    })}
                  </span>
                )
                : <span className="text-gray-700 dark:text-gray-200">{refText || '—'}</span>}
          </div>

          {/* 展开的词 → 逐音素**诊断明细表**:对齐区间 / 真实峰(是否错位)/ 分 / 判定 / 说明 */}
          {score && expandedWord != null && (() => {
            const w = score.words[expandedWord]
            const phones = w?.phones ?? []
            if (phones.length === 0) return null
            return (
              <div className="overflow-x-auto rounded-md border border-gray-200 bg-white text-xs dark:border-gray-700 dark:bg-gray-900">
                <div className="flex items-center justify-between border-b border-gray-100 px-2 py-1 text-gray-500 dark:border-gray-700">
                  <span>「<b>{w.ref}</b>」音素诊断（点波形时间段定位;灰=引擎没对齐,不算你错）</span>
                  <span className="cursor-pointer hover:text-gray-700 dark:hover:text-gray-300" onClick={() => setExpandedWord(null)}>收起 ✕</span>
                </div>
                <table className="w-full whitespace-nowrap">
                  <thead className="text-gray-400">
                    <tr>
                      <th className="px-2 py-1 text-left font-normal">音素</th>
                      <th className="px-2 py-1 text-left font-normal">对齐区间</th>
                      <th className="px-2 py-1 text-left font-normal">真实峰</th>
                      <th className="px-2 py-1 text-left font-normal">你的分</th>
                      <th className="px-2 py-1 text-left font-normal">判定</th>
                      <th className="px-2 py-1 text-left font-normal">说明</th>
                    </tr>
                  </thead>
                  <tbody>
                    {phones.map((p: ShadowPhoneResult, i) => {
                      const sty = pronStyle(p.pron_status)
                      const span = (p.t_start != null && p.t_end != null) ? `${p.t_start.toFixed(2)}–${p.t_end.toFixed(2)}s` : '—'
                      // 真实峰:落在区间内→「区间内」;在外→显示偏移(错位指标)
                      let peakCell = '—'
                      let misaligned = false
                      if (p.peak_t != null && p.t_start != null && p.t_end != null) {
                        if (p.peak_t >= p.t_start && p.peak_t <= p.t_end) {
                          peakCell = '区间内'
                        } else {
                          const off = p.peak_t < p.t_start ? p.peak_t - p.t_start : p.peak_t - p.t_end
                          peakCell = `@${p.peak_t.toFixed(2)}s (${off > 0 ? '+' : ''}${off.toFixed(2)}s)`
                          misaligned = Math.abs(off) > 0.08 // >~80ms 视为明显错位
                        }
                      }
                      const note = p.pron_status === 'uncertain'
                        ? '没对齐好/没听准,不算读错'
                        : (p.expected_ph && p.actual_ph
                          ? `读成了 ${p.actual_ph}`
                          : (misaligned && p.pron_status !== 'ok'
                            ? '对齐错位,引擎没对准'
                            : (p.hint ?? (p.pron_status === 'ok' ? '清晰' : '后验偏弱'))))
                      return (
                        <tr key={i} className="border-t border-gray-50 dark:border-gray-800">
                          <td className="px-2 py-1 font-mono">{phoneLabel(p)}</td>
                          <td className="px-2 py-1 text-gray-500 dark:text-gray-400">{span}</td>
                          <td className={`px-2 py-1 ${misaligned ? 'text-amber-600 dark:text-amber-400' : 'text-gray-500 dark:text-gray-400'}`}>{peakCell}</td>
                          <td className="px-2 py-1">
                            <span className="inline-flex items-center gap-1">
                              <span className="inline-block h-1.5 w-10 rounded bg-gray-200 dark:bg-gray-700">
                                <span className={`block h-1.5 rounded ${p.pron_status === 'uncertain' ? 'bg-gray-400' : p.pron_status === 'bad' ? 'bg-red-400' : p.pron_status === 'warn' ? 'bg-amber-400' : 'bg-green-400'}`} style={{ width: `${Math.round((p.score ?? 0) * 100)}%` }} />
                              </span>
                              {p.pron_status === 'uncertain' ? '—' : `${Math.round((p.score ?? 0) * 100)}`}
                            </span>
                          </td>
                          <td className={`px-2 py-1 ${sty.cls}`}>{sty.label}</td>
                          <td className="px-2 py-1 text-gray-500 dark:text-gray-400">{note}</td>
                        </tr>
                      )
                    })}
                  </tbody>
                </table>
              </div>
            )
          })()}

          {/* 图例 + 评分细则说明(让用户看懂分数 + 区分"读错"vs"引擎没对齐") */}
          {score && (
            <div className="text-[11px] text-gray-500 dark:text-gray-400">
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                <span className="text-green-600 dark:text-green-400">● 达标</span>
                <span className="text-amber-600 dark:text-amber-400">● 偏弱</span>
                <span className="text-red-500 dark:text-red-400">● 错读</span>
                <span className="text-gray-400">● 存疑(引擎没对齐,不算错)</span>
                <span className="cursor-pointer underline" onClick={() => setShowRules(v => !v)}>评分细则 {showRules ? '▲' : '▼'}</span>
              </div>
              {showRules && (
                <div className="mt-1 rounded bg-gray-50 p-2 leading-relaxed dark:bg-gray-800">
                  逐音素打「发音分」(0–100),自下而上聚合到词、句。<b>点词可展开音素明细</b>。
                  <b>灰色「存疑」</b>= 引擎没把这个音对齐好/没听准(连读快词常见),<b>不算你读错</b>、不拉低分。
                  通过 = 句分 ≥ {Math.round(prefs.passThreshold * 100)}% 且严重错读音素 ≤ 1。
                  分数由模型给出仅供参考(标定持续优化中),可点「听我的」回放自行判断。
                </div>
              )}
            </div>
          )}

          {/* 状态 + 分数 */}
          <div className="flex min-h-6 items-center gap-2 text-sm">
            {phase === 'recording' && (
              <span className="flex items-center gap-1 text-amber-600 dark:text-amber-400">
                <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-red-500" /> 录音中…
              </span>
            )}
            {phase === 'scoring' && (
              <span className="flex items-center gap-1 text-gray-500"><Loader2 size={14} className="animate-spin" /> 识别评分中…</span>
            )}
            {phase === 'result' && score && (
              <span className={`flex items-center gap-1 ${score.passed ? 'text-green-600 dark:text-green-400' : 'text-red-500 dark:text-red-400'}`}>
                {score.passed ? <CheckCircle2 size={16} /> : <XCircle size={16} />}
                {score.passed ? '通过' : '未通过'} · {Math.round(score.score * 100)}%
              </span>
            )}
            {info && <span className="text-gray-500 dark:text-gray-400">{info}</span>}
            {error && <span className="text-red-500 dark:text-red-400">{error}</span>}
          </div>

          {/* 操作按钮 */}
          <div className="flex flex-wrap items-center gap-2">
            <Button
              variant={phase === 'recording' ? 'danger' : 'primary'}
              size="sm"
              onClick={handleManualStartStop}
              disabled={phase === 'scoring' || phase === 'idle'}
              title={phase === 'recording' ? '停止录音' : '开始录音'}
            >
              {phase === 'recording' ? <Square size={14} /> : <Mic size={14} />}
              {phase === 'recording' ? '停止' : '开始'}
            </Button>
            <Button variant="outline" size="sm" onClick={retry} disabled={phase === 'recording' || phase === 'scoring'} title="重读当前">
              <RotateCcw size={14} /> 重读
            </Button>
            <Button variant="outline" size="sm" onClick={replayThenShadow} disabled={phase === 'recording' || phase === 'scoring'} title="重听参考再读">
              重听
            </Button>
            <Button variant="outline" size="sm" onClick={playMyRecording} disabled={!myAudioUrl || phase === 'recording'} title="回放上一次自己读的录音">
              <Volume2 size={14} /> 听我的
            </Button>
            <Button variant="outline" size="sm" onClick={() => advanceUnit(false)} disabled={phase === 'recording' || phase === 'scoring'} title="跳过当前">
              <SkipForward size={14} /> 跳过
            </Button>
            <Button variant="outline" size="sm" onClick={handleAnnotate} title="标注重点，后续重点学习">
              <Star size={14} /> 标注重点
            </Button>
          </div>
        </>
      )}
    </div>
  )
}
