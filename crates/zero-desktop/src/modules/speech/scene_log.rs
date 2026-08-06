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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri_plugin_clipboard_manager::ClipboardExt;
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
/// 待粘贴内容的保质期。**没有它就会误记**：auto_copy 写完剪贴板，用户没粘（或用右键菜单
/// 粘的，键盘钩子看不见），几小时后用户复制了自己的东西按 Ctrl+V —— 钩子看到「有待交付
/// 内容」，就会把那段陈年语音文本配上此刻这个毫不相干的应用记成一次交付，并顺带污染
/// `paste_watch::LAST_DELIVERY`（纠错采集的应用上下文取自那里）。与纠错采集的配对时间窗
/// 取同一量级：超过这么久没粘，就当它不会被粘了。
const PENDING_TTL: Duration = Duration::from_secs(180);
/// 同一段（session+segment+模式+内容种类）的累计交付被视为「还是那次交付」的时长。
/// 合并链的增量打字都在几秒内发生，给到分钟级足够宽松；超窗则另起一行，不会把两次
/// 不相干的交付并成一条。
const SCENE_MERGE_WINDOW: Duration = Duration::from_secs(300);

/// 一条待落库的交付事件。
pub struct SceneEvent {
    pub session_id: Option<String>,
    pub segment_id: i64,
    pub delivery_mode: &'static str,
    pub content_kind: &'static str,
    /// 实际交付出去的文本；`auto_copy` 走合并链时是拼接后的整段。
    pub text: String,
    /// `text` 是不是该段的**累计全文**（`auto_paste` 合并链逐次增量打字时为 true）。
    /// 为 true 时同一段的后续事件覆盖同一行，而不是各记一行半截话，见
    /// [`crate::modules::speech::db::repository::update_scene_text`]。
    pub cumulative: bool,
}

/// `auto_copy` 链路：最近一次写进剪贴板、尚待用户粘贴的内容 + 它的过期时刻。
///
/// 只留最近一次——剪贴板本身就只有一份，新的写入即覆盖旧的。
static PENDING_CLIPBOARD: Mutex<Option<(SceneEvent, Instant)>> = Mutex::new(None);

/// `PENDING_CLIPBOARD` 的过期时刻（相对 [`base_instant`] 的毫秒数；0 = 无待交付内容）。
///
/// 存在的唯一理由：**键盘钩子回调里要判断这一下 `Ctrl+V` 是不是语音交付**，而那里不能加锁
/// （钩子有超时限制，被别的线程持锁挡一下就可能被系统摘钩）。存的是「截止时刻」而非布尔，
/// 这样过期判定同样无锁完成——布尔镜像没法自己过期，正是上面 `PENDING_TTL` 那条注释里
/// 描述的误记路径的成因。
static PENDING_DEADLINE_MS: AtomicU64 = AtomicU64::new(0);

/// 场景记录总开关。关掉后不再往 `speech_scenes` 落任何新行；待交付标记与应用抓拍照旧
/// （见 [`set_enabled`]）。默认开——这是个常开的全量收集功能。
static SCENE_LOG_ENABLED: AtomicBool = AtomicBool::new(true);

/// 进程内的单调时间基准。给 [`PENDING_DEADLINE_MS`] 提供「毫秒数」参照系——`Instant` 本身
/// 塞不进 atomic。
fn base_instant() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    base_instant().elapsed().as_millis() as u64
}

/// 场景记录开关的当前值。
pub fn is_enabled() -> bool {
    SCENE_LOG_ENABLED.load(Ordering::Relaxed)
}

/// 设置场景记录开关。**只管落库这一件事**：待交付内容照常暂存——那个标记同时是键盘钩子
/// 判断「这一下 Ctrl+V 是不是语音交付」的依据，纠错采集的应用抓拍也挂在同一个判断上，
/// 顺手清掉会把采集的应用上下文一并弄丢。落库的闸门在 worker 里。
pub fn set_enabled(enabled: bool) {
    SCENE_LOG_ENABLED.store(enabled, Ordering::Relaxed);
    info!(target: "speech", "[delivery] scene log enabled={enabled}");
}

