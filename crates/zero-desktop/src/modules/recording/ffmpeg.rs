//! ffmpeg 定位与探测（方案 ①：用系统上已有的 ffmpeg，不打包、不自动下载）。
//!
//! 查找顺序：**设置里的显式路径 → PATH → 常见安装位置**。任何一步命中都要能真正
//! 跑起来才算数——存在一个同名文件不代表它能执行，所以命中后统一用 `-version` 实测。
//!
//! 找不到时返回的错误串是直接要给用户看的，所以写清楚「怎么办」而不是只说「没找到」。

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 找不到 ffmpeg 时给前端的提示（附带自助路径）。
pub const NOT_FOUND_HINT: &str = "未找到 ffmpeg。请安装 ffmpeg 并把 ffmpeg.exe 加入 PATH，\
或在「录屏设置」里直接填 ffmpeg.exe 的完整路径（下载：https://www.gyan.dev/ffmpeg/builds/）。";

/// Windows 上常见的 ffmpeg 落脚点（scoop / choco / winget / 手动解压）。
#[cfg(windows)]
const COMMON_DIRS: &[&str] = &[
    r"C:\ffmpeg\bin",
    r"C:\Program Files\ffmpeg\bin",
    r"C:\ProgramData\chocolatey\bin",
];

#[cfg(not(windows))]
const COMMON_DIRS: &[&str] = &["/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"];

#[cfg(windows)]
const EXE: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const EXE: &str = "ffmpeg";

/// 按 `hint`（设置里的路径，可为空）解析出一个**实测可执行**的 ffmpeg。
pub fn resolve(hint: &str) -> Result<PathBuf> {
    let hint = hint.trim();
    if !hint.is_empty() {
        // 允许填目录：自动补 ffmpeg.exe，省得用户纠结填到哪一层。
        let p = PathBuf::from(hint);
        let candidate = if p.is_dir() { p.join(EXE) } else { p };
        if !candidate.exists() {
            bail!("设置里的 ffmpeg 路径不存在: {}", candidate.display());
        }
        if !runnable(&candidate) {
            bail!("ffmpeg 无法执行: {}", candidate.display());
        }
        return Ok(candidate);
    }

    // PATH：直接用裸名交给系统解析。
    let bare = PathBuf::from(EXE);
    if runnable(&bare) {
        return Ok(bare);
    }

    for dir in COMMON_DIRS {
        let candidate = Path::new(dir).join(EXE);
        if candidate.exists() && runnable(&candidate) {
            return Ok(candidate);
        }
    }

    bail!("{NOT_FOUND_HINT}")
}

/// 实测能否执行：跑 `-version` 看退出状态。
fn runnable(exe: &Path) -> bool {
    let mut cmd = Command::new(exe);
    cmd.arg("-version");
    hide_console(&mut cmd);
    matches!(cmd.output(), Ok(o) if o.status.success())
}

/// 取版本首行（`ffmpeg version 8.0 ...`），供设置页显示「已找到哪一个」。
pub fn version_line(exe: &Path) -> Option<String> {
    let mut cmd = Command::new(exe);
    cmd.arg("-version");
    hide_console(&mut cmd);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().map(|s| s.trim().to_string())
}

/// Windows 下不要给子进程弹黑框（`CREATE_NO_WINDOW`）。
/// 录屏期间 ffmpeg 全程存活，弹个控制台窗口既碍事，还会被录进画面里。
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// 组装编码参数：从 stdin 读 BGRA 原始帧 → H.264 → mp4。
///
/// - `-f rawvideo -pix_fmt bgra -s WxH -r FPS -i -`：告诉 ffmpeg 管道里是什么
///   （原始帧没有任何自描述信息，这些必须由我们给）。
/// - `-pix_fmt yuv420p`：播放器/浏览器通吃的像素格式；代价是宽高必须是偶数
///   （调用方已把矩形对齐到偶数，见 `session`）。
/// - `-movflags +faststart`：把 moov 挪到文件头，边下边播、拖动定位不卡。
pub fn encode_args(w: u32, h: u32, fps: u32, crf: u32, out: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "bgra".into(),
        "-s".into(),
        format!("{w}x{h}"),
        "-r".into(),
        fps.to_string(),
        "-i".into(),
        "-".into(),
        "-c:v".into(),
        "libx264".into(),
        "-preset".into(),
        "veryfast".into(),
        "-crf".into(),
        crf.to_string(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-movflags".into(),
        "+faststart".into(),
        out.to_string_lossy().into_owned(),
    ]
}
