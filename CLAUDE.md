# CLAUDE.md

**toolkit 工具中台**（tools-server）：把 ASR / 抖音 / RAG / 长任务等基础能力集中到一个 Cargo
workspace，作为 zero/Agent 生态的统一工具底座。架构目标：
`wechat → Agent(zero) ⇄ llm(GB10) ⇄ tools-server(toolkit-*) ⇄ english`，本仓库即「tools-server」。

> 本仓库由 `github-commit-info` 提级改名而来：原 `github-commit-info` 现降级为众多工具中的一个
> CLI crate。提级规划见 [docs/toolkit-rfc/2026-06-10-toolkit-elevation/plan.md](docs/toolkit-rfc/2026-06-10-toolkit-elevation/plan.md)。

---

## workspace 成员与职责

依赖方向自下而上：`toolkit-core → toolkit-tasks → toolkit-server`；业务 crate（douyin/rag）被 server 装配。

| crate | 职责 |
|---|---|
| `toolkit-core` | 领域类型 + SQLite schema/迁移（`schema.rs` 的 `DDL_V1`）+ URL 模式识别。`open_pool` / `migrate` / `new_task_id` / `now_iso8601`。 |
| `toolkit-tasks` | **通用长任务引擎**：`TaskKind` trait + `Registry` 注册、`submit` 即 spawn、`run_task` 状态机、`store` 持久化到 `tasks` 表。 |
| `toolkit-llm` | **统一 OpenAI 兼容 LLM 客户端**：`LlmConfig`（含 `from_env`）+ `LlmClient`（`complete`/`chat` + 指数退避重试 + 响应解析）+ `prompt_hash`。任何需调大模型的内部 crate 都走它，不要自行拼 HTTP。**不持有提示词**（提示词由功能层/可配目录决定后传入）。 |
| `toolkit-server` | axum daemon。`bootstrap` 装配 pool/migrate/registry/recovery；`/api/web`、`/api/web/audio`（TTS 代理）、`/api/web/douyin`、`/api/web/llm`（**公共大模型：连接配置/可配提示词/连通性自测/对话总结**）、`/api/agent`、`/api/browser`、`/api/internal`（**出口代理 worker 通道**：register/heartbeat/长轮询 egress/result,共享 token）、`/api/web/egress`（出口观测 + probe 自测）路由 + web 控制台。systemd 安装 / 自更新（`custom-utils` updater）。 |
| `zero-desktop` | 统一 Tauri 桌面壳：cookie 采集（抖音/同花顺 headless_chrome/CDP + msToken + 上传 G10）、speech / english / codeloop / g10-deploy / 音乐 / 网络策略 等模块。**需 Tauri 工具链**，CI 式环境通常排除。 |
| `asr-client` | 通用 FunASR `/transcribe` HTTP 客户端（multipart 上传 + 强类型响应 + 错误归类）。**任何需要离线 ASR 的内部 crate 都走它**，不要自行拼 multipart。端点契约权威源在 streaming-speech `docs/asr-transcribe-api.md`。 |
| `douyin` | 抖音 web 工具：a-bogus 签名、creator/works/tags API、下载 + ASR 管线（**通过 `asr-client` 调 FunASR**）、LLM 整理（`refine`）、knowledge md 生成。既是库（被 server 调）也有独立 daemon/CLI。 |
| `rag` | 抖音 knowledge md 的语义检索 → sqlite-vec。CLI `ingest`/`search`，HTTP `serve`。 |
| `github-commit-info` | 独立 CLI：取 GitHub 仓库指定时间范围 commit。 |
| `hf-watcher` | 独立 CLI：HuggingFace trending / model-card 监听。 |
| `egress-pool` | **出口代理轻模型核心**（借出口,非分发算力）：`EgressRequest/Response` 线格式 + in-memory `Registry`（worker 通道/请求路由/session 绑定）+ `Pool`/`Session` 进程内句柄。两原语：`pool.fetch`（匿名短租轮换 IP）/ `pool.session(typ,account)`（钉死长租,同一出口 IP + 连续 cookie,按账号复用）。共用策略「同类型独占、类型间共用」。详见 [docs/distributed-worker-design.md](docs/distributed-worker-design.md) 的「轻模型」节。 |
| `toolkit-worker` | **出口代理节点二进制**（pull 模型执行端）：register/心跳/长轮询 `/api/internal/egress/next` → 本机 reqwest 代发 → 回传;per-session cookie jar。各出口机手动/自更新拉起 `toolkit-worker run`（**零参数**，配置在 `~/.config/toolkit-worker/`）。**另带 remote-exec 命令执行面**：首次启动自动申请临时权限，等你在桌面端批准，见下方专节。 |
| `worker-core` | **远程执行（remote-exec）执行端内核 + 双端共用线格式**：`proto`（`ExecRequest/ExecResponse`、请求头常量、上限常量、`validate`/`script_hash`;controller 与 worker 都用它,不许私拼 JSON）、`Executor`（写带 BOM 的临时 ps1 → `powershell.exe -File` 逐参数 spawn → 有界捕获 → 超时 `taskkill /T /F` 杀树 → 清临时目录）、本地 JSONL 审计。**不含任何网络逻辑**（那在 `toolkit-worker`）。 |

