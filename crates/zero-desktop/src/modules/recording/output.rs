//! 录屏输出：目录约定、文件命名、sidecar 元数据。
//!
//! sidecar（`<name>.json`）存录制时长/分辨率/帧率——mp4 的时长要解析容器才能拿到，
//! 而我们在停止的那一刻本来就知道，写一行 json 比让前端去 ffprobe 便宜得多。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// recordings 目录：`<workspace>/recordings/`。
pub fn recordings_dir(workspace: &Path) -> PathBuf {
    workspace.join("recordings")
}

/// 录屏设置文件路径。
pub fn settings_path(workspace: &Path) -> PathBuf {
    recordings_dir(workspace).join("settings.json")
}

/// 生成一个新的录屏文件路径：`<dir>/<yyyyMMdd-HHmmss>.mp4`（目录顺带建好）。
pub fn new_video_path(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("创建录屏目录失败: {}", dir.display()))?;
    let now = chrono::Local::now();
    let mut path = dir.join(format!("{}.mp4", now.format("%Y%m%d-%H%M%S")));
    // 同秒内重录（热键连按）→ 加毫秒后缀，绝不覆盖已有文件。
    if path.exists() {
        path = dir.join(format!(
            "{}-{:03}.mp4",
            now.format("%Y%m%d-%H%M%S"),
            now.timestamp_subsec_millis()
        ));
    }
    Ok(path)
}

/// 一次录制的 sidecar 元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingMeta {
    /// 录制时长（毫秒，不含暂停时间）。
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// 实际写进 ffmpeg 的帧数（含补帧，用于排查掉帧）。
    pub frames: u64,
}

fn meta_path(video: &Path) -> PathBuf {
    video.with_extension("json")
}

/// 写 sidecar（失败只告警：视频本身已经录好了，元数据丢了不该算录制失败）。
pub fn write_meta(video: &Path, meta: &RecordingMeta) {
    match serde_json::to_string_pretty(meta) {
        Ok(json) => {
            if let Err(e) = std::fs::write(meta_path(video), json) {
                log::warn!(target: "recording", "写录屏元数据失败: {e}");
            }
        }
        Err(e) => log::warn!(target: "recording", "序列化录屏元数据失败: {e}"),
    }
}

/// 读 sidecar（不存在/损坏 → None）。
pub fn read_meta(video: &Path) -> Option<RecordingMeta> {
    let s = std::fs::read_to_string(meta_path(video)).ok()?;
    serde_json::from_str(&s).ok()
}

/// 删除 sidecar（随视频一起删，避免索引堆积孤儿条目）。
pub fn remove_meta(video: &Path) {
    let _ = std::fs::remove_file(meta_path(video));
}
