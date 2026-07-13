use crate::net_policy::NetPolicyState;
use std::path::PathBuf;
use std::sync::Arc;

/// 最小顶层应用状态：本 app 只有网络策略一个模块，只保留 net_policy 用到的字段
/// （workspace 路径 + `NetPolicyState` 句柄）。对齐 zero-desktop 的 `AppState` 装配方式，
/// 但去掉了 english/speech/cookie/codeloop/g10_deploy/music 等不相关模块。
#[derive(Clone)]
pub struct AppState {
    #[allow(dead_code)]
    pub workspace: PathBuf,
    pub net_policy: Arc<NetPolicyState>,
}

impl AppState {
    pub fn new(workspace: PathBuf) -> anyhow::Result<Self> {
        let net_policy = Arc::new(NetPolicyState::new(workspace.clone()));
        Ok(Self {
            workspace,
            net_policy,
        })
    }
}
