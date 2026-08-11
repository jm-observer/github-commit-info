// 录屏模块共享类型（与 Rust 侧 serde 结构一一对应）。

/** 录屏设置（`recording_get_settings` / `recording_save_settings`）。 */
export interface RecordingSettings {
  hotkey: string;
  fps: number;
  /** ffmpeg.exe 路径；空串 = 自动查找（PATH + 常见安装位置）。 */
  ffmpeg_path: string;
  save_dir: string;
  capture_cursor: boolean;
  /** x264 CRF，越小越清晰、文件越大。 */
  crf: number;
  /** 热键触发时是否先框选区域（false = 直接录整块屏）。 */
  select_region: boolean;
}

/** 录制状态（`recording_status`，前端轮询）。 */
export interface RecordingStatus {
  state: "idle" | "recording" | "paused";
  elapsed_ms: number;
  path: string;
  width: number;
  height: number;
  fps: number;
}

/** ffmpeg 探测结果（`recording_detect_ffmpeg`）。 */
export interface FfmpegInfo {
  found: boolean;
  path: string;
  version: string;
  error: string;
}

/** 一条历史录屏（`recording_list`）。 */
export interface RecordingItem {
  name: string;
  path: string;
  modified_ms: number;
  size: number;
  /** 时长（毫秒；sidecar 缺失时为 0）。 */
  duration_ms: number;
  width: number;
  height: number;
  fps: number;
}

/** 毫秒 → `mm:ss` / `h:mm:ss`。 */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return h > 0 ? `${h}:${mm}:${String(s).padStart(2, "0")}` : `${mm}:${String(s).padStart(2, "0")}`;
}

/** 字节 → 人类可读体积。 */
export function formatSize(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
}
