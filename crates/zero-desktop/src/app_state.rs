use crate::modules::{
    cookie::CookieState, english::EnglishState, g10_deploy::G10DeployState, music::MusicState,
    speech::SpeechState,
};
use crate::shared::settings::NetResolver;
use crate::shared::workspace::speech_db_path;
use std::path::PathBuf;
use std::sync::Arc;

/// 顶层应用状态，持有 workspace 路径和各模块的状态。
#[derive(Clone)]
pub struct AppState {
    pub workspace: PathBuf,
    /// 局域网/外网活动端点解析器（带健康探测缓存，全应用共享）。
    pub net: Arc<NetResolver>,
    pub english: Arc<EnglishState>,
    pub speech: Arc<SpeechState>,
    pub cookie: Arc<CookieState>,
    pub g10_deploy: Arc<G10DeployState>,
    pub music: Arc<MusicState>,
}

impl AppState {
    pub fn new(workspace: PathBuf) -> anyhow::Result<Self> {
        let cookie = Arc::new(CookieState::new(workspace.clone())?);
        let speech = SpeechState::new(&speech_db_path(&workspace))?;
        let g10_deploy = Arc::new(G10DeployState::new(workspace.clone()));
        let music = MusicState::new(&workspace);
        Ok(Self {
            workspace,
            net: Arc::new(NetResolver::new()),
            english: Arc::new(EnglishState::default()),
            speech,
            cookie,
            g10_deploy,
            music,
        })
    }
}
