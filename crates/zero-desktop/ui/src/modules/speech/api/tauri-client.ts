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
  auto_start: boolean;
}

export const DEFAULT_REMOTE_URL = 'ws://192.168.0.68:8090/stream';

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

export type SampleLabel = 'asr_wrong' | 'hotword' | 'bad_optimize' | 'ok' | 'other';

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
  audio_path?: string | null;
  audio_status: 'saved' | 'expired' | 'fetch_failed' | 'skipped' | string;
  hotword_sync?: 'added' | 'exists' | 'failed' | null;
  marked_at: string;
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
  applySettings: (newSettings: AppSettings) =>
    invoke('speech_apply_settings', { newSettings }),
  markSample: (args: MarkSampleArgs) => invoke<Sample>('speech_mark_sample', args),
  listSamples: () => invoke<Sample[]>('speech_list_samples'),
  exportSamples: () => invoke<string>('speech_export_samples'),
  openInFolder: (path: string) => invoke('speech_open_in_folder', { path }),
};
