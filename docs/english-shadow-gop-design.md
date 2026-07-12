# English 跟读发音级评测(GOP)设计

> 状态:设计稿(待评审)  ·  日期:2026-06-25  ·  范围:新增 GB10 发音评测服务 +
> `toolkit-server` hybrid shadow 判分聚合 + `zero-desktop` 前端展示
>
> 关联:[english-shadow-design.md](english-shadow-design.md)(v1 现状,§10 Phase 3 预留本升级) ·
> [english-shadow-todo.md](english-shadow-todo.md)(TODO-3) ·
> [shadow/mod.rs](../crates/toolkit-server/src/shadow/mod.rs)(当前 v1 ASR 文本对齐内核) ·
> CLAUDE.md 语音底座 / GB10 服务清单

## 1. 目标与一句话定义

把跟读判分从「v1 的 ASR 文本对齐(只看**内容/可懂度**)」升级为「**内容正确性 +
音素级发音评测**」的 hybrid 判分:

- **ASR 内容判定**回答:用户读的是不是这句话、有没有漏词/错词。
- **GOP 发音判定**回答:这些词的音发得像不像,并定位具体读错的音素(如 `think` 的 /θ/
  发成 /s/)。
- **产品通过判定**由 toolkit-server 聚合两类分数,而不是让 GOP 单独替代 v1。

**核心手段**:GOP(Goodness of Pronunciation)——把参考文本经 G2P 展开成「应当发出的音素序列」,
对用户录音做**强制对齐(forced alignment)**,用声学模型在每个音素时间段上的**后验概率**衡量
「这个音发得有多像标准音」,得到逐音素 GOP 分,再自底向上聚合到词、句。GOP 只负责
**发音诊断**,不直接决定最终 `passed`。

明确**不做**:不追求和雅思/托福口语官方评分严格对标;不做韵律(语调/节奏/重音)完整评测——
v2 先把**分段音准(segmental)**做扎实,**超音段(suprasegmental)** 留 v3。

## 2. 关键决策(待拍板,给出推荐)

> **评测用的是「语音声学模型」,不是大模型(LLM)。** GOP 走「声学后验 + 强制对齐」路线,
> 与本仓公共大模型层(vLLM / OpenAI 兼容通道)**无关**,不读 `LLM_*` 配置、不消耗 LLM 额度。
> 且这套声学模型与现网 `:9101` FunASR(Paraformer/SenseVoice,服务 v1-ASR 文本对齐)**是两套
> 独立模型**:GOP 服务需新起 wav2vec2,权重/运维归 streaming-speech 仓。

| # | 决策点 | 推荐取值 | 备选 / 理由 |
|---|---|---|---|
| 1 | 部署形态 | **GB10 上新增独立发音评测微服务**(端口 `:8098`),由 **streaming-speech 仓**维护,照 audio-cleanup/FunASR 的模式 | 不塞进 toolkit-server 进程(模型重、依赖 GPU/Kaldi/torch);本仓只做代理 + hybrid 聚合 |
| 2 | 声学模型 / 算法 | **wav2vec2 / SSL + CTC 音素后验 的 GOP**(GPU 友好,贴合 GB10;speechocean762 上有成熟配方;响应 `model` 标识如 `wav2vec2-gop-v1`)。**非 LLM**,与 `:9101` FunASR 独立 | 备选 Kaldi nnet3 经典 GOP(成熟但运维重、CPU 链路);二者皆"对齐+后验"范式,接口形状一致 |
| 3 | G2P(参考文本→音素) | **CMUdict 词典优先 + g2p_en 兜底未登录词**(ARPAbet 音素集) | 在线 G2P 不稳;词典覆盖足够日常英语跟读 |
| 4 | 强制对齐 | **复用声学模型自身的 CTC/对齐**(wav2vec2 segmentation),不引第二套 MFA | 单模型链路短、依赖少;MFA 作为离线校验备选 |
| 5 | 分数标定 | 用 **speechocean762**(开源 L2 英语发音评测集,带音素/词/句三级人工分)做映射标定到 `0..1` | 后端契约统一 `0..1`;前端展示时乘 100 成"71 分";通过线用真实数据校准 |
| 6 | 通过判定 | **toolkit-server 聚合判定**:`content_score >= 内容阈值` + `pronunciation_score >= 发音阈值` + `severe_phone_count <= 上限` | ASR 与 GOP 互补:ASR 防漏词/错词,GOP 防发音不准;不让任一模型单独承担全部语义 |
| 7 | 声纹门控(是否本人) | **v2 不做**:`/assess` 只评"发音标不标准",不校验"是不是本人在读"。留作可选前置门控(见 §7) | 语音底座已有声纹能力(FunASR `:9101 /embed` + orchestrator `app.db` 声纹门控);若产品需防"放原音/代读刷分",再在 `/assess` 前挂一道声纹比对,不与发音评测耦合 |
| 8 | scorer 模式 | 新增 **`SHADOW_SCORER=asr\|gop\|hybrid`**,默认 `asr`;`GOP_BASE_URL` 仅提供 GOP 上游地址 | 支持灰度、回滚、A/B 调阈值。`hybrid` 是目标形态;`gop` 只作调试/实验 |

