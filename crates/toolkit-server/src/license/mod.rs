//! 软件授权 · 在线续期（服务端集成层，设计 `docs/license-impl-design.md` §6.2/§3.4）。
//!
//! **验签/签名核心在 `custom-utils` 的 `util_license`（`license-sign` feature）**——本模块
//! 只做服务端集成：`licenses` 台账（[`store`]）+ 续期私钥装配（[`signer`]）+ HTTP 路由
//! （[`routes`]）。服务端**不验签、只签发**：`POST /api/license/refresh` 现签一份新的续期
//! `TKL1` + `TKR1` 响应封套，真正的信任判断（委托证书链、日期不变量、机器指纹匹配…）全部
//! 留给客户端的 `evaluate` 状态机。
//!
//! renewal 私钥只在 [`Signer`] 里短暂持有（内存，不落日志、不回显）；未配置三个必需 env
//! （见 [`Signer::from_env`]）时 `AppState.license_signer = None`，`/api/license/refresh`
//! 返回 503（与 TTS/LLM 未配置时的既有约定一致），不 panic。

pub mod ratelimit;
pub mod routes;
pub mod signer;
pub mod store;

pub use ratelimit::RateLimiter;
pub use signer::Signer;
