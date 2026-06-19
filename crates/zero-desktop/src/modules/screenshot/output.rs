//! 截图输出：落盘 `<workspace>/screenshots/`（剪贴板写入在 commands 里，需 AppHandle）。
//! 设计文档 §4 输出契约。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// screenshots 目录：`<workspace>/screenshots/`。
pub fn screenshots_dir(workspace: &Path) -> PathBuf {
    workspace.join("screenshots")
}

/// 冻结帧临时文件（覆盖式）：供叠加窗口经 asset 协议加载。文件名带前导点便于识别。
pub fn frozen_frame_path(workspace: &Path) -> PathBuf {
    screenshots_dir(workspace).join(".frozen-frame.png")
}

/// 截图设置文件路径。
pub fn settings_path(workspace: &Path) -> PathBuf {
    screenshots_dir(workspace).join("settings.json")
}

/// 把 PNG bytes 落盘到 `<workspace>/screenshots/<yyyyMMdd-HHmmss-mmm>.png`，返回绝对路径。
/// 同秒多张用毫秒后缀避免覆盖。
pub fn save_png(workspace: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let dir = screenshots_dir(workspace);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建截图目录失败: {}", dir.display()))?;
    let now = chrono::Local::now();
    let name = format!(
        "{}-{:03}.png",
        now.format("%Y%m%d-%H%M%S"),
        now.timestamp_subsec_millis()
    );
    let path = dir.join(name);
    std::fs::write(&path, bytes).with_context(|| format!("写截图失败: {}", path.display()))?;
    Ok(path)
}
