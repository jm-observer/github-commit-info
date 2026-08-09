use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// 幂等建立 workspace 目录树。
pub fn ensure_workspace(path: &Path) -> Result<()> {
    let subdirs = [
        "logs",
        "english",
        "english/audio-cache",
        "speech",
        "cookie",
        "cookie/login_profile/douyin",
        "cookie/login_profile/ths",
        "music",
        "music/covers",
    ];
    for sub in &subdirs {
        let dir = path.join(sub);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create workspace subdir: {}", dir.display()))?;
    }
    Ok(())
}

/// speech 模块的 SQLite 数据库路径。
pub fn speech_db_path(workspace: &Path) -> PathBuf {
    workspace.join("speech").join("speech_history.db")
}

/// speech 模块的根目录。
#[allow(dead_code)]
pub fn speech_dir(workspace: &Path) -> PathBuf {
    workspace.join("speech")
}

/// cookie 模块的 SQLite 数据库路径。
pub fn cookie_db_path(workspace: &Path) -> PathBuf {
    workspace.join("cookie").join("state.db")
}

/// cookie 模块的设置文件路径（保留，仅存 cookie 自己的本地偏好，G10 配置在 app.json）。
#[allow(dead_code)]
pub fn cookie_settings_path(workspace: &Path) -> PathBuf {
    workspace.join("cookie").join("settings.json")
}

/// 抖音登录 Chrome profile 目录。
pub fn douyin_profile_dir(workspace: &Path) -> PathBuf {
    workspace
        .join("cookie")
        .join("login_profile")
        .join("douyin")
}

/// 同花顺登录 Chrome profile 目录。
pub fn ths_profile_dir(workspace: &Path) -> PathBuf {
    workspace.join("cookie").join("login_profile").join("ths")
}

/// 同花顺 cookies.json 路径（与 stock-trade 兼容的格式）。
pub fn ths_cookies_path(workspace: &Path) -> PathBuf {
    workspace.join("cookie").join("ths_cookies.json")
}

/// english 模块根目录。
#[allow(dead_code)]
pub fn english_dir(workspace: &Path) -> PathBuf {
    workspace.join("english")
}

/// english 音频缓存目录。
pub fn english_audio_cache_dir(workspace: &Path) -> PathBuf {
    workspace.join("english").join("audio-cache")
}
