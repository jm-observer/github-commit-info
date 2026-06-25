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
import { Mic, Square, SkipForward, RotateCcw, Star, Loader2, CheckCircle2, XCircle } from 'lucide-react'
import { AudioPlayerService } from '../services/AudioPlayerService'
import ApiService from '../services/ApiService'
import type { Sentence, ShadowScore, ShadowStat, ShadowWordResult } from '../types'
import { Button } from '../../speech/components/ui/Button'
import {
  captureUtterance,
  scoreShadow,
  fetchShadowStats,
  splitWords,
  type CaptureHandle
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
    }
  }
  return w.status === 'ok'
    ? 'text-green-600 dark:text-green-400'
    : w.status === 'wrong'
      ? 'text-red-500 underline decoration-wavy dark:text-red-400'
      : 'text-gray-400 line-through dark:text-gray-500'
}

/** 从判分结果收集需要提示的错读音素（warn/bad），附所属词，供针对性纠音展示。 */
function collectPhoneHints(score: ShadowScore): { word: string, ph: string, hint: string }[] {
  const out: { word: string, ph: string, hint: string }[] = []
  for (const w of score.words) {
    for (const p of w.phones ?? []) {
      if (p.pron_status === 'ok') continue
      const hint = p.hint
        ?? (p.expected_ph && p.actual_ph ? `${p.expected_ph} 读成了 ${p.actual_ph}` : `${p.ph} 发音偏弱`)
      out.push({ word: w.ref, ph: p.ph, hint })
    }
  }
  return out
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

  // 异步流程里读到的「当前态」走 ref，避开事件回调闭包过期。
  const prefsRef = useRef(prefs)
  const unitRef = useRef<ActiveUnit | null>(null)
  const captureRef = useRef<CaptureHandle | null>(null)
  prefsRef.current = prefs

  const persist = useCallback((next: ShadowPrefs) => {
    setPrefs(next)
    writeShadowPrefs(next)
    audioService.setShadowGate(next.enabled)
  }, [audioService])

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
      unitRef.current = null
      setPhase('idle')
      setScore(null)
    }

    audioService.addEventListener('onAwaitShadow', onAwait)
    audioService.addEventListener('onSentenceChange', onSentenceChange)
    audioService.setShadowGate(prefsRef.current.enabled)
    return () => {
      audioService.removeEventListener('onAwaitShadow', onAwait)
      audioService.removeEventListener('onSentenceChange', onSentenceChange)
      captureRef.current?.stop()
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

          {/* 参考文本 / 逐词标色 */}
          <div className="min-h-10 rounded-md bg-gray-50 px-3 py-2 text-sm dark:bg-gray-800">
            {score
              ? (
                <span className="leading-relaxed">
                  {score.words.map((w, i) => (
                    <span
                      key={i}
                      className={wordColorClass(w)}
                      title={w.pron_status && w.score != null ? `发音 ${Math.round(w.score * 100)}%` : undefined}
                    >
                      {w.ref}{' '}
                    </span>
                  ))}
                </span>
              )
              : <span className="text-gray-700 dark:text-gray-200">{refText || '—'}</span>}
          </div>

          {/* 错读音素提示（仅 GOP 后端有 phones 时显示；针对性纠音） */}
          {score && (() => {
            const hints = collectPhoneHints(score)
            if (hints.length === 0) return null
            return (
              <div className="flex flex-wrap gap-1.5 text-xs">
                {hints.map((h, i) => (
                  <span
                    key={i}
                    className="rounded bg-red-50 px-1.5 py-0.5 text-red-600 dark:bg-red-900/30 dark:text-red-300"
                  >
                    <b>{h.word}</b> · {h.hint}
                  </span>
                ))}
              </div>
            )
          })()}

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
