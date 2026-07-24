-- 语音纠错一键采集：区分样本来源（手工标注 UI vs 专用快捷键复制采集），并记录采集时
-- 涉及的完整 segment_ids（一条样本可能由多段 burst 拼成，segment_id 单列只存首段）。
-- 实际 ALTER 语句在 db/schema.rs::run_migrations 里按列存在性守卫执行，此文件仅作记录。
ALTER TABLE speech_samples ADD COLUMN source TEXT NOT NULL DEFAULT 'ui';
ALTER TABLE speech_samples ADD COLUMN segment_ids TEXT;
