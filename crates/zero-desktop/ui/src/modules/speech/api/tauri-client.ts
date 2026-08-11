import { invoke } from '@tauri-apps/api/core';

export interface Segment {
  id: number | null;
  segment_id?: number | null;
  revision?: number;
  start: number;
  end: number;
  wall_start: string;
  wall_end: string;
  text_raw: string;
  text_optimized?: string;
  text_english?: string;
  text_secondary?: string;
  secondary_kind?: string;
  speaker?: string;
  optimize_status: 'pending' | 'running' | 'success' | 'failed';
  translate_status: 'blocked' | 'pending' | 'running' | 'success' | 'failed';
}

export interface RecordingState {
  recording: boolean;
}

export interface InputDeviceInfo {
  name: string;
  is_default: boolean;
}

export interface InitStatus {
  status: number;
  error?: string;
}

export type AsrLanguage = '' | 'zh' | 'en' | 'ja' | 'ko' | 'yue';
export type AutoCopyMode = 'off' | 'english' | 'optimized_zh';

export interface AppSettings {
  asr_language: AsrLanguage;
  auto_copy_mode: AutoCopyMode;
  merge_window_ms: number;
  remote_url: string;
  remote_url_presets: string[];
  want_secondary: boolean;
  notify_sound: boolean;
  auto_paste: boolean;
  auto_paste_rewrite_retype: boolean;
  auto_start: boolean;
  capture_enabled: boolean;
  /** 场景记录总开关（每次交付都记 speech_scenes）。与手动采集的 capture_enabled 是两条线。 */
  scene_log_enabled: boolean;
}

/**
 * 最终回退地址。正常路径走 `SpeechAPI.defaultRemoteUrl()`——它按 G10 的局域网/外网设置
 * 派生，与录音时实际连接的地址（`speech_start_recording` 用 `resolved.asr_url`）同源。
 * 只有后端还没返回时才短暂用到这个常量。
 */
export const DEFAULT_REMOTE_URL = 'ws://192.168.0.68:8788/api/asr/stream';

export interface SegmentDiscardedEvent {
  revision: number;
  segment_id: number;
  decision: 'DISCARD';
  reason: string;
  source: 'rule' | 'llm';
  confidence: number | null;
  occurred_at_ms: number;
}

export interface SegmentUpdatedEvent {
  id: number;
  segment_id: number;
  revision: number;
  start_sec: number;
  end_sec: number;
  wall_start: string;
  wall_end: string;
  text_raw: string;
  optimize_status: Segment['optimize_status'];
  translate_status: Segment['translate_status'];
  text_optimized?: string;
  text_english?: string;
  text_secondary?: string;
  secondary_kind?: string;
  speaker?: string;
  created_at: string;
}

// 音频清洗 API/类型已迁至 modules/audio-clean/api/clean-client.ts（CleanAPI）。

// speaker_wrong = 声纹把说话人认错（与 asr_wrong 的「文字识别错」正交，同一段可能只错一个）。
export type SampleLabel =
  | 'asr_wrong'
  | 'speaker_wrong'
  | 'hotword'
  | 'bad_optimize'
  | 'ok'
  | 'other';

export interface Sample {
  id: number;
  segment_id: number;
  session_id?: string | null;
  label: SampleLabel | string;
  text_raw: string;
  text_optimized?: string | null;
  text_english?: string | null;
  text_secondary?: string | null;
  correction?: string | null;
  note?: string | null;
  /** 标注时该段被识别成的说话人；speaker_wrong 下与 correction（正确的人）成对。 */
  speaker?: string | null;
  audio_path?: string | null;
  audio_status: 'saved' | 'expired' | 'fetch_failed' | 'skipped' | string;
  hotword_sync?: 'added' | 'exists' | 'failed' | null;
  marked_at: string;
  source: 'ui' | 'copy' | string;
  segment_ids?: string | null;
}

/** 场景记录按应用聚合的一行。 */
export interface SceneAppStat {
  app_exe?: string | null;
  count: number;
  chars: number;
  last_at?: string | null;
}

