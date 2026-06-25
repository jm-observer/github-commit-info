# English 跟读判分 — 优化需求 TODO

> 状态:待排期  ·  日期:2026-06-25  ·  范围:`zero-desktop` english 跟读面板 + `toolkit-server` shadow 模块
>
> 关联:[english-shadow-design.md](english-shadow-design.md)(现状 v1 设计) ·
> [english-shadow-gop-design.md](english-shadow-gop-design.md)(发音级评测设计) ·
> [ShadowPanel.tsx](../crates/zero-desktop/ui/src/modules/english/shadow/ShadowPanel.tsx) ·
> [ShadowService.ts](../crates/zero-desktop/ui/src/modules/english/shadow/ShadowService.ts) ·
> [shadow/mod.rs](../crates/toolkit-server/src/shadow/mod.rs)

本文件汇总用户 2026-06-25 提出的跟读判分优化诉求,逐条记录「诉求 / 现状 / 约束 / 落地拆分」,
作为后续排期与验收依据。完成一项就把 `[ ]` 勾掉并补「落地说明」。

---

## TODO-1 开启即全自动,免手动操作

- [ ] **诉求**:用户一旦勾选「开启」,跟读区就不要再有那种需要手动反复点的操作(开始/停止/重读…
  类「复制粘贴式」的重复手操),应当全自动连贯跑下来。
  > 待用户最终确认:此处「复制粘贴的操作」指的是跟读面板里那排**手动按钮**(开始/停止/重读/重听/
  > 跳过)。已按此理解记录;若另有所指(如独立「语音」识别 tab),回头修订本条。
- **现状**:代码其实已基本具备——采集模式选「自动」(VAD 自动判停)+ 勾「通过即自动跳」就能
  「连放→连录→连判→连跳」。但默认没开成全自动,手动按钮始终显示,体验上像要人工驱动。
  见 [ShadowPanel.tsx:370](../crates/zero-desktop/ui/src/modules/english/shadow/ShadowPanel.tsx:370)
  那排操作按钮、[ShadowPanel.tsx:176](../crates/zero-desktop/ui/src/modules/english/shadow/ShadowPanel.tsx:176)
  的 `captureMode === 'auto'` 自动开录分支。
- **落地拆分**(改动小,可先做):
  1. 开启跟读后默认进入「全自动模式」:`captureMode=auto` + `autoAdvanceOnPass=true` 作为开启默认。
  2. 全自动模式下**隐藏/收起**手动按钮排,只保留一个「暂停/退出自动」出口和必要的兜底(跳过)。
  3. 失败时也自动走「重听参考→重读」一轮(可设最多重试 N 次后自动跳过,避免卡死),不需要人点。
  4. 设置项保留「手动模式」开关,给嘈杂环境/调试兜底。
- **验收**:勾「开启」后,除了点一次上方「播放」,全程不用再碰跟读区任何按钮即可一路练下去。

## TODO-2 实时录播 + 实时评分(不等参考音频播完)

- [ ] **诉求**:不要等上方参考音频播完才开始录,应当能实时录、实时给分。
- **现状**:当前是**串行闸门**——参考音频按次数全部播完 → `onAwaitShadow` → 才开录 → 离线整段
  ASR → 出分。见 [AudioPlayerService.ts:240](../crates/zero-desktop/ui/src/modules/english/services/AudioPlayerService.ts:240)。
- **硬约束(必须先解决,否则做不了)**:
  1. **回声污染**:边外放参考音频边开麦,麦克风会把参考音频一起录进去,ASR 分不清参考与跟读。
     - 对策 A(推荐、最省事):要求/提示用户**戴耳机**,参考音频不外漏到麦克风。
     - 对策 B:上**回声消除(AEC)**,工程量大且 webview 内稳定性存疑。
  2. **实时评分要换 ASR 通道**:现走 FunASR **离线整段** `/transcribe`(录完一段才出文字);
     要边说边出分,需接**流式 ASR**(GB10 `:9100` WebSocket,见 CLAUDE.md 语音底座),是另一套管线。
- **落地拆分**(分两步,先小后大):
  1. **第一步(务实)**:不追求"边放边录",但把「放完→自动开录」做到零延迟无感衔接(并入 TODO-1
     的全自动)。先满足"不用手动、连贯"。
  2. **第二步(实时流式,大改,单独立项)**:戴耳机前提下,参考音频播放与录音并行;逐词实时点亮 +
     实时发音分。**已单列设计** → [english-shadow-realtime-design.md](english-shadow-realtime-design.md)
     (方案④流式 GOP:流式声学 + 在线对齐 + WS,整句结束用批量 GOP 出权威分;批量 GOP 是其地基/finalizer)。
     注:**不走流式 ASR**(ASR 会脑补正确词,抹掉发音错误),而是流式**发音评测**。
- **验收**:第一步——放完即录无明显停顿;第二步——戴耳机时可边跟读边看到逐词实时反馈与分数。

## TODO-3 发音级真评分(GOP),替换"只看内容对不对"的 v1 内核

