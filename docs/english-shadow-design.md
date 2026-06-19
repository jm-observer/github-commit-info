# English 跟读判分（Shadow Reading）设计

> 状态:设计稿（待评审）  ·  日期:2026-06-18  ·  范围:`zero-desktop` english 模块 + `toolkit-server`
>
> 关联:[CLAUDE.md 语音底座/ASR](../CLAUDE.md) · [docs/runbook-audioforge-e2e.md](runbook-audioforge-e2e.md)

## 1. 目标与一句话定义

在 english 听力的播放流程上叠加一个**可开关的、闯关式跟读判分**:用户跟着读,系统用 ASR 把
用户音频转写后与参考文本对齐打分,**通过则(可选)自动进下一个,不通过就留在原地重读**;同时记录每个
单元的成功/失败次数,用户可随时跳过或「标注重点」留待后续重点学习。

明确**不做**「真·实时逐词点亮(卡拉OK 流式对齐)」——逐词对错通过「整段录完→一次性对齐→逐词标色」
实现,无需流式,体验更稳。

## 2. 关键决策(已拍板)

| # | 决策 | 取值 |
|---|---|---|
| 1 | 「通过」判定 | 内容命中率阈值,默认 **0.9**(参考文本里 ≥90% 的词被正确读出),设置可调。详见 §6 |
| 2 | 统计存储 | **toolkit.db**(toolkit-server),新增 `shadow_attempt` + `shadow_stat` 两表 |
| 3 | 跟读粒度 | **句 + 词都做**,共享同一判分内核;词由句子文本切分而来。详见 §4 |

## 3. 整体形态与状态机

跟读是套在现有播放循环之上的一层「闸门」。现状(见
[AudioPlayerService.ts](../crates/zero-desktop/ui/src/modules/english/services/AudioPlayerService.ts)):
`句 → 该句多段 audio → 每段播 maxPlayCount 次 → _nextSentence 自动进下一句`。

开启跟读后,在「一个单元的参考音频播放完」与「自动进入下一个单元」之间插入判分闸门:

```
播放参考(句/词音频)
      │
      ▼
[跟读总开关 ON?]──否──▶ 维持原自动循环(纯听力)
      │是
      ▼
开录(自动开麦 + VAD 判停 / 手动兜底按钮)
      │
      ▼
上传音频 + 参考文本 → toolkit-server 判分
      │
      ▼
   passed?
   ├─ 是 ─▶ success_count+1 ─▶ [通过即自动跳 ON?] ─是─▶ 下一个单元
   │                                              └─否─▶ 停在原地,等手动下一步
   └─ 否 ─▶ fail_count+1 ─▶ 留在本单元,提示重读(逐词标红错处)

任意时刻可手动: [重读] [跳过] [标注重点] [上/下一个]
```

**闸门如何接入 AudioPlayerService(不改其播放内核)**:新增一个「推进闸门」开关。当跟读开启时,
`_nextSentence` / 词级推进不再由 `setTimeout` 自动触发,而是 emit 一个新事件
`onAwaitShadow`(携带当前单元),由前端 `ShadowController` 接管:判分通过且自动跳 → 调
`nextSentence()`;不通过 → 调「重播当前」。跟读关闭时行为与今天完全一致(零回归)。

需要在 `AudioPlayerService` 增补:
- `setShadowGate(enabled: boolean)`:开启后到达单元末尾时 emit `onAwaitShadow` 而非自动前进。
- 事件 `onAwaitShadow: { unit }`、`replayCurrent()`(重读用,重置 playCount 重播当前音频)。
- 词级推进的小游标(见 §4)。

## 4. 粒度:句 / 词

抽象出**跟读单元 ShadowUnit**,句和词共用判分与统计:

```ts
type ShadowUnit =
  | { kind: 'sentence'; sentenceId: number; refText: string }
  | { kind: 'word'; sentenceId: number; wordIndex: number; refText: string }
```