## 常用命令

```bash
# 构建 / 测试（desktop 需 Tauri，CI 式环境排除）
cargo check --workspace
cargo test  --workspace
cargo check --workspace --exclude zero-desktop         # 无 Tauri 工具链时
cargo fmt

# 本地起 server（workspace = 所有持久状态的根目录）
cargo run -p toolkit-server -- serve --workspace ./data --bind 127.0.0.1:8788
# 健康检查
curl http://127.0.0.1:8788/api/web/health
```

```powershell
# G10 交叉编译 + 部署（aarch64-linux，Docker 跨编译镜像 → scp 到 G10），见 deploy-g10.ps1
pwsh ./deploy-g10.ps1            # 完整构建并部署
pwsh ./deploy-g10.ps1 -SkipBuild # 仅复制已有产物
```

`deploy-g10.ps1` 的 `$Bins` 列表（crate→bin）控制部署哪些二进制；新增工具时在此追加一行。

## 关键约定

- **TaskKind 注册**：实现 `toolkit_tasks::TaskKind`（关联 `Input`/`Output` + `const KIND` + `async fn run`），
  在 `toolkit-server` 的 `bootstrap()` 里 `registry.register::<T>()`。抖音 kind 在
  `crates/toolkit-server/src/douyin/kinds.rs::register_all` 统一注册：`douyin_download` /
  `douyin_transcribe` / `douyin_list_works`（文件状态轮询型）+ `douyin_text_refine`（LLM 整理，
  进程内逐条调）+ `douyin_pipeline`（整链编排）。`submit()` 校验 kind 后立即 spawn，返回 `task_id`。
- **SQLite 迁移**：单文件 `<workspace>/toolkit.db`。schema 是整块 `DDL_V1`（`CREATE TABLE IF NOT EXISTS`，
  幂等），版本号写 `meta.schema_version`。改 schema 即改 `toolkit-core/src/schema.rs` 并 bump
  `SCHEMA_VERSION`；当前**没有增量迁移框架**，靠幂等 DDL。
- **长任务状态机**：`queued → running → succeeded/failed`；进程启动时 `recover_interrupted` 把残留的
  `queued/running` 标为 `interrupted`（不自动重跑）。任务体 panic 被 `run_task` 捕获转 `failed`。
  运行中用 `TaskCtx::report_progress(json)` 写 `tasks.progress`。抖音 kind 的形态是「调下游 submit
  → 每 2s 轮询下游状态写进 progress → 终态返回/报错」。
- **输出契约（CLI 工具）**：`douyin` / `hf-watcher` / `github-commit-info` 向 **stdout 输出单行紧凑 JSON**；
  业务失败输出 `{error, error_kind}` 且**退出码 0**（仅进程级异常退出码非 0）。应用日志走
  `custom-utils` logger（prod 落文件，绝不污染 stdout）。
