//! 远程执行（remote-exec）第一期的 controller 端调度：per-worker 单任务槽。
//!
//! 独立 in-memory 状态机，**不把状态塞进 `egress-pool`**——两者只共享稳定的
//! `worker_id` 概念（进程内也是各自独立的 `Registry`/`Coordinator` 实例）。
//! 设计出处：`docs/remote-exec-design.md` 第一期 §5.3。
//!
//! 状态机要点：
//! - 一个 worker 同一时刻只有一个任务槽（`Queued` → `Picked` → 清空）。
//! - `Queued`：已放入槽，等待 worker 的 `next()` 长轮询领取；领取前超时 → 原子清理
//!   （只清理属于该 `id` 的槽位，防止与后到的领取竞态）。
//! - `Picked`：worker 已领取，等待其经 `result()` 回传终态；槽位只在收到**匹配的**
//!   结果时才清空——若调用方 `/run` 等结果超时放弃（`unknown`），槽位仍保留，直到
//!   真正的结果到达或 worker 用新 `instance_id` 重新 `register()`（视为进程重启，
//!   自然丢弃旧槽位）。第一期不建设 reaper，这是明确的已知限制。
//!
//! 并发原语：单把 [`std::sync::Mutex`] 保护所有可变状态；每个 worker 一个
//! [`tokio::sync::Notify`] 用于唤醒阻塞在 `next()`/`wait_picked()` 上的等待方
//! （参考 `crates/egress-pool/src/lib.rs` 的 `pending` + 唤醒写法，但这里换成
//! `Notify` 而非一次性 `oneshot` 队列，因为槽位是「当前状态」而不是「一次性消息」，
//! 用状态轮询 + 唤醒能避免 mpsc 队列在超时清理时的陈旧消息竞态）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{oneshot, Notify};
use worker_core::proto::ExecRequest;

pub use worker_core::proto::ExecResponse;

/// 心跳超过此时长视为失联（worker 每次 `next()` 长轮询都顺带续心跳）。
pub const HEARTBEAT_TTL: Duration = Duration::from_secs(30);
/// `/run` 等待 worker 领取任务的默认上限。
pub const PICKUP_WAIT: Duration = Duration::from_secs(30);
/// 领取后，在 `timeout_secs` 之外再多等的宽限期。
pub const RESULT_GRACE: Duration = Duration::from_secs(30);
/// worker `next()` 长轮询单次等待上限（< 常见反代/中间件空闲超时）。
pub const NEXT_LONG_POLL_WAIT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Queued,
    Picked,
}

struct Slot {
    req: ExecRequest,
    state: SlotState,
    /// 入队时刻的 ISO8601（供成功结果落审计用「起止时间」）。
    queued_at: String,
}

struct WorkerEntry {
    instance_id: String,
    last_heartbeat: Instant,
    powershell: Option<String>,
    hostname: Option<String>,
    slot: Option<Slot>,
    notify: std::sync::Arc<Notify>,
}

/// `id -> 等待回传的 /run 调用方`。真正的归属校验(worker_id/instance_id/id 三者匹配)
/// 由 [`Coordinator::result`] 通过槽位状态完成,这里只需要一个一次性通道。
struct PendingResult {
    tx: oneshot::Sender<ExecResponse>,
}

#[derive(Default)]
struct Inner {
    workers: HashMap<String, WorkerEntry>,
    /// server `id` -> 等待回传的 `/run` 调用方。
    pending: HashMap<String, PendingResult>,
}

/// 进程内 exec 调度器。`Default` 即空状态，配合 `Arc<Coordinator>` 放进 `AppState`。
#[derive(Default)]
pub struct Coordinator {
    inner: Mutex<Inner>,
}

/// worker 快照，供 `GET /api/web/exec/workers` 展示。
#[derive(Debug, Clone, Serialize)]
pub struct WorkerSnapshot {
    pub worker_id: String,
    pub instance_id: String,
    pub online: bool,
    pub busy: bool,
    pub hostname: Option<String>,
    pub powershell: Option<String>,
    pub seconds_since_heartbeat: u64,
}

/// `heartbeat()` 结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    Ok,
    UnknownWorker,
    StaleInstance,
}

/// `next()` 结果。
#[derive(Debug, Clone)]
pub enum NextOutcome {
    Job(ExecRequest),
    Idle,
    UnknownWorker,
    StaleInstance,
}

