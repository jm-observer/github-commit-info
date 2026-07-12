# 发音评测单元(PronEvaluator)设计

> 状态:设计稿(待评审)  ·  日期:2026-06-26  ·  范围:可复用「发音评测单元」组件(词/短语/句通用)
> + LLM 二次评分/反馈 + 失败词 drill-down
>
> 关联:[english-shadow-gop-design.md](english-shadow-gop-design.md)(批量 GOP) ·
> [english-shadow-scoring-ui-design.md](english-shadow-scoring-ui-design.md)(评分明细表) ·
> [english-shadow-realtime-design.md](english-shadow-realtime-design.md)(流式) ·
> 公共大模型层(CLAUDE.md「LLM 中枢」)

## 1. 目标与动机

一句话:**把「一段文本 → 评分 / 朗读 / 听标准 / 听自己 / 大模型二次评」收成一个自洽、可复用的单元**,
词、短语、句子**同一套**。

动机:
- 跟读时某个词发音失败 → 想**单独练这个词**(听标准音、反复读、看分),现在没有入口。
- 评分/录音/TTS/回放 这几件事散在 ShadowPanel 里、耦合跟读流程,无法独立复用。
- 模型给的分有局限(连读、个别音素),想**叫大模型再看一眼**,把结构化评测结果讲成人话 +
  给个二次意见。

## 2. 核心抽象:`PronEvaluator`

一个**展示无关**的 React 组件(调用方决定内联 / 弹层 / 整页),输入一段文本,内部自带五个能力:

```
              ┌─────────────────── PronEvaluator(text) ───────────────────┐
   输入 text  │  ① 评分(GOP)   ② 朗读(录音)   ③ 听标准(TTS)            │
  (词/短语/句)│  ④ 听自己(回放)            ⑤ 大模型二次评分/反馈(可选)   │
              │  ── 逐音素诊断明细表(复用 scoring-ui)──                  │
              └────────────────────────────────────────────────────────────┘
```

**Props(草案)**:
```ts
interface PronEvaluatorProps {
  text: string                    // 词 / 短语 / 句
  unitKind?: 'word' | 'phrase' | 'sentence'  // 缺省按 token 数自动判定
  sentenceId?: number             // 落库归属(可选;独立练习可不传/用临时 id)
  voiceId?: string                // 标准音 TTS 音色(缺省取设置默认)
  autoTtsOnMount?: boolean        // 进入即预拉标准音
  onScored?: (score: ShadowScore) => void
}
```

**单元类型(`unitKind`)**:只影响 **UX 文案 / 落库单元 / TTS 时长预算**,**不影响 GOP**(引擎对任意
文本 G2P 后评分,且现已始终返回逐音素)。自动判定:1 token=word、2–4=phrase、更多=sentence。

### 2.1 嵌套 / 层级(段落 → 句 → 词 → 音标)

评测单元是**递归**的,四层:**段落 ⊃ 句 ⊃ 词 ⊃ 音标(音素)**。每层都能评/下钻;同一个
`PronEvaluator` 组件按 `level` 自适应,**子节点失败 → 在原地展开/弹出同组件评子节点**:

```
Paragraph ── 编排:逐句评(不对整段做一次巨型 GOP)── 句聚合分
  └ Sentence ── 一次 GOP(返回逐词 + 逐音素)── 词级明细表
       └ Word ── 单词 GOP + 听标准/朗读/听自己/LLM ── 逐音素诊断
            └ Phoneme(音标)── 单音纠音卡:IPA + 发音要领 + 最小对立对 + 听标准/录/对比/LLM
```

**前三层是「评测」,最深的音标层是「纠音卡」(性质不同)**:
- **段落级** = 一组句子的**编排壳**(顺序读/自动连跳,复用现 ShadowPanel 连跳)+ 汇总弱项。
- **句级** = 一次 GOP → 逐词;某词 `bad` → 「单独练」→ **词级**(同组件,`level=word`)。
- **词级** = 单词 GOP + 五能力齐全;逐音素表里某音 `bad`(如 "this" 的 /ɪ/) → 「练这个音」→ **音标级**。
- **音标级(新增,最细)** = **不做标准 GOP**(孤立音素没法 G2P、TTS 也难合成单音)。改成**单音纠音卡**:
  - **IPA + 发音要领**:口型/舌位/送气(可配音素知识库或让 LLM 生成)。
  - **最小对立对(minimal pairs)**:如 /ɪ/–/iː/ → `this/these`、`ship/sheep`(对比训练,概念见
    `pronunciation-coach-overview` §1)。
  - **听标准 / 朗读 / 听自己 / 对比**:听标准走**含该音的最小词/对立对**的 TTS(不是孤立音素)。
  - **评分(可选)**:对"含该音的最小词"做一次词级 GOP,看那个音是否改善。
  - **LLM**:把"你这个 /ɪ/ 偏 /iː/"讲成口型纠正法。