## 3. 整体链路

```text
zero-desktop(webview)        Tauri 后端            toolkit-server                         GB10 服务
─────────────────────        ──────────            ────────────────                       ───────
采集用户录音 ──invoke──▶ english_shadow_  ──HTTP──▶ shadow::score ──multipart──▶ FunASR :9101 /transcribe
 (MediaRecorder→blob)        score(代理)              │  内容判定(content_score)
                                                     │
                                                     └──multipart──▶ GOP :8098 /assess
                                                        发音诊断(pronunciation_score, phones[])

渲染内容/发音双分 + 音素高亮 ◀──────────── 聚合 { score, passed, content_score,
 + 错读音素提示                             pronunciation_score, words[], stat } 落库
```

- **不改桌面端对外 HTTP 形状**:`POST /api/web/shadow/score` 与 `GET /api/web/shadow/stats`
  路径、query、raw audio body 维持现状;只有 **toolkit-server → GOP 服务** 这一跳使用 multipart。
- **兼容扩展响应**:既有 `score`、`passed`、`words[].status` 保留语义;新增
  `content_score`、`pronunciation_score`、`pron_status`、`phones[]`、`models` 等字段。
  老前端忽略新增字段照常工作,新前端使用新增字段展示发音诊断。
- **新增 GB10 服务**:发音评测自成一个微服务,内部封装"G2P + 对齐 + GOP + 标定",
  对外只暴露一个 `POST /assess`。部署/模型权重/运维归 streaming-speech 仓,本仓持消费侧契约。

## 4. GB10 发音评测服务契约(`:8098`,streaming-speech 维护)

照 audio-cleanup(`/clean`)的 multipart + 头部元信息风格。

### `POST /assess`

- **请求**:multipart——`audio`(用户录音字节,WebM/Opus 或 WAV) + 字段 `ref_text`(参考文本)
  + 可选 `lang`(默认 `en`)+ 可选 `granularity`(`sentence|word`,影响返回详尽度)。
- **响应**:`application/json`:

```jsonc
{
  "ref_text": "I think so",
  "pronunciation_score": 0.71,
  "severe_phone_count": 1,
  "words": [
    {
      "ref": "I",
      "pronunciation_score": 0.95,
      "pron_status": "ok",
      "phones": [
        { "expected": "AY", "ipa": "aɪ", "score": 0.95, "status": "ok" }
      ]
    },
    {
      "ref": "think",
      "pronunciation_score": 0.42,
      "pron_status": "bad",
      "phones": [
        {
          "expected": "TH",
          "ipa": "θ",
          "score": 0.18,
          "status": "bad",
          "actual": "S",
          "actual_ipa": "s",
          "hint": "/θ/ 听起来像 /s/"
        },
        { "expected": "IH", "ipa": "ɪ", "score": 0.71, "status": "ok" },
        { "expected": "NG", "ipa": "ŋ", "score": 0.40, "status": "warn" },
        { "expected": "K",  "ipa": "k", "score": 0.66, "status": "ok" }
      ]
    },
    {
      "ref": "so",
      "pronunciation_score": 0.88,
      "pron_status": "ok",
      "phones": [ /* ... */ ]
    }
  ],
  "model": "wav2vec2-gop-v1"
}
```