/// `result()` 成功时附带的原始请求信息，供路由层拼审计记录（operator/shell/cwd/script_hash 等
/// 都只在 [`ExecRequest`] 里，`ExecResponse` 本身不携带）。
#[derive(Debug, Clone)]
pub struct CompletedJob {
    pub req: ExecRequest,
    pub queued_at: String,
}

/// `result()` 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultOutcome {
    UnknownWorker,
    StaleInstance,
    /// 该 worker 当前槽位为空，或槽位里的 `id` 与回传的不一致（含伪造/迟到/重复回传）。
    IdMismatch,
}

/// `submit()` 失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    WorkerNotExecCapable,
    WorkerOffline,
    WorkerBusy,
}

/// `wait_picked()` 结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickedOutcome {
    /// 已确认被领取，或槽位已经消失（见下方 `wait_picked` 内的说明——两者都直接
    /// 放行去等结果，不再单独区分）。
    Picked,
    NotPickedUp,
}

/// `/run` 端到端结果，直接映射为 HTTP 语义（见路由层）。
#[derive(Debug)]
pub enum RunOutcome {
    NotExecCapable,
    Offline,
    Busy,
    NotPickedUp,
    Completed(ExecResponse),
    /// 已领取但拿不到确认结果（`/run` 侧等待超时或等待期间 worker 消失）。
    Unknown,
}

fn is_online(w: &WorkerEntry) -> bool {
    w.last_heartbeat.elapsed() < HEARTBEAT_TTL
}

impl Coordinator {
    /// 仅测试用：把某 worker 的心跳强行拨到 TTL 之外，模拟失联，无需真的 sleep 30s。
    #[cfg(test)]
    fn force_stale_heartbeat(&self, worker_id: &str) {
        let mut g = self.inner.lock().unwrap();
        if let Some(w) = g.workers.get_mut(worker_id) {
            w.last_heartbeat = Instant::now() - HEARTBEAT_TTL - Duration::from_secs(1);
        }
    }

    // ---------- worker 侧 ----------

    /// worker 注册 / 重注册。重注册（无论同实例续连还是新实例重启）总是重建全新条目：
    /// 第一期不承诺跨重启保留在途任务，这是设计明确的已知限制（§5.3）。
    pub fn register(
        &self,
        worker_id: &str,
        instance_id: &str,
        powershell: Option<&str>,
        hostname: Option<&str>,
    ) {
        let mut g = self.inner.lock().unwrap();
        g.workers.insert(
            worker_id.to_string(),
            WorkerEntry {
                instance_id: instance_id.to_string(),
                last_heartbeat: Instant::now(),
                powershell: powershell.map(|s| s.to_string()),
                hostname: hostname.map(|s| s.to_string()),
                slot: None,
                notify: std::sync::Arc::new(Notify::new()),
            },
        );
        log::info!("exec worker registered: {worker_id} instance={instance_id}");
    }

    /// 刷新心跳，校验 `instance_id`。
    pub fn heartbeat(&self, worker_id: &str, instance_id: &str) -> HeartbeatOutcome {
        let mut g = self.inner.lock().unwrap();
        match g.workers.get_mut(worker_id) {
            None => HeartbeatOutcome::UnknownWorker,
            Some(w) if w.instance_id != instance_id => HeartbeatOutcome::StaleInstance,
            Some(w) => {
                w.last_heartbeat = Instant::now();
                HeartbeatOutcome::Ok
            }
        }
    }

    /// 长轮询领取下一个任务（顺带刷新心跳、校验 `instance_id`）。
    pub async fn next(&self, worker_id: &str, instance_id: &str, wait: Duration) -> NextOutcome {
        let deadline = Instant::now() + wait;
        loop {
            let notify = {
                let mut g = self.inner.lock().unwrap();
                let w = match g.workers.get_mut(worker_id) {
                    Some(w) => w,
                    None => return NextOutcome::UnknownWorker,
                };
                if w.instance_id != instance_id {
                    return NextOutcome::StaleInstance;
                }
                w.last_heartbeat = Instant::now();
                if let Some(slot) = &mut w.slot {
                    if slot.state == SlotState::Queued {
                        slot.state = SlotState::Picked;
                        let req = slot.req.clone();
                        w.notify.notify_waiters(); // 唤醒可能在等 picked 的 wait_picked()
                        return NextOutcome::Job(req);
                    }
                }
                w.notify.clone()
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return NextOutcome::Idle;
            }
            let _ = tokio::time::timeout(remaining, notify.notified()).await;
        }
    }