- **workspace 目录布局**（`toolkit-server --workspace` 根）：`toolkit.db`、`douyin/{cookies.json,tasks,transcripts,refined,works}`、
  `downloads/douyin/`、`knowledge/douyin/`、`web/`（静态控制台，缺失则用内嵌最小 HTML）。
  `douyin/refined/<aweme_id>.json` = LLM 整理稿（与 ASR 原文 `transcripts/<aweme_id>.json` 并列）。
- **自更新**：各 bin 的 `REPO_OWNER`/`REPO_NAME` 常量指向 `jm-observer/toolkit`；改名后已统一为 `toolkit`。

## 追踪（trace-hub）

`toolkit-server` 启动时若设了环境变量 `TRACE_HUB_ENDPOINT` 则接入 trace-hub（`custom-utils` 0.15 +
`trace` feature），**未设则完全无副作用**。`toolkit-tasks` 的 runner 用 `SpanScope` 两阶段 API 给每个
任务打 anchor（submit 时 in-flight + 输入摘要）+ 完成 span（成功/失败 + 耗时）。创建任务的 HTTP handler
透传 W3C `traceparent`。详见下方《文档目录》。

## 语音底座（ASR / TTS）

- **ASR**：**统一走 streaming-speech 仓的 FunASR**（同机 GB10:9101，`/transcribe`
  multipart 端点，Paraformer/SenseVoice/Whisper GPU 全套 + 声纹门控 + 实时流式管线 +
  离线整段转写）。原本仓的 `crates/asr-server`（sherpa-onnx）已于 2026-06 物理退役
  （crate 删除、deploy/asr-tts 只剩 TTS、deploy-g10.ps1 bin 清单移除）。
  - **客户端**：本仓 `crates/asr-client`（通用 multipart 客户端 + 强类型响应），
    任何内部 crate 需要 ASR 都走它。
  - **消费方**：当前是 `crates/douyin`（process 任务），`asr_url` 默认
    `http://127.0.0.1:9101/transcribe`。
  - **端点契约**：streaming-speech `docs/asr-transcribe-api.md`（权威源）。
  - FunASR 服务部署归 streaming-speech 仓维护（`scripts/release-server.ps1`），
    本仓不再持有 ASR 镜像/配置。
- **TTS 代理**：`toolkit-server` 的 `/api/web/audio/tts`（POST，转发请求体到上游
  CosyVoice2 `POST /tts`，回传 WAV bytes）与 `/api/web/audio/voices`（GET，代理 `/voices`）。
  上游地址由环境变量 **`TTS_BASE_URL`**（如 `http://127.0.0.1:8095`）配置；**未配置时
  两端口返回 503** 并提示。TTS 生成可能 10s+，代理超时 180s。调用上有 `SpanScope`
  两阶段 trace（`tts_proxy` / `tts_voices` span；trace 未启用时 no-op）。本阶段只代理，
  不落盘 / 不任务化（落盘任务化是 Phase 3 AudioForge）。
- **音频清洗**：streaming-speech 仓的 audio-cleanup 服务（同机 GB10 `127.0.0.1:8097`，
  `POST /clean` multipart：脏音频 → 人声分离/降噪/删停顿/响度归一化 → 干净音频）。
  - **客户端**：本仓 `crates/audio-clean-client`（照抄 asr-client 形状，multipart 上传 +
    二进制响应 + `X-Cleanup-*` 头解析）。
  - **消费方**：`crates/douyin`（process 任务可选 `clean_audio=true`，带 BGM 视频先去乐再
    ASR，提升识别率）；`toolkit-server` 的 `/api/web/audio/clean`（multipart 透传代理，给
    zero-desktop 等桌面端）。
  - **代理配置**：环境变量 **`CLEAN_BASE_URL`**（如 `http://127.0.0.1:8097`）；**未配置时
    `/api/web/audio/clean` 返回 503**，配置但上游不可达 → 502。代理超时 600s。
  - **端点契约 / 服务部署**：streaming-speech `docs/audio-cleanup-api.md` +
    `server/audio-cleanup/`（部署归 streaming-speech 仓维护）。
- **编排**：`deploy/asr-tts/`（compose + README）——**仅 TTS**。

## GB10 服务清单（同机 / 内网）

