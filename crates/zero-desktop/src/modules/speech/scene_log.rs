//! 场景记录（delivery log）——**日常全量收集**：每次语音结果真正落进外部应用时记一行。
//!
//! 与 [`super::capture`] 是两条独立的线，别混：
//! - `capture`：你**发现某处写错了**，改好、复制、按 `Ctrl+Alt+X` 才记一条，带你的修正 `Y'`。
//!   天然只覆盖出错的那几条，是纠错语料。
//! - 本模块：**每一次交付都记**，没有 `Y'`。用来回答「我在哪个软件里、说什么样的话」——
//!   后续按应用定制中文优化（术语倾向、口语化程度、标点风格）要的是这个全量样貌，
//!   拿只含错误的样本去统计会严重偏样。
//!
//! 两条交付链路的记录时刻，与 `paste_watch` 里的应用抓拍点一一对应：
//! - `auto_paste`：`SendInput` 打字成功之后，由 `remote` 侧直接记（文本/段号/应用此刻齐备）。
//! - `auto_copy`：真正的交付时刻是用户按下 `Ctrl+V`，而那是在**低级键盘钩子回调**里——
//!   那里绝不能碰数据库（钩子有超时限制，阻塞会被系统摘钩）。故钩子只 `send` 一个无参信号，
//!   由本模块的 worker 去配对「最近写进剪贴板的待交付内容」并落库。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};

use crate::modules::speech::db::repository::{NewScene, SampleAppContext};
use crate::modules::speech::db::SpeechDatabase;
use crate::modules::speech::lock_utils::mutex_lock;
use crate::modules::speech::paste_watch;

/// 取应用抓拍的时间窗。事件发出到 worker 处理只隔几毫秒，给足余量即可；超窗说明这次交付
/// 压根没抓到应用（或抓拍是上一次交付留下的），宁可留空也不张冠李戴。
const APP_SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(10);
/// 去抖窗口：同一段文本粘进同一个应用，这么短的间隔内重复出现视为手滑连按，只记一次。
const DEDUP_WINDOW: Duration = Duration::from_secs(3);

/// 一条待落库的交付事件。
pub struct SceneEvent {
    pub session_id: Option<String>,
    pub segment_id: i64,
    pub delivery_mode: &'static str,
    pub content_kind: &'static str,
    /// 实际交付出去的文本；`auto_copy` 走合并链时是拼接后的整段。
    pub text: String,
}

/// `auto_copy` 链路：最近一次写进剪贴板、尚待用户粘贴的内容。
///
/// 只留最近一次——剪贴板本身就只有一份，新的写入即覆盖旧的。
static PENDING_CLIPBOARD: Mutex<Option<SceneEvent>> = Mutex::new(None);

/// `PENDING_CLIPBOARD` 是否非空的无锁镜像。
///
/// 存在的唯一理由：**键盘钩子回调里要判断这一下 `Ctrl+V` 是不是语音交付**，而那里不能加锁
/// （钩子有超时限制，被别的线程持锁挡一下就可能被系统摘钩）。读个 atomic 是安全的。
static HAS_PENDING: AtomicBool = AtomicBool::new(false);

/// 剪贴板里是否躺着待交付的语音内容。**供键盘钩子判断**：为 false 时这一下 `Ctrl+V`
/// 只是用户在粘自己的东西，与语音无关——既不该抓拍、更不该记账。
pub fn has_pending_clipboard() -> bool {
    HAS_PENDING.load(Ordering::Relaxed)
}

static LOG_TX: OnceLock<UnboundedSender<Option<SceneEvent>>> = OnceLock::new();

/// 建通道；返回接收端交给 [`run_scene_log_worker`]。重复调用返回 `None`。
pub fn init_scene_channel() -> Option<UnboundedReceiver<Option<SceneEvent>>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    match LOG_TX.set(tx) {
        Ok(()) => Some(rx),
        Err(_) => None,
    }
}