    /// worker 回传结果。校验 `instance_id` + 槽位 `id` 匹配；成功则清空槽位并唤醒
    /// 等待中的 `/run` 调用方，返回原始请求信息供审计。
    pub fn result(
        &self,
        worker_id: &str,
        instance_id: &str,
        resp: ExecResponse,
    ) -> Result<CompletedJob, ResultOutcome> {
        let mut g = self.inner.lock().unwrap();
        let w = match g.workers.get_mut(worker_id) {
            Some(w) => w,
            None => return Err(ResultOutcome::UnknownWorker),
        };
        if w.instance_id != instance_id {
            return Err(ResultOutcome::StaleInstance);
        }
        let matches = matches!(&w.slot, Some(s) if s.req.id == resp.id);
        if !matches {
            return Err(ResultOutcome::IdMismatch);
        }
        let slot = w.slot.take().expect("checked Some above");
        w.notify.notify_waiters();
        if let Some(pending) = g.pending.remove(&resp.id) {
            let _ = pending.tx.send(resp);
        }
        Ok(CompletedJob {
            req: slot.req,
            queued_at: slot.queued_at,
        })
    }

    // ---------- operator 侧（`/run`）----------

    /// 观测快照：仅在线 worker（exec 通道注册即代表允许 exec）。
    pub fn list_workers(&self) -> Vec<WorkerSnapshot> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<WorkerSnapshot> = g
            .workers
            .iter()
            .filter(|(_, w)| is_online(w))
            .map(|(id, w)| WorkerSnapshot {
                worker_id: id.clone(),
                instance_id: w.instance_id.clone(),
                online: true,
                busy: w.slot.is_some(),
                hostname: w.hostname.clone(),
                powershell: w.powershell.clone(),
                seconds_since_heartbeat: w.last_heartbeat.elapsed().as_secs(),
            })
            .collect();
        v.sort_by(|a, b| a.worker_id.cmp(&b.worker_id));
        v
    }

    /// 放入任务槽（不等待）。busy/offline/not-exec-capable 立即返回错误。
    fn submit(
        &self,
        worker_id: &str,
        req: ExecRequest,
    ) -> Result<oneshot::Receiver<ExecResponse>, SubmitError> {
        let mut g = self.inner.lock().unwrap();
        let w = g
            .workers
            .get_mut(worker_id)
            .ok_or(SubmitError::WorkerNotExecCapable)?;
        if !is_online(w) {
            return Err(SubmitError::WorkerOffline);
        }
        if w.slot.is_some() {
            return Err(SubmitError::WorkerBusy);
        }
        let id = req.id.clone();
        let (tx, rx) = oneshot::channel();
        w.slot = Some(Slot {
            req,
            state: SlotState::Queued,
            queued_at: toolkit_core::now_iso8601(),
        });
        w.notify.notify_waiters();
        g.pending.insert(id, PendingResult { tx });
        Ok(rx)
    }

    /// 只清理「仍是 `Queued` 且 `id` 匹配」的槽位，返回是否真的清理了。
    /// 若此刻已被领取（`Picked`）则不清，返回 `false`——调用方据此判断该按
    /// `not_picked_up` 还是继续等结果处理。
    fn clear_if_queued(&self, worker_id: &str, id: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        let cleared = match g.workers.get_mut(worker_id) {
            Some(w) => match &w.slot {
                Some(s) if s.req.id == id && s.state == SlotState::Queued => {
                    w.slot = None;
                    true
                }
                _ => false,
            },
            None => false,
        };
        if cleared {
            g.pending.remove(id);
        }
        cleared
    }

    async fn wait_picked(&self, worker_id: &str, id: &str, wait: Duration) -> PickedOutcome {
        let deadline = Instant::now() + wait;
        loop {
            let notify = {
                let g = self.inner.lock().unwrap();
                match g.workers.get(worker_id) {
                    Some(w) => match &w.slot {
                        Some(s) if s.req.id == id => {
                            if s.state == SlotState::Picked {
                                return PickedOutcome::Picked;
                            }
                            w.notify.clone()
                        }
                        // 槽位已不是这个 id 了:要么 `result()` 已经把它整个消费完并清空
                        // （worker 领取 + 回传全发生在我们两次唤醒之间，快到我们都没观察到
                        // `Picked` 这个中间态)，要么 worker 用新 instance 重新 `register()`
                        // 整体替换了旧条目。两种情况都不必在这里纠结——直接放行去等
                        // `rx`：真正完成的会立刻收到结果；因重启丢失的会在结果等待阶段
                        // 超时收敛为 `unknown`。
                        _ => return PickedOutcome::Picked,
                    },
                    None => return PickedOutcome::Picked,
                }
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // 原子清理：只清理仍属于该 id 的排队槽位；若此刻已被 picked 则视为 Picked。
                return if self.clear_if_queued(worker_id, id) {
                    PickedOutcome::NotPickedUp
                } else {
                    PickedOutcome::Picked
                };
            }
            let _ = tokio::time::timeout(remaining, notify.notified()).await;
        }
    }

    /// 端到端同步执行：入槽 → 等领取（`pickup_wait`）→ 等结果（`timeout_secs + result_grace`）。
    /// 直接对结构体可测，无需起 HTTP（供单测复用）。
    pub async fn run(
        &self,
        worker_id: &str,
        req: ExecRequest,
        pickup_wait: Duration,
        result_grace: Duration,
    ) -> RunOutcome {
        let id = req.id.clone();
        let timeout_secs = req.timeout_secs;
        let rx = match self.submit(worker_id, req) {
            Ok(rx) => rx,
            Err(SubmitError::WorkerNotExecCapable) => return RunOutcome::NotExecCapable,
            Err(SubmitError::WorkerOffline) => return RunOutcome::Offline,
            Err(SubmitError::WorkerBusy) => return RunOutcome::Busy,
        };
        match self.wait_picked(worker_id, &id, pickup_wait).await {
            PickedOutcome::NotPickedUp => return RunOutcome::NotPickedUp,
            PickedOutcome::Picked => {}
        }
        let total_wait = Duration::from_secs(timeout_secs) + result_grace;
        match tokio::time::timeout(total_wait, rx).await {
            Ok(Ok(resp)) => RunOutcome::Completed(resp),
            _ => RunOutcome::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use worker_core::proto::{ExecState, DEFAULT_OUTPUT_LIMIT_BYTES, SHELL_POWERSHELL};

    fn sample_req(id: &str) -> ExecRequest {
        ExecRequest {
            id: id.to_string(),
            operator: "alice".into(),
            shell: SHELL_POWERSHELL.into(),
            script: "Get-ChildItem".into(),
            args: vec![],
            cwd: None,
            env: vec![],
            timeout_secs: 5,
            stdout_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
            stderr_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
        }
    }

    fn completed(id: &str) -> ExecResponse {
        ExecResponse {
            id: id.to_string(),
            state: ExecState::Completed,
            exit_code: Some(0),
            stdout: "hi".into(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            duration_ms: 10,
            error: None,
        }
    }

    #[tokio::test]
    async fn run_against_unregistered_worker_is_not_exec_capable() {
        let co = Coordinator::default();
        let outcome = co
            .run(
                "ghost",
                sample_req("id-1"),
                Duration::from_millis(50),
                Duration::from_millis(50),
            )
            .await;
        assert!(matches!(outcome, RunOutcome::NotExecCapable));
    }

    #[tokio::test]
    async fn offline_worker_rejected() {
        let co = Coordinator::default();
        co.register("w1", "inst1", None, None);
        assert!(co.list_workers()[0].online);

        co.force_stale_heartbeat("w1");
        // 心跳过期后：观测面下线,submit 直接拒绝 offline。
        assert!(co.list_workers().is_empty());
        let err = co.submit("w1", sample_req("id-1")).unwrap_err();
        assert_eq!(err, SubmitError::WorkerOffline);
    }

    #[tokio::test]
    async fn busy_when_slot_occupied() {
        let co = Coordinator::default();
        co.register("w1", "inst1", None, None);
        let _rx = co.submit("w1", sample_req("id-1")).unwrap();
        let err = co.submit("w1", sample_req("id-2")).unwrap_err();
        assert_eq!(err, SubmitError::WorkerBusy);
    }

    #[tokio::test]
    async fn not_picked_up_when_nobody_polls() {
        let co = Coordinator::default();
        co.register("w1", "inst1", None, None);
        let outcome = co
            .run(
                "w1",
                sample_req("id-1"),
                Duration::from_millis(30),
                Duration::from_secs(1),
            )
            .await;
        assert!(matches!(outcome, RunOutcome::NotPickedUp));
        // 清理后槽位应已释放，可以立即提交新任务。
        let snap = co.list_workers();
        assert!(!snap[0].busy);
    }

    #[tokio::test]
    async fn heartbeat_and_next_reject_stale_instance() {
        let co = Coordinator::default();
        co.register("w1", "inst-a", None, None);
        assert_eq!(
            co.heartbeat("w1", "inst-b"),
            HeartbeatOutcome::StaleInstance
        );
        assert!(matches!(
            co.next("w1", "inst-b", Duration::from_millis(10)).await,
            NextOutcome::StaleInstance
        ));
        assert_eq!(co.heartbeat("w1", "inst-a"), HeartbeatOutcome::Ok);
    }

    #[tokio::test]
    async fn heartbeat_next_result_unknown_worker() {
        let co = Coordinator::default();
        assert_eq!(co.heartbeat("nope", "x"), HeartbeatOutcome::UnknownWorker);
        assert!(matches!(
            co.next("nope", "x", Duration::from_millis(10)).await,
            NextOutcome::UnknownWorker
        ));
        assert_eq!(
            co.result("nope", "x", completed("id-1")).unwrap_err(),
            ResultOutcome::UnknownWorker
        );
    }

    #[tokio::test]
    async fn result_rejects_mismatched_id_and_stale_instance() {
        let co = Coordinator::default();
        co.register("w1", "inst1", None, None);
        let _rx = co.submit("w1", sample_req("id-1")).unwrap();

        // 槽位里是 id-1，回传 id-2 → IdMismatch。
        assert_eq!(
            co.result("w1", "inst1", completed("id-2")).unwrap_err(),
            ResultOutcome::IdMismatch
        );
        // 旧实例回传 → StaleInstance（且不影响槽位）。
        assert_eq!(
            co.result("w1", "inst-old", completed("id-1")).unwrap_err(),
            ResultOutcome::StaleInstance
        );
        // 正确 id + 正确 instance → Ok，且携带原始请求信息。
        let done = co.result("w1", "inst1", completed("id-1")).unwrap();
        assert_eq!(done.req.id, "id-1");
        assert_eq!(done.req.operator, "alice");
        // 槽位已释放。
        assert!(!co.list_workers()[0].busy);
        // 重复回传同一 id → 槽位已空 → IdMismatch。
        assert_eq!(
            co.result("w1", "inst1", completed("id-1")).unwrap_err(),
            ResultOutcome::IdMismatch
        );
    }

    #[tokio::test]
    async fn run_completes_happy_path_with_concurrent_worker() {
        let co = Arc::new(Coordinator::default());
        co.register("w1", "inst1", Some("7.4"), Some("host1"));

        let co2 = co.clone();
        let worker_task = tokio::spawn(async move {
            loop {
                match co2.next("w1", "inst1", Duration::from_millis(500)).await {
                    NextOutcome::Job(req) => {
                        co2.result("w1", "inst1", completed(&req.id)).unwrap();
                        break;
                    }
                    NextOutcome::Idle => continue,
                    other => panic!("unexpected next outcome: {other:?}"),
                }
            }
        });

        let outcome = co
            .run(
                "w1",
                sample_req("id-1"),
                Duration::from_secs(2),
                Duration::from_secs(2),
            )
            .await;
        worker_task.await.unwrap();
        match outcome {
            RunOutcome::Completed(resp) => {
                assert_eq!(resp.stdout, "hi");
                assert_eq!(resp.id, "id-1");
            }
            other => panic!("unexpected run outcome: {other:?}"),
        }
        assert!(!co.list_workers()[0].busy);
    }

    #[tokio::test]
    async fn run_returns_unknown_when_result_never_arrives_after_pickup() {
        let co = Arc::new(Coordinator::default());
        co.register("w1", "inst1", None, None);

        let co2 = co.clone();
        // worker 领取但从不回传结果（模拟失联）。
        let worker_task = tokio::spawn(async move {
            match co2.next("w1", "inst1", Duration::from_millis(500)).await {
                NextOutcome::Job(req) => req.id,
                other => panic!("unexpected next outcome: {other:?}"),
            }
        });

        let mut req = sample_req("id-1");
        req.timeout_secs = 1; // 缩短等待,测试只关心「拿不到结果→unknown」
        let outcome = co
            .run(
                "w1",
                req,
                Duration::from_secs(2),
                Duration::from_millis(100),
            )
            .await;
        let _ = worker_task.await.unwrap();
        assert!(matches!(outcome, RunOutcome::Unknown));
        // 未收到结果,槽位仍标记 busy(第一期已知限制:无 reaper)。
        assert!(co.list_workers()[0].busy);
    }
}
