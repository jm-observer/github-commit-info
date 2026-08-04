//! remote-exec 第一期:controller 端 exec 通道——调度([`coordinator`])+ 路由([`routes`])+
//! 集中审计([`audit`])。设计见 `docs/remote-exec-design.md` 第一期 §4/§5/§7/§8。
//!
//! 与出口代理(`egress-pool` / `routes::internal`)刻意独立:不共享状态、不共享凭据、
//! 不共享中间件,只共享「稳定 `worker_id`」这一概念。

pub mod audit;
pub mod coordinator;
pub mod routes;

pub use coordinator::Coordinator;
