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

/** 单个参考词的判定结果（逐词标色用）。与后端 shadow::WordResult 对齐。 */
export interface ShadowWordResult {
  ref: string
  status: 'ok' | 'wrong' | 'missing'
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