| 端口 | 服务 | 维护仓 |
|---|---|---|
| `:9100` | FunASR 流式 ASR WebSocket（orchestrator 上游） | streaming-speech |
| `:9101` | FunASR `/transcribe` + `/embed`（离线 ASR / 声纹） | streaming-speech |
| `:8095` | CosyVoice2 TTS | streaming-speech |
| `:8097` | audio-cleanup `/clean`（音频清洗） | streaming-speech |
| `:8098` | pronunciation-assess `/assess`（英语发音评测 GOP；shadow GOP 后端上游，`GOP_BASE_URL`） | streaming-speech |
| `:8788` | toolkit-server（Web API + 代理 + 控制台 + **内嵌 ASR 编排**） | 本仓 |
| `:8000` | vLLM（OpenAI 兼容 LLM） | 第三方/外部 |

> **orchestrator（StreamSpeech ASR 编排层）已并入 toolkit-server 同进程**：其 axum 路由 nest 在
> `/api/asr` 下（WS=`/api/asr/stream`、HTTP=`/api/asr/api/*`），不再独立 `:8090` 部署。`orchestrator`
> crate 现为 **lib + bin** 双形态——lib 暴露 `router(ctx)`/`init_ctx()` 供 toolkit-server 挂载，bin 仅
> 留作独立调试。下游地址（`ASR_WS`/`ASR_EMBED`/`VLLM_BASE`/`VLLM_MODEL`）经 toolkit-server 的 env 传入，
> 缺省即本机回环。其 `app.db`（声纹/段落/配置）落在 toolkit-server 的 workspace 下，与 `toolkit.db` 并列。
> **桌面端（zero-desktop）只需配「局域网 IP / 外网域名」两个 host**：协议/端口/ASR 路径按局域网（http/ws
> :8788）或外网（https/wss :28080 反代）的固定约定派生，`auto` 模式经 health 探测自动选路（见
> `zero-desktop/src/shared/settings.rs`）。

## 公共大模型层（LLM 中枢）

把「大模型连接配置 + 可配提示词」集中到一处，各功能（抖音整理、对话总结、codeloop 文案…）共用，
不再各自读 env / `include_str!`。

- **统一客户端**：`toolkit-llm` crate（`LlmClient::complete/chat` + 指数退避重试 + 响应解析）。
  需调大模型的内部 crate 一律走它。
- **连接配置（运行时可改）**：存 `toolkit.db` 的 `llm_config` 单行表。解析顺序 **DB > env**
  （`LLM_BASE_URL`/`LLM_MODEL`/`LLM_API_KEY`）。`toolkit-server::llm::resolve_config / resolve_client`
  统一解析；两者都没配 → 明确报错。
- **可配提示词目录**：内置默认在 `toolkit-server::llm::builtins()` 登记（name + 语义版本 + 默认
  文本 + 占位符）；DB 的 `llm_prompts` 表行**覆盖**内置默认，删行即「恢复内置默认」。当前登记：
  `douyin_refine`（`{TRANSCRIPT}`）、`chat_summary`（`{CONVERSATION}`）、`codeloop_codex_review` /
  `codeloop_claude_revision`（codeloop CLI 会话指令模板，占位符见 `codeloop_core::prompt`）。
  **新增可配提示词 = 在 `builtins()` 加一行**。
- **HTTP（`/api/web/llm`）**：`GET/PUT /config`、`GET /prompts`、`GET/PUT/DELETE /prompts/{name}`
  （DELETE=重置内置）、`POST /ping`（连通性自测）、`POST /summarize`（对话总结，用 `chat_summary`）。
- **会话（`llm_sessions` / `llm_messages` 两表）**：`GET/POST /sessions`、
  `GET/PUT/DELETE /sessions/{id}`（PUT=重命名，DELETE=连消息一并删；两表无外键无级联，
  删除在事务里显式删两次）、`POST /sessions/{id}/messages`（续聊）。续聊准入走
  `routes.rs::can_continue` 白名单（当前仅 `chat_test`；**新 kind 如 agent 需显式加入**）。
  发给模型的历史经 `context_window` 截窗（`MAX_CONTEXT_MESSAGES=40`，保留开头连续 system +
  尾部最近若干条），**落库仍是全量**；后续可迭代为用 `chat_summary` 压缩被裁的旧轮。
