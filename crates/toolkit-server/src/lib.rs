//! toolkit-server：axum 服务，装配 toolkit-core / toolkit-tasks + 业务模块（Plan 2+）。

pub mod audioforge;
mod audiostore;
pub mod codeloop;
pub mod config;
#[path = "douyin/mod.rs"]
pub mod douyin_mod;
pub mod llm;
pub mod routes;
pub mod shadow;
pub mod state;
mod static_assets;

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

pub use config::Config;
pub use state::AppState;

/// workspace 根：优先 `TOOLKIT_WORKSPACE` 环境变量；未设置时回退到
/// `$HOME/.config/toolkit-server`（Windows 走 `%USERPROFILE%`）。
/// 与 `LinuxService` 安装期 `{workspace}` 模板默认对齐。
pub fn workspace_dir() -> Result<PathBuf> {
    if let Some(ws) = std::env::var_os("TOOLKIT_WORKSPACE") {
        return Ok(PathBuf::from(ws));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("TOOLKIT_WORKSPACE 与 HOME/USERPROFILE 均未设置，无法定位 workspace")?;
    Ok(PathBuf::from(home).join(".config").join("toolkit-server"))
}

/// 启动服务，阻塞直至 Ctrl+C。
pub async fn run(cfg: Config) -> Result<()> {
    let web_dir = cfg.web_dir.clone();
    let state = bootstrap(&cfg)?;
    serve_with_web(cfg.bind, state, &web_dir).await
}

/// 仅做装配（pool/migrate/registry/recovery），不监听 socket。供测试复用。
pub fn bootstrap(cfg: &Config) -> Result<AppState> {
    std::fs::create_dir_all(&cfg.workspace)
        .with_context(|| format!("create workspace {}", cfg.workspace.display()))?;
    let db_path = cfg.workspace.join("toolkit.db");
    let pool = toolkit_core::open_pool(&db_path)?;
    toolkit_core::migrate(&pool)?;

    let mut registry = toolkit_tasks::Registry::new();
    registry.register::<toolkit_tasks::EchoTask>();
    douyin_mod::kinds::register_all(&mut registry);
    audioforge::register_all(&mut registry);
    codeloop::register_all(&mut registry);

    let recovered = toolkit_tasks::recover_interrupted(&pool)?;
    if recovered > 0 {
        log::info!("recovered {recovered} interrupted task(s) from prior run");
    }

    // codeloop 会话存储在用户 home（~/.codex、~/.claude），不在 workspace。
    // 定位不到 home 时回退到 workspace 根（locate 自然查无，不致命）。
    let session_store = agent_session::store::Store::from_env()
        .unwrap_or_else(|_| agent_session::store::Store::with_home(cfg.workspace.clone()));

    Ok(AppState {
        pool,
        registry: Arc::new(registry),
        db_path,
        workspace: cfg.workspace.clone(),
        session_store: Arc::new(session_store),
    })
}

/// 起 axum 服务（无 Web 静态目录）。供测试用——总是走内嵌最小 dashboard。
pub async fn serve(bind: SocketAddr, state: AppState) -> Result<()> {
    serve_with_web(bind, state, std::path::Path::new("/__nonexistent__")).await
}

/// 起 axum 服务并按 web_dir 是否存在决定 / 路由形态。
pub async fn serve_with_web(
    bind: SocketAddr,
    state: AppState,
    web_dir: &std::path::Path,
) -> Result<()> {
    // orchestrator(ASR 编排层）同进程挂载到 /api/asr：WS 在 /api/asr/stream，HTTP 在
    // /api/asr/api/*。其 app.db 落在 toolkit-server 的 workspace 下，与 toolkit.db 并列。
    // 嵌入模式：把 toolkit.db 池子注入 orchestrator，LLM 连接配置 / 提示词优先经公共层
    // （`llm/mod.rs::builtins` 登记的 `asr_optimize_zh` / `asr_translate`）解析。详见
    // docs/llm-and-voice-enhancement-plan.md 节 A。
    let orch_ctx = orchestrator::init_ctx_with_toolkit_pool(&state.workspace, state.pool.clone())
        .context("orchestrator init_ctx (ASR 编排层)")?;
    let app = build_router(state, web_dir).nest("/api/asr", orchestrator::router(orch_ctx));
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    log::info!("toolkit-server listening on {bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("axum serve")
}

/// 装配 Router。`web_dir` 存在 → 静态托管；否则内嵌最小 HTML。
pub fn build_router(state: AppState, web_dir: &std::path::Path) -> axum::Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let mut router = axum::Router::new()
        .nest("/api/web", routes::web::router())
        .nest(
            "/api/web/audio",
            routes::audio::router()
                .merge(audioforge::routes::router())
                .merge(audiostore::routes::router()),
        )
        .nest("/api/web/douyin", douyin_mod::routes::router())
        .nest("/api/web/llm", llm::routes::router())
        // English 跟读判分（FunASR 转写 + 词级对齐 + 落 toolkit.db）。
        .nest("/api/web/shadow", shadow::routes::router())
        .nest("/api/agent", routes::agent::router())
        .nest("/api/browser", routes::browser::router())
        // english 后端反代:LAN 模式桌面端走 http://<host>:8788/api/english/* 绕开自签证书。
        .nest("/api/english", routes::english::router());

    if web_dir.exists() {
        log::info!("serving static web/ from {}", web_dir.display());
        router = router.fallback_service(ServeDir::new(web_dir));
    } else {
        log::info!(
            "web_dir {} not present; falling back to embedded dashboard",
            web_dir.display()
        );
        router = router
            .route("/", axum::routing::get(static_assets::dashboard))
            .route("/app.js", axum::routing::get(static_assets::app_js))
            .route("/style.css", axum::routing::get(static_assets::style_css))
            .route("/hub.js", axum::routing::get(static_assets::hub_js))
            .route("/hub.css", axum::routing::get(static_assets::hub_css))
            .route(
                "/codeloop.js",
                axum::routing::get(static_assets::codeloop_js),
            )
            .route(
                "/codeloop.css",
                axum::routing::get(static_assets::codeloop_css),
            );
    }

    // /hub、/codeloop（无扩展名）在两种模式下都需要显式路由（ServeDir 不自动映射目录省略名）
    router = router
        .route("/hub", axum::routing::get(static_assets::hub))
        .route("/codeloop", axum::routing::get(static_assets::codeloop));

    router.layer(cors).with_state(state)
}

/// 起一个本地随机端口供测试用。返回 (listener, addr)。
pub async fn bind_ephemeral() -> Result<(tokio::net::TcpListener, SocketAddr)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

/// 用于测试：把现成 listener + state 跑起来（无静态 web 目录，走嵌入式 HTML）。
pub async fn serve_with_listener(listener: tokio::net::TcpListener, state: AppState) -> Result<()> {
    let router = build_router(state, std::path::Path::new("/__nonexistent__"));
    axum::serve(listener, router).await?;
    Ok(())
}

/// helper：构造 Config 给测试用
pub fn test_config(workspace: PathBuf) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        workspace,
        web_dir: PathBuf::from("/__nonexistent__"),
    }
}
