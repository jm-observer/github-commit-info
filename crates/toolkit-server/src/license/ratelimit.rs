//! 进程内简单滑动窗口限流器，供 `POST /api/license/refresh`（免 Bearer 端点）用（设计 §6.2：
//! "接口必须做每 IP/每 lic_id 限流"）。**从简但要有**：单进程内存态，重启即清零，不做分布式
//! 协调——这层的目的是挡掉简单的脚本刷量/误重试风暴，不是安全边界本身（真正的边界是签名 +
//! 不可变 root 锚 + 客户端机器匹配，见设计 §6.2 末段）。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 单个 key（`ip:<addr>` 或 `lic:<lic_id>`）的滑动窗口计数器集合。
pub struct RateLimiter {
    window: Duration,
    max_hits: usize,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new(max_hits: usize, window: Duration) -> Self {
        Self {
            window,
            max_hits,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// 默认给 `/api/license/refresh` 用的参数：每 key 每分钟最多 10 次。刷新是随机延迟
    /// 18~30h 一次的正常客户端行为（设计 §6.1），偶发短时间内重试（网络抖动/客户端重连）
    /// 应当放行，但脚本化刷量必须挡住。
    pub fn default_refresh() -> Self {
        Self::new(10, Duration::from_secs(60))
    }

    /// 检查并记录一次命中：key 在当前窗口内的命中数已达上限则返回 `false`（拒绝，不计入
    /// 本次）；否则记录本次命中并返回 `true`。**同一把锁内完成"清理过期 + 判断 + 记录"**，
    /// 避免两个并发请求都读到"未满"而一起放行导致实际超限（TOCTOU）。
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut guard = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(key.to_string()).or_default();
        while let Some(&front) = entry.front() {
            if now.duration_since(front) > self.window {
                entry.pop_front();
            } else {
                break;
            }
        }
        if entry.len() >= self.max_hits {
            return false;
        }
        entry.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit_then_rejects() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        assert!(rl.check("a"));
        assert!(rl.check("a"));
        assert!(rl.check("a"));
        assert!(!rl.check("a"));
        // 不同 key 互不影响。
        assert!(rl.check("b"));
    }

    #[test]
    fn window_expiry_frees_capacity() {
        let rl = RateLimiter::new(1, Duration::from_millis(20));
        assert!(rl.check("a"));
        assert!(!rl.check("a"));
        std::thread::sleep(Duration::from_millis(30));
        assert!(rl.check("a"));
    }
}
