/**
 * 跟读（Shadow Reading）偏好，存 localStorage，跨重启保留。
 * 设计见 docs/english-shadow-design.md §8。
 */

export type ShadowGranularity = 'sentence' | 'word'
export type ShadowCaptureMode = 'auto' | 'button'

export interface ShadowPrefs {
  /** 跟读总开关（整个流程级）。默认关。 */
  enabled: boolean
  /** 粒度：整句 / 逐词。 */
  granularity: ShadowGranularity
  /** 通过即自动跳下一个；关则通过也停在原地等手动。 */
  autoAdvanceOnPass: boolean
  /** 命中率阈值 0~1。 */
  passThreshold: number
  /** auto=播完自动开麦+静音判停；button=点按钮才录。 */
  captureMode: ShadowCaptureMode
}

const KEY = 'english_shadow_prefs_v1'

export const DEFAULT_SHADOW_PREFS: ShadowPrefs = {
  enabled: false,
  granularity: 'sentence',
  autoAdvanceOnPass: true,
  passThreshold: 0.9,
  captureMode: 'auto'
}

export function readShadowPrefs(): ShadowPrefs {
  if (typeof window === 'undefined') return { ...DEFAULT_SHADOW_PREFS }
  try {
    const raw = window.localStorage.getItem(KEY)
    if (!raw) return { ...DEFAULT_SHADOW_PREFS }
    const parsed = JSON.parse(raw) as Partial<ShadowPrefs>
    return { ...DEFAULT_SHADOW_PREFS, ...parsed }
  } catch {
    return { ...DEFAULT_SHADOW_PREFS }
  }
}

export function writeShadowPrefs(prefs: ShadowPrefs): void {
  if (typeof window === 'undefined') return
  window.localStorage.setItem(KEY, JSON.stringify(prefs))
}