/** 场景记录按「应用 + 窗口标题」聚合的一行（收集期只原始聚合，不解析归纳）。 */
export interface SceneTitleStat {
  app_exe?: string | null;
  app_title?: string | null;
  count: number;
  chars: number;
  last_at?: string | null;
}

/** 场景记录总览（每次交付都记的全量日志，非手动采集的纠错样本）。 */
export interface SceneStats {
  total: number;
  today: number;
  total_chars: number;
  /** 抓到应用上下文的条数；与 total 的差即抓拍覆盖率缺口。 */
  with_app: number;
  distinct_apps: number;
  /** 被纠错样本回标过的记录数；统计真实表达风格时应排除这部分（含 ASR/LLM 错误）。 */
  corrected: number;
  last_at?: string | null;
  top_apps: SceneAppStat[];
  /** 按「应用 + 窗口标题」的具体场景排行（最多 15 项）。 */
  top_titles: SceneTitleStat[];
}

export interface MarkSampleArgs extends Record<string, unknown> {
  segmentId: number;
  sessionId?: string | null;
  textRaw: string;
  textOptimized?: string | null;
  textEnglish?: string | null;
  textSecondary?: string | null;
  label: SampleLabel;
  correction?: string | null;
  note?: string | null;
  syncHotword?: boolean;
  /** 卡片上当前识别到的说话人，原样带下去做快照（未知传 null）。 */
  speaker?: string | null;
}

// All commands prefixed with speech_ to match zero-desktop backend naming.
export const SpeechAPI = {
  startRecording: () => invoke('speech_start_recording'),
  stopRecording: () => invoke('speech_stop_recording'),
  getRecordingState: () => invoke<RecordingState>('speech_get_recording_state'),
  fetchRemoteHistory: (limit: number) =>
    invoke<Record<string, unknown>[]>('speech_fetch_remote_history', { limit }),
  listDevices: () => invoke<InputDeviceInfo[]>('speech_list_input_devices'),
  getSelectedDevice: () => invoke<string | null>('speech_get_selected_device'),
  setInputDevice: (deviceName: string | null) =>
    invoke('speech_set_input_device', { deviceName }),
  getInitStatus: () => invoke<InitStatus>('speech_get_init_status'),
  clearResults: () => invoke('speech_clear_results'),
  copyToClipboard: (text: string) =>
    invoke('speech_copy_text_to_clipboard', { text }),
  getSettings: () => invoke<AppSettings>('speech_get_settings'),
  /** 当前生效的内置 ASR 地址（按 G10 局域网/外网设置派生，含 token query）。 */
  defaultRemoteUrl: () => invoke<string>('speech_default_remote_url'),
  applySettings: (newSettings: AppSettings) =>
    invoke('speech_apply_settings', { newSettings }),
  /** 试听某段识别的原始音频（base64 WAV）。服务端只保留 1 天，过期报错。 */
  fetchSegmentAudio: (segmentId: number) =>
    invoke<string>('speech_fetch_segment_audio', { segmentId }),
  markSample: (args: MarkSampleArgs) => invoke<Sample>('speech_mark_sample', args),
  listSamples: () => invoke<Sample[]>('speech_list_samples'),
  exportSamples: () => invoke<string>('speech_export_samples'),
  /** 同音候选挖掘导出（P2a）：对纠错样本跑挖掘，写 JSON 供人工审读，返回文件路径。 */
  exportHomophoneCandidates: () =>
    invoke<string>('speech_export_homophone_candidates'),
  /**
   * 热词候选挖掘导出（P1.5）：纠错样本 Y' 侧 + 场景记录新词发现两路挖掘，去掉已在
   * `asr.hotwords` 里的词，写 JSON 供人工审读，返回文件路径。只导出，不改配置。
   */
  exportHotwordCandidates: () =>
    invoke<string>('speech_export_hotword_candidates'),
  sceneStats: () => invoke<SceneStats>('speech_scene_stats'),
  openInFolder: (path: string) => invoke('speech_open_in_folder', { path }),
};