- `pron_status` / phone `status` 三档:`ok`(达标) / `warn`(偏弱) / `bad`(明显错读)。
  阈值在服务内按标定给。
- `actual` / `actual_ipa` / `hint`:可选,从"期望音素 vs 实际最可能音素"映射出,用于前端提示。
- GOP 服务**不返回最终 `passed`**。最终通过与否由 toolkit-server 按 scorer 模式和阈值聚合。
- **未配置 / 不可达**:`SHADOW_SCORER=asr` 时不调用 GOP;`hybrid` / `gop` 模式下未配置
  `GOP_BASE_URL` 返回 503;配置了但上游不可达返回 502。线上默认 `asr`,保证未配置时不破现网。

### 服务内部流水线(streaming-speech 仓实现,本仓不持有)

```text
ref_text ──G2P(CMUdict+g2p_en)──▶ 期望音素序列(ARPAbet)
user_audio ──解码/重采样 16k──▶ 声学特征
              │
              ▼
   wav2vec2 + CTC 音素后验 ──forced align──▶ 每音素时间段 + 后验
              │
              ▼
   逐音素 GOP = f(canonical 音素后验, 时长归一) ──标定(speechocean762)──▶ 0..1
              │
   聚合:音素 → 词(均值/最低分加权) → 句
```

## 5. toolkit-server 侧改动(`crates/toolkit-server/src/shadow/`)

判分内核做成**可切换 / 可聚合 scorer**,接口形状不变:

```rust
// 现状:mod.rs 的 score(kind, ref, hyp, threshold) 是 v1-ASR 文本对齐。
// 升级:抽象 ShadowScorer,按 env 选择。
enum ShadowScorer {
    Asr,    // 只走 FunASR + 文本对齐,保持 v1 行为。
    Gop,    // 只走 :8098 /assess,用于调试/实验,不推荐线上单独使用。
    Hybrid, // FunASR 内容分 + GOP 发音分并行,由 toolkit-server 聚合。
}
```

- **ScoreResult 向下兼容扩展**:
  - `score` / `passed` 仍是产品层最终结果。
  - 新增 `content_score: Option<f64>`、`pronunciation_score: Option<f64>`、
    `severe_phone_count: Option<u32>`、`models: Option<ScoreModels>`。
  - `WordResult.status` 保持 `ok|wrong|missing`,只表达内容对齐状态。
  - `WordResult` 新增可选 `content_score`、`pronunciation_score`、`pron_status`、
    `phones: Option<Vec<PhoneResult>>`。
- **配置**:
  - `SHADOW_SCORER=asr|gop|hybrid`,默认 `asr`。
  - `GOP_BASE_URL=http://127.0.0.1:8098`,解析风格对齐 `CLEAN_BASE_URL`/`TTS_BASE_URL`。
  - `SHADOW_CONTENT_THRESHOLD`、`SHADOW_PRON_THRESHOLD`、`SHADOW_MAX_SEVERE_PHONES`
    可后续加入;初版也可复用 query `threshold` 作为最终通过线,但文档建议显式拆分。
- **聚合规则(初版建议)**:

```text
asr:
  score = content_score
  passed = content_score >= content_threshold

gop:
  score = pronunciation_score
  passed = pronunciation_score >= pron_threshold && severe_phone_count <= max_severe_phones

hybrid:
  score = 0.45 * content_score + 0.55 * pronunciation_score
  passed = content_score >= content_threshold
        && pronunciation_score >= pron_threshold
        && severe_phone_count <= max_severe_phones
```

  权重和阈值是产品参数,上线前用真实录音校准;不要写死在前端。
