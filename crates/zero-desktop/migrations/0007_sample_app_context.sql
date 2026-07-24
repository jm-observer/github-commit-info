-- 同音字纠错 P0（数据收集）：记录样本交付时所处的应用上下文。
--
-- 设计见 docs/2026-07-24-homophone-correction/design.md §1。要点：
-- 1. 取值时刻 = 实际交付动作发生的那一刻（auto_paste 打字前 / auto_copy 按下 Ctrl+V 时），
--    不是收到 LLM 优化结果时——两者间隔 1~2 秒，用户可能已切换窗口。
-- 2. 收集期**宽记原始事实、不做归类**：不加 app_profile（编程/聊天/写作 之类的分组）。
--    分组维度应由收集到的数据决定，不能先拍脑袋定；将来再加列回填即可，原始信息都在。
-- 3. app_title 含聊天对象名/文档名/网页标题，只落本地库；导出 JSON 会带出来。
--
-- 实际 ALTER 语句在 db/schema.rs::run_migrations 里按列存在性守卫执行（沿用 0006 的做法），
-- 此文件仅作记录。
ALTER TABLE speech_samples ADD COLUMN app_exe       TEXT;  -- "Code.exe"
ALTER TABLE speech_samples ADD COLUMN app_path      TEXT;  -- 全路径，区分同名 exe
ALTER TABLE speech_samples ADD COLUMN app_title     TEXT;  -- 窗口标题（浏览器场景唯一线索）
ALTER TABLE speech_samples ADD COLUMN app_class     TEXT;  -- 窗口类名，如 Chrome_WidgetWin_1
ALTER TABLE speech_samples ADD COLUMN delivery_mode TEXT;  -- auto_paste | auto_copy
