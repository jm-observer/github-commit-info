use tauri::State;

use crate::app_state::AppState;
use crate::modules::speech::settings::{
    apply_settings_to_state, get_settings_from_state, CombinedSettings,
};

#[tauri::command]
pub fn speech_get_settings(state: State<'_, AppState>) -> Result<CombinedSettings, String> {
    let speech = state.speech.clone();
    get_settings_from_state(&speech)
}

/// 当前生效的内置 ASR 地址 = 按 G10 网络设置（局域网/外网）派生的 `asr_url`。
///
/// 录音时 `speech_start_recording` 本来就会用它覆盖 `SpeechState.remote_url`，但下拉里的
/// 「内置」项此前是编译期常量（写死内网 IP），换到外网就与实际连接的地址不一致，看着像配错。
/// 这里把显示层也接到同一个派生源上。未配置 host 时回退常量。
#[tauri::command]
pub async fn speech_default_remote_url(state: State<'_, AppState>) -> Result<String, String> {
    let resolved = state.net.resolve(&state.workspace).await;
    Ok(if resolved.asr_url.is_empty() {
        crate::modules::speech::settings::DEFAULT_REMOTE_URL.to_string()
    } else {
        resolved.asr_url
    })
}

#[tauri::command]
pub async fn speech_apply_settings(
    new_settings: CombinedSettings,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let speech = state.speech.clone();
    apply_settings_to_state(new_settings, &speech).await
}