**递归 props**:
```ts
interface PronUnit {
  text: string                  // 段落/句/词文本;音标级为该音素(如 'IH' 或 '/ɪ/')
  level: 'paragraph' | 'sentence' | 'word' | 'phoneme'
  children?: PronUnit[]         // 段→句[]、句→词[]、词→音标[](按 GOP 结果懒生成)
  phoneme?: { ipa: string, arpabet: string, minimalPairs?: [string, string][] }  // 音标级附带
}
```
`children` 懒生成:句评完拿逐词结果、词评完拿逐音素结果 → 失败子项即可下钻。下钻 = **打开同组件**
(弹层/内联),不另写界面 —— 这就是"模板可嵌套"。落库以叶子(句/词)的 `shadow_attempt` 为准;
音标级是纠音练习,可不落分或落到"含该音的最小词"上。

## 3. 五个能力 = 复用现有 + 一处新增

| 能力 | 复用什么(已具备) | 需新增 |
|---|---|---|
| **① 评分(GOP)** | `ShadowService.scoreShadow`(批量)/ `streamScore`(流式)→ toolkit `/api/web/shadow/score[/stream]` → `:8098`。词/句通用 | 无 |
| **② 朗读(录音)** | `captureUtterance`(webm)/ `streamingCapture`(16k PCM) | 无 |
| **③ 听标准(TTS)** | TTS 代理 `/api/web/audio/tts`(已有 `english_tts_preview` Tauri 命令落盘 WAV);按 text 缓存,一次性 `Audio` 播放 | 轻量:按 text 缓存 + 播放封装(不污染统一播放器) |
| **④ 听自己(回放)** | 已实现「听我的录音」(`myAudioUrl` + 一次性 `Audio`) | 无 |
| **⑤ 大模型二次评分/反馈** | 公共大模型层(`/api/web/llm`、`toolkit-llm`、`llm_sessions`) | **新增**:`shadow_feedback` 提示词 + 一个反馈端点(见 §4) |

> 关键认知:**前四个能力 90% 是把现有零件搬进一个组件**;真正的新工作是 ⑤(LLM)+ 组件化。

## 4. 大模型二次评分/反馈(⑤,新增)

把 **GOP 的结构化结果**(逐音素分/状态/期望→实际/uncertain + 句分)+ ref_text 喂给大模型,产出:
- **人话反馈**:「你的 `think` 里 /θ/ 发成了 /s/,舌尖轻触上下齿之间送气试试」(教学纠音)。
- **二次意见(可选)**:一个 holistic 0–100 + 一句总评,作为对 GOP 的**补充视角**(尤其 GOP 把某音
  标 uncertain/可能误判时,给个"读起来其实没问题/确实偏了"的判断)。

**诚实边界(重要,写进 prompt 与 UI)**:
- 走 GB10 现成 **vLLM `gemma-4-26B-A4B-it`** 的版本:该模型是**文本 + 图像(视觉)多模态,但
  无音频**(查证:Gemma 4 音频仅 E2B/E4B/12B 有,26B-A4B 无)。所以它**听不到录音**,只能基于 GOP
  的结构化数据 + transcript **复述/解读 + 常识纠音**,**不是独立"听音"评分**。叫「**二次解读/教学反馈**」
  更准;holistic 分只当「AI 参考」,**不覆盖 GOP 落库主分**。
- **真·独立听音二次评分的路径(明确可行)**:换/加一个**带音频的 Gemma 4 变体(12B 或 E4B)**——
  这俩官方支持音频。可在 GB10 另起一个 vLLM 实例(或与现 26B 共存/择一),把 wav 直接喂给它做
  listen-based 二次分。属本设计 **Phase 4**,需 GB10 资源评估(12B 比 26B 小,显存可控)。
- (视觉 hack:把音频转**梅尔频谱图**喂现 26B 的视觉通道——但通用视觉 LLM 不会读频谱做音素级评测,
  **不靠谱,不推荐**,仅备注。)

**落地**:
- 提示词:`builtins()` 加 `shadow_feedback`(占位符 `{REF_TEXT}` / `{GOP_JSON}` / `{SCORE}`),
  可在控制台覆盖文案(同 douyin_refine 模式)。
