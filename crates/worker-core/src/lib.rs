//! worker-core：远程执行（remote-exec）的**执行端内核**。
//!
//! 见 `docs/remote-exec-design.md` 第一期。本 crate 只负责「拿到一个 [`proto::ExecRequest`]
//! 就在本机跑出 [`proto::ExecResponse`]」以及本地审计，**不含任何网络/注册/长轮询逻辑**
//! （那些在 `toolkit-worker`）。线格式在 [`proto`]，controller 与 worker 共用。

pub mod audit;
pub mod exec;
pub mod proto;

pub use exec::{cleanup_stale_tmp, Executor};
pub use proto::{ExecRequest, ExecResponse, ExecState};