/// `auto_copy`：把「刚写进剪贴板、等着被粘贴」的内容登记下来。此刻还**没有**交付，
/// 直到用户按下 `Ctrl+V` 才算数，故只暂存不落库。
pub fn record_clipboard_pending(event: SceneEvent) {
    *mutex_lock(&PENDING_CLIPBOARD) = Some(event);
    HAS_PENDING.store(true, Ordering::Relaxed);
}

/// `auto_copy`：用户按下了 `Ctrl+V`。**在键盘钩子回调里调用**，因此只发一个无参信号，
/// 不读库、不加重锁、不分配。
pub fn signal_paste_delivered() {
    if let Some(tx) = LOG_TX.get() {
        let _ = tx.send(None);
    }
}

/// `auto_paste`：打字已经打进外部窗口了，直接把这条交付送去落库。
pub fn log_typed_scene(event: SceneEvent) {
    let Some(tx) = LOG_TX.get() else {
        warn!(target: "speech", "[delivery] log_typed_scene before init_scene_channel");
        return;
    };
    let _ = tx.send(Some(event));
}

/// 后台落库 worker。`None` = auto_copy 的粘贴信号（去暂存里取内容），`Some` = auto_paste 直送。
pub async fn run_scene_log_worker(
    db: std::sync::Arc<std::sync::Mutex<Option<SpeechDatabase>>>,
    mut rx: UnboundedReceiver<Option<SceneEvent>>,
) {
    // 去抖状态：上一条落库的 (模式, 文本, 应用) 与时刻。
    let mut last_written: Option<(String, String, Option<String>, Instant)> = None;

    while let Some(msg) = rx.recv().await {
        let event = match msg {
            Some(e) => e,
            // 全局 Ctrl+V 每次都会来，但只有「剪贴板里正躺着待交付的语音内容」才算一次交付。
            // 暂存为空 = 用户在粘贴自己的东西，与语音无关，直接忽略——本功能不窥探普通粘贴。
            None => {
                let taken = mutex_lock(&PENDING_CLIPBOARD).take();
                HAS_PENDING.store(false, Ordering::Relaxed);
                match taken {
                    Some(pending) => pending,
                    None => continue,
                }
            }
        };

        let app = paste_watch::last_delivery_app(APP_SNAPSHOT_MAX_AGE)
            .map(|a| SampleAppContext {
                app_exe: a.exe,
                app_path: a.path,
                app_title: a.title,
                app_class: a.class,
                delivery_mode: Some(a.mode.to_string()),
            })
            .unwrap_or_default();

        // 去抖：同模式 + 同文本 + 同应用且在窗口内 → 视为重复触发，不重复记账。
        let now = Instant::now();
        if let Some((mode, text, exe, at)) = &last_written {
            if mode == event.delivery_mode
                && text == &event.text
                && exe == &app.app_exe
                && now.duration_since(*at) <= DEDUP_WINDOW
            {
                continue;
            }
        }

        let db_handle = { mutex_lock(&db).clone() };
        let Some(db_handle) = db_handle else {
            warn!(target: "speech", "[delivery] speech db not initialized, dropping delivery log");
            continue;
        };

        let new = NewScene {
            session_id: event.session_id.clone(),
            segment_id: event.segment_id,
            delivery_mode: event.delivery_mode.to_string(),
            content_kind: event.content_kind.to_string(),
            text: event.text.clone(),
            app: app.clone(),
            delivered_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        };

        match db_handle.insert_scene(new).await {
            Ok(id) => {
                info!(
                    target: "speech",
                    "[delivery] logged id={id} mode={} kind={} app={:?} chars={}",
                    event.delivery_mode,
                    event.content_kind,
                    app.app_exe,
                    event.text.chars().count()
                );
                last_written = Some((
                    event.delivery_mode.to_string(),
                    event.text,
                    app.app_exe,
                    now,
                ));
            }
            Err(e) => warn!(target: "speech", "[delivery] insert_scene failed: {e:#}"),
        }
    }
    info!(target: "speech", "[delivery] worker channel closed, exiting");
}
