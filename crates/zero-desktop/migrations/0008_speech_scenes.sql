-- 场景记录：每次语音结果真正落进外部应用时记一行（日常全量收集，与「发现错误才手动采集」
-- 的 speech_samples 是两条独立的线）。
--
-- 为什么不复用 speech_samples：samples 只在用户按快捷键时才写，天然只覆盖「出错的那几条」，
-- 拿它统计「我在哪个软件里说什么样的话」会严重偏样。本表要的是全量样貌。
--
-- 为什么存 text 快照而不是 join asr_* 表：auto_copy 的合并链会把连续几段拼成一条再粘贴，
-- 实际交付出去的那串文本在 asr_llm_results 里没有对应行，join 反推不出来。
--
-- 本文件是这张表的权威 DDL：db/schema.rs 用 include_str! 把它整体 execute_batch（全
-- IF NOT EXISTS，幂等，无条件执行）。改表就改这里。后加的列（如 correction_sample_id）
-- 在建表语句里补一份，同时在 schema.rs 里加一条 ALTER 守卫兜住「建表被 IF NOT EXISTS
-- 跳过」的老库。
CREATE TABLE IF NOT EXISTS speech_scenes (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id    TEXT,
  segment_id    INTEGER NOT NULL,   -- 关联 asr_raw_records / asr_llm_results；合并链取末段
  delivery_mode TEXT NOT NULL,      -- auto_paste | auto_copy
  content_kind  TEXT NOT NULL,      -- optimized_zh | english
  text          TEXT NOT NULL,      -- 实际交付出去的文本
  char_count    INTEGER NOT NULL,
  app_exe       TEXT,
  app_path      TEXT,
  app_title     TEXT,
  app_class     TEXT,
  delivered_at  TEXT NOT NULL,
  -- 被哪条纠错样本改过（speech_samples.id）。NULL = 用户未对这次交付主动纠错。
  -- 关联方向：交付在前、纠错在后，纠错样本落库时按 segment_id 回标这里。
  -- 用途：统计用户真实表达风格时排除「被改过」的记录——那条交付文本里含着 ASR/LLM 的错，
  -- 不是用户想说的。语义是「已知被改」vs「未标记」，不是「拒绝」vs「接受」（多数交付
  -- 用户既不改也不采集，落在未标记）。
  correction_sample_id INTEGER
);

CREATE INDEX IF NOT EXISTS idx_speech_scenes_time ON speech_scenes(delivered_at);
CREATE INDEX IF NOT EXISTS idx_speech_scenes_app_time ON speech_scenes(app_exe, delivered_at);
CREATE INDEX IF NOT EXISTS idx_speech_scenes_segment ON speech_scenes(segment_id);
