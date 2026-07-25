use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoCopyMode {
    Off,
    #[default]
    English,
    OptimizedZh,
}

/// Default auto-copy "stitch window" (ms).
pub const DEFAULT_MERGE_WINDOW_MS: u64 = 3000;

/// Upper bound for the configurable stitch window (ms).
pub const MAX_MERGE_WINDOW_MS: u64 = 60_000;

fn default_merge_window_ms() -> u64 {
    DEFAULT_MERGE_WINDOW_MS
}

fn default_notify_sound() -> bool {
    true
}

fn default_voice_commands_enabled() -> bool {
    true
}

fn default_capture_enabled() -> bool {
    true
}

fn default_scene_log_enabled() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct LlmSettings {
    #[serde(default)]
    pub auto_copy_mode: AutoCopyMode,
    #[serde(default = "default_merge_window_ms")]
    pub merge_window_ms: u64,
    #[serde(default)]
    pub want_secondary: bool,
    #[serde(default = "default_notify_sound")]
    pub notify_sound: bool,
    /// 识别结果出来后，除写剪贴板外，是否直接「打字」进当前焦点输入框（自动粘贴）。
    /// 仅在 `auto_copy_mode` 选了具体内容（中文优化/英文翻译）时才有意义。
    #[serde(default)]
    pub auto_paste: bool,
    /// 自动粘贴时，若同一段（ref）的优化稿被 LLM 改写（新文本不是旧文本的前缀），
    /// 是否按公共前缀长度发退格键回退已输入字符、再补打新尾巴。
    /// 关闭 → 改写保守跳过，剩余文本留给剪贴板兜底。
    /// 开启 → 输入更跟手，但若焦点已被切走会误删别处的内容，谨慎使用。
    #[serde(default)]
    pub auto_paste_rewrite_retype: bool,
    /// 程序启动（语音页就绪）时是否自动开始识别录音。
    #[serde(default)]
    pub auto_start: bool,
    /// 本地语音命令开关：优化稿命中固定短语（如"发送"）时执行对应动作而非进剪贴板。
    /// 默认开启；UI 可后续暴露开关。详见 [`crate::modules::speech::voice_commands`]。
    #[serde(default = "default_voice_commands_enabled")]
    pub voice_commands_enabled: bool,
    /// 语音纠错一键采集总开关：关闭后专用快捷键触发即忽略（不读剪贴板、不落库）。
    #[serde(default = "default_capture_enabled")]
    pub capture_enabled: bool,
    /// 场景记录总开关：关闭后每次交付都不再落 `speech_scenes`（也不暂存待粘贴内容）。
    /// 与采集开关分开——场景记录是常开的全量收集，记的是交付文本 + 窗口标题，标题里可能
    /// 含聊天对象/文档名/网页标题这类比语音本身更敏感的信息，得留个能停的出口。
    #[serde(default = "default_scene_log_enabled")]
    pub scene_log_enabled: bool,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            auto_copy_mode: AutoCopyMode::default(),
            merge_window_ms: DEFAULT_MERGE_WINDOW_MS,
            want_secondary: false,
            notify_sound: true,
            auto_paste: false,
            auto_paste_rewrite_retype: false,
            auto_start: false,
            voice_commands_enabled: true,
            capture_enabled: true,
            scene_log_enabled: true,
        }
    }
}
