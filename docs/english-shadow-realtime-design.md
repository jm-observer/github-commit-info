# English 跟读实时发音评测(流式 GOP)设计

> 状态:设计稿(待评审)  ·  日期:2026-06-25  ·  范围:GB10 新增**流式**发音评测能力 +
> `toolkit-server` shadow WS 中继 + `zero-desktop` 边读边评前端
>
> 关联:[english-shadow-gop-design.md](english-shadow-gop-design.md)(批量 GOP,本稿的地基与"压舱石") ·
> [english-shadow-todo.md](english-shadow-todo.md)(TODO-2 实时录评) ·
> streaming-speech `server/pronunciation-assess/`(现批量服务) ·
> streaming-speech `docs/pronunciation-coach-overview.md`(概念)

## 1. 目标与一句话定义

把发音评测从「**批量**:读完整句 → 一次性出分」升级为「**流式**:边读边逐词/音素点亮,
说完即有结果」——对应 [todo](english-shadow-todo.md) TODO-2 的「实时录、实时评」终态(方案④)。

**核心体验**:用户对着已知参考文本朗读,词随着读到陆续上色(绿/黄/红),错读音素实时高亮;
整句读完时分数落定。对标 ELSA / Speak 的即时反馈闭环。

明确**不做**(留后续):自由对话评测、多说话人、超音段(语调/重音/流利度)的实时评——
本稿只把**分段音准(segmental)的流式化**做扎实。

## 2. 关键决策(待拍板,给出推荐)

> **本稿是 [批量 GOP](english-shadow-gop-design.md) 的流式扩展,不是另起炉灶。** 批量版的
> 「G2P + 标定 + 聚合 + 输出契约 + 前端」整套复用;本稿只新增「流式声学 + 在线对齐 + WS 传输」。
> 见 §5「复用边界」。

| # | 决策点 | 推荐取值 | 备选 / 理由 |
|---|---|---|---|
| 1 | 整体架构 | **流式出即时反馈 + 整句落定后批量重算权威分(混合)** | 临时分给体验、批量分给准确度/落库;批量 GOP 成为 finalizer + 校准 oracle(见 §4) |
| 2 | 流式声学模型 | **先试分块 wav2vec2 + 注意力掩码 lookahead**(改造现有 timit-phoneme 模型) | 备选:流式 CTC/RNN-T 音素模型(properly,需找/训模型)。**模型可得性是头号 gating 风险**,Phase 0 先验证 |
| 3 | 强制对齐 | **在线/帧同步对齐**:对期望音素图做流式 Viterbi + 部分回溯,边界随新帧滚动修正 | 离线 `torchaudio.forced_align` 不能用(要整段);这是本稿最实的新算法 |
| 4 | 传输 | **WebSocket**:音频块上行、增量分事件下行 | 批量的 HTTP multipart 不适合流;复用 orchestrator 已有的 WS nest 经验(`/api/asr/stream`) |
| 5 | 分数标定 | **复用批量的 `Calibration`/speechocean762 框架,重拟 a/b**(流式模型原始分分布不同) | 不重造标定;但流式 GOP 原始分语义变了,阈值要重标(Phase 1 用批量分做 oracle 对齐) |
| 6 | 临时分 vs 权威分 | 流式途中发**临时分**(允许抖动、随对齐落定 finalize);整句结束触发批量 `/assess` 出**权威分**覆盖 + 落库 | 落库只认权威分,避免临时抖动污染统计 |
| 7 | 录音前提 | **戴耳机 / 播完再录**(沿用 TODO-1/2 硬约束);回声是流式致命伤 | 边外放参考边开麦 → 回声进 GOP;AEC 工程量大、webview 内稳定性存疑,暂不依赖 |

## 3. 整体链路

```
zero-desktop(webview)        toolkit-server                 GB10 流式评测(:8098 WS)
─────────────────────        ──────────────                 ────────────────────────
麦克风流 ──WS 音频块──▶ /api/web/shadow/stream ──WS 中继──▶ /assess/stream(新增)
 (16k PCM 分块)            (鉴权 + host 派生)                 分块声学 + 在线对齐 + 临时 GOP
逐词实时点亮 ◀── 增量分事件 ◀────────────────── ◀── partial 事件(words[]/phones[] 增量)
                                                            (整句结束)
读完 ──"end"──▶ 触发一次批量 /assess(现有) ──▶ 权威分覆盖 + 落 shadow_attempt
```

