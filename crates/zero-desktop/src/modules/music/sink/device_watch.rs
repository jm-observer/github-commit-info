//! 系统默认输出设备变更监听（轮询式）。
//!
//! 两个 sink（WASAPI 独占 / cpal 共享）都在建流那一刻取一次「系统默认输出设备」，之后流就
//! 钉死在那台设备上——独占流更是绕过共享混音器，Windows 把默认设备切到蓝牙耳机后，音乐仍
//! 写在原来的音箱上。本模块起一条**轮询线程**（1s 一轮）比对默认设备 id，变了就给引擎发
//! [`AudioCommand::OutputDeviceChanged`]，由引擎原位重建 sink 跟过去。
//!
//! 为什么轮询而不是 `IMMNotificationClient`：后者要手写 COM vtable（本仓只有 `windows-sys`，
//! 无 `#[implement]` 宏），而换耳机场景对 1s 内的延迟完全不敏感——用几十行无 unsafe 的轮询
//! 换掉一百多行 unsafe COM，是划算的。
//!
//! [`current_default_output_id`] 同时被 `build_sink` 调用，把「本次流用的设备 id」记进
//! [`super::SinkHandle`]，引擎据此判断收到的变更是否真的与当前流不同（避免无谓重建）。

use std::time::Duration;

use crossbeam_channel::Sender;
use tracing::info;

use super::super::engine::AudioCommand;

/// 轮询间隔。换设备是人的动作，1s 足够跟手；一次 COM 枚举的开销可忽略。
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// 起监听线程。`tx` 为引擎命令通道；通道断开（引擎退出）即线程自然结束。
pub fn spawn(tx: Sender<AudioCommand>) {
    let _ = std::thread::Builder::new()
        .name("music-device-watch".into())
        .spawn(move || run(tx));
}

fn run(tx: Sender<AudioCommand>) {
    #[cfg(windows)]
    if wasapi::initialize_mta().is_err() {
        tracing::warn!(target: "music", "设备监听线程初始化 COM(MTA) 失败，默认设备变更将不被跟随");
        return;
    }

    let mut last = current_default_output_id();
    info!(target: "music", "默认输出设备监听已启动（当前: {last:?}）");

    loop {
        std::thread::sleep(POLL_INTERVAL);
        let now = current_default_output_id();
        if now == last {
            continue;
        }
        info!(target: "music", "系统默认输出设备变更: {last:?} → {now:?}");
        last = now.clone();
        if tx.send(AudioCommand::OutputDeviceChanged(now)).is_err() {
            info!(target: "music", "命令通道断开，设备监听线程退出");
            return;
        }
    }
}

/// 当前系统默认输出设备的标识。取不到（无设备 / 枚举失败）返回 `None`。
///
/// Windows 用 WASAPI endpoint id（全局唯一字符串）；其它平台退化为 cpal 设备名。
/// **必须与 sink 建流时记录的 id 同源**，否则引擎的「是否已在目标设备上」比较会恒不相等。
#[cfg(windows)]
pub fn current_default_output_id() -> Option<String> {
    let enumerator = wasapi::DeviceEnumerator::new().ok()?;
    let device = enumerator
        .get_default_device(&wasapi::Direction::Render)
        .ok()?;
    device.get_id().ok()
}

#[cfg(not(windows))]
pub fn current_default_output_id() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host().default_output_device()?.name().ok()
}