- **落库**:`shadow_attempt` 增加可选摘要字段 `content_score`、`pronunciation_score`、
  `severe_phone_count`、`scorer_mode`、`asr_model`、`gop_model`、`detail_json`。
  `detail_json` 存完整 words/phones 明细,便于回看/重算;摘要字段方便统计和调阈值。
  迁移按本仓现有习惯做幂等 `ALTER TABLE ... ADD COLUMN` / 启动迁移,不因纯加列 bump
  `SCHEMA_VERSION`。
- **shadow_stat** 维持不变:成功/失败计数语义照旧,只是 `passed` 由 scorer 聚合规则给出。
- **代理 trace**:沿用 `SpanScope` 两阶段(`shadow_asr` / `shadow_gop` / `shadow_score_merge` span);
  未启用 trace 时 no-op。

## 6. 前端展示(`zero-desktop` ShadowPanel)

- 句级:同时显示内容分与发音分;主分仍用 `score`。例如"内容 92 / 发音 71"。
- 词级:保留老的内容状态 `ok|wrong|missing`;新增按 `pronunciation_score` / `pron_status`
  上色(绿/黄/红),点词展开**音素级明细**。
- 错读音素:`bad` 音素红色高亮 + 显示 `hint`(「/θ/ 听起来像 /s/」),给针对性纠音。
- `stat` 计数语义不变:统计最终 `passed` 的成功/失败次数。
- 向下兼容:GOP 后端未启用(仍 v1)时,`phones` 为空,面板退回现有逐词 ok/wrong/missing 展示。

## 7. 风险与取舍

- **模型选型与运维成本**:wav2vec2-GOP 需 GPU 推理 + 权重落地 + speechocean762 标定,
  归 streaming-speech 仓;本仓只对接契约,降低耦合。立项前先在该仓跑通 speechocean762 配方。
- **延迟**:hybrid 模式会多一次 GOP 调用。跟读是交互式,需控制单句评测在 ~1s 量级;
  必要时 ASR 与 GOP 并行、降采样/裁短/批量优化。代理超时参照 clean(可给 60s 上限,
  实际目标 < 2s)。
- **G2P 覆盖**:未登录词(专名/缩写)G2P 不准 → 该词降级为内容判定或跳过音素评分,不拖垮整句。
- **录音质量**:GOP 对底噪/回声更敏感。沿用 TODO-1 的"戴耳机 + 播完再录"前提;可前置接
  audio-cleanup(`:8097`)降噪后再评测。
- **分数可解释性**:GOP 原始 log 后验不可直接示人,必须经 speechocean762 标定到 `0..1`,
  且"通过线"要用真实数据校准,否则用户体感分数飘忽。
- **双分数认知成本**:hybrid 会同时暴露内容分和发音分。前端文案要克制:主显示最终分,
  辅助显示"内容 / 发音",避免用户被三套分数淹没。
- **声纹门控(防代读/放原音)**:GOP 只回答"这段录音发音标不标准",**不校验"是不是本人在读"**——
  直接把标准原音回放,GOP 会给高分。若产品上要防作弊,需在 `/assess` 前置一道声纹门控
  (先过 FunASR `:9101 /embed` 比对注册声纹,不匹配则拒评/标记),与发音评测解耦、按需开启。
  本期(v2)**不做**,列为开放项;真要做时按 §2 决策 7 落地。
- **范围蔓延**:本期只做分段音准;语调/重音/流利度(超音段)明确留 v3,避免一次吃太多。
- **评分可解释 + 对齐明细 UI**:把评分细则透明化 + 新增「对齐可靠性/uncertain」(真机发现长词
  中段会被强制对齐误杀,需把"没对齐上"的音素从 bad 改判 uncertain,不冤枉用户)→ 单列设计
  [english-shadow-scoring-ui-design.md](english-shadow-scoring-ui-design.md)。
- **实时化(流式)**:本稿是**批量**评测(读完整句出分)。「边读边评」的流式版单列设计 →
  [english-shadow-realtime-design.md](english-shadow-realtime-design.md);本批量稿是其**地基 + finalizer +
  校准 oracle**(流式途中出临时分,整句结束仍调本批量 `/assess` 出权威分落库)。

