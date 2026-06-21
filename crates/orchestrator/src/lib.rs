//! StreamSpeech 编排层:对客户端的 WebSocket(`/stream`)<-> ASR 服务 + vLLM。业务胶水,
//! 不做推理。**lib + bin 双形态**:`bin`(`main.rs`)是独立 systemd 守护;`lib` 暴露
//! [`router`] / [`init_ctx`] 供 toolkit-server 同进程 `.nest("/api/asr", ..)` 挂载。

mod db;
mod protocol;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use custom_utils::updater::LinuxService;

use axum::{
    body::Bytes,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use custom_utils::trace::{self, LlmCall, SpanRecord, SpanStatus, TraceContext};
use db::Db;
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientControl, Hello, ServerEvent};
use serde_json::json;
use std::collections::HashMap;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as TMessage;

#[derive(Clone)]
struct Cfg {
    asr_ws: String,
    asr_embed: String,
    vllm_base: String,
    vllm_model: String,
}

/// 编排层运行时上下文(下游地址 + DB)。对外不透明:toolkit-server 只在 [`init_ctx`] 与
/// [`router`] 之间原样传递,不访问字段。
#[derive(Clone)]
pub struct AppCtx {
    cfg: Cfg,
    db: Arc<Db>,
    /// 嵌入 toolkit-server 时持有的公共 LLM 层连接池（toolkit.db），用于读 `llm_config` /
    /// `llm_prompts`。standalone bin 模式恒为 `None`，所有 LLM 配置回退到 orchestrator
    /// 自有的 env / app.db / 编译期默认。详见节 A LLM 配置收拢。
    toolkit_pool: Option<toolkit_core::SqlitePool>,
}

/// 运行时 Cfg。迁入 toolkit 后 orchestrator 是宿主 systemd 进程(非容器),故 asr /
/// vLLM 默认地址改走**本机回环**:asr 容器已把 9100/9101 发布到 127.0.0.1(见
/// streaming-speech 仓 server/compose.yaml),vLLM 是宿主进程。监听地址不进 Cfg,由
/// serve 的入参直接决定(嵌入态无监听)。
fn cfg() -> Cfg {
    Cfg {
        asr_ws: std::env::var("ASR_WS").unwrap_or_else(|_| "ws://127.0.0.1:9100".into()),
        asr_embed: std::env::var("ASR_EMBED")
            .unwrap_or_else(|_| "http://127.0.0.1:9101/embed".into()),
        vllm_base: std::env::var("VLLM_BASE").unwrap_or_else(|_| "http://127.0.0.1:8085/v1".into()),
        vllm_model: std::env::var("VLLM_MODEL").unwrap_or_else(|_| "default".into()),
    }
}

static SEG_ID: AtomicU64 = AtomicU64::new(1);

// Defaults for the LLM keys seeded into config (editable in the console
// "配置" tab). Also the fallback if a key is somehow missing from the DB.
//
// 节 B（ASR 中文优化加固）：在保留口语病修正的基础上，追加四条新规则：
//   ① 英文 / 代码标识符保持原样（避免把 `Tauri` 写成"塔里"）。
//   ② 数字 / 日期 / 金额 / 版本号统一阿拉伯数字与标准写法。
//   ③ 逐句对齐，不删、不增、不合并、不压缩、不总结。
//   ④ 已通顺则原样返回。
const DEFAULT_OPTIMIZE_PROMPT: &str = "你是中文口语转写规整器。任务:仅修正口语病(去除\"那/就是/啊/什么的\"等口头语、合并自我重复如\"最左侧是最左侧是\"、补齐缺失标点、改正同音错字),输出通顺的书面中文。严格保留原句所有信息点和原有顺序;禁止归纳、概括、合并要点、改写为列表或重排语序;长句保持长句,不要为了简洁而压缩。\
\n规则:\
\n- 英文单词、代码标识符(驼峰/蛇形/含数字)保持原样,不要意译或音译,例如 Tauri 不要写成\"塔里\"。\
\n- 数字、日期、金额、版本号统一阿拉伯数字与标准写法,例如\"二零二六年六月\"→\"2026 年 6 月\"、\"v 一点零\"→\"v1.0\"。\
\n- 逐句对齐原文,不要合并/压缩/总结,不要删减信息。\
\n严格要求:只输出整理后的文本本身;不要解释、不要选项、不要markdown、不要追问、不要任何前后缀;若已通顺则原样返回。";
const DEFAULT_TRANSLATE_PROMPT: &str = "Translate the user's sentence into natural English. Output ONLY the translation itself — no explanations, no options, no quotes, no markdown.";

/// 节 A：公共 LLM 层（toolkit.db `llm_prompts`）中的提示词名（与 toolkit-server `llm::builtins`
/// 登记的 `NAME_ASR_OPTIMIZE_ZH` / `NAME_ASR_TRANSLATE` 必须保持一致，两边都用裸字符串避免
/// 循环依赖）。
const PROMPT_NAME_OPTIMIZE: &str = "asr_optimize_zh";
const PROMPT_NAME_TRANSLATE: &str = "asr_translate";

/// 解析 LLM 端点（base, model）。
///
/// 优先级：toolkit 公共层（嵌入模式才有）→ orchestrator 自身 app.db（旧路径，向后兼容）→
/// env / 编译期默认（[`Cfg`]）。公共层一行内 base 或 model 任一空 → 视为未配置，跳到回退。
fn resolve_llm_endpoint(
    toolkit_pool: Option<&toolkit_core::SqlitePool>,
    db: &Db,
    c: &Cfg,
) -> (String, String) {
    if let Some(pool) = toolkit_pool {
        if let Ok(Some(cfg)) = toolkit_core::llm_store::get_config(pool) {
            let base = cfg.base_url.trim();
            let model = cfg.model.trim();
            if !base.is_empty() && !model.is_empty() {
                return (base.to_string(), model.to_string());
            }
        }
    }
    let base = db
        .config_get("vllm.base")
        .unwrap_or_else(|| c.vllm_base.clone());
    let model = db
        .config_get("vllm.model")
        .unwrap_or_else(|| c.vllm_model.clone());
    (base, model)
}

/// 解析提示词文本。优先级：公共层 `llm_prompts.<name>` → app.db `<legacy_key>` →
/// `default_text`（编译期内置）。空白文本视为未覆盖。
fn resolve_prompt_text(
    toolkit_pool: Option<&toolkit_core::SqlitePool>,
    db: &Db,
    name: &str,
    legacy_key: &str,
    default_text: &str,
) -> String {
    if let Some(pool) = toolkit_pool {
        if let Ok(Some(p)) = toolkit_core::llm_store::get_prompt(pool, name) {
            if !p.text.trim().is_empty() {
                return p.text;
            }
        }
    }
    db.config_get(legacy_key)
        .unwrap_or_else(|| default_text.to_string())
}

/// 取热词原文（公共层 / orchestrator app.db 同一键 `asr.hotwords`，仅 orchestrator 一处来源）。
fn resolve_hotwords(db: &Db) -> String {
    db.config_get("asr.hotwords").unwrap_or_default()
}

