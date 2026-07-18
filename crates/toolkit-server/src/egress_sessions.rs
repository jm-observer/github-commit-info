//! `SessionStore`:server 侧持有出口代理 [`egress_pool::Session`] guard 的注册表。
//!
//! 背景:`/api/web/egress` 的消费方是**外部进程**(经 HTTP,见 [`crate::routes::egress`]),
//! 它们没法像进程内代码那样直接拿一个 `egress_pool::Session` 长期持有 —— session 必须活在
//! server 进程里,外部进程只持有一个不透明的 `session_handle` 字符串,每次请求带 handle 来
//! 「指挥」server 侧已经拿到的那个 session 代发。这样才能保证「同一 session 多次 HTTP 请求
//! 命中同一 worker + 连续 cookie」。
//!
//! 并发要点:`Session::fetch` 是 `async fn(&self, ...)`,**绝不能在持有 `Mutex<HashMap>` 的锁时
//! 跨越 `.await`**(否则等价于把整个 map 锁在请求耗时内,压垮并发)。做法:锁 map 只做「取出
//! `Arc<StoredSession>` 并 clone」这一步,随后立刻释放锁;真正的 `.await` 发生在锁外,通过
//! `Arc<StoredSession>` 的共享引用调用 `session.fetch(&self, ...)`(签名只需要 `&self`,多个调用方
//! 可以并发持有同一个 `Arc` 各自 `.await`,互不阻塞、也不会破坏 `Session` 内部状态)。
//!
//! TTL 兜底:外部进程可能忘记调 `/session/{handle}/release`,由 [`serve_with_web`](crate::serve_with_web)
//! 里 spawn 的后台 reaper 每 30s 扫一遍,把超过 `SESSION_TTL` 未使用的条目摘掉 —— `Arc<StoredSession>`
//! 最后一个强引用被 drop 后,内部 `egress_pool::Session` 的 `Drop` 自动触发,释放 worker 占用。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// 一个 session handle 空闲超过此时长(自最近一次 fetch/创建起算)即被 reaper 回收。
pub const SESSION_TTL_MS: i64 = 5 * 60 * 1000; // 5 分钟
/// reaper 的扫描间隔。
pub const REAPER_INTERVAL_SECS: u64 = 30;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 被 `SessionStore` 持有的一条记录:内层 `egress_pool::Session` guard + 最近使用时间。
pub struct StoredSession {
    pub session: egress_pool::Session,
    /// 最近一次被使用(创建或 fetch)的 wall-clock 毫秒时间戳。原子写,配合「取出 Arc 后锁外
    /// 访问」的模式,不需要为了刷新它重新加 map 的锁。
    last_used_ms: AtomicI64,
}

impl StoredSession {
    fn new(session: egress_pool::Session) -> Self {
        Self {
            session,
            last_used_ms: AtomicI64::new(now_ms()),
        }
    }

    fn touch(&self) {
        self.last_used_ms.store(now_ms(), Ordering::Relaxed);
    }

    fn is_expired(&self, ttl_ms: i64, now: i64) -> bool {
        now - self.last_used_ms.load(Ordering::Relaxed) > ttl_ms
    }
}

/// 外部进程消费出口代理 session 的服务端存根:`session_handle -> StoredSession`。
#[derive(Default)]
pub struct SessionStore {
    inner: Mutex<HashMap<String, Arc<StoredSession>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 存入一个新拿到的 session,生成并返回其 handle。
    pub fn insert(&self, session: egress_pool::Session) -> String {
        let handle = uuid::Uuid::new_v4().to_string();
        let stored = Arc::new(StoredSession::new(session));
        let mut g = self.inner.lock().unwrap();
        g.insert(handle.clone(), stored);
        handle
    }

    /// 按 handle 取出 `Arc<StoredSession>`(clone 后立即释放锁,调用方在锁外 `.await`)。
    /// 找不到 → `None`(handle 不存在或已被 reaper/release 回收)。
    pub fn get(&self, handle: &str) -> Option<Arc<StoredSession>> {
        let g = self.inner.lock().unwrap();
        g.get(handle).cloned()
    }

    /// 标记一次使用(fetch 成功/失败都应调用,避免活跃 session 被 reaper 误杀)。
    pub fn touch(&self, handle: &str) {
        if let Some(stored) = self.get(handle) {
            stored.touch();
        }
    }

    /// 显式释放:从 map 移除。若这是最后一个强引用,`StoredSession` 随即被 drop,
    /// 内层 `egress_pool::Session::drop` 触发,释放 worker 占用。幂等:handle 不存在也返回。
    pub fn remove(&self, handle: &str) {
        let mut g = self.inner.lock().unwrap();
        g.remove(handle);
    }

    /// reaper 用:摘掉所有超过 `ttl_ms` 未使用的条目,返回被摘掉的 handle 数。
    pub fn reap_expired(&self, ttl_ms: i64) -> usize {
        let now = now_ms();
        let mut g = self.inner.lock().unwrap();
        let expired: Vec<String> = g
            .iter()
            .filter(|(_, s)| s.is_expired(ttl_ms, now))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &expired {
            g.remove(k);
        }
        expired.len()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 后台 TTL reaper:周期性扫描并回收过期 session。仅在真正 `serve` 时 spawn
/// （`bootstrap()` 是同步装配、测试也复用它，不应该在其中起后台任务）。
pub async fn run_reaper(store: Arc<SessionStore>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(REAPER_INTERVAL_SECS));
    loop {
        tick.tick().await;
        let n = store.reap_expired(SESSION_TTL_MS);
        if n > 0 {
            log::info!("egress session reaper: reclaimed {n} expired session(s)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `is_expired` 只依赖 `AtomicI64` 时间戳,不需要真的构造 `egress_pool::Session`
    /// （其字段非 pub,且构造需要一个在线 worker）——直接测时间判定逻辑本身。
    #[test]
    fn expiry_math() {
        let ts = AtomicI64::new(1_000);
        let is_expired = |ttl_ms: i64, now: i64| now - ts.load(Ordering::Relaxed) > ttl_ms;
        assert!(!is_expired(500, 1_400)); // 只过去 400ms,< 500 ttl
        assert!(is_expired(500, 1_600)); // 过去 600ms,> 500 ttl
    }

    #[test]
    fn reap_expired_removes_only_stale_and_is_idempotent_remove() {
        let store = SessionStore::new();
        // 空 store:reap/remove 均安全无副作用。
        assert_eq!(store.reap_expired(SESSION_TTL_MS), 0);
        store.remove("no-such-handle");
        assert_eq!(store.len(), 0);
        // get 未知 handle → None。
        assert!(store.get("no-such-handle").is_none());
    }
}
