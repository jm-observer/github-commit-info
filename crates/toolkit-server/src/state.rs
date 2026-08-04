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
}