/// 剪贴板里是否躺着**尚未过期的**待交付语音内容。**供键盘钩子判断**：为 false 时这一下
/// `Ctrl+V` 只是用户在粘自己的东西，与语音无关——既不该抓拍、更不该记账。
pub fn has_pending_clipboard() -> bool {
    let deadline = PENDING_DEADLINE_MS.load(Ordering::Relaxed);
    deadline != 0 && now_ms() < deadline
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
///
/// 不受场景记录开关影响：这只是个进程内标记（内容本来就在剪贴板里），关掉记录也仍需要它
/// 让键盘钩子认出「这一下 Ctrl+V 是语音交付」，好给纠错采集抓拍应用上下文。
pub fn record_clipboard_pending(event: SceneEvent) {
    let expires_at = Instant::now() + PENDING_TTL;
    *mutex_lock(&PENDING_CLIPBOARD) = Some((event, expires_at));
    PENDING_DEADLINE_MS.store(now_ms() + PENDING_TTL.as_millis() as u64, Ordering::Relaxed);
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
    if !is_enabled() {
        return;
    }
    let Some(tx) = LOG_TX.get() else {
        warn!(target: "speech", "[delivery] log_typed_scene before init_scene_channel");
        return;
    };
    let _ = tx.send(Some(event));
}

/// 合并键：同一会话的同一段、同一交付方式与内容种类，视为「同一次交付」。
type SceneKey = (Option<String>, i64, &'static str, &'static str);

/// 取待交付内容，并确认它此刻**仍然**代表一次真实的语音交付。
///
/// 两道校验，缺一不可：
/// - **没过期**：`PENDING_TTL` 内没被粘走就当它不会被粘了（无锁镜像也会同步过期，但信号
///   可能正好在过期边缘发出，这里按精确时刻再判一次）。
/// - **剪贴板内容仍是它**：用户完全可能在中间复制了别的东西再粘贴——那一下 Ctrl+V 粘的是
///   用户自己的内容，与语音无关。钩子里读不了剪贴板（不能阻塞），到 worker 这层才读得起。
///   读失败（剪贴板被别的进程短暂独占是常事）不作数，退回只看时效。
fn take_verified_pending(app: &tauri::AppHandle) -> Option<SceneEvent> {
    let taken = mutex_lock(&PENDING_CLIPBOARD).take();
    PENDING_DEADLINE_MS.store(0, Ordering::Relaxed);
    let (pending, expires_at) = taken?;
    if Instant::now() > expires_at {
        info!(target: "speech", "[delivery] pending clipboard expired, ignoring this paste");
        return None;
    }
    match app.clipboard().read_text() {
        Ok(current) if current != pending.text => {
            info!(
                target: "speech",
                "[delivery] clipboard no longer holds the pending voice text, ignoring this paste"
            );
            None
        }
        Ok(_) => Some(pending),
        Err(e) => {
            // 读不到就别较真：内容多半没变，时效校验已经挡住了陈旧暂存。
            warn!(target: "speech", "[delivery] clipboard read_text failed ({e}), falling back to TTL check only");
            Some(pending)
        }
    }
}

/// 后台落库 worker。`None` = auto_copy 的粘贴信号（去暂存里取内容），`Some` = auto_paste 直送。
pub async fn run_scene_log_worker(
    app: tauri::AppHandle,
    db: std::sync::Arc<std::sync::Mutex<Option<SpeechDatabase>>>,
    mut rx: UnboundedReceiver<Option<SceneEvent>>,
) {
    // 去抖状态：上一条落库的 (模式, 文本, 应用) 与时刻。
    let mut last_written: Option<(String, String, Option<String>, Instant)> = None;
    // 合并链状态：某段最近落的那行 id 与时刻，供累计全文覆盖同一行。
    let mut open_rows: HashMap<SceneKey, (i64, Instant)> = HashMap::new();

    while let Some(msg) = rx.recv().await {
        let event = match msg {
            Some(e) => e,
            // 全局 Ctrl+V 每次都会来，但只有「剪贴板里正躺着待交付的语音内容」才算一次交付。
            // 暂存为空 = 用户在粘贴自己的东西，与语音无关，直接忽略——本功能不窥探普通粘贴。
            None => match take_verified_pending(&app) {
                Some(pending) => pending,
                None => continue,
            },
        };

        // 开关可能在事件发出与处理之间被关掉，落库前再确认一次。
        if !is_enabled() {
            continue;
        }

        let now = Instant::now();
        open_rows.retain(|_, (_, at)| now.duration_since(*at) <= SCENE_MERGE_WINDOW);

        let app_ctx = paste_watch::last_delivery_app(APP_SNAPSHOT_MAX_AGE)
            .map(|a| SampleAppContext {
                app_exe: a.exe,
                app_path: a.path,
                app_title: a.title,
                app_class: a.class,
                delivery_mode: Some(a.mode.to_string()),
            })
            .unwrap_or_default();

        // 去抖：同模式 + 同文本 + 同应用且在窗口内 → 视为重复触发，不重复记账。
        if let Some((mode, text, exe, at)) = &last_written {
            if mode == event.delivery_mode
                && text == &event.text
                && exe == &app_ctx.app_exe
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

        let delivered_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let key: SceneKey = (
            event.session_id.clone(),
            event.segment_id,
            event.delivery_mode,
            event.content_kind,
        );

        // 合并链：这一段刚落过行 → 用累计全文覆盖它，而不是再记一行半截话。
        if event.cumulative {
            if let Some((row_id, _)) = open_rows.get(&key).copied() {
                match db_handle
                    .update_scene_text(
                        row_id,
                        event.text.clone(),
                        app_ctx.clone(),
                        delivered_at.clone(),
                    )
                    .await
                {
                    Ok(true) => {
                        info!(
                            target: "speech",
                            "[delivery] updated id={row_id} mode={} kind={} chars={}",
                            event.delivery_mode,
                            event.content_kind,
                            event.text.chars().count()
                        );
                        open_rows.insert(key, (row_id, now));
                        last_written = Some((
                            event.delivery_mode.to_string(),
                            event.text,
                            app_ctx.app_exe,
                            now,
                        ));
                        continue;
                    }
                    // 行没了（用户清过库）→ 忘掉它，走下面的新增。
                    Ok(false) => {
                        open_rows.remove(&key);
                    }
                    Err(e) => {
                        warn!(target: "speech", "[delivery] update_scene_text failed: {e:#}");
                        continue;
                    }
                }
            }
        }

        let new = NewScene {
            session_id: event.session_id.clone(),
            segment_id: event.segment_id,
            delivery_mode: event.delivery_mode.to_string(),
            content_kind: event.content_kind.to_string(),
            text: event.text.clone(),
            app: app_ctx.clone(),
            delivered_at,
        };

        match db_handle.insert_scene(new).await {
            Ok(id) => {
                info!(
                    target: "speech",
                    "[delivery] logged id={id} mode={} kind={} app={:?} chars={}",
                    event.delivery_mode,
                    event.content_kind,
                    app_ctx.app_exe,
                    event.text.chars().count()
                );
                if event.cumulative {
                    open_rows.insert(key, (id, now));
                }
                last_written = Some((
                    event.delivery_mode.to_string(),
                    event.text,
                    app_ctx.app_exe,
                    now,
                ));
            }
            Err(e) => warn!(target: "speech", "[delivery] insert_scene failed: {e:#}"),
        }
    }
    info!(target: "speech", "[delivery] worker channel closed, exiting");
}