- **codeloop 提示词**：走 Codex/Claude **CLI 会话**通道，纳入此目录仅为统一管理文案，与 HTTP 大模型
  通道无关；`kind.rs` 在任务启动时按 name 从 DB 解析模板（缺失回退 `codeloop_core` 内置常量）。
  zero-desktop 内嵌的 codeloop 也接入：循环启动时经 `/api/web/llm/prompts/{name}` 拉一次生效模板
  （失败回退内置，绝不阻塞循环；headless smoke 无 AppState，恒用内置）。登记名常量收拢在
  `codeloop_core::prompt::PROMPT_NAME_*`，两端共用。continuation / implement 等未登记模板仍走内置。

## 抖音知识管线（流 A，Phase 2）

补齐了 plan 流 A 的「LLM 整理文本」与「整链编排」两块缺口：

- **TextRefine**（`douyin_text_refine` kind / `POST /api/web/douyin/refine`）：读 ASR 原文
  （`douyin/transcripts/<id>.json`）→ 调 GB10 vLLM（OpenAI 兼容 chat completions）纠错/去口语
  水词/分段/小结 → 落整理稿 `douyin/refined/<id>.json`（带 `model` / `prompt_version` /
  `prompt_hash` / `refined_at`）。输入显式 `aweme_ids` 或留空整理「全部已转写未整理」。单条失败
  重试 3 次（指数退避），最终失败进 output 的 `failures[]`，不拖垮整批。**幂等**：已整理跳过。
- **整理稿进 RAG**：`kb_publish` 把整理稿写进 knowledge md 的 `## 整理稿（LLM）` 段（置于 ASR 原文
  之前，rag 优先索引整理后的可读文本），frontmatter 记 `has_refined` + refined 元信息；原文栏保留。
- **CreatorPipeline**（`douyin_pipeline` kind / `POST /api/web/douyin/pipeline`）：输入
  `handle`（unique_id/URL）+ 可选 `tags` 筛选 + `stages` 开关，串联
  `sync_works(可选)→download→transcribe(ASR)→refine→kb_publish→rag_ingest`。进度聚合写
  `progress.{stage,stage_index,stage_total,stage_progress}`。任一环节失败 → 任务 failed，已完成成果
  保留（各下游任务自身幂等，重跑跳过已完成 item）。`rag_ingest` 通过 spawn `rag` 二进制完成
  （需 `rag_config` JSON 路径；rag 定位优先 `RAG_BIN` 否则同目录 `rag`）。
- **LLM 配置（公共大模型层，见下方专节）**：连接配置（base_url/model/api_key）走 `toolkit-server`
  的 `llm` 层解析——**DB（`llm_config` 表，控制台 `/api/web/llm/config` 可改）优先，缺失回退环境变量**
  `LLM_BASE_URL`/`LLM_MODEL`/`LLM_API_KEY`。**两者都没配时 refine / 含 refine 的 pipeline 提交后
  立即 failed**（不空跑下载/ASR）。
- **整理 prompt 管理**：内置默认 = `crates/douyin/src/refine_prompt.md`（`{TRANSCRIPT}` 占位符，随
  crate 编译，登记为可配提示词 `douyin_refine`）；**控制台可在 `/api/web/llm/prompts/douyin_refine`
  覆盖文案、删除即恢复内置默认**，无需重编译。改内置文案后 bump `refine.rs::PROMPT_VERSION`。每条
  整理稿记生效提示词的 `prompt_version` + `prompt_hash`（sm3 短哈希），prompt 变了哈希就变，可识别
  旧产物、删 `refined/` 后重跑对比。
- **端到端验收**：见 [docs/runbook-pipeline-e2e.md](docs/runbook-pipeline-e2e.md)。

## 英语音频生产线（流 B，Phase 3）

补齐 plan 流 B 的「文本 → TTS 逐句音频 → 学习包草稿 → english 导入消费」供给侧：