- **句模式**:单元 = 整句。参考音频 = 该句现有音频;参考文本 = `sentence.text`。
- **词模式**:把 `sentence.text` 用分词规则切成词序列(英文按空白/标点切,剥标点;保留原始 index
  以便逐词标色与统计对齐)。一个句子内**逐词闯关**:词1 通过→词2→…→末词通过→进下一句。
  - 参考音频:v1 **不为单词单独准备音频**——先整句播一遍给发音参照,再逐词跟读判分(短音频
    ASR 不稳,见 §9 风险)。后续可用 AudioForge/TTS 给每词生成参考音频(见 §10 Phase 2)。
  - 词级判分的参考文本就是该词;命中率退化为「这个词读对了没」(可结合编辑距离容忍轻微偏差)。

粒度由设置项 `granularity: 'sentence' | 'word'` 决定;两者可在同一套 UI/状态机下切换。

## 5. 前后端职责划分

```
zero-desktop (webview)                Tauri 后端           toolkit-server (GB10)        FunASR (GB10:9101)
─────────────────────                 ──────────           ─────────────────────        ──────────────────
ShadowController                                                                          
  · 监听 onAwaitShadow                                                                     
  · getUserMedia 采集音频  ──invoke──▶ english_shadow_   ──HTTP──▶ POST /api/web/english/  ──asr-client──▶ /transcribe
    (MediaRecorder→blob)              score(代理转发)              shadow/score                (Whisper, 英文)
  · 渲染分数/逐词标色/计数            english_shadow_     ◀─────── { transcript, score,   ◀──────────────
  · 通过→nextSentence/停              stats(代理转发)              passed, words[] } + 落库
「标注重点」→ 复用 ApiService.annotateSentence(走 G10 english 后端,不进 toolkit.db)
```

- **音频采集**:webview `getUserMedia` + `MediaRecorder` 最简、跨平台。产物 blob 经 Tauri `invoke`
  传给后端(与现有 `english_tts_preview` / `english_tts_voices` 的 invoke 代理风格一致,见
  [ApiService.ts](../crates/zero-desktop/ui/src/modules/english/services/ApiService.ts))。
- **判分 + 落库**:统一在 toolkit-server,内核走 `asr-client` 调 FunASR。对齐/打分逻辑集中此处,
  将来换音素级 GOP 只动这里。
- **「标注重点」**:复用 english 后端已有的 `sentence.annotate` / `is_annotated`
  机制(`AudioPlayerService.toggleAnnotation` 已存在),**不**重复造,也不进 toolkit.db。

### HTTP 端点(toolkit-server,新增 `english` 路由组)

