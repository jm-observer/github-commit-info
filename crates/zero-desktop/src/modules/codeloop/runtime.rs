//! Codeloop 运行时抽象：把 UI 强耦合（AppHandle.emit + pending 全局 oneshot 表）从引擎里抽出来。
//!
//! 两个 trait：
//! - `LoopEvents`：进度上报。UI 版本走 Tauri emit；headless 版本走 stdout/JSONL。
//! - `ConfirmPolicy`：逐步确认门 + ASK_USER 回答。UI 版本走 oneshot + 前端拍板；headless
//!   版本走 AutoConfirm + 可选 ASK_USER 答案映射。
//!
//! 引擎（`mod::drive` / `send_and_resolve` / `confirm_gate`）只看 trait 对象，不知道宿主。
//! `codeloop_start`（Tauri 命令）和 `codeloop-smoke`（CLI 子命令）都调同一个 `run_codeloop`。
//!
//! 设计来源：`docs/codeloop-headless-smoke-runner-plan.md` §架构。

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex};

/// 与 mod.rs 共享的进度事件名（前端 listen 它刷新状态条 / 触发 ASK_USER 弹窗）。
pub const EV_PROGRESS: &str = "codeloop_progress";

// ------------------------- 跨会话传递的逐步确认门 -------------------------

/// 确认门的三种结果（与 mod.rs 旧 Gate 同义）。
#[derive(Debug, Clone, Copy)]
pub enum Gate {
    /// 用户确认 / 全自动 → 放行。
    Approve,
    /// 用户否决 → 主动中止。
    Reject,
    /// 等待超时 → 按超时中止（保守：不自动发送）。
    Timeout,
}

/// 一次确认门请求（diag 信息：用于 emit / 控制台打印）。
pub struct ConfirmRequest<'a> {
    pub loop_id: i64,
    pub seq: i64,
    pub direction: &'a str,
    pub title: &'a str,
    pub content: &'a str,
}

/// 一次 ASK_USER 请求。
pub struct AskUserRequest<'a> {
    pub loop_id: i64,
    pub seq: i64,
    pub asked_by: &'a str,
    pub question: &'a str,
}

// ------------------------- 进度上报 trait -------------------------

/// 进度上报：把循环的运行时事件（phase / verdict / awaiting_*）外发给宿主。
pub trait LoopEvents: Send + Sync + 'static {
    /// 上报一次进度。`value` 已带或不带 `loop_id`，实现可自行补齐。
    fn progress(&self, loop_id: i64, value: Value);
}

/// Tauri 适配器：保持原 `app.emit(EV_PROGRESS, ...)` 行为不变。
pub struct TauriLoopEvents {
    app: AppHandle,
}