## 8. 分阶段落地

- **Phase A — 服务先行(streaming-speech 仓)** ✅ **已部署 + 端到端实测通过(2026-06-25,GB10)**
  1. ✅ 引擎跑通(`server/pronunciation-assess/gop.py`):G2P + wav2vec2 CTC + forced_align + **峰值 GOP**
     + 聚合。GB10 实测「I think so」(中文嗓读英文)→ 正确区分:AY/NG/K/S/OW ~0.98 ok,`think` 的
     **TH→F / IH→IY** 误读精确定位,句分 0.62,单次 ~1.5s。⏳ **speechocean762 真标定**未做
     (默认 sigmoid 占位,绝对分偏严;`calibration.json` 经 `GOP_CALIBRATION` 注入,重标无需重编译)。
  2. ✅ `:8098 /assess` 微服务部署在 GB10(`compose.assess.yaml` + 离线 `compose.override.yaml`),
     `restart:unless-stopped`;契约文档 `streaming-speech/docs/pronunciation-assess-api.md`。
     单测 `test_gop.py` 10 + `test_app.py` 7 全绿。
  - **部署中踩平的集成坑**:① 模型 vocab 形态各异 → gop.py 多候选 vocab 桥(`_resolve_token_ids`,
    同音素多写法取后验最大,含 `AH→ax` schwa);② nltk≥3.9 tagger 改名 `_eng` → Dockerfile 双名下载;
    ③ torchaudio 2.11 弃用 `load` → 改 **ffmpeg 子进程**解码;④ CTC 尖峰 → GOP 取**峰值帧**。
    GB10 容器无外网 → 模型/语料宿主预下离线挂载;**文件 bind mount 换 inode 需 `docker restart`**。
  - **真机调优(2026-06-26)**:
    - 🔄 **换 L2 模型**:`vitouphy/...timit-phoneme`(连读长词中段被对齐整段误杀)→
      **`slplab/wav2vec2-large-robust-L2-english-phoneme-recognition`**(专训非母语英语,带 `*_err` 误读
      标记)。delicious 的 L 0.04→0.86、schwa "a" 0.01→0.92,th→s 真错读仍抓。
    - 🔄 **重标定** `a=1.2,b=−2.0`(L2 raw 分布);**放宽通过** `threshold 0.6 + bad≤1`;
      **始终返回 phones**(整句模式也带音素明细,供 UI 表);**对齐可靠性 uncertain** + **`_err` 细分**
      (发音不准 vs 替换)。详见 [english-shadow-scoring-ui-design.md](english-shadow-scoring-ui-design.md)。
    - ⏳ 仍待:speechocean762 正式标定。
- **Phase B — toolkit-server 对接(本仓)**
  3. shadow 内核抽象 `ShadowScorer`;新增 `SHADOW_SCORER` + `GOP_BASE_URL`;
     先接 mock `/assess` 打通协议测试,再接真实 GOP 服务。
  4. `ScoreResult` 兼容扩展;实现 `asr` / `gop` / `hybrid` 三种模式与聚合规则。
  5. `shadow_attempt` 加摘要字段 + `detail_json`;补幂等迁移。
- **Phase C — 前端展示(本仓 zero-desktop)**
  6. 内容/发音双分展示;词/音素级上色 + 错读音素 hint;GOP 未启用时零回归退回 v1 展示。
- **Phase D — 验收**:补 `docs/runbook-shadow-gop-e2e.md`——真实英文朗读(含故意错读 th→s)
  →`/assess` 定位出该音素 → toolkit-server 透传 → 前端高亮;阈值标定后通过率符合直觉。

## 9. 与 v1 的关系(一句话)

v1(ASR 文本对齐)**不删**,作为默认 `SHADOW_SCORER=asr` 与嘈杂/无 GPU 环境兜底;
GOP 是叠加的"更严格、更细"的发音诊断能力。目标线上形态是 `hybrid`:ASR 管内容,
GOP 管发音,toolkit-server 统一聚合 `score/passed`,**对桌面端入口形状不变**,可灰度、可回滚。
