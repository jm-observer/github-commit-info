/**
 * English 模块类型定义（从 shared/types 内化，无 AntD 依赖）。
 */

export interface Audio {
  id: number
  url?: string
  src?: string
  [key: string]: any
}

export interface Sentence {
  id: number
  text: string
  audios: Audio[]
  is_annotated?: boolean
  has_error?: boolean
  [key: string]: any
}

/** 音频包（「按包来听」）。与后端 package.list 返回项对齐（迁自 mini-program Package）。 */
export interface Package {
  id: number
  title: string
  description?: string | null
  sentence_count?: number
  [key: string]: any
}

/** 发音四档（GOP 后端）。`uncertain`=引擎没把这个音对齐好,不判对错。与后端 pron_status 对齐。 */
export type ShadowPronStatus = 'ok' | 'warn' | 'bad' | 'uncertain'

/** 单个参考音素的发音评测结果（GOP 后端）。与后端 shadow::PhoneResult 对齐。 */
export interface ShadowPhoneResult {
  ph: string
  score: number
  pron_status: ShadowPronStatus
  /** 错读时的期望/实际音素（结构化，展示以此为准；hint 仅兜底文案）。 */
  expected_ph?: string
  actual_ph?: string
  hint?: string
  /** 对齐可靠性：false → 引擎没对齐好(uncertain),不算读错。 */
  reliable?: boolean
  /** 该音素对齐时间段(秒)。 */
  t_start?: number
  t_end?: number
  /** 诊断：该音素全局峰时间(秒)。落在 [t_start,t_end] 外 = 对齐错位。 */
  peak_t?: number
  /** 诊断：对齐段内 canonical 峰值 log 后验(原始 GOP,≤0)。 */
  gop_raw?: number
}

/**
 * 单个参考词的判定结果（逐词标色用）。与后端 shadow::WordResult 对齐。
 *
 * `status`（内容对错，v1 + 回退态恒有）与 `pron_status`/`score`/`phones`（发音维度，
 * 仅 GOP 后端）是两套独立维度——见 docs/english-shadow-gop-design.md §5。
 * GOP 未启用时后三者缺失，按 `status` 回退渲染，零回归。
 */
export interface ShadowWordResult {
  ref: string
  status: 'ok' | 'wrong' | 'missing'
  /** 词级发音分 0~1（GOP）。 */
  score?: number
  /** 发音三档（GOP）。 */
  pron_status?: ShadowPronStatus
  /** 逐音素明细（GOP；granularity=sentence 时上游省略）。 */
  phones?: ShadowPhoneResult[]
}

/** 一个跟读单元的累计统计。与后端 shadow::store::StatRow 对齐。 */
export interface ShadowStat {
  kind: 'sentence' | 'word'
  sentence_id: number
  word_index?: number
  success_count: number
  fail_count: number
  last_score?: number
  last_passed?: boolean
  last_at?: string
}

/** 一次跟读判分结果。与后端 /api/web/shadow/score 响应对齐。 */
export interface ShadowScore {
  transcript: string
  ref_text: string
  score: number
  passed: boolean
  asr_model?: string
  words: ShadowWordResult[]
  /** 严重错读音素总数（GOP 后端）。 */
  bad_phone_count?: number
  /** 评测模型标识（GOP 后端，如 wav2vec2-gop-v1）。 */
  model?: string
  stat?: ShadowStat | null
}

export interface EnvConfig {
  apiBaseUrl: string
  customerId?: number
  audioCoverUrl?: string
}

export type AudioPlayerEventType =
  | 'onPlayStateChange'
  | 'onStatusTextChange'
  | 'onSentenceChange'
  | 'onPlayComplete'
  | 'onPlayNext'
  | 'onPlayPrevious'
  | 'onToggleAnnotation'
  | 'onToggleReportError'
  | 'onPlayCountChange'
  | 'onTextToggle'
  | 'onAwaitShadow'

export interface AudioPlayerEventData {
  onPlayStateChange: { isPlaying: boolean }
  onStatusTextChange: { statusText: string }
  onSentenceChange: { sentences: Sentence[]; currentSentenceIndex: number }
  onPlayComplete: Record<string, never>
  onPlayNext: { sentenceId?: number; sentenceIndex: number; sentence: Sentence }
  onPlayPrevious: { sentenceId?: number; sentenceIndex: number; sentence: Sentence }
  onToggleAnnotation: { sentenceId: number; isAnnotated: boolean; sentence: Sentence }
  onToggleReportError: { sentenceId: number; hasError: boolean; sentence: Sentence }
  onPlayCountChange: {
    playCount: number
    currentSentenceIndex: number
    currentAudioIndex: number
    maxPlayCount: number
  }
  onTextToggle: { showText: boolean }
  /** 跟读闸门：当前句参考音频播放完毕、等待用户跟读判分（仅 shadowGate 开启时触发）。 */
  onAwaitShadow: { sentence: Sentence; sentenceIndex: number }
}

export interface AudioPlayerState {
  isPlaying: boolean
  currentSentenceIndex: number
  currentAudioIndex: number
  playCount: number
  maxPlayCount: number
  stopMode: 'halfHour' | 'roundEnd' | null
  statusText: string
  sentences: Sentence[]
  currentSentence: Sentence | null
  currentAudio: Audio | null
  showText?: boolean
}

export interface CacheItem {
  key: string
  filePath: string
  url: string
  size: number
  createdAt: number
}

export interface CacheStats {
  totalSize: number
  totalCount: number
  items: CacheItem[]
}

export interface ApiRequest {
  method: string
  params?: Record<string, any>
}

export interface ApiResponse<T = any> {
  code: number
  msg: string
  data: T
}
