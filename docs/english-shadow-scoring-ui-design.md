# 跟读发音评分「可解释 + 对齐明细」设计

> 状态:**已实现,待真机部署验证**(代码全绿;GB10 toolkit-server / 桌面构建上线后端到端确认)
> ·  日期:2026-06-26  ·  范围:评分细则透明化 + 「对齐可靠性」+ `zero-desktop` ShadowPanel 明细表
>
> 关联:[english-shadow-gop-design.md](english-shadow-gop-design.md)(批量 GOP 契约/评分) ·
> [english-shadow-realtime-design.md](english-shadow-realtime-design.md)(流式) ·
> streaming-speech `server/pronunciation-assess/`(引擎)+ `docs/pronunciation-assess-api.md`(契约)

## 1. 目标与背景

**目标**:把发音评分**做成用户看得懂的明细表**——逐词/逐音素列出「你的分 + 状态 + 期望音素 vs 实际
听到 + 提示」,让用户一眼知道**哪个音读得不够好、该怎么改**。

**背景(为什么现在做)**:真机实测发现两类"低分"被混在一起,用户无从分辨:
1. **真读错**:如 `think` 的 /θ/ 读成 /s/ —— 应该提示、应该扣分。
2. **引擎没对齐**:如 `delicious` 中段 `L-IH-SH` 被强制对齐甩到错帧 → 后验≈0 → 误判 bad
   (实测:`L` 真实峰值在第 94 帧、对齐却放在第 77 帧;第二个 `IH` 对到了句首 "It" 的元音上)。
   **这不是用户的错**,却和真读错一样显示成红色,体验上"明明读对了却一直不过"。

所以本设计两件事:**(A) 评分细则透明展示**;**(B) 新增「对齐可靠性」维度**,把"没对齐上"的音素
单独标成 **uncertain(存疑)**,既如实告诉用户"这里引擎没听准、不算你错",也不让它拉低分数/拦住通过。

## 2. 评分细则(展示给用户 + 开发口径)

### 2.1 三级分怎么来

```
参考文本 ──G2P──▶ 期望音素序列(ARPAbet)
用户录音 ──wav2vec2 CTC──▶ 逐帧音素后验 ──强制对齐──▶ 每音素时间段
每音素:GOP = 该音素时间段内「峰值帧的 canonical 后验」(log)  ──标定──▶ 0~1
词分  = 截尾聚合(见 2.3)
句分  = 词分聚合
通过  = 句分 ≥ 阈值 且 严重错读音素数 ≤ 容忍值(见 2.4)
```

### 2.2 状态分档(每音素/词)

| 状态 | 含义 | 颜色(建议) |
|---|---|---|
| `ok` | 达标(分 ≥ ok_min) | 绿 |
| `warn` | 偏弱(warn_min ≤ 分 < ok_min) | 黄 |
| `bad` | 明显错读(分 < warn_min)**且对齐可靠** | 红 |
| `uncertain`(新增) | 引擎没把这个音对齐好/没听准,**不判定对错** | 灰 + 问号 |

> 阈值 `ok_min`/`warn_min` 由标定给(见 2.5),展示时一并说明当前值。

### 2.2.1 错读细分:发音不准(质量) vs 替换(substitution)

一个 `bad` 音素,看它**段内最强竞争音**是什么(L2 模型每音素带 `*_err` 误读标记):
- **竞争音 = 该音素自己的 `_err` 变体**(如 `IH` 的 `ih_err`)→ **发音不准**(音对了但偏/不到位)。
  `actual_ph` 留空,hint 形如「**/ɪ/ 发音不够准**」。(已落地:`gop.py` 的 `strip_err_marker`。)
