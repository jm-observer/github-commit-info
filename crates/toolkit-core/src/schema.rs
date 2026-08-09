//! SQLite schema v1 完整 DDL。详见 `docs/toolkit-rfc/2026-06-04-initial-skeleton/data-model.md`。

pub const SCHEMA_VERSION: i64 = 1;

pub const DDL_V1: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS creators (
    unique_id      TEXT PRIMARY KEY,
    sec_uid        TEXT NOT NULL UNIQUE,
    nickname       TEXT NOT NULL,
    avatar_url     TEXT,
    signature      TEXT,
    follower_count INTEGER,
    aweme_count    INTEGER,
    verified       INTEGER NOT NULL DEFAULT 0,
    raw            TEXT NOT NULL,
    added_at       TEXT NOT NULL,
    last_synced_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_creators_sec_uid  ON creators(sec_uid);
CREATE INDEX IF NOT EXISTS idx_creators_added_at ON creators(added_at);

CREATE TABLE IF NOT EXISTS works (
    aweme_id          TEXT PRIMARY KEY,
    unique_id         TEXT NOT NULL,
    desc_text         TEXT NOT NULL DEFAULT '',
    tags              TEXT NOT NULL DEFAULT '[]',
    create_time       TEXT NOT NULL,
    cover_url         TEXT,
    video_url         TEXT,
    duration_ms       INTEGER,
    statistics        TEXT NOT NULL DEFAULT '{}',
    raw               TEXT NOT NULL,
    downloaded_path   TEXT,
    downloaded_at     TEXT,
    transcribed       INTEGER NOT NULL DEFAULT 0,
    transcript_path   TEXT,
    transcribed_at    TEXT,
    kb_published_mode TEXT,
    kb_published_at   TEXT,
    discovered_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_works_unique_id     ON works(unique_id);
CREATE INDEX IF NOT EXISTS idx_works_create_time   ON works(unique_id, create_time DESC);
CREATE INDEX IF NOT EXISTS idx_works_downloaded    ON works(unique_id, downloaded_at);
CREATE INDEX IF NOT EXISTS idx_works_kb_published  ON works(unique_id, kb_published_mode);

CREATE TABLE IF NOT EXISTS tasks (
    task_id      TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    state        TEXT NOT NULL,
    input        TEXT NOT NULL,
    output       TEXT,
    error        TEXT,
    progress     TEXT NOT NULL DEFAULT '{}',
    created_at   TEXT NOT NULL,
    started_at   TEXT,
    finished_at  TEXT,
    callback_url TEXT,
    callback_delivered_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_tasks_state       ON tasks(state);
CREATE INDEX IF NOT EXISTS idx_tasks_kind_state  ON tasks(kind, state);
CREATE INDEX IF NOT EXISTS idx_tasks_created_at  ON tasks(created_at DESC);

CREATE TABLE IF NOT EXISTS cookies (
    id                INTEGER PRIMARY KEY CHECK (id = 1),
    raw               TEXT NOT NULL,
    parsed            TEXT NOT NULL,
    captured_at       TEXT NOT NULL,
    last_validated_at TEXT,
    status            TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS browser_sessions (
    session_id   TEXT PRIMARY KEY,
    user_agent   TEXT,
    first_seen   TEXT NOT NULL,
    last_seen    TEXT NOT NULL,
    current_url  TEXT
);
CREATE INDEX IF NOT EXISTS idx_browser_sessions_last_seen ON browser_sessions(last_seen DESC);

-- 公共大模型连接配置（单行）。DB 行存在则优先于环境变量，便于运行时在控制台改地址/模型/key
-- 而无需重启或改 systemd 环境。纯加表、IF NOT EXISTS 幂等：migrate() 每次启动
-- execute_batch(DDL_V1) 都会建出，故不需要、也不应 bump SCHEMA_VERSION（bump 不更新已有
-- DB 的 meta）。
CREATE TABLE IF NOT EXISTS llm_config (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    base_url   TEXT,
    model      TEXT,
    api_key    TEXT,
    updated_at TEXT NOT NULL
);

-- 可配提示词注册表：按名字存（如 douyin_refine / chat_summary）。DB 行存在则
-- 覆盖各功能编译期内置默认；version/hash 保留溯源，builtin_hash 记录覆盖时的内置基线哈希，
-- 供控制台提示「已修改/可重置」。纯加表幂等，不 bump SCHEMA_VERSION。
CREATE TABLE IF NOT EXISTS llm_prompts (
    name         TEXT PRIMARY KEY,
    text         TEXT NOT NULL,
    version      TEXT NOT NULL,
    hash         TEXT NOT NULL,
    builtin_hash TEXT,
    updated_at   TEXT NOT NULL
);

-- English 跟读判分明细：每次跟读尝试一行（可回看 / 调阈值后重算）。kind=sentence|word；
-- word 模式 word_index 为句内词序号，sentence 模式为 NULL。纯加表、IF NOT EXISTS 幂等，
-- 同 llm_*，不 bump SCHEMA_VERSION。见 docs/english-shadow-design.md §7。
-- detail_json：GOP 发音级评测的音素/词级明细（v1-ASR 内核为 NULL）。新库由下方 DDL 直接建出；
-- 存量库由 migrations.rs 的幂等 ALTER 补列（CREATE TABLE IF NOT EXISTS 不会给已存在的表加列）。
-- 见 docs/english-shadow-gop-design.md §5。
CREATE TABLE IF NOT EXISTS shadow_attempt (
    id          TEXT PRIMARY KEY,
    customer_id INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    sentence_id INTEGER NOT NULL,
    word_index  INTEGER,
    ref_text    TEXT    NOT NULL,
    transcript  TEXT,
    score       REAL    NOT NULL,
    passed      INTEGER NOT NULL,
    detail_json TEXT,
    created_at  TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_shadow_attempt_unit
    ON shadow_attempt(customer_id, sentence_id, word_index);

-- English 跟读单元累计统计（读取快；由 attempt 累加维护，可随时按 attempt 重建）。
-- word_index 用 -1 占位代表「整句」单元，以进入复合主键。
CREATE TABLE IF NOT EXISTS shadow_stat (
    customer_id   INTEGER NOT NULL,
    kind          TEXT    NOT NULL,
    sentence_id   INTEGER NOT NULL,
    word_index    INTEGER NOT NULL DEFAULT -1,
    success_count INTEGER NOT NULL DEFAULT 0,
    fail_count    INTEGER NOT NULL DEFAULT 0,
    last_score    REAL,
    last_passed   INTEGER,
    last_at       TEXT,
    PRIMARY KEY (customer_id, sentence_id, word_index, kind)
);

-- 音频统一仓库（audio-store）：内容寻址 blob 仓库，按 id 收拢音频字节，供 english 等消费方
-- 持引用消复制。**只存音频字节本身的元信息、不持任何产品语义**（句子/课程/包等都在消费方）。
-- id = sm3(bytes) 前 8 字节短哈希（`aud_` 前缀），同内容自然去重（内容寻址幂等）。字节落
-- `<workspace>/audio-store/<id>.wav`，本表记元信息。纯加表、IF NOT EXISTS 幂等：同 llm_config /
-- llm_* / shadow_*，migrate() 启动即建出，故不需要、也不应 bump SCHEMA_VERSION。
-- 见 docs/audio-store-design.md。
CREATE TABLE IF NOT EXISTS audio_blob (
    id           TEXT PRIMARY KEY,
    bytes        INTEGER NOT NULL,
    duration     REAL,
    content_type TEXT NOT NULL DEFAULT 'audio/wav',
    source       TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

-- 大模型会话记录：把「对话测试」的交互式聊天与各业务的大模型调用（抖音整理 / 对话总结）
-- 统一以 session 形式落库，供桌面端「大模型会话」模块只读回看（对话测试面板可续聊）。
-- kind 标记来源（chat_test | douyin_refine | chat_summary），为后续接入 zero agent 预留
-- kind="agent"；metadata / 每条消息 meta 均为 JSON blob，可承载 aweme_id / prompt 版本哈希 /
-- 将来的工具调用信息，无需改表。纯加表、IF NOT EXISTS 幂等：同 llm_* / shadow_* /
-- audio_blob，migrate() 启动即建出，故不需要、也不应 bump SCHEMA_VERSION。
CREATE TABLE IF NOT EXISTS llm_sessions (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,              -- chat_test | douyin_refine | chat_summary
    title       TEXT NOT NULL DEFAULT '',
    model       TEXT,
    prompt_name TEXT,                       -- chat_test 为 NULL
    status      TEXT NOT NULL DEFAULT 'ok', -- ok | error
    metadata    TEXT NOT NULL DEFAULT '{}', -- JSON: aweme_id / unique_id / prompt_version / prompt_hash
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_llm_sessions_kind_created ON llm_sessions(kind, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_llm_sessions_created      ON llm_sessions(created_at DESC);

-- 会话内逐条消息（seq 为 session 内 0-based 顺序）。role = system|user|assistant；
-- meta JSON 记 latency_ms 等逐条元信息。纯加表、IF NOT EXISTS 幂等，不 bump SCHEMA_VERSION。
CREATE TABLE IF NOT EXISTS llm_messages (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    role       TEXT NOT NULL,
    content    TEXT NOT NULL,
    meta       TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_llm_messages_session ON llm_messages(session_id, seq);

-- 远程执行（remote-exec）第一期：per-worker exec 专用凭据。secret 明文只在 `exec-cred add`
-- 时展示一次，落库只存 `sm3(salt||secret)` 的 hex（salt 随机 16 字节）。revoked_at 非空即吊绝：
-- 拒绝该 worker 后续领取任务/回传结果（不是正在执行命令的 emergency stop，见设计 §4.2）。
-- 纯加表、IF NOT EXISTS 幂等：同 llm_* / shadow_* / audio_blob / llm_sessions，
-- migrate() 启动即建出，故不需要、也不应 bump SCHEMA_VERSION。
-- 见 docs/remote-exec-design.md 第一期 §4.2。
-- expires_at：临时授权的到期时间（unix 秒）。NULL = 永不过期（`exec-cred add` 手工签发的老形态）。
-- 走「worker 申请 → 面板批准 N 小时」通道签发的凭据都带到期时间，到点 verify 自动失败。
-- 存量库由 migrations.rs 的幂等 ALTER 补列。
CREATE TABLE IF NOT EXISTS exec_worker_creds (
    worker_id   TEXT PRIMARY KEY,
    secret_hash TEXT NOT NULL,
    salt        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    revoked_at  INTEGER,
    expires_at  INTEGER
);

-- worker 临时权限申请：worker 端 `run` 首次启动（或凭据过期）时自行提交，落此表等人工批准；
-- zero-desktop 的面板拉待审批列表、点批准并选时长 → 签发带 expires_at 的凭据写回本表的
-- issued_secret（**一次性领取**：worker 轮询取走后立即清空，DB 不长期留明文）。
--
-- 该端点公网可达且**不需要凭据**（这是"申请"本身的前提），故靠三道兜底防刷：同 worker_id
-- 去重（重复申请只刷新同一行）、pending 总数上限、pending 超 24h 自动过期（见 exec_requests.rs）。
-- 纯加表、IF NOT EXISTS 幂等，不 bump SCHEMA_VERSION。
CREATE TABLE IF NOT EXISTS exec_cred_requests (
    worker_id     TEXT PRIMARY KEY,
    label         TEXT NOT NULL DEFAULT '',
    hostname      TEXT NOT NULL DEFAULT '',
    os            TEXT NOT NULL DEFAULT '',
    state         TEXT NOT NULL,           -- pending | approved | rejected
    requested_at  INTEGER NOT NULL,
    decided_at    INTEGER,
    approved_by   TEXT,
    expires_at    INTEGER,                 -- 批准时确定的凭据到期时间
    issued_secret TEXT                     -- 待 worker 领取的明文 secret；领走即置 NULL
);
CREATE INDEX IF NOT EXISTS idx_exec_cred_requests_state ON exec_cred_requests(state, requested_at DESC);

-- 软件授权（license）台账：在线续期（`POST /api/license/refresh`）签发新令牌时的权威数据源，
-- 也是控制台管理端点（`/api/web/license`）的存储。**不是签名核心**——验签/状态机/委托证书全在
-- custom-utils 的 `util_license`（license-sign feature）；这里只是「一台客户机器授权了什么、
-- 续期时该给多久」的记账。字段对应设计文档 §6.2：
--   business_deadline  商务硬上限（root 签死的锚，续期不得越过，此处只是台账副本用于计算）；
--   grant_window_days  每次续期把 expires_at 推到 now + 此值（封顶 business_deadline）；
--   lease_days         在线租约天数，NULL = 纯离线模式（不给 lease_until）；
--   machine_ids        JSON：MachineFingerprint 数组（decode_mreq1 落库前解出的结构）；
--   features/max_version 随续期原样透传（不得在续期时扩权，客户端自己也会核对锚）；
--   revoked_at          非空即吊销，refresh 立即拒绝（403）。
-- 纯加表、IF NOT EXISTS 幂等：同 llm_* / shadow_* / audio_blob / llm_sessions /
-- exec_worker_creds，migrate() 启动即建出，故不需要、也不应 bump SCHEMA_VERSION。
-- 见 docs/license-impl-design.md §6.2。
CREATE TABLE IF NOT EXISTS licenses (
    lic_id             TEXT PRIMARY KEY,
    product            TEXT NOT NULL,
    subject            TEXT NOT NULL,
    contact_email      TEXT,
    machine_ids        TEXT NOT NULL DEFAULT '[]',
    not_before         TEXT NOT NULL,
    business_deadline  TEXT NOT NULL,
    grant_window_days  INTEGER NOT NULL,
    lease_days         INTEGER,
    grace_days         INTEGER NOT NULL DEFAULT 14,
    features           TEXT NOT NULL DEFAULT '[]',
    max_version        TEXT,
    revoked_at         TEXT,
    note               TEXT NOT NULL DEFAULT '',
    created_at         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_licenses_product ON licenses(product);

-- license_alerts：临期邮件提醒去重表（设计 docs/license-impl-design.md §4.3/§7）。
-- 每 (lic_id, threshold_days) 只发一次提醒；命中即插一行，之后同一阈值不再重复发信。
-- 邮件是带外提醒，不参与授权判定——这张表纯粹为了不刷屏，与 licenses 台账本身无关。
-- 纯加表、IF NOT EXISTS 幂等：同上面一批，不 bump SCHEMA_VERSION。
CREATE TABLE IF NOT EXISTS license_alerts (
    lic_id          TEXT NOT NULL,
    threshold_days  INTEGER NOT NULL,
    sent_at         TEXT NOT NULL,
    PRIMARY KEY (lic_id, threshold_days)
);
"#;