- **AudioForge**（`audio_forge` kind / `POST /api/web/audio/forge`）：输入句子清单
  （每句 `text` + 可选 `translation`/`note`/逐句 `voice_id`）+ 包级 `voice_id` + 可选
  `tts_params`（语速/instruct，平铺进上游 body）+ 包元信息（`package_name`/`topic`/`language`）。
  逐句调上游 TTS（直接 `TTS_BASE_URL/tts`，复用 Phase 1 配置；单句失败重试 3 次指数退避，
  最终失败进 output 的 `failures[]`，不拖垮整批）→ 音频落
  `<workspace>/audioforge/<package_id>/NNN.wav` → 生成 `manifest.json`（包元信息 + 句子数组：
  序号/文本/译文/注释/音频文件名/时长(解析 WAV 头)/voice/tts_params/生成时间）。产物即「学习包草稿」。
  **未配置 TTS_BASE_URL 时任务提交后立即 failed**。trace：`audio_forge_batch` 顶层 span +
  逐句 `tts_one` 子 span。
- **下载途径**（供 english 拉取，零人工传文件）：`GET /api/web/audio/forge/{package_id}/manifest.json`
  与 `GET /api/web/audio/forge/{package_id}/{NNN.wav}`。路径段白名单校验（无分隔符/`..`/盘符）
  + canonicalize 后必须仍在 `audioforge/` 内，**防路径穿越**。
- **抖音整理稿抽句**（可选来源快捷方式）：输入 `from_refined: {unique_id?, aweme_ids}` →
  读 `douyin/refined/<id>.json` 整理稿，**按句切分全文**（标点切分，剥离 markdown 标题/列表
  前缀）。**简化实现，待迭代**为「英语片段精选」（见 runbook 说明）。来源标记 source =
  `manual`/`from_refined`/`mixed`。
- **manifest 契约**：`manifest_version=1`；english `package.import` 据此消费。详见
  [docs/runbook-audioforge-e2e.md](docs/runbook-audioforge-e2e.md)。
- **配置**：复用 Phase 1 的 `TTS_BASE_URL`（如 `http://127.0.0.1:8095`）。
- **端到端验收**：见 [docs/runbook-audioforge-e2e.md](docs/runbook-audioforge-e2e.md)。

## 远程命令执行（remote-exec，第一期）

给 `toolkit-worker` 加的「命令执行面」：worker 主动出连 controller，operator 下发 PowerShell
脚本拿 stdout/stderr/退出码。**与 egress 面共享稳定 `worker_id`，其余全部独立**（凭据 / 路由 /
调度状态 / 审计 / 中止语义）。设计见 [docs/remote-exec-design.md](docs/remote-exec-design.md)；
第一期只做 Windows/PowerShell + 单任务 + 同步 `/run`，异步任务/排队/远程取消/本地控制面是第二期。

- **worker 侧零参数**：对方机器上只跑 `toolkit-worker run`。身份、controller、凭据全在
  workspace（`~/.config/toolkit-worker/`：`config.json` + 单独的 `exec-secret` + `remote-exec/`）。
  **密钥刻意不进 config.json**——配置文件是会被截图/贴出来排查的东西。controller 默认值就是外网
  入口 `https://spark.for-memory.site:38788`（caddy TLS → G10:8788；28080 是 english 自己的入口）。
- **worker id 自动派生**：`w-<sm3(物理网卡 MAC 排序拼接 + 主机名)[..8]>`。**排除虚拟网卡**
  （VMware/Hyper-V/vEthernet/TUN/WSL…）、**全部 MAC 排序后一起哈希**（不是「取第一块」，
  否则插拔网线就换 id），算出来立刻固化进 config，之后永远以配置为准。面板上认人靠单独的
  `label`（`run --label`，默认主机名）。
- **临时权限（申请 → 批准 N 小时）**：`run` 首次启动或凭据过期时**自动**提交申请
  （`POST /api/internal/exec/access/request`，**该端点不要求凭据**——申请的前提就是还没有凭据），
  然后每 10s 轮询 `access/poll` 停在「等待批准」；你在 zero-desktop「远程节点」页批准并选时长
  （1h/8h/20h/3d，默认 20h，上限 7 天）→ worker 领到带 `expires_at` 的 secret 落盘 → 自动进主循环。
  **到期不退出进程**，回到申请态等续期。明文 secret 在 `issued_secret` 里只暂存到 worker 领走那一刻。
  防刷三道：同 worker_id 去重 / pending 上限 32 / pending 24h TTL（**不按来源 IP 限频**——外网在
  caddy 反代之后，这里只能看到反代自己，`X-Forwarded-For` 可伪造）。
