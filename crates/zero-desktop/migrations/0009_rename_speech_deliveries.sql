-- 场景记录表改名：speech_deliveries → speech_scenes。
--
-- 背景：这张表最初叫 speech_deliveries，后随「场景记录」定名改为 speech_scenes。0008 的建表
-- 语句是 CREATE TABLE IF NOT EXISTS，对老库只会**新建一张空表**，改名前攒下的历史行会被留在
-- 旧表里成为孤儿，统计口径直接断档。故本迁移必须在 0008 建表**之前**执行。
--
-- 实际语句在 db/schema.rs::run_migrations 里按表存在性守卫执行，此文件仅作记录：
--   · 只有旧表 → DROP 两个旧索引（0008 会以新名重建同定义索引，留着就是重复索引）后 RENAME，
--     行 id 原样保留（speech_samples.correction_sample_id 等外部引用不受影响）。
--   · 两表并存（先跑过新版、又回滚到老版写了旧表）→ 把旧表行按列子集搬进新表（不带 id，
--     交给自增避免主键冲突）后 DROP 旧表。
-- 两条路径都收敛到「旧表不再存在」，因此天然幂等。
DROP INDEX IF EXISTS idx_speech_deliveries_time;
DROP INDEX IF EXISTS idx_speech_deliveries_app_time;
ALTER TABLE speech_deliveries RENAME TO speech_scenes;