- **传输分层(同批量稿 §3 的精神)**:外部桌面端 ↔ toolkit-server 走 **WS**(toolkit-server 作
  网关,统一鉴权/host 派生);**只有 toolkit-server ↔ :8098** 是内部 WS。
- **混合**:流式途中是临时分(秒级点亮);整句结束**复用现有批量 `/assess`** 出权威分,覆盖临时分、
  落库。用户感知不到切换。

## 4. 核心架构:批量 GOP 当"压舱石"

生产级流式评测的标准形态,不是"流式取代批量",而是**流式套在批量外面**:

```
说话中 ── 流式声学 + 在线对齐 ──▶ 逐词/音素「临时分」(秒级,允许抖动)
说完一句 ── 触发现有批量 /assess ──▶「权威分」(覆盖临时分,落库用这个)
```

- **批量 GOP = finalizer**:整句权威分仍由可信的批量链路给,流式部分再糙,最终分不失真。
- **批量 GOP = 校准 oracle**:流式分应逼近批量分,这是验证流式模型/对齐对不对的标尺(Phase 1)。
- 因此本稿**风险可控**:流式是"加一层即时反馈",底座(准确度/落库/契约)不动。

## 5. 复用边界(批量 GOP 哪些留、哪些换)

| 模块 | 流式是否复用 | 说明 |
|---|---|---|
| G2P(ref_text→ARPAbet) | ✅ 原样 | streaming-speech `gop.py` 纯函数区 |
| 标定 `Calibration` + speechocean762 | ✅ 框架复用,**重拟 a/b** | 流式原始分分布不同 |
| 聚合 音素→词→句 | ✅ 原样 | 纯逻辑 |
| 输出契约(words/phones/pron_status/expected_ph/actual_ph/hint/bad_phone_count) | ✅ 复用为**事件 schema** | 从"一次性 JSON"变"增量推送同字段" |
| IPA↔ARPAbet 桥、hint 拼装 | ✅ 原样 | |
| 前端 ShadowPanel 渲染 | ✅ 大部分留 | 加「未定→趋稳」过渡态 |
| toolkit `ScoreResult`/`PhoneResult` | ✅ 复用 | finalize 仍走它;事件用其子集 |
| 声学模型(wav2vec2 双向) | ❌ 换 | 非流式;换分块/流式音素模型(决策 2) |
| 强制对齐(forced_align 离线) | ❌ 重写 | 在线帧同步对齐(决策 3) |
| 传输(HTTP multipart) | ❌ 换 WS | 决策 4;toolkit gop.rs 从 HTTP 代理 → WS 中继 |
| 峰值 GOP 计算 | ⚠️ 概念留、时机变 | span 未闭合 → 先临时分、对齐落定 finalize |

一句话:**「语言/打分/契约/前端」整套活;「声学+对齐+传输」是新工程。**

## 6. WS 契约(草案,`:8098 /assess/stream`)

照 orchestrator `/stream` 的"首帧 hello + 二进制音频 + 文本事件"风格。

**上行**:
| 帧 | 内容 |
|---|---|
| `hello`(JSON,首帧) | `{ "type":"hello", "ref_text":"I think so", "granularity":"word", "sample_rate":16000, "lang":"en" }` |
| audio(二进制) | 16k 单声道 PCM s16le,建议每帧 ~200–320ms |
| `end`(JSON) | `{"type":"end"}` → 触发整句 finalize |

**下行**(JSON 事件,`type` 区分;分值 0~1):
| type | 关键字段 | 含义 |
|---|---|---|
| `ready` | — | 已就绪,可推音频 |
| `partial` | `words[]`(同批量 schema,可含部分词的临时 `pron_status`/`score`/`phones`) | 增量临时分;前端按词 id/序号增量刷新 |
| `final` | 完整 `{sentence_score, words[], bad_phone_count, model}`(= 批量 `/assess` 响应) | 整句权威分,覆盖临时分 |
| `error` | `code`/`message` | 错误 |

> `partial` 事件复用批量响应的**字段子集**;`final` 事件就是批量 `/assess` 的完整响应。
> toolkit-server 中继时:`final` 走现有 `ScoreResult` 落 `shadow_attempt`(临时分不落库)。

## 7. toolkit-server 侧改动

- **新增 WS 端点 `/api/web/shadow/stream`**:中继桌面端 ↔ `:8098 /assess/stream`,透传 hello/音频/事件;
  鉴权 + host 派生同现有 `/score`。参照 orchestrator 在 `/api/asr` 下 nest WS 的做法。