- **两套凭据**：内部面 per-worker secret（`exec_worker_creds` 存 `sm3(salt||secret)` + `expires_at`，
  请求头 `x-worker-id`/`x-exec-secret`/`x-instance-id`，每次请求查库并判过期）；消费面只认
  `TOOLKIT_EXEC_TOKEN`（`token:operator`，operator 由命中的 token 注入），**不叠加**
  `TOOLKIT_API_TOKEN`。未配置 exec token 则 `/api/web/exec/*` 根本不挂载。
- **桌面端审批面**：`exec` 模块 + 「远程节点」页。用**独立的 `exec_token` 设置项**，不复用
  `g10_token`——批准一台机器 = 授予在它上面执行任意命令的权限，安全边界高一档。
- **手工签发仍在**：`toolkit-server exec-cred add|revoke|list`（永久凭据，`expires_at` 为 NULL），
  用于不方便走申请流程的场景。第一期 `revoke`/到期都只阻止**领取新任务**，**杀不掉已在执行的命令**
  （远程可靠中止是第二期）。
- **端点**：内部 `/api/internal/exec/{register,heartbeat,next,result}` + 免凭据的
  `/api/internal/exec/access/{request,poll}`；消费 `/api/web/exec/{workers,run,requests,creds}` +
  `requests/{id}/{approve,reject}`。`/run` 返回 `{state,source,id,exec,reason}`；
  `404 worker_not_exec_capable` / `409 worker_offline|worker_busy` / `504 not_picked_up` /
  `502 unknown`（**命令可能已执行，禁止自动重试**）。
- **执行语义**：脚本写带 BOM 的 `user.ps1`（保 `param()`/`#requires`），`wrapper.ps1` 设 UTF-8 后
  `& user.ps1 @Args` 并以 `exit $LASTEXITCODE` 回传真实退出码（**少这行会把 `exit N` 吞成 0**）；
  `powershell.exe -NoProfile -NonInteractive -File` 逐参数传；超时 `taskkill /T /F /PID` 杀整棵树
  后仍 `wait()` 回收，`timed_out` 的 exit_code 为 null。
- **审计**：controller（`<workspace>/remote-exec/audit/`）与 worker（`<workspace>/remote-exec/audit/`）
  双写 JSONL，保留 30 天，**只记元信息 + 脚本 SM3 短哈希，绝不记正文**。
- **CLI 只剩**：`run`（零参数）/ `status` / `update` / `install`(Linux) / `proxy`；`list` 仅 Linux。
  **`scan` 已删**。出口选择参数是历史包袱且**实测只有 Linux 有效**：`--interface`
  （`SO_BINDTODEVICE`）只在 Linux 编译进 CLI；`--local-address` 在 Windows 上绑非默认路由网卡
  会直接发不出包（实测每张非默认网卡探测全失败）——换出口走 net-policy（已拆独立仓）。
- **已知取舍**：Windows 上临时目录/脚本只靠继承 ACL（凭据文件用 `icacls` 收紧了）；Unix 杀树只
  `kill -9` 直接 pid；controller 重启后在途任务只能得到连接失败或 `unknown`。

## 文档目录（动手前按主题查）