- 端点:`POST /api/web/shadow/feedback`(body = ref_text + GOP 结构化结果)→ 填 prompt → 调
  `toolkit-llm` → 返回 `{feedback, holistic_score?}`;以 `kind=shadow_feedback` 落 `llm_sessions`
  (可在「大模型会话」模块回看)。LLM 未配 → 503/明确提示(同 summarize 约定)。
- 桌面:`english_shadow_feedback` Tauri 命令代理之(带 g10_token,同 score)。

## 5. 组件化与入口

- **`PronEvaluator` 组件**(`zero-desktop/.../english/pron/PronEvaluator.tsx`):自持评分/录音/TTS/回放/
  反馈状态 + 复用 scoring-ui 明细表。展示无关:
  - **失败词 drill-down**:ShadowPanel 明细表里某词 `bad` → 「单独练」按钮 → 弹层打开
    `<PronEvaluator text={word} unitKind="word" />`。
  - **独立练习入口**:english 模块加「发音练习」页,输入框粘任意词/短语/句 → 同一组件。
  - **ShadowPanel 复用**:整句跟读本身可逐步重构为 `PronEvaluator` 的一个「自动连跳」编排壳(非首期必须)。
- **播放纪律**:标准音 / 自己录音都用**一次性 `Audio`**,不进统一播放器/歌单(符合「单例+底栏唯一控制」
  播放模型);打开评测单元时若参考播放器在放,暂停它。

## 6. 数据 / 契约

- **评分**:复用 `shadow_attempt`(独立练习可用临时 sentence_id 或新 `unit` 概念;首期复用即可)。
- **TTS 缓存**:按 `sm3(text+voice)` 缓存 WAV 到 english 音频缓存目录,命中免重生成(同 `english_tts_preview`
  落盘思路)。
- **LLM**:`llm_sessions` 加 `kind=shadow_feedback` 一行/次,metadata 记 ref_text/句分/prompt 版本。
- 不需要改 schema(均为已有表 + 已有缓存目录)。

## 7. 分阶段落地

- **Phase 1 — 组件抽取(纯前端,复用现有)**:把评分/录音/听标准(TTS)/听自己/明细表收进
  `PronEvaluator`(`level=sentence|word`);ShadowPanel 失败词加「单独练」弹层入口(下钻到 word 级)。
  **无需后端改动**(TTS/GOP 都已具备)。
- **Phase 2 — 嵌套层级**:段落级编排壳(逐句评 + 汇总弱项)+ 句→词→**音标**的递归下钻;`children` 懒生成。
  - **音标级纠音卡**:逐音素表里某音 `bad` → 「练这个音」→ 单音卡(IPA + 发音要领 + 最小对立对 +
    听标准/录/对比/LLM)。需一份**音素知识/最小对立对数据**(ARPAbet→IPA→口型要领→minimal pairs;
    可静态表 + LLM 兜底生成)。听标准走"含该音的最小词/对立对"TTS。
- **Phase 3 — LLM 二次解读/反馈(toolkit + 桌面)**:`shadow_feedback` 提示词 + `/api/web/shadow/feedback`
  端点(喂 GOP 结构化结果,现 26B 文本模型)+ `english_shadow_feedback` 命令 + 组件「让 AI 讲讲」按钮 +
  反馈面板;落 `llm_sessions`。**holistic 分标「AI 参考」,不覆盖 GOP 主分**。
- **Phase 4 — 独立练习页**:english 加「发音练习」入口(粘任意词/短语/句/段落)。
- **Phase 5(需 GB10 资源评估)— 音频 LLM 独立二次评分**:在 GB10 起带音频的 **Gemma 4 12B / E4B**
  vLLM,把 wav 直接喂它做 listen-based 二次分(现 26B-A4B 无音频)。

## 8. 开放问题

- 独立练习的「单元身份/落库」:复用 sentence_id 够不够,还是引入轻量 `pron_unit`(text 内容寻址)做历史
  追踪?(首期复用,后续按需。)
- LLM holistic 分与 GOP 分**冲突时**的呈现(谁为准):建议 **GOP 为评测主分**,LLM 分标「AI 参考」。
- TTS 单词音色:句子音色未必适合孤立单词(语调);是否给单词用更"字典发音"的音色/参数。
- 反馈延迟:LLM 反馈是异步的(秒级),按钮触发、loading 态,不阻塞评分主流程。
