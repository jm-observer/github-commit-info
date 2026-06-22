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

-- codeloop 跨会话复核循环的 ASK_USER 握手表（见 RFC §10.3）。
-- 纯加表、IF NOT EXISTS 幂等：migrate() 每次启动 execute_batch(DDL_V1) 都会建出，
-- 故不需要、也不应 bump SCHEMA_VERSION（bump 不更新已有 DB 的 meta）。
CREATE TABLE IF NOT EXISTS codeloop_io (
    task_id     TEXT,
    seq         INTEGER,
    asked_by    TEXT,
    question_json TEXT,
    answer_text TEXT,
    created_at  TEXT,
    answered_at TEXT,
    PRIMARY KEY(task_id, seq)
);

-- 公共大模型连接配置（单行）。DB 行存在则优先于环境变量，便于运行时在控制台改地址/模型/key
-- 而无需重启或改 systemd 环境。纯加表、IF NOT EXISTS 幂等：同 codeloop_io，不 bump SCHEMA_VERSION。
CREATE TABLE IF NOT EXISTS llm_config (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    base_url   TEXT,
    model      TEXT,
    api_key    TEXT,
    updated_at TEXT NOT NULL
);

-- 可配提示词注册表：按名字存（如 douyin_refine / chat_summary / codeloop_*）。DB 行存在则
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
-- 同 codeloop_io / llm_*，不 bump SCHEMA_VERSION。见 docs/english-shadow-design.md §7。
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
-- `<workspace>/audio-store/<id>.wav`，本表记元信息。纯加表、IF NOT EXISTS 幂等：同 codeloop_io /
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
"#;