- [docs/toolkit-design.md](docs/toolkit-design.md) — 中台整体设计。
- [docs/douyin-design.md](docs/douyin-design.md) / [docs/douyin-cli.md](docs/douyin-cli.md) — 抖音工具设计与 CLI/HTTP API 参考。
- [docs/rag-service-design.md](docs/rag-service-design.md) — RAG 检索服务设计。
- [docs/runbook-pipeline-e2e.md](docs/runbook-pipeline-e2e.md) — 抖音知识管线端到端验收 runbook（Phase 2）。
- [docs/runbook-audioforge-e2e.md](docs/runbook-audioforge-e2e.md) — 英语音频生产线端到端验收 runbook（Phase 3）。
- [docs/audio-store-design.md](docs/audio-store-design.md) — 音频统一仓库（audio-store）设计：blob 内容寻址仓库收拢音频字节，english 持引用消复制。
- [docs/english-shadow-design.md](docs/english-shadow-design.md) — 英语跟读判分（v1：ASR 文本对齐）设计。
- [docs/english-shadow-todo.md](docs/english-shadow-todo.md) — 跟读判分优化需求 TODO（全自动 / 实时录评 / 发音级评分）。
- [docs/english-shadow-gop-design.md](docs/english-shadow-gop-design.md) — 跟读发音级评测（GOP 音素评分）设计。
- [docs/english-shadow-realtime-design.md](docs/english-shadow-realtime-design.md) — 跟读实时发音评测（流式 GOP：流式声学 + 在线对齐 + WS，批量 GOP 作 finalizer）设计。
- [docs/runbook-shadow-realtime-e2e.md](docs/runbook-shadow-realtime-e2e.md) — 跟读实时发音评测端到端验收 runbook（直连 :8098 → toolkit 中继 → 桌面 三层 + 落库/降级校验）。
- [docs/english-shadow-scoring-ui-design.md](docs/english-shadow-scoring-ui-design.md) — 评分可解释 + 对齐明细 UI 设计：评分细则透明化 + 新增「对齐可靠性/uncertain」(把没对齐上的音素从 bad 改判存疑,不冤枉用户) + 音素明细表展示。
- [docs/english-pron-evaluator-design.md](docs/english-pron-evaluator-design.md) — 发音评测单元(PronEvaluator)设计：词/短语/句通用的可复用组件(评分+朗读+听标准TTS+听自己+LLM 二次反馈),含失败词 drill-down 单练。
- [docs/wan-access-todo.md](docs/wan-access-todo.md) — 外网入口待办（桌面端 tokio-tungstenite 没开 TLS feature，外网档 wss 语音识别连不上）。
- [docs/remote-exec-todo.md](docs/remote-exec-todo.md) — remote-exec 待办（部署版本落后导致面板看不到申请、worker 轮询未判状态码把 404 误报成解析失败）。
- [docs/remote-exec-design.md](docs/remote-exec-design.md) — 远程命令执行 / 远程排查底座设计（第一期：同步 `/run` 闭环，已落地；第二期：异步任务 · 排队 · 远程取消 · 本地控制面，未做）。
- [docs/distributed-worker-design.md](docs/distributed-worker-design.md) — 分布式爬虫底座设计（公共库 · Worker · 出口 IP 池）：统一模型（一支 fleet 两个面 / 库的两档接入：请求级 egress 代理 + 任务级 worker 分发 / cookie·匿名 IP 策略分裂 / proxy_pool 提级）+ pull 调度 + 两段式自更新。
- **网络出口策略（net-policy）已于 2026-07 拆为独立仓** → [jm-observer/net-policy](https://github.com/jm-observer/net-policy)（本地 `D:\git\net-policy`）。六个 `net-policy-*` crate、`docs/net-policy/` 文档集、`package-net-policy.ps1` 全部迁出，本仓不再持有；历史经 `git filter-repo` 完整保留在新仓。
- [docs/toolkit-rfc/2026-06-04-initial-skeleton/data-model.md](docs/toolkit-rfc/2026-06-04-initial-skeleton/data-model.md) — SQLite 数据模型。
- [docs/toolkit-rfc/2026-06-10-toolkit-elevation/plan.md](docs/toolkit-rfc/2026-06-10-toolkit-elevation/plan.md) — 提级为统一工具中台的分阶段规划。
- [docs/retrospective.md](docs/retrospective.md) — 复盘记录。

## 编码约定

- 平台 Windows 11 / PowerShell 优先；提交走 Conventional Commits（中文 message，与既有 git log 一致）。
- 库代码用 `anyhow::Result` + `?` + `.context`；`main.rs`/测试可 `unwrap`。
- 异步上下文禁同步阻塞 I/O；SQL 全参数化。