- **竞争音 = 另一个音素**(如 `/θ/` 段里 `/s/` 很强)→ **替换错读**。`actual_ph=S`,hint「**/θ/ 读成了 /s/**」。
> 避免把 `_err` 当成"读成了 IH_ERR"这类无意义文案;明细表「说明」列据此区分。

### 2.3 词分聚合(抗对齐噪声)

旧:`0.6·均值 + 0.4·最低` —— **一个被误杀的音素(0.05)就把词分拖垮**。
新:**先剔除 `uncertain` 音素**,再对剩余可靠音素做 `0.6·均值 + 0.4·最低`;若一个词的音素**全 uncertain**,
该词整体标 uncertain(不计分、不拦通过)。这样 `delicious` 中段被误杀时,词分由可靠音素决定,不冤枉。

### 2.4 通过判定(放宽 + 解耦对齐噪声)

`passed = 句分 ≥ threshold 且 bad 音素数 ≤ max_bad` —— ✅ **已落地**(`MAX_BAD_PHONES=1`)。
- `max_bad` **1**(放宽"零 bad"——真人读一句几乎总有 1 个边界音;`uncertain` 不计入 bad)。
- `threshold` 默认 **0.6**(`DEFAULT_THRESHOLD`/`passThreshold`;GOP 分天花板 ~0.9,旧默认 0.9 不合理)。
- 二者都可配(面板滑杆 + 服务端默认)。

### 2.5 声学模型 + 标定(展示"分数怎么标的")

- **声学模型**(✅ 已落子):`slplab/wav2vec2-large-robust-L2-english-phoneme-recognition`(专训非母语
  英语,小写 ARPAbet + `*_err` 误读标记 + 弱读 `ax`)。实测远胜通用 TIMIT 版(连读长词中段不再整段误杀)。
  详见 streaming-speech `README`。
- **标定**:GOP 原始 log 后验经 `sigmoid(a·(raw − b))` 映射 0~1;`a/b/ok_min/warn_min` 存
  `calibration.json`(`GOP_CALIBRATION`)。**当前 `a=1.2,b=−2.0`**,按 L2 真机 raw 手调(临时值,
  正式版用 speechocean762 拟合)。UI「评分细则」标注"标定持续优化中"。

## 3. 新增「对齐可靠性」(本设计核心机制)

判断一个音素的低分到底是**真读错**还是**没对齐上**:

**设计目标信号(理想,可逐步用上)**:① forced_align 给该音素 token 的对齐置信度(per-token score);
② 段内 canonical 后验绝对值;③ 真实后验峰值是否落在对齐 span 内(`peak_t` 落在 `[t_start,t_end]` 外 = 错位)。

**当前落地启发式(✅ 已实现,`gop.py`)**:对一个 `bad` 音素,看
「**段内 canonical 峰值是否极低(< 标定 floor)**」**且**「**段内有没有明确的替代音**(最强竞争音是否够强)」:
- 两者都满足(没发出 + 没替代)→ 标 **`uncertain`**(`reliable=false`),不判错;
- 有明确替代音(如 `/θ/` 段里 `/s/` 很强)→ 一律保留 `bad` —— **绝不漏判真读错**。
> 暂未单独用 ② 后验绝对值阈值之外的 forced_align score / 峰值在 span 外做判定(③ 的 `peak_t` 仅作**诊断
> 透出**给明细表,不参与 reliable 判定;避免重复音素全局峰歧义)。这些是后续可加严的信号。

**效果**:`delicious` 中段被误杀的音可判 uncertain(灰),不红、不拉低词分、不拦通过;`think` 的
/θ/→/s/ 有清晰替代音,仍判 bad。

> 产品化口号:**不确定就不判错**。引擎侧据上述启发式算 `reliable`,经契约透出。

## 4. 契约扩展(向下兼容,全 Option/默认)

`PhoneResult` ✅ **已落地字段**(服务端 `gop.py` → toolkit `shadow::PhoneResult` → 前端 types,全 Option):

| 字段 | 类型 | 说明 |
|---|---|---|
| `reliable` | bool? | `false` → 没对齐好,前端按 `uncertain` 呈现 |
| `pron_status` | 枚举 | `ok\|warn\|bad\|uncertain` |
| `t_start` / `t_end` | float? | 该音素对齐时间段(秒) |
| `peak_t` | float? | 诊断:canonical 全局峰时间(秒),落在 `[t_start,t_end]` 外 = 对齐错位 |
| `gop_raw` | float? | 诊断:对齐段内 canonical 峰值 log 后验(≤0) |
| `expected_ph`/`actual_ph`/`hint` | str? | 错读时的期望/实际音素 + 文案(见 §2.2.1) |

**未来可选(暂未实现)**:`align_conf`(forced_align per-token score 归一)——若 §3 要加严 reliable 判定时引入。

`WordResult` 可加 `pron_status` 扩 `uncertain`。`ScoreResult` 句级 `bad_phone_count` 语义不变
(**只数 reliable 的 bad**)。所有新增字段 `skip_serializing_if None`,老前端忽略即零回归。

## 5. UI 设计(ShadowPanel 明细表)

### 5.1 结构

- **句级**:顶部「句发音分 xx% · 通过/未通过 · 评分细则 ⓘ」。点 ⓘ 展开**评分细则说明**(2.x 的人话版 +
  当前阈值/标定状态)。
- **词级行**:逐词色块(绿/黄/红/灰);点词**展开音素明细表**。
- **音素明细表**(每行一个音素)—— ✅ **当前前端实际列**(`ShadowPanel`):

| 列 | 内容 |
|---|---|
| 音素 | ARPAbet(如 `TH`;IPA 映射后续可加) |
| 对齐区间 | `t_start–t_end`(秒) |
| 真实峰 | `peak_t` 落区间内→「区间内」;落区间外→`@x.xxs (±Δs)` 标黄(对齐错位诊断) |
| 你的分 | 0~100(进度条 + 数字);`uncertain` 显「—」 |
| 判定 | 达标/偏弱/错读/存疑(四档色) |
| 说明 | 合并文案:替换→「读成了 /s/」、质量→「/ɪ/ 发音不够准」、存疑→「没对齐好,不算读错」、ok→「清晰」 |

> 注:**没有独立「期望→实际」列**,该信息(`expected_ph`/`actual_ph`)已并入「说明」。「真实峰/对齐区间」
> 是诊断增强(超出 §5 初版设想),保留——直接对应排错时看的"对齐错位"。

### 5.2 颜色 / 图标语义(配 legend)

- 绿 ✓ ok · 黄 ! warn · 红 ✗ bad(真错读)· 灰 ? uncertain(存疑/没对齐)。
- **关键 UX**:uncertain **不渲染成"错"**——灰底问号 + 文案"这里引擎没听准,不算你读错",消除
  "读对了却显示红/不过"的挫败。

### 5.3 解释入口

- 「评分细则 ⓘ」折叠面板:一段人话(怎么算分、ok/warn/bad/uncertain 啥意思、阈值多少、
  "灰色=引擎没对齐,不怪你")+ 指向本文档。
- 鼠标悬浮每列表头有 tooltip。

## 6. 分阶段落地

- **Phase 1 — 引擎透出可靠性(streaming-speech)** ✅ **已实现**:`gop.py` 据「canonical 在对齐段是否
  发出 + 段内有无明确替代音」判 `reliable`,没把握的 bad 音素改判 `uncertain`(**安全口径:有明确替代音
  一律保留 bad,绝不漏判真错读**);`t_start`/`t_end` 输出;词分剔除 uncertain、`bad_phone_count` 只数
  可靠 bad。契约 `docs/pronunciation-assess-api.md` 已补。(`align_conf` 暂未单列,reliable 布尔够用。)
- **Phase 2 — 契约直通(toolkit)** ✅ **已实现**:`shadow::PhoneResult` 加 `reliable`/`t_start`/`t_end`,
  `pron_status` 接纳 `uncertain`;`gop.rs` `AssessPhone`/`map_phone` 透传;`store` detail_json 全量,无需改表。
- **Phase 3 — UI 明细表(zero-desktop)** ✅ **已实现**:ShadowPanel 点词展开**逐音素表**(音素/你的分进度条/
  状态四档/说明)+ 图例 + 「评分细则」折叠说明;`uncertain` 灰呈现「引擎没听准,不算你读错」。
- **Phase 4 — 通过判定放宽** ✅ **已实现**:`MAX_BAD_PHONES=1` + `DEFAULT_THRESHOLD`/`passThreshold` 0.9→0.6;
  passed = 句分≥阈值 且 bad≤1(uncertain 不计)。标定状态在「评分细则」里标注「持续优化中」。
  > **待真机生效**:toolkit-server redeploy(gop.rs 改动)+ 桌面重构建(前端明细表/默认阈值)。

## 7. 开放问题

- `reliable` 判定阈值(align_conf / 后验floor / 峰值在span外)需用真机样本标定,别把真错读误标 uncertain。
- uncertain 比例过高时(整句大半 uncertain)说明录音/对齐整体差 → 顶部给"整体没听清,建议重录"提示。
- 是否在 UI 画**波形 + 音素时间段**(t_start/t_end 已具备)——增强但非首版必需,留 v2。
- 词分剔除 uncertain 后若可靠音素太少(<2),词分可信度低 → 词也标 uncertain。
