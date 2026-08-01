//! 语音纠错一键采集（in-place correction capture）。
//!
//! 用户改完中文优化文本后自己复制整段、按专用快捷键（`Ctrl+Alt+C`），程序读当前剪贴板拿到
//! `Y'`，把「交付给用户的优化稿 `O` → 用户改好的 `Y'`」+ 原始 ASR `R` + segment_ids 落一条
//! `speech_samples`。**本期只采集文本对，不分类、不碰音频、不写剪贴板**（音频后置 P2）。
//!
//! 架构要点（与设计文档 `docs/2026-07-21-speech-correction-capture/design.md` 的差异，以此为准）：
//! - 「burst 封存」改为本模块的 worker 侧分组：`record_delivery` 只管往 ring buffer 里追加/合并
//!   最近若干次交付，真正的「哪些交付算一个 burst」在采集触发时才现算（`group_bursts`）。
//! - 音频本期整个不做：不落盘、不拉取，`audio_status` 固定 `"skipped"`。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use tauri_plugin_clipboard_manager::ClipboardExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{info, warn};

use crate::modules::speech::commands::notify::bounce_tray_twice;
use crate::modules::speech::db::repository::{NewSample, SampleAppContext};
use crate::modules::speech::db::SpeechDatabase;
use crate::modules::speech::llm_settings::LlmSettings;
use crate::modules::speech::lock_utils::{mutex_lock, read_lock, write_lock};

/// 交付 ring buffer 容量：最近多少次「交付」（每次 optimized/translated 的最终文本）纳入候选。
const DELIVERY_RING_CAP: usize = 24;
/// 一次采集只在这个时间窗内的交付里找配对；超窗的交付视为「太久没管了」，不采。
const CAPTURE_TIME_WINDOW: Duration = Duration::from_secs(180);
/// 相似度阈值：`1.0 - similarity(O, Y') <= THRESHOLD` 才算「像同一段被改过」。采集期宁松勿严。
const SIMILARITY_THRESHOLD: f32 = 0.5;
/// 已采集过的 `Y'` 文本去重 ring buffer 容量。
const CAPTURED_RING_CAP: usize = 16;

/// 一次交付：某个 segment ref 最终交付给用户的优化稿 `o` 与拼接原文 `r`。
///
/// 同一 `ref_id` 反复回发累计文本时（合并链模式），队尾条目会被就地替换而非重复追加，
/// 因此 ring buffer 里的每个 `ref_id` 至多出现一次、且始终是该 ref 的最新文本。
#[derive(Clone)]
struct Delivered {
    ref_id: i64,
    o: String,
    r: String,
    at: Instant,
    session_id: Option<String>,
}

/// 一次分组（burst）：从新到旧扫描交付、按时间间隔归并出来的一段连续交付。
#[derive(Debug, Clone)]
struct Burst {
    o: String,
    r: String,
    segment_ids: Vec<i64>,
    session_id: Option<String>,
    /// 该 burst 内最新一次交付的时刻，供调用方按 `CAPTURE_TIME_WINDOW` 做时间窗过滤。
    newest_at: Instant,
}

/// 一键采集所需的运行时状态：最近交付 + 已采集去重。
pub struct CaptureState {
    deliveries: RwLock<VecDeque<Delivered>>,
    captured: RwLock<VecDeque<String>>,
}

impl CaptureState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            deliveries: RwLock::new(VecDeque::new()),
            captured: RwLock::new(VecDeque::new()),
        })
    }

    /// 记录一次交付。同一 `ref_id` 若正好是队尾（最近一次交付），就地替换（同段累计改写）；
    /// 否则追加新条目。超过 `DELIVERY_RING_CAP` 从队首淘汰最旧的。
    pub fn record_delivery(&self, ref_id: i64, o: String, r: String, session_id: Option<String>) {
        let mut deliveries = write_lock(&self.deliveries);
        let replace = matches!(deliveries.back(), Some(d) if d.ref_id == ref_id);
        if replace {
            let back = deliveries.back_mut().expect("checked Some above");
            back.o = o;
            back.r = r;
            back.at = Instant::now();
            back.session_id = session_id;
        } else {
            deliveries.push_back(Delivered {
                ref_id,
                o,
                r,
                at: Instant::now(),
                session_id,
            });
            while deliveries.len() > DELIVERY_RING_CAP {
                deliveries.pop_front();
            }
        }
    }
}