- **finalize 落库**:收到 `final` 事件 → 复用 `shadow::store::record_attempt`(权威分,`detail_json` 照旧)。
- **后端选择**:`GOP_BASE_URL` 未配 → 流式端点直接 503/不可用(流式无 v1 回退;v1 没有流式语义)。
  批量 `/score` 仍按现有 `ScoreBackend` 回退 v1,**两条路径独立**。
- **trace**:WS 会话级 span(`shadow_stream`)。

## 8. 前端(zero-desktop ShadowPanel)

- **采集改流式**:`ShadowService` 现在是"录完整段 → POST";新增"边录边推 PCM 块到 WS"通道
  (`captureUtterance` 旁路一个流式版,或新建 `streamUtterance`)。
- **增量渲染**:`partial` 事件到达 → 按词增量上色;词进入"已定"前用浅色/动画表"评估中"。
- **落定**:`final` 事件 → 用权威分覆盖,走现有 ShadowPanel 展示逻辑(音素 hint 等)。
- **降级**:WS 不可用(未配 / 旧服务)→ 回退现有"录完整段批量评测"(方案①)。零回归。

## 9. 风险与取舍

- **流式音素模型(原头号风险,已显著收敛)** — Phase 0/0.5(见 §10)证明分块 wav2vec2 在「长左 +
  ~160ms 右瞻」下**声学后验**够用、**延迟够快**。风险从「模型不可用」收敛后,各分项现状:
  - ① **延迟/算力** — ✅ 短句已测够快(p95<100ms@20s,实时 0.17x);仅长段/多并发需状态缓存。
  - ② **上下文保留** — ✅ 句级用增长前缀重算即可(O(n²) 在短句无感);长段才需缓存。
  - ③ **在线对齐稳定性** — 🟡 **Phase 1 原型 GREEN,未完全解除**。在线-final ≈ 批量(翻判 0–3.3%,真人 1.9–2.5%)、
    **committed→final 跳变 0%**、延迟可压 ~80ms。但 **commit 前 live 临时分在错读/模糊音素会抖**(swing ~0.68、
    偶发 live 翻判 + early-commit)→ UX 须 tentative 渲染。仍需:更多真人样本、live 抖动 UX 策略、early-commit 兜底。
  - ④ **回声** — ⚠️ 实测会推高分歧 + 引入对齐漂移(全左 top1 88%、漂移 0/3 帧);**戴耳机/AEC 是硬前提**。
  - ⑤ **真人音频** — 🟡 已起步(LibriTTS 2 条:翻判 1.9–2.5%,与合成一致);仍需清晰/中式/错读各组扩量。
  **总评:模型/延迟两块 GREEN;在线对齐是"原型 GREEN、落定后稳",但 commit 前 live 抖动 + 回声 + 真人扩量
  仍是进 Phase 2 要带着解决的工程/验证项。不是"风险全清",但方案④的核心地基(模型可用 + 对齐可行)已立。**
- **在线对齐难**:帧同步 Viterbi + 部分回溯,边界随新帧滚动;CTC 尖峰 + 流式不确定性叠加,易抖。
  临时分允许抖动、由 finalize 兜底是关键缓解。
- **回声/录音质量**:流式致命伤。必须戴耳机或上 AEC;否则参考音混入 → 评测失真。
- **延迟预算**:分块 + 增量推理 + WS 往返要压在"点亮跟得上嘴"(目标 < 300–500ms/块)。
- **标定漂移**:流式原始分与批量不同分布,阈值要重标;Phase 1 用批量分做 oracle 对齐。
- **范围蔓延**:只做分段音准的流式化;超音段、自由对话留后续。

## 10. 分阶段落地(方案④,分步 de-risk)