注:跟读判分是**交互式低延迟**,按 TTS/clean 代理那种**同步端点**做,**不**做成 TaskKind(长任务+轮询不适用)。

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/web/english/shadow/score` | multipart:`audio` 文件 + 表单字段 `customer_id` / `kind`(sentence\|word) / `sentence_id` / `word_index?` / `ref_text` / `threshold?`。→ FunASR 转写 → 对齐打分 → 落 `shadow_attempt` + 累加 `shadow_stat` → 返回判分结果 |
| `GET` | `/api/web/english/shadow/stats` | query:`customer_id` + `sentence_ids`(逗号分隔)。批量返回这些句子(及其词)的成功/失败次数、上次分数/结果,供进入播放时一次性回填 |

判分响应体:

```jsonc
{
  "transcript": "the quick brown fox",        // ASR 识别到的用户朗读
  "ref_text":   "the quick brown fox jumps",
  "score":      0.8,                            // 内容命中率 0~1
  "passed":     false,                          // score >= threshold
  "words": [                                    // 逐词对齐结果,供标色
    { "ref": "the",   "status": "ok" },
    { "ref": "quick", "status": "ok" },
    { "ref": "brown", "status": "ok" },
    { "ref": "fox",   "status": "ok" },
    { "ref": "jumps", "status": "missing" }     // ok | wrong | missing
  ],
  "stat": { "success_count": 3, "fail_count": 5, "last_score": 0.8 }  // 累加后的最新统计
}
```

## 6. 判分算法(v1:ASR 文本对齐)

1. 用户音频 → FunASR `/transcribe`(走 `asr-client`)→ 转写文本 hyp。**务必走英文模型(Whisper)**,
   不要中文 Paraformer,否则英文识别会差(见 §9)。
2. 规范化:小写、去标点、数字/缩写归一(可迭代)、按空白切词。
3. 参考文本 ref 与 hyp 做**词级对齐**(最小编辑距离 / LCS),产出每个 ref 词的
   `ok | wrong | missing` 及 hyp 的多读 `extra`。
4. **命中率** `score = ok 词数 / ref 词数`;`passed = score >= threshold`(默认 0.9,设置可调)。
5. **词模式**:ref 仅一个词,`score ∈ {0,1}` 或对单词内部用字符级编辑距离给容忍(避免 ASR 把
   "cat" 听成 "cap" 直接判 0 太苛刻)。

> ASR 是「宽容」的:它衡量**「内容/可懂度」**,不衡量发音细腻度(口音重但能听懂仍判对)。对跟读够用,
> 知道这条线在哪即可。要发音级评分(「th 发成 s」)需音素级 GOP,属 Phase 2(见 §10)。

## 7. 数据模型(toolkit.db,新增)

按 [schema.rs](../crates/toolkit-core/src/schema.rs) 约定:整块幂等 `DDL_V1`
(`CREATE TABLE IF NOT EXISTS`),改后 bump `SCHEMA_VERSION`。

```sql
-- 每次跟读尝试的明细(可用于回看/调阈值/重算)
CREATE TABLE IF NOT EXISTS shadow_attempt (
  id           TEXT PRIMARY KEY,          -- new_task_id 风格
  customer_id  INTEGER NOT NULL,          -- english 后端的 customer_id(谁在练)
  kind         TEXT    NOT NULL,          -- 'sentence' | 'word'
  sentence_id  INTEGER NOT NULL,          -- english 后端的句子 id
  word_index   INTEGER,                   -- 词模式:句内词序号;句模式:NULL
  ref_text     TEXT    NOT NULL,
  transcript   TEXT,                      -- ASR 结果
  score        REAL    NOT NULL,
  passed       INTEGER NOT NULL,          -- 0/1
  created_at   TEXT    NOT NULL           -- now_iso8601
);
CREATE INDEX IF NOT EXISTS idx_shadow_attempt_unit
  ON shadow_attempt(customer_id, sentence_id, word_index);