- [ ] **诉求**:想真正「判断用户发音是否标准」。
- **现状(重要,需对齐预期)**:v1 **并不评发音标准度**,只做 **ASR 文本对齐 = 内容/可懂度**。
  口音重但 ASR 仍听对 → 判满分;发音标准但 ASR 抽风听错 → 误判不过。
  见 [shadow/mod.rs:9](../crates/toolkit-server/src/shadow/mod.rs:9) 注释与 `score_sentence`/`score_word`。
- **落地**:这是一次实质升级,接**音素级 GOP(Goodness of Pronunciation)发音评测**,
  设计单列于 → [english-shadow-gop-design.md](english-shadow-gop-design.md)。接口形状保持不变
  (设计文档 §10 Phase 3 已预留:替换打分内核即可)。
- **设计已细化(契约拍板,见 GOP 设计文档)**——动手前直接照以下定论,勿再走老坑:
  - **评测=语音声学模型(wav2vec2-GOP),非 LLM**;与 `:9101` FunASR 两套独立模型(GOP §2)。
  - **传输分层**:外部 `POST /api/web/shadow/score` **维持 raw body + query 不动**,
    **只有 toolkit-server → :8098 `/assess` 才转 multipart**——别误改桌面端协议(GOP §3)。
  - **数值全链路 `0~1`**(对齐 v1),仅 UI `×100` 展示(GOP §2 决策 5)。
  - **发音三档另起新字段 `pron_status`(ok/warn/bad),不动既有 `status`(ok/wrong/missing)**——
    后者是严格联合类型,重载会破前端(GOP §5)。
  - `passed = sentence_score >= threshold && bad_phone_count == 0`(GOP §2 决策 6)。
  - **`GOP_BASE_URL` 未配 → 一定回退 v1;配了不可达 → 502**(GOP §4)。
  - **`shadow_attempt` 加 `detail_json` 走幂等 `ALTER TABLE ADD COLUMN`,不 bump `SCHEMA_VERSION`**
    (遵循 schema.rs 惯例,GOP §5)。
  - 错读诊断以结构化 `expected_ph`/`actual_ph` 为准,`hint` 仅展示文案;`transcript` 为 optional 不可依赖。
  - **声纹门控(是否本人在读)v2 不做**,列为开放项(GOP §2 决策 7 / §7)。
- **落地进度(2026-06-25)**:本仓侧 **Phase B + C 已实现**(等 streaming-speech 仓 Phase A 的
  `:8098 /assess` 服务到位即可端到端启用):
  - Phase B(toolkit-server):`ScoreBackend{AsrAlign,Gop}` 按 `GOP_BASE_URL` 切换
    ([shadow/mod.rs](../crates/toolkit-server/src/shadow/mod.rs));GOP 代理
    [shadow/gop.rs](../crates/toolkit-server/src/shadow/gop.rs)(raw→multipart 转发 `:8098`,
    未配回退 v1 / 不可达 502);`ScoreResult`/`WordResult` 兼容扩展(新增 `pron_status`/`score`/
    `phones`/`bad_phone_count`/`model`,全 `Option`,不动 `status`);`shadow_attempt` 加
    `detail_json`(幂等 ALTER,不 bump,[migrations.rs](../crates/toolkit-core/src/migrations.rs))。
  - Phase C(zero-desktop):types 扩展 `pron_status`/`phones`/`ShadowPhoneResult`;
    [ShadowPanel.tsx](../crates/zero-desktop/ui/src/modules/english/shadow/ShadowPanel.tsx) 按
    `pron_status` 上色 + 错读音素 hint,GOP 未启用回退 `status` 零回归。
    `detail_json`(幂等 ALTER,不 bump,[migrations.rs](../crates/toolkit-core/src/migrations.rs))。
  - Phase A(streaming-speech 仓,**服务骨架已落地**):`server/pronunciation-assess/` —— `gop.py`
    (G2P + wav2vec2 CTC + forced_align + GOP + 标定 + 聚合,torch 惰性导入)+ `app.py`(aiohttp
    `:8098 /assess`,模型常驻 + Semaphore(1) + 503/504)+ Dockerfile/compose + 契约文档
    `streaming-speech/docs/pronunciation-assess-api.md`。无 GPU 单测 8+7 全绿。
  - 验证:`cargo test`(toolkit-core 6 + toolkit-server 38,含 GOP 映射 / passed 判定 / 迁移幂等)+
    前端 `tsc --noEmit` + Python 单测(test_gop 8 + test_app 7)全绿。
  - **未完(需 GB10 实机)**:Phase A 的「speechocean762 真标定 + 拉模型确认 vocab」+ Phase D
    端到端 runbook(真录音含故意错读 th→s);故本条暂不勾。
- **验收**:能给出音素/单词/句子三级发音分,并指出具体错读音素(如「th 发成 s」),而不仅是词对错。

---

## 优先级建议

1. **先做 TODO-1 + TODO-2 第一步**(同一改动批次,纯前端 + 交互,风险低、见效快)。
2. **再评估 TODO-3 GOP**(需新增 GB10 发音评测服务,跨 streaming-speech 仓协作,见 GOP 设计文档)。
3. **TODO-2 第二步(实时流式)** 体验最理想但成本最高,放最后,且依赖"戴耳机"前提先达成共识。