- **Phase 0 — 流式声学可行性(streaming-speech)** ⚠️ **初步通过,谨慎 GO;只验了声学后验,未验对齐(2026-06-25,GB10)**
  脚本:`server/pronunciation-assess/phase0_streaming_check.py`(整段 oracle vs 分块+左上下文+右瞻)。
  4 句(7~30 音素,CosyVoice 中文嗓读英文)结果:
  - **全左上下文 + 160ms 右瞻**:top1 ≈ 95%、(固定 oracle 对齐下)status 翻判 ≈ 6%。
  - **左上下文砍到 640ms**:top1 ≈ 80%、翻判 ≈ 30% → 崩。
  - **核心发现:决定精度的是「左上下文」,不是右瞻。** 但"全左上下文"在工程上有两种实现,代价不同:
    ① 每块在**增长前缀**上重跑模型 = 可模拟流式,但算力随句长 O(n²) 增长;② 真·**状态缓存**流式
    需确认模型结构能安全缓存(transformer self-attention + 卷积前端边界),**不是改一下就有**。
  - **架构 + 成本双约束**:流式编码器须**保留完整/长因果左上下文** + 仅右侧 ~160ms lookahead;
    **不可用小滑窗砍左上下文**。短句可用①增长前缀重算;长段 / 多并发须②状态缓存模型或半流式
    sliding-window finalization。
  - **本 Phase 严格只证明了一件事**:分块 wav2vec2 在「长左 + 小右瞻」下,**声学后验**与整段足够接近,
    值得进 Phase 1 原型。**没有证明**的(都留到 Phase 1/0.5):
    - **在线对齐稳定性**:翻判指标复用了整段 oracle 的 alignment(只换后验),**在线 Viterbi 的抖动
      未被验证**——这是方案④的另一半风险,Phase 1 必须真跑在线对齐再测翻判。
    - **延迟/算力**:未测 5/10/20s 音频下每块 p50/p95 推理耗时(增长前缀重算的隐性成本)。
    - **样本与指标**:4 条合成带口音音频太薄;需**真人清晰 / 真人中式 / 故意错读 / 噪声回声**各一组;
      指标不能只看 status 翻判(贴阈值会放大),要并看 raw 分差 / margin-to-threshold / top-k / 后验 KL。
  - **结论(措辞收紧)**:头号风险**从「模型不可用」收敛为「上下文保留 + 在线对齐 + 延迟」工程风险**
    ——是好消息,但**不到"不必换/训模型"的强结论**,生产化仍取决于上述未验项。
- **Phase 0.5 — 补全 Phase 0 留白(streaming-speech)** ✅ **已测(2026-06-25,GB10)**
  脚本扩展:`phase0_streaming_check.py` 加 `--mode latency`(增长前缀逐块计时 + 理想缓存下界)、
  `--augment noise|echo`、富指标(top1/top3/KL/分差/oracle 对齐翻判/**流式对齐翻判**/翻判贴阈值占比/对齐漂移)。
  - **延迟(GREEN,短句)**:增长前缀(全左)每块 p95 = 27 / 48 / 97ms @ 5 / 10 / 20s,**全部 << 320ms 块预算**,
    实时率 0.07–0.17x。理想缓存恒 ~17ms。**结论:句级跟读(2–6s)增长前缀重算够用,v1 不必状态缓存**;
    缓存只在长段(>20–30s)/ 多并发(Semaphore(1) 串行,~3 路即争用)才需要。
  - **精度(全左上下文,GREEN)**:top1 93–97%、top3 ~100%、分差 ~0.01–0.02、翻判 ~3%、对齐漂移 0/0。
    **故意错读 th→s**:full-left 流式与批量**判定完全一致**(翻判 0%、分差 0.000,TH=0.0 bad 被保留)
    → 流式不抹平真错误。
  - **关键细化(对 GPT 的"对齐风险"的部分回应)**:左上下文受限时,**oracle 固定对齐**翻判 ~50%,但
    **在流式后验上重对齐**翻判降到 ~10% → 「对齐与打分用同一份流式后验」能自纠大部分漂移,
    在线对齐风险**比预想小**。但此处"流式对齐"仍是流式后验上的**离线 forced_align**(全局 Viterbi),
    **非真帧同步在线**,留 Phase 1 验。
  - **翻判指标确被阈值放大(GPT 对)**:翻判中贴阈值占比 67–100%,全左下真实分差极小(~0.02)→ 翻判多为
    边界音素的"档位擦边",非实质分歧。
  - **噪声 OK / 回声是真威胁**:+噪声 SNR15 全左 top1 93.9%(稳);**+回声**全左 top1 88.3%、翻判 6.7%、
    漂移 0/3 帧 → 回声会推高分歧 + 引入对齐漂移。**强化"戴耳机 / AEC"前提**(决策 7)。
  - **仍欠**:① 真·帧同步在线 Viterbi(Phase 1);② **真人**音频(目前全合成);③ 真人中式/方言口音组。