/// 抽取热词列表(忽略空行和注释)。每行可为 "词" 或 "词 权重",权重对 LLM 没意义,这里只取词面。
fn parse_hotwords(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        // 行尾可选权重("词 1.5")仅当最后一个 token 是数字时剥掉;否则整行都是词面,
        // 以保留 "claude code" 这类含空格的多词术语(否则会被截成 "claude")。
        .map(|l| match l.rsplit_once(char::is_whitespace) {
            Some((head, tail)) if tail.parse::<f32>().is_ok() => head.trim_end().to_string(),
            _ => l.to_string(),
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// 构造发给 LLM 的 user message。
/// - context: 20 秒内历史优化文本(按时序排列),供 LLM 感知话题连贯性。
/// - primary: 主模型原始识别文本。
/// - secondary: 可选的次模型双候选;有则以 `(主模型名, 次模型名, 次模型文本)` 形式
///   呈现,两行各以真实模型名标注(「【sensevoice 识别】」/「【paraformer 识别】」),
///   让 LLM 结合各模型已知强弱择优合并成一条。
///
/// 无 context 且无 secondary 时直接返回 primary,与旧行为完全相同。
fn build_optimize_user_msg(
    context: &[String],
    primary: &str,
    secondary: Option<(&str, &str, &str)>,
) -> String {
    if context.is_empty() && secondary.is_none() {
        return primary.to_string();
    }
    let mut s = String::new();
    if !context.is_empty() {
        s.push_str("【近期上文，仅供参考，禁止输出】\n");
        for ctx in context {
            s.push_str(ctx);
            s.push('\n');
        }
        s.push('\n');
    }
    match secondary {
        Some((pname, sname, sec)) => {
            s.push_str(&format!("【{pname} 识别】"));
            s.push_str(primary);
            s.push_str(&format!("\n【{sname} 识别】"));
            s.push_str(sec);
        }
        None => s.push_str(primary),
    }
    s
}

/// 把 asr.hotwords 词表拼到中文润色 prompt 末尾,作为 SenseVoice 等不支持声学热词
/// 的模型的兜底。词表为空时返回原 prompt 不动。
fn optimize_prompt_with_hotwords(prompt: String, hotwords_raw: &str) -> String {
    let words = parse_hotwords(hotwords_raw);
    if words.is_empty() {
        return prompt;
    }
    let list = words.join("、");
    format!(
        "{prompt}\n\n【ASR 同音字纠错】本场景必出现术语:{list}。\
         请主动检查原文是否包含与上列任一术语同音或近音的字串(汉字不同但读音相同/相近,例如 \
         huìhuà 既可写作\"绘画\"也可写作\"会话\");若有,即使字面看起来已通顺,\
         也应改为术语词。这是高优先级修正,优先于一般的口语病规整。"
    )
}

const APP: &str = "orchestrator";
const REPO_OWNER: &str = "jm-observer";
const REPO_NAME: &str = "toolkit";
/// install 时写进 unit 的默认监听地址;serve 经 clap default/env 读取。
pub const DEFAULT_BIND: &str = "0.0.0.0:8090";
/// systemd watchdog 心跳间隔(秒)。
const WATCHDOG_SEC: u32 = 60;

/// 安装/自更新统一描述。ExecStart 由 `{workspace}` 模板在 install 时实拼;
/// `bind` 写进生成 unit `[Service]` 的 `Environment=ORCH_BIND=<bind>`。
pub fn linux_service(bind: &str) -> LinuxService {
    LinuxService::new(APP, REPO_OWNER, REPO_NAME, env!("CARGO_PKG_VERSION"))
        .bin_name(APP)
        .description("orchestrator: StreamSpeech 编排层 (axum WS + SQLite + Web 管理台)")
        .exec_args("serve --workspace {workspace}")
        .env("ORCH_BIND", bind)
        .watchdog_sec(WATCHDOG_SEC)
        .restart_sec(5)
}

/// workspace 默认目录:`~/.config/orchestrator`(与 install 默认一致),存 app.db。
pub fn workspace_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME 环境变量未设置"))?;
    Ok(PathBuf::from(home).join(".config").join(APP))
}

/// 全链路追踪(trace-hub):仅当设 TRACE_HUB_ENDPOINT 才启用,未设则全程 no-op。
pub fn init_trace() {
    if let Ok(ep) = std::env::var("TRACE_HUB_ENDPOINT") {
        trace::init(trace::TraceConfig::new(ep, "orchestrator"));
        tracing::info!("trace-hub tracing enabled");
    }
}

/// 起 daemon:构造 ctx(开库/播种/清理任务)+ 装路由 + 绑端口起 axum。独立运行入口。
pub async fn serve(bind: String, workspace: PathBuf) -> anyhow::Result<()> {
    let ctx = init_ctx(&workspace)?;
    let asr_ws = ctx.cfg.asr_ws.clone();
    let app = router(ctx);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("orchestrator listening on {bind} (asr={asr_ws})");
    axum::serve(listener, app).await?;
    Ok(())
}

/// 打开 workspace 下的 app.db、播种运行时配置、挂保留清理后台任务,构造 [`AppCtx`]。
/// **独立 serve 入口**——不传 toolkit pool，所有 LLM 配置走 orchestrator 自身的 env / app.db /
/// 编译期默认。嵌入 toolkit-server 模式应改用 [`init_ctx_with_toolkit_pool`]，把 toolkit.db
/// 池子注入进来，LLM 连接/提示词优先走公共层（节 A）。
///
/// 同步函数,但内部 `tokio::spawn` 清理任务,故须在 tokio 运行时内调用(两个调用方都在 async 上下文)。
pub fn init_ctx(workspace: &std::path::Path) -> anyhow::Result<AppCtx> {
    init_ctx_inner(workspace, None)
}

/// 嵌入 toolkit-server 模式入口：把宿主的 toolkit pool 注入 [`AppCtx`]，
/// LLM 连接配置 / 提示词优先经公共层（`toolkit-core::llm_store`）解析（节 A）。
pub fn init_ctx_with_toolkit_pool(
    workspace: &std::path::Path,
    toolkit_pool: toolkit_core::SqlitePool,
) -> anyhow::Result<AppCtx> {
    init_ctx_inner(workspace, Some(toolkit_pool))
}

fn init_ctx_inner(
    workspace: &std::path::Path,
    toolkit_pool: Option<toolkit_core::SqlitePool>,
) -> anyhow::Result<AppCtx> {
    let c = cfg();

    std::fs::create_dir_all(workspace).ok();
    let db_path = workspace.join("app.db");
    let db_path = db_path.to_str().context("workspace 路径含非法 UTF-8")?;
    let db = Arc::new(Db::open(db_path)?);
    // Resume the segment-id counter past anything on disk so a restart
    // never reuses an id and overwrites an existing row / its audio.
    SEG_ID.store(db.max_segment_id() as u64 + 1, Ordering::Relaxed);
    // Seed runtime-tunable defaults (editable in the console "配置" tab).
    for (k, v) in [
        ("asr.spk_threshold", "0.35"),
        ("asr.sentence_gap_ms", "1500"),
        // 中英文混合识别:默认 sensevoice(多语 zh/en,正确转出英文术语,低延迟适合流式);
        // 纯中文场景可在控制台热切回 paraformer。paraformer|sensevoice|whisper-turbo|whisper-large-v3
        ("asr.model", "sensevoice"),
        // 次模型(对比用):空=禁用。客户端 hello.want_secondary=true 时生效;
        // 同枚举集合,自动避免与主模型重复。默认 paraformer = 混合 vs 纯中文 A/B。
        ("asr.secondary_model", "paraformer"),
        ("asr.gate_to_enrolled", "on"), // on=仅识别已启用声纹 | off=识别所有人
        // 领域热词:每行一个词,可选 "词 权重"(权重 ≥1.0,默认 1.0)。
        // 同时喂给:(a) ASR 声学层(Paraformer hotword= / Whisper initial_prompt);
        // (b) LLM 润色 prompt 末尾(SenseVoice 不支持声学热词时的兜底)。
        ("asr.hotwords", ""),
        // LLM config — defaults from env/const, then live-editable in console.
        ("vllm.model", c.vllm_model.as_str()),
        ("vllm.base", c.vllm_base.as_str()),
        ("llm.optimize_prompt", DEFAULT_OPTIMIZE_PROMPT),
        ("llm.translate_prompt", DEFAULT_TRANSLATE_PROMPT),
    ] {
        if db.config_get(k).is_none() {
            db.config_set(k, v);
        }
    }
    // 每小时清理过期(>1天)的逐段音频 blob;文本记录保留。启动即跑一次。
    {
        let dbp = db.clone();
        tokio::spawn(async move {
            loop {
                let n = dbp.audio_purge_expired();
                if n > 0 {
                    tracing::info!("[retention] purged {n} audio blob(s) older than 1 day");
                }
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }
    Ok(AppCtx {
        cfg: c,
        db,
        toolkit_pool,
    })
}

/// 构建 orchestrator 的 axum 路由（自带 state，产出 `Router<()>` 便于宿主 `.nest()` 挂载）。
/// 独立 serve 与 toolkit-server 嵌入共用同一张路由表。
pub fn router(ctx: AppCtx) -> Router {
    Router::new()
        .route("/stream", get(ws_upgrade))
        .route("/health", get(health))
        .route("/", get(console))
        .route("/segment/{id}", get(console))
        .route("/api/stats", get(api_stats))
        .route("/api/history", get(api_history))
        .route("/api/speakers", get(api_speakers))
        .route("/api/speakers/enroll", post(api_speaker_enroll))
        .route("/api/voiceprints", get(api_voiceprints))
        .route("/api/asr-config", get(api_asr_config))
        .route("/api/speakers/{id}", delete(api_speaker_delete))
        .route("/api/speakers/{id}/rename", post(api_speaker_rename))
        .route("/api/speakers/{id}/enabled", post(api_speaker_enabled))
        .route("/api/segments/{id}/audio", get(api_segment_audio))
        .route("/api/segments/{id}/text", post(api_segment_set_text))
        .route("/api/segments/{id}/rerun", post(api_segment_rerun))
        .route(
            "/api/segments/{id}",
            get(api_segment_get).delete(api_segment_delete),
        )
        .route("/api/segments", delete(api_segments_clear))
        .route("/api/config", get(api_config_get).post(api_config_set))
        .with_state(ctx)
}

/// 健康端点：供 G10 部署面板探测连通性 + 版本（返回 2xx JSON）。
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn ws_upgrade(
    State(ctx): State<AppCtx>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // WS 升级首选:从 HTTP 升级请求头取 traceparent。浏览器侧无法塞自定义头时
    // 走 hello 帧的 traceparent 字段兜底(见 handle_client)。
    let upgrade_remote = trace::extract_traceparent(|h| {
        headers
            .get(h)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    });
    ws.on_upgrade(move |s| handle_client(s, ctx, upgrade_remote))
}

async fn handle_client(mut sock: WebSocket, ctx: AppCtx, upgrade_remote: Option<TraceContext>) {
    let c = ctx.cfg.clone();
    let db = ctx.db.clone();
    let toolkit_pool = ctx.toolkit_pool.clone();
    let session_id = format!("s{}", SEG_ID.load(Ordering::Relaxed));
    db.session_start(&session_id);
    let started = Instant::now();
    let stream_start_ms = trace::now_ms();

    // 1) hello
    let hello: Hello = match sock.recv().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str(&t) {
            Ok(h) => h,
            Err(e) => return send_fatal(&mut sock, "bad_hello", &e.to_string()).await,
        },
        _ => return,
    };

    // ws_stream 根 span:本会话子树根。优先用 WS 升级头的 traceparent;否则 hello 帧
    // 字段兜底;再不行则起独立 trace(不孤儿,仍能看本服务内部)。
    let remote = upgrade_remote.or_else(|| {
        hello
            .traceparent
            .as_deref()
            .and_then(TraceContext::from_traceparent)
    });
    let stream_ctx = remote
        .as_ref()
        .map(TraceContext::child)
        .unwrap_or_else(TraceContext::root);
    let hello_flags = (
        hello.want_optimize,
        hello.want_translate,
        hello.want_secondary,
    );

    // ws_stream 两阶段 emit anchor：WS 实时会话可能持续数十秒-数分钟，trace-hub
    // 在会话进行中就要看到「session 在进行」+ hello 参数；客户端意外断开 / 进程
    // 崩溃也已落库。close 时下面 emit_end 用同 span_id 覆盖填累计段数 + 时长。
    let stream_scope = trace::enabled().then(|| {
        let scope = trace::SpanScope::new(stream_ctx.clone(), "ws_stream")
            .with_summary(json!({
                "session_id": session_id.clone(),
                "want_optimize": hello_flags.0,
                "want_translate": hello_flags.1,
                "want_secondary": hello_flags.2,
            }))
            .with_request_body(
                json!({
                    "session_id": session_id.clone(),
                    "language": hello.language.clone(),
                    "want_optimize": hello_flags.0,
                    "want_translate": hello_flags.1,
                    "want_secondary": hello_flags.2,
                })
                .to_string(),
            );
        scope.emit_start();
        scope
    });
    let _ = sock
        .send(Message::Text(
            ServerEvent::Ready {
                session_id: session_id.clone(),
            }
            .json()
            .into(),
        ))
        .await;

    // 2) 连接 ASR 服务(升级请求头注入 traceparent,asr-server 端按 HeaderMap 提取)
    let asr_req = match c.asr_ws.as_str().into_client_request() {
        Ok(mut r) => {
            if let Ok(v) = HeaderValue::from_str(&stream_ctx.to_traceparent()) {
                r.headers_mut().insert("traceparent", v);
            }
            r
        }
        Err(e) => return send_fatal(&mut sock, "asr_unreachable", &e.to_string()).await,
    };
    let (asr, _) = match tokio_tungstenite::connect_async(asr_req).await {
        Ok(x) => x,
        Err(e) => return send_fatal(&mut sock, "asr_unreachable", &e.to_string()).await,
    };

    // 拆分:客户端写端交给 asr_reader。LLM(优化/翻译)按段并发后,会有多个
    // 任务并发写这个 sink,故包一层 async Mutex。
    let (cli_tx, mut cli_rx) = sock.split();
    let cli_tx = Arc::new(tokio::sync::Mutex::new(cli_tx));
    let (mut asr_tx, asr_rx) = asr.split();

    // Negotiate per-session knobs with asr (currently just want_secondary).
    // asr defaults to false if the message never arrives, so this is safe
    // to send before any audio frames. On send failure we surface a
    // non-fatal error and fall through — the next PCM frame in the main
    // loop will close cleanly if the asr socket is really gone.
    let cfg_msg = serde_json::json!({
        "type": "config",
        "want_secondary": hello.want_secondary,
    })
    .to_string();
    if let Err(e) = asr_tx.send(TMessage::text(cfg_msg)).await {
        let _ = cli_tx
            .lock()
            .await
            .send(Message::Text(
                ServerEvent::Error {
                    code: "asr_handshake".into(),
                    message: format!("asr config handshake failed: {e}"),
                    fatal: false,
                }
                .json()
                .into(),
            ))
            .await;
    }

    // 本会话上行 PCM 的滚动缓冲(16k mono s16le)。asr_reader 收到段时按
    // t_start/t_end 切片存为 WAV(留 1 天,供试听/下载/声纹/纠错样本)。
    // 与 asr 同一字节流、同一时钟;reset 时一起清零保持对齐。
    let pcm_buf = std::sync::Arc::new(std::sync::Mutex::new(PcmBuf::new()));

    // asr_reader:全程并发读 ASR(流式 VAD 会在过程中持续吐 segment),
    // 转发并按需调 vLLM,收到 done 则发 Done 结束。
    let reader = tokio::spawn(asr_reader(
        asr_rx,
        AsrReaderCtx {
            cli_tx,
            hello,
            c: c.clone(),
            session_id: session_id.clone(),
            db: db.clone(),
            pcm_buf: pcm_buf.clone(),
            stream_ctx: stream_ctx.clone(),
            toolkit_pool: toolkit_pool.clone(),
        },
    ));

    // 主循环:客户端音频/控制 -> ASR
    loop {
        match cli_rx.next().await {
            Some(Ok(Message::Binary(pcm))) => {
                if let Ok(mut b) = pcm_buf.lock() {
                    b.push(&pcm);
                }
                if asr_tx.send(TMessage::Binary(pcm.to_vec())).await.is_err() {
                    break;
                }
            }
            Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientControl>(&t) {
                Ok(ClientControl::Reset) => {
                    if let Ok(mut b) = pcm_buf.lock() {
                        b.clear();
                    }
                    let _ = asr_tx.send(TMessage::text(r#"{"type":"reset"}"#)).await;
                }
                Ok(ClientControl::Stop) => {
                    let _ = asr_tx.send(TMessage::text(r#"{"type":"flush"}"#)).await;
                    break;
                }
                Err(_) => {}
            },
            _ => {
                // 断线/关闭:让 ASR 收尾,asr_reader 会发 done
                let _ = asr_tx.send(TMessage::text(r#"{"type":"flush"}"#)).await;
                break;
            }
        }
    }

    // 等 asr_reader 处理完 flush 后的收尾(它负责发 Done)
    let reader_out = tokio::time::timeout(std::time::Duration::from_secs(30), reader).await;
    db.session_end(&session_id, started.elapsed().as_secs_f64());

    // ws_stream 根 span:本会话子树根。即便 reader 超时也尽量发出供查问题。
    if let Some(scope) = stream_scope {
        let segments_count = match &reader_out {
            Ok(Ok(n)) => *n,
            _ => 0,
        };
        let asr_model = ctx.db.config_get("asr.model").unwrap_or_default();
        let llm_model = ctx
            .db
            .config_get("vllm.model")
            .unwrap_or_else(|| c.vllm_model.clone());
        scope.emit_end(
            None,
            SpanStatus::Ok,
            Some(json!({
                "segments": segments_count,
                "asr_model": asr_model,
                "llm_model": llm_model,
            })),
        );
    }
    let _ = stream_start_ms; // 兼容旧打日志路径
}

/// 并发读取 ASR,逐段转发客户端,并按需调 vLLM 优化/翻译。收到 done 发 Done。
/// 返回本会话累计段数(供 handle_client 写到 ws_stream 概要)。
type AsrRx = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;
type CliTx = Arc<tokio::sync::Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>;

struct AsrReaderCtx {
    cli_tx: CliTx,
    hello: Hello,
    c: Cfg,
    session_id: String,
    db: Arc<Db>,
    pcm_buf: Arc<std::sync::Mutex<PcmBuf>>,
    stream_ctx: TraceContext,
    /// 公共 LLM 层连接池（嵌入模式下来自 toolkit-server）。None 时回退到 env / app.db。
    toolkit_pool: Option<toolkit_core::SqlitePool>,
}

async fn asr_reader(mut asr_rx: AsrRx, ctx: AsrReaderCtx) -> u64 {
    let AsrReaderCtx {
        cli_tx,
        hello,
        c,
        session_id,
        db,
        pcm_buf,
        stream_ctx,
        toolkit_pool,
    } = ctx;
    let mut seg_count: u64 = 0;
    // 会话墙上时钟锚点:音频 t=0 ≈ asr_reader 起点的真实时刻。每段 wall = anchor + 偏移,
    // 把音频时间线整体映射到真实时间线(单调、不重叠、时长准确)。
    let session_anchor = chrono::Local::now();
    fn wall_at(anchor: chrono::DateTime<chrono::Local>, secs: f64) -> String {
        let dt = anchor + chrono::Duration::milliseconds((secs * 1000.0).round() as i64);
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    }
    async fn send(tx: &CliTx, json: String) {
        let _ = tx.lock().await.send(Message::Text(json.into())).await;
    }
    let mut llm_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // (t_start_ms, t_end_ms) → orchestrator segment id. Populated when a
    // primary `segment` arrives; used to pair the asr's later `secondary`
    // event back to the same client-visible segment id. Quantising to ms
    // matches the float precision both sides round through.
    let mut seg_by_time: HashMap<(i64, i64), u64> = HashMap::new();
    fn time_key(t0: f64, t1: f64) -> (i64, i64) {
        ((t0 * 1000.0).round() as i64, (t1 * 1000.0).round() as i64)
    }
    // 合并模式:断链边界 = 客户端「合并间隔」(hello.merge_window_ms,与
    // 客户端复制 stitch 用同一个值),相邻段音频时间间隔 >= 间隔即开新链。
    // 本会话内固定(改间隔需重连,语义同 want_*)。0 = 关闭合并(逐段独立 +
    // 历史上下文注入的旧行为),与客户端 merge_window_ms=0 关 stitch 一致。
    let merge_window_s = hello.merge_window_ms as f64 / 1000.0;
    let merge_on = hello.merge_window_ms > 0;
    // 当前活跃的合并链:累积相邻 VAD 段的原始 ASR,段间间隔超阈值即换新。
    struct ActiveChain {
        id: u64,
        asr: String,
        t_start: f64,
        t_end: f64,
    }
    let mut chain: Option<ActiveChain> = None;
    // chain_id -> 该链已累积的次模型识别文本。合并模式下次模型按 VAD 段产出,
    // 多段属同一链,这里按链累积后整体展示(同 primary 链的累积方式)。
    let mut sec_accum: HashMap<u64, String> = HashMap::new();
    // chain_id -> 已发出 Optimized / Translated 的最大输入字符数。合并模式下
    // 同一链随每个新段重复触发润色,并发结果可能乱序到达;只放行"输入不短
    // 于已发出"的结果,保证较短的旧结果不会覆盖较长的新结果(latest-wins)。
    let opt_emitted: Arc<std::sync::Mutex<HashMap<u64, usize>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    let tr_emitted: Arc<std::sync::Mutex<HashMap<u64, usize>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    while let Some(Ok(msg)) = asr_rx.next().await {
        let TMessage::Text(t) = msg else { continue };
        let v: serde_json::Value = match serde_json::from_str(&t) {
            Ok(v) => v,
            Err(e) => {
                // 静默丢弃会把协议/asr bug 藏起来——截断打 warn 便于排障。
                let snippet: String = t.chars().take(200).collect();
                tracing::warn!("[orch] unparseable asr message ({e}): {snippet}");
                continue;
            }
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("segment") => {
                seg_count += 1;
                let raw_text = v
                    .get("text")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let t0 = v.get("t_start").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let t1 = v.get("t_end").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let speaker = v.get("speaker").and_then(|x| x.as_str());

                // 合并模式:把本段并入当前链(与上段间隔超阈值则开新链),id 用
                // 链 id,对外文本/时间范围用整条链;关闭时退化为每段一个新 id(旧行为)。
                let (id, surface_text, seg_t0, seg_t1) = if merge_on {
                    // 断链:本段 t0 与上段 t_end 的音频间隔 >= 合并间隔(与客户端
                    // next_clipboard_text 的 `< window` 合并条件镜像)。
                    let need_new = match &chain {
                        Some(ch) => (t0 - ch.t_end) >= merge_window_s,
                        None => true,
                    };
                    if need_new {
                        let cid = SEG_ID.fetch_add(1, Ordering::Relaxed);
                        chain = Some(ActiveChain {
                            id: cid,
                            asr: String::new(),
                            t_start: t0,
                            t_end: t1,
                        });
                    }
                    let ch = chain.as_mut().expect("chain set above");
                    ch.asr.push_str(&raw_text);
                    ch.t_end = t1;
                    (ch.id, ch.asr.clone(), ch.t_start, ch.t_end)
                } else {
                    let sid = SEG_ID.fetch_add(1, Ordering::Relaxed);
                    (sid, raw_text.clone(), t0, t1)
                };
                db.segment_upsert(
                    id as i64,
                    &session_id,
                    &surface_text,
                    None,
                    None,
                    seg_t0,
                    seg_t1,
                    speaker,
                );

                // asr_segment 子 span:每段一条,response_body = 该段(原始)转写文本。
                if trace::enabled() {
                    let seg_span = stream_ctx.child();
                    let dur_ms = ((t1 - t0) * 1000.0).round().max(0.0) as i64;
                    trace::record_span(SpanRecord {
                        trace_id: seg_span.trace_id,
                        span_id: seg_span.span_id,
                        parent_span_id: seg_span.parent_span_id,
                        service: String::new(),
                        kind: "asr_segment".into(),
                        flow_name: None,
                        start_ms: trace::now_ms() - dur_ms,
                        end_ms: trace::now_ms(),
                        status: SpanStatus::Ok,
                        summary: json!({
                            "seg_index": seg_count,
                            "dur_ms": dur_ms,
                            "text_len": raw_text.chars().count(),
                            "t_start": t0,
                            "t_end": t1,
                        }),
                        detail: serde_json::Value::Null,
                        request_body: None,
                        response_body: Some(raw_text.clone()),
                        body_truncated: false,
                        links: Vec::new(),
                    });
                }
                // Remember (t0,t1)→id so the asr's later `secondary` event can be
                // paired back。合并模式下多段共享 chain_id,次模型识别按链累积展示
                // (但不跑 re-polish —— 逐段双候选与整链语义冲突)。
                if hello.want_secondary {
                    seg_by_time.insert(time_key(t0, t1), id);
                }
                // forward the segment immediately — never blocked by LLM
                send(
                    &cli_tx,
                    ServerEvent::Segment {
                        id,
                        text: surface_text.clone(),
                        t_start: Some(seg_t0 as f32),
                        t_end: Some(seg_t1 as f32),
                        speaker: speaker.map(str::to_string),
                        wall_start: Some(wall_at(session_anchor, seg_t0)),
                        wall_end: Some(wall_at(session_anchor, seg_t1)),
                    }
                    .json(),
                )
                .await;

                // 音频留存(尽力):按 [seg_t0,seg_t1] 从会话 PCM 缓冲切片存 WAV。
                // 合并模式下即整条链区间(同 id 重切覆盖)。16k mono s16le =>
                // 32000 B/s;字节对齐到采样边界。
                {
                    const BPS: f64 = 16000.0 * 2.0;
                    let a = (((seg_t0 * BPS) as usize) / 2) * 2;
                    let b = ((seg_t1 * BPS).ceil() as usize).div_ceil(2) * 2;
                    let wav = pcm_buf.lock().ok().and_then(|buf| buf.slice_wav(a, b));
                    if let Some(w) = wav {
                        db.audio_put(id as i64, &w);
                    }
                }

                // optimize + translate run concurrently in a detached task so
                // segment N+1 is forwarded without waiting on N's LLM. Results
                // are keyed by `ref` id, so out-of-order arrival is fine.
                // 合并模式 + 次模型开启时,主模型润色推迟到 `secondary` 事件,
                // 届时用主链+次链双候选一次性润色(见下方 secondary 分支),避免
                // "先按主模型润一版、次模型来了再润一版"的翻倍调用与闪烁覆盖。
                // 翻译只需主模型,仍在此处随段触发。
                let defer_opt_to_secondary = merge_on && hello.want_secondary;
                let do_opt_here = hello.want_optimize && !defer_opt_to_secondary;
                if do_opt_here || hello.want_translate {
                    let (base, model) = resolve_llm_endpoint(toolkit_pool.as_ref(), &db, &c);
                    let opt_sys = do_opt_here.then(|| {
                        let tmpl = resolve_prompt_text(
                            toolkit_pool.as_ref(),
                            &db,
                            PROMPT_NAME_OPTIMIZE,
                            "llm.optimize_prompt",
                            DEFAULT_OPTIMIZE_PROMPT,
                        );
                        optimize_prompt_with_hotwords(tmpl, &resolve_hotwords(&db))
                    });
                    let tr_sys = hello.want_translate.then(|| {
                        resolve_prompt_text(
                            toolkit_pool.as_ref(),
                            &db,
                            PROMPT_NAME_TRANSLATE,
                            "llm.translate_prompt",
                            DEFAULT_TRANSLATE_PROMPT,
                        )
                    });
                    // 合并模式以整条链原始文本为输入,不注入历史上下文(链本身即
                    // 上下文,且不喂回润色结果,从根上断开滚雪球);关闭时沿用
                    // "近 20s 历史优化文本"作上下文。
                    let ctx_texts = if hello.want_optimize && !merge_on {
                        db.segments_context_before(&session_id, t0, 20.0)
                    } else {
                        Vec::new()
                    };
                    // 润色/翻译的输入:合并模式=整条链原始文本,否则=本段文本。
                    let prim = surface_text.clone();
                    let prim_len = prim.chars().count();
                    let db2 = db.clone();
                    let tx2 = cli_tx.clone();
                    let llm_ctx = stream_ctx.clone();
                    let opt_emitted2 = opt_emitted.clone();
                    let tr_emitted2 = tr_emitted.clone();
                    llm_tasks.push(tokio::spawn(async move {
                        let opt_user = build_optimize_user_msg(&ctx_texts, &prim, None);
                        let opt_fut = async {
                            match &opt_sys {
                                // 节 B：润色失败/超时回发原文 + status:"fallback"，让该段
                                // 从"优化中"落定并明确告知客户端是降级（trace 里 llm() 已记 Err）。
                                Some(s) => Some(
                                    match llm(&base, &model, s, &opt_user, Some(&llm_ctx)).await {
                                        Ok(t) => (t, false),
                                        Err(e) => {
                                            tracing::warn!(
                                                "[orch] optimize failed for seg {id}, fallback to raw: {e}"
                                            );
                                            (prim.clone(), true)
                                        }
                                    },
                                ),
                                None => None,
                            }
                        };
                        let tr_fut = async {
                            match &tr_sys {
                                Some(s) => llm(&base, &model, s, &prim, Some(&llm_ctx)).await.ok(),
                                None => None,
                            }
                        };
                        let (opt, en) = tokio::join!(opt_fut, tr_fut);
                        // 合并模式下放行 latest-wins:只有"输入不短于已发出"才落库/回发,
                        // 防并发的较短旧结果覆盖较长新结果。非合并模式每 id 唯一,直接放行。
                        if let Some((opt_text, fallback)) = opt {
                            let pass = !merge_on || {
                                let mut g = opt_emitted2.lock().unwrap();
                                if prim_len >= g.get(&id).copied().unwrap_or(0) {
                                    g.insert(id, prim_len);
                                    true
                                } else {
                                    false
                                }
                            };
                            if pass {
                                db2.segment_set_optimized(id as i64, &opt_text);
                                let status = if fallback { Some("fallback".into()) } else { None };
                                send(
                                    &tx2,
                                    ServerEvent::Optimized { r#ref: id, text: opt_text, status }.json(),
                                )
                                .await;
                            }
                        }
                        if let Some(en) = en {
                            let pass = !merge_on || {
                                let mut g = tr_emitted2.lock().unwrap();
                                if prim_len >= g.get(&id).copied().unwrap_or(0) {
                                    g.insert(id, prim_len);
                                    true
                                } else {
                                    false
                                }
                            };
                            if pass {
                                db2.segment_set_english(id as i64, &en);
                                send(&tx2, ServerEvent::Translated { r#ref: id, text: en }.json()).await;
                            }
                        }
                    }));
                }
            }
            Some("secondary") => {
                // Pair the secondary recognition back to its primary segment /
                // chain id via (t_start,t_end). If asr races ahead of orchestrator
                // (shouldn't happen — primary always emitted first) we just
                // drop the event rather than guess.
                let t0 = v.get("t_start").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let t1 = v.get("t_end").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let text = v
                    .get("text")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let kind = v.get("kind").and_then(|x| x.as_str()).map(str::to_string);
                let Some(&seg_id) = seg_by_time.get(&time_key(t0, t1)) else {
                    tracing::warn!(
                        "[orch] secondary without matching segment t=[{:.3},{:.3}]",
                        t0,
                        t1
                    );
                    continue;
                };
                // Comparison done for this window — free the map entry.
                seg_by_time.remove(&time_key(t0, t1));

                // 合并模式:按链累积次模型文本后整体展示(客户端整段替换);不跑
                // re-polish(逐段双候选与整链语义冲突)。非合并模式:逐段展示 + 可选
                // 双候选 re-polish(旧行为)。
                if merge_on {
                    let acc = sec_accum.entry(seg_id).or_default();
                    acc.push_str(&text);
                    let full = acc.clone();
                    db.segment_set_secondary(seg_id as i64, &full);
                    send(
                        &cli_tx,
                        ServerEvent::Secondary {
                            r#ref: seg_id,
                            text: full.clone(),
                            kind: kind.clone(),
                        }
                        .json(),
                    )
                    .await;

                    // 合并模式下主模型润色被推迟到此处:取该链当前累积的主链文本 +
                    // 次链文本,以双候选(【模型A】/【模型B】)一次性整链润色。沿用合并
                    // 模式"不喂历史上文"原则(链本身即上下文,且不喂回润色结果防滚雪球)。
                    // 用 opt_emitted 按主链字符数 latest-wins 防并发乱序覆盖,与主路径
                    // 共用同一守卫(两路都以主链长度为闸,单调递增)。
                    if hello.want_optimize {
                        if let Some(seg) = db.segment_get(seg_id as i64) {
                            let prim = seg.text;
                            let prim_len = prim.chars().count();
                            if prim_len > 0 {
                                let sys = {
                                    let tmpl = resolve_prompt_text(
                                        toolkit_pool.as_ref(),
                                        &db,
                                        PROMPT_NAME_OPTIMIZE,
                                        "llm.optimize_prompt",
                                        DEFAULT_OPTIMIZE_PROMPT,
                                    );
                                    optimize_prompt_with_hotwords(tmpl, &resolve_hotwords(&db))
                                };
                                let pname = db
                                    .config_get("asr.model")
                                    .unwrap_or_else(|| "主模型".into());
                                let sname = kind.clone().unwrap_or_else(|| "次模型".into());
                                let user_msg = build_optimize_user_msg(
                                    &[],
                                    &prim,
                                    Some((&pname, &sname, &full)),
                                );
                                let (base_url, model) =
                                    resolve_llm_endpoint(toolkit_pool.as_ref(), &db, &c);
                                let db4 = db.clone();
                                let tx4 = cli_tx.clone();
                                let llm_ctx = stream_ctx.clone();
                                let opt_emitted4 = opt_emitted.clone();
                                llm_tasks.push(tokio::spawn(async move {
                                    // 合并模式润色推迟到此处:失败/超时回发原文 + fallback 标记,
                                    // 让该链客户端落定并明确告知降级（trace 里 llm() 已记 Err）。
                                    let (opt, fallback) = match llm(
                                        &base_url, &model, &sys, &user_msg, Some(&llm_ctx),
                                    )
                                    .await
                                    {
                                        Ok(t) => (t, false),
                                        Err(e) => {
                                            tracing::warn!(
                                                "[orch] merge optimize failed for chain {seg_id}, fallback to raw: {e}"
                                            );
                                            (prim.clone(), true)
                                        }
                                    };
                                    let pass = {
                                        let mut g = opt_emitted4.lock().unwrap();
                                        if prim_len >= g.get(&seg_id).copied().unwrap_or(0) {
                                            g.insert(seg_id, prim_len);
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if pass {
                                        db4.segment_set_optimized(seg_id as i64, &opt);
                                        let status = if fallback {
                                            Some("fallback".into())
                                        } else {
                                            None
                                        };
                                        send(
                                            &tx4,
                                            ServerEvent::Optimized {
                                                r#ref: seg_id,
                                                text: opt,
                                                status,
                                            }
                                            .json(),
                                        )
                                        .await;
                                    }
                                }));
                            }
                        }
                    }
                    continue;
                }

                db.segment_set_secondary(seg_id as i64, &text);
                send(
                    &cli_tx,
                    ServerEvent::Secondary {
                        r#ref: seg_id,
                        text: text.clone(),
                        kind: kind.clone(),
                    }
                    .json(),
                )
                .await;

                // 若开启润色,且次模型结果与主模型不同,则以主+次双候选触发 re-polish。
                // 客户端收到第二个 Optimized{ref} 时会覆盖更新,体验与首次优化一致。
                if hello.want_optimize && !text.is_empty() {
                    if let Some(seg) = db.segment_get(seg_id as i64) {
                        if seg.text != text {
                            let ctx_texts = db.segments_context_before(&session_id, t0, 20.0);
                            let sys = {
                                let tmpl = resolve_prompt_text(
                                    toolkit_pool.as_ref(),
                                    &db,
                                    PROMPT_NAME_OPTIMIZE,
                                    "llm.optimize_prompt",
                                    DEFAULT_OPTIMIZE_PROMPT,
                                );
                                optimize_prompt_with_hotwords(tmpl, &resolve_hotwords(&db))
                            };
                            let pname = db
                                .config_get("asr.model")
                                .unwrap_or_else(|| "主模型".into());
                            let sname = kind.clone().unwrap_or_else(|| "次模型".into());
                            let user_msg = build_optimize_user_msg(
                                &ctx_texts,
                                &seg.text,
                                Some((&pname, &sname, &text)),
                            );
                            let (base_url, model) =
                                resolve_llm_endpoint(toolkit_pool.as_ref(), &db, &c);
                            let db3 = db.clone();
                            let tx3 = cli_tx.clone();
                            let llm_ctx = stream_ctx.clone();
                            llm_tasks.push(tokio::spawn(async move {
                                // re-polish 仅在成功时覆盖；失败不发新事件，沿用上一版优化稿。
                                if let Ok(opt) =
                                    llm(&base_url, &model, &sys, &user_msg, Some(&llm_ctx)).await
                                {
                                    db3.segment_set_optimized(seg_id as i64, &opt);
                                    send(
                                        &tx3,
                                        ServerEvent::Optimized {
                                            r#ref: seg_id,
                                            text: opt,
                                            status: None,
                                        }
                                        .json(),
                                    )
                                    .await;
                                }
                            }));
                        }
                    }
                }
            }
            Some("error") => {
                let m = v
                    .get("message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("asr error");
                send(
                    &cli_tx,
                    ServerEvent::Error {
                        code: "asr".into(),
                        message: m.into(),
                        fatal: false,
                    }
                    .json(),
                )
                .await;
            }
            Some("done") => {
                // drain in-flight LLM tasks so optimized/translated all land
                // before Done.
                for h in llm_tasks.drain(..) {
                    let _ = h.await;
                }
                send(&cli_tx, ServerEvent::Done { session_id }.json()).await;
                return seg_count;
            }
            _ => {}
        }
    }
    for h in llm_tasks.drain(..) {
        let _ = h.await;
    }
    seg_count
}

// ── Web 管理台 HTTP API ──────────────────────────────────────────────────

async fn console() -> Html<&'static str> {
    Html(CONSOLE_HTML)
}
async fn api_stats(State(ctx): State<AppCtx>) -> Json<db::Stats> {
    Json(ctx.db.stats())
}
async fn api_history(
    State(ctx): State<AppCtx>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Vec<db::SegmentRow>> {
    let limit = q
        .get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(200)
        .clamp(1, 500);
    Json(ctx.db.segments_recent(limit))
}
async fn api_speakers(State(ctx): State<AppCtx>) -> Json<Vec<db::Speaker>> {
    Json(ctx.db.speakers_list())
}
/// Runtime-tunable asr config the asr service polls (threshold/gap).
async fn api_asr_config(State(ctx): State<AppCtx>) -> Json<serde_json::Value> {
    let f = |k: &str, d: f64| -> f64 {
        ctx.db
            .config_get(k)
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let model = ctx
        .db
        .config_get("asr.model")
        .unwrap_or_else(|| "paraformer".into());
    let secondary_model = ctx.db.config_get("asr.secondary_model").unwrap_or_default();
    let gate_to_enrolled = ctx
        .db
        .config_get("asr.gate_to_enrolled")
        .unwrap_or_else(|| "on".into());
    let hotwords = ctx.db.config_get("asr.hotwords").unwrap_or_default();
    Json(json!({
        "spk_threshold": f("asr.spk_threshold", 0.35),
        "sentence_gap_ms": f("asr.sentence_gap_ms", 1500.0) as i64,
        "model": model,
        "secondary_model": secondary_model,
        "gate_to_enrolled": gate_to_enrolled,
        "hotwords": hotwords,
    }))
}

/// Enabled voiceprints for the asr service to pull (gating source of truth).
async fn api_voiceprints(State(ctx): State<AppCtx>) -> Json<serde_json::Value> {
    let vps: Vec<serde_json::Value> = ctx
        .db
        .enabled_voiceprints()
        .into_iter()
        .map(|(name, emb)| json!({ "name": name, "embedding": emb }))
        .collect();
    Json(serde_json::Value::Array(vps))
}

/// Enroll: `?name=` + raw audio body -> asr /embed -> store voiceprint.
async fn api_speaker_enroll(
    State(ctx): State<AppCtx>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Json<serde_json::Value> {
    let name = q.get("name").cloned().unwrap_or_default();
    if name.trim().is_empty() {
        return Json(json!({"ok": false, "error": "缺少名称"}));
    }
    let resp = http_client()
        .post(&ctx.cfg.asr_embed)
        .body(body.to_vec())
        .send()
        .await;
    let j = match resp {
        Ok(r) => match r.json::<serde_json::Value>().await {
            Ok(j) => j,
            Err(e) => return Json(json!({"ok": false, "error": format!("embed 解析失败: {e}")})),
        },
        Err(e) => return Json(json!({"ok": false, "error": format!("asr 不可达: {e}")})),
    };
    let emb: Vec<f32> = match j.get("embedding").and_then(|x| x.as_array()) {
        Some(a) => a
            .iter()
            .filter_map(|x| x.as_f64())
            .map(|x| x as f32)
            .collect(),
        None => {
            let e = j
                .get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("embed 失败");
            return Json(json!({"ok": false, "error": e}));
        }
    };
    if emb.is_empty() {
        return Json(json!({"ok": false, "error": "空声纹向量"}));
    }
    let csv = emb
        .iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(",");
    match ctx.db.speaker_add(&name, &csv) {
        Ok(id) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": format!("保存失败(名称重复?): {e}")})),
    }
}

async fn api_speaker_delete(
    State(ctx): State<AppCtx>,
    Path(id): Path<i64>,
) -> Json<serde_json::Value> {
    ctx.db.speaker_delete(id);
    Json(json!({"ok": true}))
}
async fn api_speaker_rename(
    State(ctx): State<AppCtx>,
    Path(id): Path<i64>,
    Json(b): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    if let Some(n) = b.get("name").and_then(|x| x.as_str()) {
        ctx.db.speaker_rename(id, n);
    }
    Json(json!({"ok": true}))
}
async fn api_speaker_enabled(
    State(ctx): State<AppCtx>,
    Path(id): Path<i64>,
    Json(b): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let e = b.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true);
    ctx.db.speaker_set_enabled(id, e);
    Json(json!({"ok": true}))
}

/// Download/play a segment's retained audio (WAV). 404 once purged (>1 day).
async fn api_segment_audio(State(ctx): State<AppCtx>, Path(id): Path<i64>) -> Response {
    match ctx.db.audio_get(id) {
        Some(wav) => (
            [
                (header::CONTENT_TYPE, "audio/wav".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"seg-{id}.wav\""),
                ),
            ],
            wav,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "audio expired or not found").into_response(),
    }
}

/// Correct a segment's text (builds a corrected (audio,text) sample).
async fn api_segment_set_text(
    State(ctx): State<AppCtx>,
    Path(id): Path<i64>,
    Json(b): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    match b.get("text").and_then(|x| x.as_str()) {
        Some(t) => Json(json!({"ok": ctx.db.segment_set_text(id, t)})),
        None => Json(json!({"ok": false, "error": "缺少 text"})),
    }
}

/// Fetch a single segment as JSON (used by the standalone /segment/:id page).
async fn api_segment_get(State(ctx): State<AppCtx>, Path(id): Path<i64>) -> Response {
    match ctx.db.segment_get(id) {
        Some(row) => Json(row).into_response(),
        None => (StatusCode::NOT_FOUND, "segment not found").into_response(),
    }
}

/// Delete a segment (and its retained audio).
async fn api_segment_delete(
    State(ctx): State<AppCtx>,
    Path(id): Path<i64>,
) -> Json<serde_json::Value> {
    let ok = ctx.db.segment_delete(id);
    Json(json!({"ok": ok}))
}

/// Wipe ALL segment history (records + retained audio). Sessions are kept.
/// Destructive — confirmed by the UI before this is called.
async fn api_segments_clear(State(ctx): State<AppCtx>) -> Json<serde_json::Value> {
    let removed = ctx.db.segments_clear_all();
    tracing::warn!("[history] cleared {removed} segment row(s) via /api/segments DELETE");
    Json(json!({"ok": true, "removed": removed}))
}

/// Re-run optimize + translate for an existing segment, using its current
/// `text` and the latest DB-configured prompts/vLLM endpoint. Returns the
/// updated row so the UI can refresh in-place.
async fn api_segment_rerun(
    State(ctx): State<AppCtx>,
    Path(id): Path<i64>,
) -> Json<serde_json::Value> {
    let Some(row) = ctx.db.segment_get(id) else {
        return Json(json!({"ok": false, "error": "segment not found"}));
    };
    let text = row.text.clone();
    let (base, model) = resolve_llm_endpoint(ctx.toolkit_pool.as_ref(), &ctx.db, &ctx.cfg);
    let opt_sys = {
        let tmpl = resolve_prompt_text(
            ctx.toolkit_pool.as_ref(),
            &ctx.db,
            PROMPT_NAME_OPTIMIZE,
            "llm.optimize_prompt",
            DEFAULT_OPTIMIZE_PROMPT,
        );
        optimize_prompt_with_hotwords(tmpl, &resolve_hotwords(&ctx.db))
    };
    let tr_sys = resolve_prompt_text(
        ctx.toolkit_pool.as_ref(),
        &ctx.db,
        PROMPT_NAME_TRANSLATE,
        "llm.translate_prompt",
        DEFAULT_TRANSLATE_PROMPT,
    );
    // 管理台触发的 rerun 没有客户端 trace 上下文;不挂 trace 即可(None=不记 span)。
    let opt_fut = llm(&base, &model, &opt_sys, &text, None);
    let tr_fut = llm(&base, &model, &tr_sys, &text, None);
    let (opt_res, tr_res) = tokio::join!(opt_fut, tr_fut);
    let mut errs: Vec<String> = Vec::new();
    let optimized = match opt_res {
        Ok(s) => {
            ctx.db.segment_set_optimized(id, &s);
            Some(s)
        }
        Err(e) => {
            errs.push(format!("optimize: {e}"));
            None
        }
    };
    let english = match tr_res {
        Ok(s) => {
            ctx.db.segment_set_english(id, &s);
            Some(s)
        }
        Err(e) => {
            errs.push(format!("translate: {e}"));
            None
        }
    };
    Json(json!({
        "ok": errs.is_empty(),
        "error": if errs.is_empty() { None } else { Some(errs.join("; ")) },
        "optimized": optimized,
        "english": english,
    }))
}

async fn api_config_get(State(ctx): State<AppCtx>) -> Json<serde_json::Value> {
    let m: serde_json::Map<String, serde_json::Value> = ctx
        .db
        .config_all()
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    Json(serde_json::Value::Object(m))
}
async fn api_config_set(
    State(ctx): State<AppCtx>,
    Json(b): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    if let Some(o) = b.as_object() {
        for (k, v) in o {
            if let Some(s) = v.as_str() {
                ctx.db.config_set(k, s);
            }
        }
    }
    Json(json!({"ok": true}))
}

const CONSOLE_HTML: &str = r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>语音服务管理台</title><style>
body{font-family:system-ui,Segoe UI,sans-serif;margin:0;background:#0f1115;color:#e6e6e6}
header{padding:14px 20px;background:#171a21;font-size:18px;font-weight:600}
nav{display:flex;gap:6px;padding:10px 20px;background:#13161c}
nav button{background:#222733;color:#cbd5e1;border:0;padding:8px 14px;border-radius:8px;cursor:pointer}
nav button.on{background:#2563eb;color:#fff}
main{padding:20px;max-width:1000px}
.card{background:#171a21;border:1px solid #232838;border-radius:12px;padding:16px;margin-bottom:14px}
table{width:100%;border-collapse:collapse;font-size:14px}
td,th{padding:8px;border-bottom:1px solid #232838;text-align:left;vertical-align:top}
button.s{padding:4px 10px;border-radius:6px;border:0;cursor:pointer;font-size:12px}
.del{background:#7f1d1d;color:#fff}.ren{background:#334155;color:#fff}
.kpi{display:flex;gap:24px}.kpi div{font-size:13px;color:#94a3b8}.kpi b{display:block;font-size:24px;color:#fff}
.note{color:#94a3b8;font-size:13px}
input,select,textarea{background:#0f1115;border:1px solid #334155;color:#e6e6e6;border-radius:6px;padding:6px;font:inherit;box-sizing:border-box}
textarea{width:100%;min-height:96px;line-height:1.5;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:13px;resize:vertical}
.cfg-grp{margin-top:8px}
.cfg-grp h3{margin:14px 0 6px;font-size:14px;color:#cbd5e1;border-bottom:1px solid #232838;padding-bottom:4px}
.cfg-row{display:grid;grid-template-columns:minmax(180px,220px) 1fr auto;gap:14px;align-items:start;padding:10px 0;border-bottom:1px solid #1f2330}
.cfg-row:last-child{border-bottom:0}
.cfg-lbl b{display:block;color:#fff;font-size:13px}
.cfg-lbl code{font-size:11px;color:#94a3b8;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
.cfg-lbl .hint{display:block;font-size:12px;color:#94a3b8;margin-top:4px;line-height:1.5}
.cfg-ctl input[type=text],.cfg-ctl select{width:100%}
.cfg-ctl .row{display:flex;gap:8px;align-items:center}
.cfg-save{align-self:start}
/* history tab — stack original/optimized/english vertically so long text wraps */
.seg{padding:14px 0;border-bottom:1px solid #1f2330}
.seg:last-child{border-bottom:0}
.seg-hd{display:flex;align-items:center;gap:10px;flex-wrap:wrap;font-size:12px;color:#94a3b8;margin-bottom:8px}
.seg-hd .id{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;color:#64748b}
.seg-hd .sp{color:#cbd5e1;background:#1f2330;padding:2px 8px;border-radius:6px}
.seg-hd .spacer{flex:1}
.seg-fld{display:grid;grid-template-columns:60px 1fr;gap:8px 12px;margin-bottom:4px;align-items:start}
.seg-fld .k{color:#94a3b8;font-size:12px;padding-top:6px}
.seg-fld textarea,.seg-fld input{width:100%;box-sizing:border-box}
.seg-fld textarea{min-height:46px}
.seg-fld .raw{background:#0d1626;border-color:#1e3a5f}
.seg-fld .opt{color:#e6e6e6}
.seg-fld .en{color:#94a3b8;font-style:italic}
.seg-acts{display:flex;gap:8px;flex-wrap:wrap;margin-top:6px}
.seg-acts .right{margin-left:auto;display:flex;gap:8px}
.hist-hd{display:flex;align-items:start;gap:16px;justify-content:space-between;margin-bottom:10px}
.hist-hd .meta{flex:1;font-size:12px;color:#94a3b8;line-height:1.6}
.hist-hd .meta code{color:#cbd5e1;background:#1f2330;padding:1px 6px;border-radius:4px;font-size:11px}
.seg-hd .id a{color:#64748b;text-decoration:none}
.seg-hd .id a:hover{color:#cbd5e1;text-decoration:underline}
.single-wrap{max-width:820px;margin:0 auto}
.single-wrap .back{display:inline-block;margin-bottom:14px;text-decoration:none}
</style></head><body>
<header>语音服务管理台</header>
<nav><button class=on data-t=ov>概览</button><button data-t=hi>历史</button>
<button data-t=sp>声纹</button><button data-t=cf>配置</button></nav>
<main><div id=v></div></main>
<script>
const V=document.getElementById('v');let tab='ov';
const NAV=document.querySelector('nav');
document.querySelectorAll('nav button').forEach(b=>b.onclick=()=>{
 // When entering the standalone /segment/:id page, the nav buttons should
 // bounce back to "/" with the desired tab active; on the main console they
 // just switch tabs in-place.
 if(window.__singleMode){location.href='/#'+b.dataset.t;return}
 document.querySelectorAll('nav button').forEach(x=>x.classList.remove('on'));
 b.classList.add('on');tab=b.dataset.t;render()});
async function j(u,m,bd){const o={method:m||'GET'};if(bd){o.headers={'content-type':'application/json'};o.body=JSON.stringify(bd)}return (await fetch(u,o)).json()}
async function render(){
 if(tab=='ov'){const s=await j('/api/stats');
  V.innerHTML=`<div class=card><div class=kpi>
  <div>会话数<b>${s.sessions}</b></div><div>识别段<b>${s.segments}</b></div>
  <div>累计录音<b>${(s.total_recording_sec/60).toFixed(1)}分</b></div>
  <div>今日录音<b>${(s.today_recording_sec/60).toFixed(1)}分</b></div></div></div>`}
 else if(tab=='hi'){const h=await j('/api/history');renderHistory(h)}
 else if(tab=='sp'){const sp=await j('/api/speakers');const ac=await j('/api/asr-config');
  const gv=String(ac.gate_to_enrolled==null?'on':ac.gate_to_enrolled).toLowerCase();
  const gate=!(gv==='off'||gv==='0'||gv==='false'||gv==='no');
  V.innerHTML='<div class=card>'+
  `<p><label style="cursor:pointer"><input type=checkbox ${gate?'checked':''} onchange="setGate(this.checked)"> <b>仅识别声纹列表中已启用的用户</b></label></p>`+
  '<p class=note>勾选=只识别命中已启用声纹的语音,其余丢弃;取消=识别所有人(命中已启用声纹时仍标注说话人)。未注册任何声纹时恒等于识别所有人。</p>'+
  '<p class=note>注册:点"录制注册"→对麦克风清晰说约5秒,或上传音频。</p>'+
  '<p>注册名:<input id=enm placeholder="如:张三"> '+
  '<button class="s ren" onclick="enrollFile()">⬆ 上传音频注册</button> '+
  '<input type=file id=enf accept="audio/*" style="display:none">'+
  '<button class="s ren" onclick="enroll()">● 录制注册(需HTTPS)</button> '+
  '<span id=est></span></p>'+
  '<table><tr><th>名称</th><th>启用</th><th>创建</th><th></th></tr>'+
  sp.map(s=>`<tr><td>${esc(s.name)}</td><td><input type=checkbox ${s.enabled?'checked':''} onchange="en(${s.id},this.checked)"></td><td>${s.created_at}</td>
  <td><button class="s ren" onclick="rn(${s.id})">改名</button> <button class="s del" onclick="dl(${s.id})">删除</button></td></tr>`).join('')+'</table></div>'}
 else if(tab=='cf'){const c=await j('/api/config');renderConfig(c)}
}
const CFG_META={
 'asr.model':{label:'ASR 模型',group:'ASR',kind:'select',options:['paraformer','sensevoice','whisper-turbo','whisper-large-v3'],
  hint:'识别后端模型。默认 sensevoice(中英文混合:中文为主、能正确转出英文术语,低延迟适合流式);paraformer 纯中文优先(英文会被音译);whisper-turbo/whisper-large-v3 多语种自动识别(turbo 更快、large-v3 更准,延迟较高)。修改后 ASR 服务在 ~15s 内热切换(无需重启;切换期间在跑的那段会用旧模型完成)。首次切到 Whisper 会加载权重,耗时稍长。'},
 'asr.secondary_model':{label:'次模型(对比)',group:'ASR',kind:'select',options:['','paraformer','sensevoice','whisper-turbo','whisper-large-v3'],
  hint:'仅在桌面端打开「次模型对比」开关时生效。空=禁用。与主模型重复会被自动跳过。次模型只跑识别(不参与润色/翻译),用于对比中文识别能力。首次有会话启用时才会真正加载权重。'},
 'asr.spk_threshold':{label:'声纹匹配阈值',group:'ASR',
  hint:'0~1 的相似度门槛。值越高越严格(误收他人↓、漏收自己↑)。常用 0.30~0.45。ASR 服务每 15s 轮询热更新,无需重启。'},
 'asr.sentence_gap_ms':{label:'句子切分静音 (毫秒)',group:'ASR',
  hint:'连续静音超过该时长视为一句结束并下发。1000~2000 为常用区间。无需重启,~15s 内生效。'},
 'asr.gate_to_enrolled':{label:'声纹门控',group:'ASR',kind:'select',options:['on','off'],
  hint:'on = 只识别已在「声纹」tab 启用的声纹的语音段,其余直接丢弃;off = 识别所有人,命中已启用声纹时仍标注说话人。未注册任何声纹时,两种设置都等同于「识别所有人」。无需重启,~15s 内生效。'},
 'asr.hotwords':{label:'领域热词',group:'ASR',kind:'textarea',
  hint:'每行一个词,可选「词 权重」形式(权重 ≥1.0,默认 1.0)。例:\n会话\n复盘 2.0\n请求头\n双向喂入:(a) ASR 声学层 — Paraformer 走 hotword=,Whisper 拼到 initial_prompt,SenseVoice 不支持热词会自动跳过;(b) LLM 润色 system prompt 末尾(SenseVoice 的兜底)。改完 ASR 服务 ~15s 内热生效,LLM 在处理下一条新分段时读取。'},
 'vllm.base':{label:'vLLM 服务地址',group:'LLM',
  hint:'OpenAI 兼容根地址(以 /v1 结尾)。容器内访问主机服务请用 host.docker.internal,例如 http://host.docker.internal:12340/v1。改完下一条新分段就用新值,无需重启。'},
 'vllm.model':{label:'vLLM 模型名',group:'LLM',
  hint:'必须与 vLLM 启动时 --served-model-name 一致,例如 gemma-4-26B-A4B-it。'},
 'llm.optimize_prompt':{label:'中文润色提示词',group:'LLM',kind:'textarea',
  hint:'system 角色提示词。把原始口语转写整理成通顺书面中文。改完下一条新分段生效。'},
 'llm.translate_prompt':{label:'英文翻译提示词',group:'LLM',kind:'textarea',
  hint:'system 角色提示词。把整理后的中文翻成自然英文。改完下一条新分段生效。'},
};
const CFG_GROUPS=['ASR','LLM','其他'];
function renderConfig(c){
 const ks=Object.keys(c).sort();
 if(!ks.length){V.innerHTML='<div class=card><p class=note>暂无配置项。</p></div>';return}
 const buckets={};ks.forEach(k=>{const g=(CFG_META[k]&&CFG_META[k].group)||'其他';(buckets[g]=buckets[g]||[]).push(k)});
 let html='<div class=card>'+
  '<p class=note>所有配置项即时生效,无需重启:asr.* 由 ASR 服务每 15s 轮询;vllm.* / llm.* 在处理下一条分段时读取。</p>';
 CFG_GROUPS.forEach(g=>{
  const list=buckets[g];if(!list||!list.length)return;
  html+='<div class=cfg-grp><h3>'+esc(g)+'</h3>';
  list.forEach(k=>{html+=renderCfgRow(k,c[k])});
  html+='</div>';
 });
 html+='</div>';V.innerHTML=html;
}
function renderCfgRow(k,v){
 const meta=CFG_META[k]||{};const kind=meta.kind||'text';const id='cf_'+cssId(k);
 const lbl=meta.label?`<b>${esc(meta.label)}</b><code>${esc(k)}</code>`:`<b>${esc(k)}</b>`;
 const hint=meta.hint?`<span class=hint>${esc(meta.hint)}</span>`:'';
 let ctl;
 if(kind==='textarea'){
  ctl=`<textarea id="${id}" rows=5>${esc(v||'')}</textarea>`;
 }else if(kind==='select'){
  const opts=(meta.options||[]).map(o=>`<option value="${esc(o)}"${o===v?' selected':''}>${esc(o)}</option>`).join('');
  ctl=`<select id="${id}">${opts}</select>`;
 }else{
  ctl=`<input id="${id}" type=text value="${escA(v||'')}">`;
 }
 return `<div class=cfg-row><div class=cfg-lbl>${lbl}${hint}</div><div class=cfg-ctl>${ctl}</div><button class="s ren cfg-save" onclick="cs('${jsKey(k)}')">保存</button></div>`;
}
function cssId(k){return k.replace(/[^a-zA-Z0-9_-]/g,'_')}
function jsKey(k){return k.replace(/'/g,"\\'")}
function renderHistory(h){
 const headHtml=
  '<div class=hist-hd>'+
   '<div class=meta>'+
    '<div>音频保留 1 天:▶ 试听、⬇ 下载(可作声纹注册输入)。改「原文」后点「保存原文」生成纠错样本;「重新优化/翻译」会按当前原文 + 配置中的提示词重跑 LLM,结果立刻覆盖优化和英文两栏。「单独打开」会在新标签页里展示这一条,带音频播放器,适合细看或保存外链。</div>'+
    '<div style="margin-top:6px">存储:GB10 容器内 <code>SQLite</code>(<code>/data/app.db</code>,挂载到 Docker volume <code>orch-data</code>)。文本表 <code>segments</code> 永久保存,音频表 <code>segment_audio</code> 每小时清理 1 天前的 blob,会话表 <code>sessions</code> 仅用于"录音时长"统计。</div>'+
   '</div>'+
   '<div><button class="s del" onclick="clearAllHistory()" title="删除所有历史记录(不可恢复)">清空全部历史</button></div>'+
  '</div>';
 if(!h||!h.length){V.innerHTML='<div class=card>'+headHtml+'<p class=note style="margin-top:14px">暂无历史记录。</p></div>';return}
 V.innerHTML='<div class=card>'+headHtml+
  '<audio id=hap controls style="width:100%;margin:10px 0;display:none"></audio>'+
  h.map(r=>renderSeg(r)).join('')+
  '</div>';
}
async function clearAllHistory(){
 if(!confirm('确认清空全部历史?\n\n会删除所有识别记录(原文/优化/翻译)和保留的音频。\n会话时长统计不受影响。\n此操作不可恢复。'))return;
 if(!confirm('再确认一次:真的要清空全部历史吗?'))return;
 const d=await j('/api/segments','DELETE');
 if(d.ok){alert('已清空 '+d.removed+' 条记录');render()}
 else{alert('清空失败')}
}
function renderSeg(r,opts){
 opts=opts||{};
 const audioBtn=r.has_audio
  ?`<button class="s ren" onclick="playSeg(${r.id})">▶ 试听</button> <a class="s ren" style="text-decoration:none;display:inline-block" href="/api/segments/${r.id}/audio">⬇ 下载</a>`
  :'<span class=note>(音频已过期)</span>';
 const openLink=opts.hideOpen?''
  :`<a class="s ren" style="text-decoration:none;display:inline-block" href="/segment/${r.id}" target="_blank" rel="noopener" title="在新标签页单独打开此条记录">↗ 单独打开</a>`;
 const idHtml=opts.hideOpen?`<span class=id>#${r.id}</span>`
  :`<span class=id><a href="/segment/${r.id}" target="_blank" rel="noopener" title="在新标签页打开">#${r.id}</a></span>`;
 return `<div class=seg id=seg_${r.id}>
  <div class=seg-hd>
   ${idHtml}
   <span>${esc(r.ts)}</span>
   ${r.speaker?`<span class=sp>${esc(r.speaker)}</span>`:''}
   <span class=spacer></span>
   ${audioBtn}
  </div>
  <div class=seg-fld><div class=k>原文</div><textarea id="tx_${r.id}" class=raw rows=2>${esc(r.text||'')}</textarea></div>
  <div class=seg-fld><div class=k>优化</div><div id="opt_${r.id}" class=opt>${esc(r.optimized||'')||'<span class=note>(尚未优化)</span>'}</div></div>
  <div class=seg-fld><div class=k>英文</div><div id="en_${r.id}" class=en>${esc(r.english||'')||'<span class=note>(尚未翻译)</span>'}</div></div>
  <div class=seg-acts>
   <button class="s ren" onclick="saveSeg(${r.id})">保存原文</button>
   <button class="s ren" onclick="rerunSeg(${r.id},this)">重新优化/翻译</button>
   ${openLink}
   <div class=right><button class="s del" onclick="delSeg(${r.id},${opts.singleMode?'true':'false'})">删除</button></div>
  </div>
 </div>`;
}
async function rerunSeg(id,btn){
 const t=btn.textContent;btn.disabled=true;btn.textContent='处理中...';
 try{
  const d=await j('/api/segments/'+id+'/rerun','POST',{});
  if(d.optimized!=null){document.getElementById('opt_'+id).textContent=d.optimized}
  if(d.english!=null){document.getElementById('en_'+id).textContent=d.english}
  if(!d.ok){alert('LLM 部分失败:'+(d.error||'?'))}
 }catch(e){alert('请求失败:'+e)}
 finally{btn.disabled=false;btn.textContent=t}
}
async function delSeg(id,singleMode){
 if(!confirm('删除该条记录?(原文/优化/翻译及保留的音频都会删除,且不可恢复)'))return;
 const d=await j('/api/segments/'+id,'DELETE');
 if(d.ok){
  if(singleMode){location.href='/#hi';return}
  const el=document.getElementById('seg_'+id);if(el)el.remove();
 }else{alert('删除失败')}
}
function esc(s){return (s+'').replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]))}
function escA(s){return esc(s).replace(/"/g,'&quot;')}
async function saveSeg(id){const t=document.getElementById('tx_'+id).value;const d=await j('/api/segments/'+id+'/text','POST',{text:t});alert(d.ok?'已保存':'保存失败:'+(d.error||'?'))}
function playSeg(id){const a=document.getElementById('hap');a.style.display='block';a.src='/api/segments/'+id+'/audio';a.play()}
async function dl(id){if(confirm('删除该声纹?')){await j('/api/speakers/'+id,'DELETE');render()}}
async function rn(id){const n=prompt('新名称');if(n){await j('/api/speakers/'+id+'/rename','POST',{name:n});render()}}
async function en(id,e){await j('/api/speakers/'+id+'/enabled','POST',{enabled:e})}
async function cs(k){const el=document.getElementById('cf_'+cssId(k));if(!el){alert('找不到输入框: '+k);return}await j('/api/config','POST',{[k]:el.value});const b=event&&event.target;if(b){const t=b.textContent;b.textContent='已保存';setTimeout(()=>{b.textContent=t},900)}else{alert('已保存')}}
async function setGate(on){await j('/api/config','POST',{'asr.gate_to_enrolled':on?'on':'off'});render()}
function enName(){const n=(document.getElementById('enm')||{}).value;return (n||'').trim()}
async function doEnroll(blob,name){
 const est=document.getElementById('est');est.textContent='上传中...';
 const r=await fetch('/api/speakers/enroll?name='+encodeURIComponent(name),{method:'POST',body:blob});
 const d=await r.json();est.textContent='';
 if(d.ok){alert('注册成功');render()}else{alert('注册失败:'+(d.error||'?'))}
}
function enrollFile(){
 const name=enName();if(!name){alert('请先填注册名');return}
 const f=document.getElementById('enf');
 f.onchange=()=>{if(f.files[0])doEnroll(f.files[0],name)};
 f.click();
}
async function enroll(){
 const name=enName();if(!name){alert('请先填注册名');return}
 if(!navigator.mediaDevices||!navigator.mediaDevices.getUserMedia){
  alert('浏览器麦克风需 HTTPS 或 localhost。请改用"上传音频注册",或用 HTTPS 访问。');return}
 const est=document.getElementById('est');
 let stream;try{stream=await navigator.mediaDevices.getUserMedia({audio:true})}catch(e){alert('无法访问麦克风:'+e);return}
 const mr=new MediaRecorder(stream);const chunks=[];
 mr.ondataavailable=e=>chunks.push(e.data);
 mr.onstop=()=>{stream.getTracks().forEach(t=>t.stop());doEnroll(new Blob(chunks),name)};
 mr.start();est.textContent='录音中…(5秒)';setTimeout(()=>mr.stop(),5000);
}
async function renderSingleSegment(id){
 window.__singleMode=true;
 // Hide tab nav since we're showing one row in isolation; tab buttons would
 // be misleading. Keep them visible-but-disabled for orientation? Just hide.
 if(NAV)NAV.style.display='none';
 let r;
 try{const resp=await fetch('/api/segments/'+id);
  if(!resp.ok){V.innerHTML='<div class="card single-wrap"><a class="s ren back" href="/">← 返回管理台</a><p class=note style="margin-top:14px">未找到 #'+id+' 的记录(可能已删除)。</p></div>';return}
  r=await resp.json();
 }catch(e){V.innerHTML='<div class="card single-wrap"><a class="s ren back" href="/">← 返回管理台</a><p class=note style="margin-top:14px">加载失败:'+esc(String(e))+'</p></div>';return}
 const audioSrc=r.has_audio?('/api/segments/'+r.id+'/audio'):'';
 V.innerHTML='<div class="single-wrap">'+
  '<a class="s ren back" href="/" style="text-decoration:none">← 返回管理台</a>'+
  '<div class=card>'+
   (audioSrc?'<audio id=hap controls autoplay style="width:100%;margin-bottom:14px" src="'+audioSrc+'"></audio>'
            :'<p class=note style="margin-bottom:10px">(原始音频已过期或未保存,只剩文本)</p>')+
   renderSeg(r,{singleMode:true,hideOpen:true})+
  '</div></div>';
}
function bootstrap(){
 const m=location.pathname.match(/^\/segment\/(\d+)\/?$/);
 if(m){renderSingleSegment(parseInt(m[1],10));return}
 // hash-based jump (e.g. coming back from a /segment page with /#hi)
 const h=(location.hash||'').replace('#','');
 if(h&&['ov','hi','sp','cf'].includes(h)){
  tab=h;document.querySelectorAll('nav button').forEach(b=>b.classList.toggle('on',b.dataset.t===h));
 }
 render();
}
bootstrap();
</script></body></html>"#;

/// Bounded session PCM buffer (16k mono s16le). Keeps only the most recent
/// ~3 min so a long session can't grow memory without limit; `base` is the
/// absolute byte offset of `data[0]`. Per-sentence segments finalize shortly
/// after speech, so their [t0,t1] window is always within this cap.
struct PcmBuf {
    data: Vec<u8>,
    base: usize,
}

impl PcmBuf {
    const CAP: usize = 180 * 16000 * 2; // ~180s of 16k mono s16le

    fn new() -> Self {
        Self {
            data: Vec::new(),
            base: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
        if self.data.len() > Self::CAP {
            let drop = self.data.len() - Self::CAP;
            self.data.drain(..drop);
            self.base += drop;
        }
    }

    fn clear(&mut self) {
        self.data.clear();
        self.base = 0;
    }

    /// WAV for the absolute byte range [a, b); None if it was already
    /// dropped (segment older than the retained window — best-effort).
    fn slice_wav(&self, a: usize, b: usize) -> Option<Vec<u8>> {
        if a < self.base {
            return None;
        }
        let la = a - self.base;
        let lb = (b - self.base).min(self.data.len());
        (la < lb).then(|| pcm16_to_wav(&self.data[la..lb]))
    }
}

/// Wrap 16 kHz mono s16le PCM in a canonical 44-byte WAV container so the
/// stored blob is directly playable in a browser and usable as enroll input.
fn pcm16_to_wav(pcm: &[u8]) -> Vec<u8> {
    const SR: u32 = 16000;
    let data_len = pcm.len() as u32;
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    w.extend_from_slice(&SR.to_le_bytes());
    w.extend_from_slice(&(SR * 2).to_le_bytes()); // byte rate = sr*ch*bytes
    w.extend_from_slice(&2u16.to_le_bytes()); // block align = ch*bytes
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    w.extend_from_slice(pcm);
    w
}

/// OpenAI 兼容 chat completions(指向主机上的 vLLM)。
/// base/model 由调用方从 DB config 解析(回退 env),提示词同理。
/// `ctx` Some 时记 `llm_call` span(含 request/response body);None 时纯执行。
/// 进程级共享 HTTP 客户端,**带超时**。`reqwest::Client::new()` 默认无超时:
/// 若 vLLM 连上却迟迟不返回,`send()/text()` 的 future 永不 resolve,润色任务
/// 永远 pending —— 客户端永久"优化中"、trace 也记不到这次调用(记录点在请求
/// 返回之后)。给所有出站 HTTP 设统一超时,把"卡死"转成可观测的 `Err`。
/// 详见 docs/todo-2026-06-18-optimize-hang-no-trace.md。
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // 润色/翻译是短输出(max_tokens=256),正常一两秒;取宽松值兜底卡死。
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("build reqwest client")
    })
}

async fn llm(
    base: &str,
    model: &str,
    sys: &str,
    user: &str,
    ctx: Option<&TraceContext>,
) -> anyhow::Result<String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": user}
        ],
        "temperature": 0.2,
        "max_tokens": 256,
        "stream": false
    });
    let traced = ctx.is_some() && trace::enabled();
    let start_ms = if traced { trace::now_ms() } else { 0 };
    let request_body = if traced {
        body.to_string()
    } else {
        String::new()
    };
    let result: anyhow::Result<(String, String)> = async {
        let raw = http_client()
            .post(format!("{}/chat/completions", base))
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&raw)?;
        let text = parsed["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        Ok((text, raw))
    }
    .await;
    match result {
        Ok((text, raw)) => {
            if traced {
                if let Some(c) = ctx {
                    trace::record_llm_call(LlmCall {
                        ctx: c.child(),
                        model: model.to_string(),
                        request_body,
                        response_body: raw,
                        start_ms,
                        end_ms: trace::now_ms(),
                        status: SpanStatus::Ok,
                    });
                }
            }
            Ok(text)
        }
        Err(e) => {
            if traced {
                if let Some(c) = ctx {
                    trace::record_llm_call(LlmCall {
                        ctx: c.child(),
                        model: model.to_string(),
                        request_body,
                        response_body: format!("ERROR: {e}"),
                        start_ms,
                        end_ms: trace::now_ms(),
                        status: SpanStatus::Error(e.to_string()),
                    });
                }
            }
            Err(e)
        }
    }
}

async fn send_fatal(sock: &mut WebSocket, code: &str, msg: &str) {
    let _ = sock
        .send(Message::Text(
            ServerEvent::Error {
                code: code.into(),
                message: msg.into(),
                fatal: true,
            }
            .json()
            .into(),
        ))
        .await;
}