impl TauriLoopEvents {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl LoopEvents for TauriLoopEvents {
    fn progress(&self, loop_id: i64, value: Value) {
        let mut ev = value;
        if let Some(obj) = ev.as_object_mut() {
            obj.insert("loop_id".into(), json!(loop_id));
        }
        let _ = self.app.emit(EV_PROGRESS, ev);
    }
}

/// 控制台适配器：headless smoke runner 用。文本模式打印一行可读摘要；JSONL 模式每条进度
/// 打一行 `{event:"progress", loop_id, ...}` 到 stdout。
pub struct ConsoleLoopEvents {
    json: bool,
    sink: StdMutex<Box<dyn Write + Send>>,
}

impl ConsoleLoopEvents {
    pub fn new(json: bool, sink: Box<dyn Write + Send>) -> Self {
        Self {
            json,
            sink: StdMutex::new(sink),
        }
    }
}

impl LoopEvents for ConsoleLoopEvents {
    fn progress(&self, loop_id: i64, value: Value) {
        let mut w = self.sink.lock().expect("ConsoleLoopEvents sink poisoned");
        if self.json {
            let mut obj = serde_json::Map::new();
            obj.insert("event".into(), json!("progress"));
            obj.insert("loop_id".into(), json!(loop_id));
            if let Some(src) = value.as_object() {
                for (k, v) in src {
                    obj.insert(k.clone(), v.clone());
                }
            } else {
                obj.insert("value".into(), value);
            }
            let _ = writeln!(w, "{}", Value::Object(obj));
        } else {
            let phase = value
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("progress");
            let round = value.get("round").and_then(|v| v.as_i64());
            let verdict = value.get("verdict").and_then(|v| v.as_str());
            let mut line = format!("[progress] loop={loop_id} phase={phase}");
            if let Some(r) = round {
                line.push_str(&format!(" round={r}"));
            }
            if let Some(v) = verdict {
                line.push_str(&format!(" verdict={v}"));
            }
            let _ = writeln!(w, "{line}");
        }
        let _ = w.flush();
    }
}

// ------------------------- 确认/回答 trait -------------------------

/// 跨会话传递与 ASK_USER 的拍板策略。UI 版走人工 oneshot；headless 版自动放行。
#[async_trait]
pub trait ConfirmPolicy: Send + Sync + 'static {
    /// 跨会话传递前的确认门。
    async fn confirm(&self, req: ConfirmRequest<'_>, events: &dyn LoopEvents) -> Gate;
    /// ASK_USER：返回 Some(answer) 把答案发回同一会话；返回 None 等于循环中止
    /// （引擎按 `AbortedTimeout` 收尾）。
    async fn answer_user(&self, req: AskUserRequest<'_>, events: &dyn LoopEvents)
        -> Option<String>;
}

/// UI 版：oneshot + AppState pending 表（与原行为完全等价）。
///
/// 字段直接是 Arc，可被 `RunningLoop` / Tauri 命令（`codeloop_answer` / `codeloop_confirm` /
/// `codeloop_set_auto_confirm`）共享访问。
pub struct UiConfirmPolicy {
    pub pending: Arc<Mutex<Option<Pending>>>,
    pub pending_confirm: Arc<Mutex<Option<PendingConfirm>>>,
    pub step_confirm: Arc<AtomicBool>,
    pub answer_timeout: std::time::Duration,
}

/// 一个待用户回答的问题：seq + 唤醒循环的 oneshot 发送端。
pub struct Pending {
    pub seq: i64,
    pub answer_tx: oneshot::Sender<String>,
}

/// 一个待用户拍板的传递确认：seq + 决定（true=确认发送 / false=否决）的 oneshot 发送端。
pub struct PendingConfirm {
    pub seq: i64,
    pub decide_tx: oneshot::Sender<bool>,
}

#[async_trait]
impl ConfirmPolicy for UiConfirmPolicy {
    async fn confirm(&self, req: ConfirmRequest<'_>, events: &dyn LoopEvents) -> Gate {
        if !self.step_confirm.load(Ordering::SeqCst) {
            log::info!(
                "[codeloop] 逐步确认关闭（自动），{} 直接放行",
                req.direction
            );
            return Gate::Approve;
        }
        log::info!(
            "[codeloop] 逐步确认门 {}：弹窗等用户拍板（content {} 字符）",
            req.direction,
            req.content.chars().count()
        );
        let (tx, rx) = oneshot::channel::<bool>();
        *self.pending_confirm.lock().await = Some(PendingConfirm {
            seq: req.seq,
            decide_tx: tx,
        });
        events.progress(
            req.loop_id,
            json!({
                "phase": "awaiting_confirm",
                "seq": req.seq,
                "direction": req.direction,
                "title": req.title,
                "content": req.content,
            }),
        );

        match tokio::time::timeout(self.answer_timeout, rx).await {
            Ok(Ok(true)) => Gate::Approve,
            Ok(Ok(false)) => Gate::Reject,
            _ => {
                *self.pending_confirm.lock().await = None;
                Gate::Timeout
            }
        }
    }

    async fn answer_user(
        &self,
        req: AskUserRequest<'_>,
        events: &dyn LoopEvents,
    ) -> Option<String> {
        let (tx, rx) = oneshot::channel::<String>();
        *self.pending.lock().await = Some(Pending {
            seq: req.seq,
            answer_tx: tx,
        });
        events.progress(
            req.loop_id,
            json!({
                "phase": "awaiting_input",
                "seq": req.seq,
                "asked_by": req.asked_by,
                "question": req.question,
            }),
        );
        match tokio::time::timeout(self.answer_timeout, rx).await {
            Ok(Ok(answer)) => Some(answer),
            _ => {
                *self.pending.lock().await = None;
                None
            }
        }
    }
}

/// Headless 自动放行策略：传递确认门一律 Approve；ASK_USER 在可选答案映射里按子串匹配，
/// 命中则发回，否则返回 None（循环中止）。**永远不静默回空串**——见 plan §Command Shape。
pub struct AutoConfirmPolicy {
    pub ask_answers: Option<HashMap<String, String>>,
}

impl AutoConfirmPolicy {
    pub fn new(ask_answers: Option<HashMap<String, String>>) -> Self {
        Self { ask_answers }
    }
}

#[async_trait]
impl ConfirmPolicy for AutoConfirmPolicy {
    async fn confirm(&self, req: ConfirmRequest<'_>, events: &dyn LoopEvents) -> Gate {
        events.progress(
            req.loop_id,
            json!({
                "phase": "auto_confirm",
                "seq": req.seq,
                "direction": req.direction,
                "title": req.title,
            }),
        );
        Gate::Approve
    }

    async fn answer_user(
        &self,
        req: AskUserRequest<'_>,
        events: &dyn LoopEvents,
    ) -> Option<String> {
        let answer = self.ask_answers.as_ref().and_then(|map| {
            map.iter()
                .find(|(needle, _)| req.question.contains(needle.as_str()))
                .map(|(_, v)| v.clone())
        });
        events.progress(
            req.loop_id,
            json!({
                "phase": if answer.is_some() { "auto_answered" } else { "ask_user_unanswered" },
                "seq": req.seq,
                "asked_by": req.asked_by,
                "question": req.question,
                "matched": answer.is_some(),
            }),
        );
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_events_jsonl_includes_loop_id_and_fields() {
        let buf = Arc::new(StdMutex::new(Vec::<u8>::new()));
        struct Sink(Arc<StdMutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let ev = ConsoleLoopEvents::new(true, Box::new(Sink(buf.clone())));
        ev.progress(42, json!({"phase":"reviewed","round":1,"verdict":"pass"}));
        let s = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let v: Value = serde_json::from_str(s.trim()).expect("valid jsonl");
        assert_eq!(v["event"], "progress");
        assert_eq!(v["loop_id"], 42);
        assert_eq!(v["phase"], "reviewed");
        assert_eq!(v["round"], 1);
        assert_eq!(v["verdict"], "pass");
    }

    #[tokio::test]
    async fn auto_confirm_answer_user_substring_match() {
        let mut map = HashMap::new();
        map.insert("which file".into(), "src/lib.rs".into());
        let p = AutoConfirmPolicy::new(Some(map));
        struct NoopEvents;
        impl LoopEvents for NoopEvents {
            fn progress(&self, _: i64, _: Value) {}
        }
        let req = AskUserRequest {
            loop_id: 1,
            seq: 1,
            asked_by: "codex",
            question: "Please tell me which file to edit",
        };
        let a = p.answer_user(req, &NoopEvents).await;
        assert_eq!(a.as_deref(), Some("src/lib.rs"));
    }

    #[tokio::test]
    async fn auto_confirm_answer_user_no_match_returns_none() {
        let p = AutoConfirmPolicy::new(None);
        struct NoopEvents;
        impl LoopEvents for NoopEvents {
            fn progress(&self, _: i64, _: Value) {}
        }
        let req = AskUserRequest {
            loop_id: 1,
            seq: 1,
            asked_by: "codex",
            question: "Anything?",
        };
        assert!(p.answer_user(req, &NoopEvents).await.is_none());
    }
}