/// 按 char 计的编辑距离（Levenshtein）。
fn levenshtein_chars(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (la, lb) = (a.len(), b.len());
    if la == 0 {
        return lb;
    }
    if lb == 0 {
        return la;
    }
    let mut prev: Vec<usize> = (0..=lb).collect();
    let mut cur: Vec<usize> = vec![0; lb + 1];
    for i in 1..=la {
        cur[0] = i;
        for j in 1..=lb {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[lb]
}

/// 归一化字符相似度：`1 - edit_distance / max_len`。两串都为空视为完全相同（1.0）。
pub fn similarity(o: &str, y: &str) -> f32 {
    let d = levenshtein_chars(o, y) as f32;
    let m = o.chars().count().max(y.chars().count()) as f32;
    if m == 0.0 {
        1.0
    } else {
        1.0 - d / m
    }
}

/// 从新到旧扫描 `deliveries`，把相邻（按 `at` 排序）间隔 ≤ `merge_window` 的交付归并成 burst。
/// 一个 burst 内按 `ref_id` 去重保留最新，再按时间顺序（旧→新）用 `join_dedup` 拼接 `o`/`r`。
fn group_bursts(deliveries: &VecDeque<Delivered>, merge_window: Duration) -> Vec<Burst> {
    // 按时间升序处理（VecDeque 本身就是插入顺序，等价于按 at 升序，因为 record_delivery 只 push_back）。
    let items: Vec<&Delivered> = deliveries.iter().collect();
    if items.is_empty() {
        return Vec::new();
    }

    // 先按相邻间隔切分成若干「时间段」（旧→新的分组）。
    let mut groups: Vec<Vec<&Delivered>> = Vec::new();
    let mut current: Vec<&Delivered> = vec![items[0]];
    for w in items.windows(2) {
        let (prev, next) = (w[0], w[1]);
        let gap = next.at.saturating_duration_since(prev.at);
        if gap <= merge_window {
            current.push(next);
        } else {
            groups.push(std::mem::take(&mut current));
            current.push(next);
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }

    // 组内按 ref_id 去重保留最新，再按时间顺序拼接。
    let mut out = Vec::with_capacity(groups.len());
    for group in groups {
        let mut by_ref: Vec<&Delivered> = Vec::new();
        for d in &group {
            if let Some(existing) = by_ref.iter_mut().find(|e| e.ref_id == d.ref_id) {
                *existing = d;
            } else {
                by_ref.push(d);
            }
        }
        // by_ref 保持首次出现顺序（= 时间顺序），拼接即可。
        let mut o = String::new();
        let mut r = String::new();
        let mut segment_ids = Vec::new();
        let mut session_id = None;
        let mut newest_at = by_ref[0].at;
        for d in &by_ref {
            o = if o.is_empty() {
                d.o.clone()
            } else {
                crate::modules::speech::commands::remote::join_dedup(&o, &d.o)
            };
            r = if r.is_empty() {
                d.r.clone()
            } else {
                crate::modules::speech::commands::remote::join_dedup(&r, &d.r)
            };
            segment_ids.push(d.ref_id);
            if session_id.is_none() {
                session_id = d.session_id.clone();
            }
            if d.at > newest_at {
                newest_at = d.at;
            }
        }
        segment_ids.sort_unstable();
        segment_ids.dedup();
        out.push(Burst {
            o,
            r,
            segment_ids,
            session_id,
            newest_at,
        });
    }
    // 新到旧返回，方便调用方优先匹配最近的 burst。
    out.reverse();
    out
}

/// 触发采集的信号通道发送端。跨线程（键盘钩子线程）调用 `signal_capture` 时使用。
static CAP_TX: OnceLock<UnboundedSender<()>> = OnceLock::new();

/// 建立采集信号通道：返回接收端交给 worker，发送端存入全局 `CAP_TX` 供任意线程 `signal_capture`。
pub fn init_capture_channel() -> UnboundedReceiver<()> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = CAP_TX.set(tx);
    rx
}

/// 发出一次采集信号（专用快捷键命中时调用，可来自任意线程，包括同步的键盘钩子回调）。
pub fn signal_capture() {
    if let Some(tx) = CAP_TX.get() {
        let _ = tx.send(());
    } else {
        warn!(target: "speech", "[capture] signal_capture called before init_capture_channel");
    }
}

/// 后台采集 worker：收到信号 → 读剪贴板 → 在最近交付里分组配对 → 落库 → 轻量反馈。
///
/// 任何一步失败都只 warn，不 panic、不影响录音主链路。
pub async fn run_capture_worker(
    app: tauri::AppHandle,
    db: Arc<Mutex<Option<SpeechDatabase>>>,
    capture: Arc<CaptureState>,
    llm_settings: Arc<RwLock<LlmSettings>>,
    mut rx: UnboundedReceiver<()>,
) {
    while rx.recv().await.is_some() {
        let enabled = read_lock(&llm_settings).capture_enabled;
        if !enabled {
            info!(target: "speech", "[capture] capture_enabled=false, ignoring trigger");
            continue;
        }

        let y = match app.clipboard().read_text() {
            Ok(t) if !t.trim().is_empty() => t,
            Ok(_) => {
                info!(target: "speech", "[capture] clipboard text empty, ignoring");
                continue;
            }
            Err(e) => {
                warn!(target: "speech", "[capture] clipboard read_text failed: {e}");
                continue;
            }
        };

        let merge_window = Duration::from_millis(read_lock(&llm_settings).merge_window_ms);

        let deliveries_snapshot = read_lock(&capture.deliveries).clone();
        let bursts = group_bursts(&deliveries_snapshot, merge_window);

        let now = Instant::now();
        let already_captured = {
            let captured = read_lock(&capture.captured);
            captured.iter().any(|c| c == &y)
        };
        if already_captured {
            info!(target: "speech", "[capture] Y' already captured before, ignoring");
            continue;
        }

        let best = bursts
            .into_iter()
            .filter(|b| now.saturating_duration_since(b.newest_at) <= CAPTURE_TIME_WINDOW)
            .filter_map(|b| {
                let sim = similarity(&b.o, &y);
                if 1.0 - sim <= SIMILARITY_THRESHOLD {
                    Some((b, sim))
                } else {
                    None
                }
            })
            .max_by(|(_, a), (_, b)| a.total_cmp(b));

        let Some((burst, sim)) = best else {
            info!(target: "speech", "[capture] no burst matched Y' within threshold, ignoring");
            continue;
        };

        if y == burst.o {
            info!(target: "speech", "[capture] Y' identical to delivered O, nothing to learn from, ignoring");
            continue;
        }

        let note = serde_json::json!({
            "similarity": sim,
            "segment_ids": burst.segment_ids,
        })
        .to_string();
        let segment_ids_json = serde_json::to_string(&burst.segment_ids).unwrap_or_default();
        let first_seg = burst.segment_ids.first().copied().unwrap_or(0);
        let now_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        // 交付时的应用上下文：值是**交付动作发生那一刻**抓拍的（打字前 / 按下 Ctrl+V 时），
        // 此处只是读出来，读取时刻用户已切窗口也不影响正确性。超出配对时间窗的抓拍视为
        // 过期（说明这次采集没有对应的自动交付，如用户手工敲字后复制），留空不猜。
        let app_ctx = crate::modules::speech::paste_watch::last_delivery_app(CAPTURE_TIME_WINDOW)
            .map(|a| SampleAppContext {
                app_exe: a.exe,
                app_path: a.path,
                app_title: a.title,
                app_class: a.class,
                delivery_mode: Some(a.mode.to_string()),
            })
            .unwrap_or_default();

        let new_sample = NewSample {
            segment_id: first_seg,
            session_id: burst.session_id.clone(),
            label: "other".to_string(),
            text_raw: burst.r.clone(),
            text_optimized: Some(burst.o.clone()),
            text_english: None,
            text_secondary: None,
            correction: Some(y.clone()),
            note: Some(note),
            source: "copy".to_string(),
            segment_ids: Some(segment_ids_json),
            audio_status: "skipped".to_string(),
            marked_at: now_str,
            app: app_ctx,
            // 快捷键采集走的是文本纠错线，不关心说话人。
            speaker: None,
        };

        let db_handle = mutex_lock(&db).clone();
        let Some(db_handle) = db_handle else {
            warn!(target: "speech", "[capture] speech db not initialized, dropping capture");
            continue;
        };

        match db_handle.insert_sample(new_sample).await {
            Ok(id) => {
                info!(
                    target: "speech",
                    "[capture] sample inserted id={id} sim={sim:.3} segs={:?}", burst.segment_ids
                );
                {
                    let mut captured = write_lock(&capture.captured);
                    captured.push_back(y);
                    while captured.len() > CAPTURED_RING_CAP {
                        captured.pop_front();
                    }
                }
                bounce_tray_twice(&app, false);
            }
            Err(e) => {
                warn!(target: "speech", "[capture] insert_sample failed: {e:#}");
            }
        }
    }
    info!(target: "speech", "[capture] worker channel closed, exiting");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_identical_is_one() {
        assert_eq!(similarity("你好世界", "你好世界"), 1.0);
    }

    #[test]
    fn similarity_empty_both_is_one() {
        assert_eq!(similarity("", ""), 1.0);
    }

    #[test]
    fn similarity_one_char_diff_is_high() {
        let s = similarity("今天天气真好", "今天天气真棒");
        assert!(s > 0.8, "expected high similarity, got {s}");
    }

    #[test]
    fn similarity_completely_different_is_low() {
        let s = similarity("你好世界", "完全不同的另一段话内容");
        assert!(s < 0.3, "expected low similarity, got {s}");
    }

    #[test]
    fn similarity_one_empty_one_nonempty_is_zero() {
        assert_eq!(similarity("", "abc"), 0.0);
    }

    fn mk_delivery(ref_id: i64, o: &str, r: &str, at: Instant) -> Delivered {
        Delivered {
            ref_id,
            o: o.to_string(),
            r: r.to_string(),
            at,
            session_id: None,
        }
    }

    #[test]
    fn group_bursts_single_delivery_single_burst() {
        let now = Instant::now();
        let mut d = VecDeque::new();
        d.push_back(mk_delivery(1, "你好", "你好", now));
        let bursts = group_bursts(&d, Duration::from_millis(3000));
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].o, "你好");
        assert_eq!(bursts[0].segment_ids, vec![1]);
    }

    #[test]
    fn group_bursts_merges_within_window() {
        let now = Instant::now();
        let mut d = VecDeque::new();
        d.push_back(mk_delivery(1, "今天天气真好。", "今天天气真好。", now));
        d.push_back(mk_delivery(
            2,
            "然后我们出门了。",
            "然后我们出门了。",
            now + Duration::from_millis(500),
        ));
        let bursts = group_bursts(&d, Duration::from_millis(3000));
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].o, "今天天气真好。 然后我们出门了。");
        assert_eq!(bursts[0].segment_ids, vec![1, 2]);
    }

    #[test]
    fn group_bursts_splits_beyond_window() {
        let now = Instant::now();
        let mut d = VecDeque::new();
        d.push_back(mk_delivery(1, "第一段", "第一段", now));
        d.push_back(mk_delivery(
            2,
            "第二段",
            "第二段",
            now + Duration::from_secs(10),
        ));
        let bursts = group_bursts(&d, Duration::from_millis(3000));
        assert_eq!(bursts.len(), 2);
        // 新到旧返回：第一个是最新的 burst。
        assert_eq!(bursts[0].o, "第二段");
        assert_eq!(bursts[1].o, "第一段");
    }

    #[test]
    fn group_bursts_dedups_same_ref_keeps_latest() {
        let now = Instant::now();
        let mut d = VecDeque::new();
        d.push_back(mk_delivery(1, "第一句。", "第一句。", now));
        // 同 ref 累计改写（模拟：正常场景下 record_delivery 会就地替换，这里直接测 group_bursts
        // 对「ring 里意外出现同 ref 两条」的兜底去重行为）。
        d.push_back(mk_delivery(
            1,
            "第一句。第二句。",
            "第一句。第二句。",
            now + Duration::from_millis(200),
        ));
        let bursts = group_bursts(&d, Duration::from_millis(3000));
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].segment_ids, vec![1]);
        assert_eq!(bursts[0].o, "第一句。第二句。");
    }
}
