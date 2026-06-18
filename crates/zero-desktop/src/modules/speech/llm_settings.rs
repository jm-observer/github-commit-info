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
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            auto_copy_mode: AutoCopyMode::default(),
            merge_window_ms: DEFAULT_MERGE_WINDOW_MS,
            want_secondary: false,
            notify_sound: true,
            auto_paste: false,
        }
    }
}