- **Phase 1 — 在线对齐 + 临时 GOP 原型(streaming-speech)** ✅ **原型 GREEN,值得进 Phase 2(2026-06-25,GB10)**
  脚本:`phase1_online_align.py`——期望音素 CTC 图上的**帧同步在线 Viterbi + 逐帧回溯**(真在线对齐,
  **非 oracle、非流式后验上的离线 forced_align**);音素离开前沿 `commit` 帧后落定;离线模拟分块喂入。
  按 review 已补**四个更诚实的指标**:接受终态 final、missing phone、signed lag + early-commit、live tentative 抖动。
  - **在线对齐 ≈ 批量**:全左上下文 在线-final vs 批量 翻判 **0–3.3%**、分差 ~0.01–0.02、**missing 0**
    (final 从 CTC 接受终态回溯,非末帧任意态)。真人 LibriTTS 复跑(5s 女 / 10s 男)翻判 **1.9% / 2.5%**、
    分差 0.019 / 0.025 → 真人样本同样漂亮。
  - **committed → final 跳变 0%**(分差 0.000,所有配置)→ 落定后分数不跳;延迟可压到 ~80ms 中位
    (commit=2;真人 p50 80 / p95 140–161ms),early-commit 在干净句 0。
  - **但"落定前 live"不稳(诚实):** 干净音素 live 抖动 0;**错读/模糊音素 commit 前会抖**(swing ~0.68、
    偶发 live 翻判 1/7,并抓到 1 次 early-commit / prov_end 早 19 帧)。即「committed→final 稳」**不等于**
    「实时临时分稳」——UX 须把 commit 前渲染为 **tentative(评估中)**,落定才显确定判定。
  - **压力测(左砍 640ms)**:翻判 10% 但 committed→final 跳变仍 0 → 退化来自**声学**(左上下文不足,已知),非对齐不稳。
  - **结论(措辞收紧)**:Phase 1 通过的是「**在线 Viterbi 原型值得继续**」,**不是**「在线对齐风险完全解除」。
    进 Phase 2 前/中仍需:① 更多**真人**(清晰/中式/错读)样本扩翻判与 live 抖动统计;② **commit 前 live 抖动**
    的 UX 策略(tentative 渲染 + 可能的滞后阈值);③ early-commit 兜底;④ 标定 a/b 重拟(用批量当 oracle)。
- **Phase 2 — WS 链路** ⚙️ **代码完成(2026-06-25)**
  - **2a 服务端(streaming-speech)** ✅ **GB10 实测通过**:`streaming.py`(`StreamingAssessor`:增量
    push 全左重算后验 + 在线 Viterbi + 落定 → partial;`finish` 调批量 `gop.assess` 出 final)+ app.py
    `GET /assess/stream` WS(hello/binary/end → ready/partial/final)+ `test_stream_client.py`。
    实时模式实测:partial 随"说话"渐进流入(词逐个点亮)、final 在 end 后 ~0.8s 出;契约入
    `docs/pronunciation-assess-api.md`。
  - **2b 中继(toolkit-server)** ⚙️ **代码完成(编译 + 12 测试 + fmt 绿),运行待 GB10 redeploy**:
    `shadow/stream.rs`——axum `GET /api/web/shadow/stream` 升级 WS,`tokio-tungstenite` 连 `:8098
    /assess/stream` 双向转发;**拦截 `final` 落库**(`gop::score_result_from_final` → `record_attempt`,
    权威分);`GOP_BASE_URL` 未配 → 503(流式无 v1 回退)。单元元信息走 WS query。
    **待**:toolkit-server 重新部署到 GB10 后端到端验证(桌面 WS → 中继 → :8098)。
- **Phase 3 — 前端边读边评(zero-desktop)**
  6. 流式采集 + 增量渲染 + WS 不可用降级;`final` 落定走现有展示。
- **Phase 4 — 混合 finalize + 验收**
  7. 整句结束触发批量 `/assess` 出权威分覆盖 + 落库;补 `docs/runbook-shadow-realtime-e2e.md`
     (真录音边读边点亮 → 故意错读实时高亮 → 整句权威分落定)。

## 11. 与批量 GOP / v1 的关系(一句话)

v1(ASR 文本对齐)与批量 GOP(本族 [gop 设计](english-shadow-gop-design.md))**都不删**:批量 GOP 是
本稿的**地基 + finalizer + 校准 oracle**,v1 是无 GPU/嘈杂环境兜底。流式是叠加的"即时反馈"层,
**输出契约与批量一致**,WS 不可用时平滑回退批量评测。
