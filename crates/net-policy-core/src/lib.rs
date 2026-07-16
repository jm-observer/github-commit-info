//! net-policy 纯业务库（设计文档 §3）。
//!
//! **不依赖** Tauri / axum / 管道 / 服务 / GUI / tokio / windows-sys——尽量多的纯逻辑集中于此，
//! 可在任意平台单元测试。副作用（mihomo 进程、Windows Firewall、观察器、管道 server）在
//! `net-policy-agent`；客户端在 `net-policy-client`。
//!
//! 模块：
//! - [`config`]：settings/rules 类型、校验、持久化、跨表一致性。
//! - [`valid`]：输入校验（= 跨完整性级别的安全边界，§3.1）。
//! - [`mihomo`]：mihomo `config.yaml` 生成（纯函数）。
//! - [`types`]：状态/快照/报告类型（控制面响应载荷）。
//! - [`operation`]：长操作状态机与进度模型。
//! - [`capture`]：抓包纯逻辑（协议 DTO / 参数校验 / 过滤器预算 / 状态机；真实 pktmon 后端在 agent）。
//! - [`decrypt`]：L4 应用明文纯逻辑（会话/CA/目标 DTO / 脱敏 / 状态机；真实 MITM 引擎在 agent，受 ADR 阻断）。
//! - [`protocol`]：版本化 typed 管道线协议（client/agent 共用；即 `net-policy-core::protocol`）。

pub mod capture;
pub mod config;
pub mod decrypt;
pub mod mihomo;
pub mod operation;
pub mod protocol;
pub mod routes;
pub mod types;
pub mod valid;