-- 每个单元的累计统计(读取快;由 attempt 累加维护,也可随时重算重建)
CREATE TABLE IF NOT EXISTS shadow_stat (
  customer_id   INTEGER NOT NULL,
  kind          TEXT    NOT NULL,
  sentence_id   INTEGER NOT NULL,
  word_index    INTEGER NOT NULL DEFAULT -1,  -- 句模式用 -1 占位以进主键
  success_count INTEGER NOT NULL DEFAULT 0,
  fail_count    INTEGER NOT NULL DEFAULT 0,
  last_score    REAL,
  last_passed   INTEGER,
  last_at       TEXT,
  PRIMARY KEY (customer_id, sentence_id, word_index, kind)
);
```

- 每次 `score` 端点:插一行 `shadow_attempt`,并 upsert 对应 `shadow_stat`(成功/失败计数 +1、更新
  last_*)。
- `stats` 端点直接读 `shadow_stat`,按 `sentence_id` 聚合返回(句级一行 + 词级多行)。
- 统计是 toolkit.db 自有数据,与 english 后端的句子/标注解耦;`customer_id` 是关联键。

## 8. 配置项(设置页)

跟读相关设置建议持久化(toolkit.db `meta` 或前端配置;沿用 english 现有 env 配置通道
[EnvConfigService](../crates/zero-desktop/ui/src/modules/english/services/EnvConfigService.ts)):

| 项 | 默认 | 说明 |
|---|---|---|
| `shadowEnabled` | 关 | 跟读总开关(整个流程级) |
| `granularity` | `sentence` | 跟读粒度:句 / 词 |
| `autoAdvanceOnPass` | 开 | 通过即自动跳下一个;关则通过也停在原地等手动 |
| `passThreshold` | `0.9` | 命中率阈值 |
| `captureMode` | `auto` | `auto`=播完自动开麦+VAD 判停;`button`=点按钮才录(兜底/嘈杂环境) |
| `recordWindowMs` | 按文本长度动态 | 跟读录音窗口上限,防止无限等待 |

## 9. 风险与取舍

- **英文 ASR 路由**:必须确认 FunASR 这边英文走 Whisper 分支。落地前先用 §11 的最小验证拉通,
  否则中文模型识别英文会让分数全偏低。
- **短音频(词模式)不稳**:单词音频太短,ASR 易抖。对策:词内用字符级编辑距离容忍;词模式默认
  阈值可略低;UI 给「这个词判得不准?跳过」出口。
- **采集格式**:`MediaRecorder` 默认 WebM/Opus,FunASR 端点需确认可解码(必要时后端用 ffmpeg 转
  PCM/WAV,或前端直接采 WAV)。落地前在 §11 验证一条真实音频。
- **底噪/把参考音录进去**:播放参考期间不开麦;`auto` 模式靠 VAD 判起止 + `recordWindowMs` 兜底。
- **判分延迟 vs 节奏**:judge 异步、不阻塞 UI;通过/失败的推进发生在判分返回后,期间给「识别中…」态。
- **零回归**:跟读关闭时 `AudioPlayerService` 行为必须与今天逐字节一致(闸门仅在 `shadowEnabled`
  时生效)。

## 10. 分阶段落地

- **Phase 1(MVP,句模式打通)**
  1. toolkit-server `english` 路由 + `/shadow/score`(FunASR→对齐→落库)+ `/shadow/stats`;
     `shadow_attempt`/`shadow_stat` 建表 + bump `SCHEMA_VERSION`。
  2. Tauri `english_shadow_score` / `english_shadow_stats` 代理命令。
  3. `AudioPlayerService` 加 `setShadowGate` + `onAwaitShadow` + `replayCurrent`(关时零回归)。
  4. 前端 `ShadowController` + `ShadowPanel`(采集、分数/逐词标色、计数、跳过、标注重点复用既有
     annotate)。设置项接入。
- **Phase 2(词模式 + 体验)**:句内逐词闯关游标;词级字符容忍;统计页(成功/失败 Top 难句词)。
- **Phase 3(发音级,可选)**:接音素级 GOP 评分服务,替换 §6 内核,接口形状不变。
  词参考音频用 TTS/AudioForge 生成。

## 11. 端到端验收(落地后补 runbook)

最小链路验证(动手第一步,优先做掉 §9 的两个未知):
1. 录一条真实英文朗读(WebM 或 WAV)→ 直接 `curl` FunASR `/transcribe` 确认**英文识别正常**且**格式可解码**。
2. `curl` `POST /api/web/english/shadow/score`(带一条音频 + ref_text)→ 看 `score`/`words` 合理。
3. 查 `toolkit.db` 的 `shadow_attempt` / `shadow_stat` 落库正确;再调 `/shadow/stats` 能回读。
4. 前端:开跟读 → 读对一句自动跳、读错留在原地标红 → 计数累加 → 刷新后计数仍在(DB 持久)。

完整 runbook 待 Phase 1 完成后补 `docs/runbook-english-shadow-e2e.md`。
