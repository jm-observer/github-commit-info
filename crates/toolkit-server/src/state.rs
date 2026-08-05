use agent_session::store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use toolkit_core::SqlitePool;
use toolkit_tasks::Registry;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub registry: Arc<Registry>,
    pub db_path: PathBuf,
    /// workspace 根（toolkit.db / douyin/cookies.json / downloads/ / knowledge/ 等都在此下）。
    pub workspace: PathBuf,
    /// codeloop 会话存储观测：只读解析本机 `~/.codex` / `~/.claude`（不在 workspace 下）。
    pub session_store: Arc<Store>,
    /// 出口代理 worker 注册表(轻模型「借出口」):worker 通道 + 请求路由 + session 绑定。
    /// in-memory(P0);`/api/internal/*` 与消费侧 `egress_pool::Pool` 共享它。
    pub egress: Arc<egress_pool::Registry>,
    /// 出口代理消费 HTTP 面(F4)的 session 存根:server 侧持有 `egress_pool::Session` guard,
    /// 外部进程经 `session_handle` 间接指挥。详见 [`crate::egress_sessions`]。
    pub egress_sessions: Arc<crate::egress_sessions::SessionStore>,
    /// 远程执行(remote-exec)第一期调度器:per-worker 单任务槽,in-memory,进程内共享。
    /// 与 `egress` 刻意独立(只共享 `worker_id` 概念),详见 [`crate::exec`]。
    pub exec: Arc<crate::exec::Coordinator>,
    /// remote-exec 集中审计:JSONL 落 `<workspace>/remote-exec/audit/`。
    pub exec_audit: Arc<crate::exec::audit::AuditLog>,
    /// 软件授权 · 在线续期私钥(设计 `docs/license-impl-design.md` §3.4/§6.2)。`None` = 未配置
    /// `LICENSE_RENEWAL_SEED`/`LICENSE_RENEWAL_KID`/`LICENSE_RENEWAL_CERT` 三者之一,
    /// `/api/license/refresh` 据此返回 503(与 TTS/LLM 未配置同风格)。
    pub license_signer: Option<Arc<crate::license::Signer>>,
    /// `/api/license/refresh` 的进程内限流器(每 IP + 每 lic_id 各查一次),与 signer 是否
    /// 配置无关——即便 signer=None 也共用同一限流器实例,避免探测未配置状态本身被刷。
    pub license_rate_limiter: Arc<crate::license::RateLimiter>,
}
