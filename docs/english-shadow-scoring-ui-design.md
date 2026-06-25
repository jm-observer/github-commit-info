# 跟读发音评分「可解释 + 对齐明细」设计

> 状态:设计稿(待评审)  ·  日期:2026-06-26  ·  范围:评分细则透明化 + 新增「对齐可靠性」
> + `zero-desktop` ShadowPanel 明细表展示
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

### 2.3 词分聚合(抗对齐噪声)

旧:`0.6·均值 + 0.4·最低` —— **一个被误杀的音素(0.05)就把词分拖垮**。
新:**先剔除 `uncertain` 音素**,再对剩余可靠音素做 `0.6·均值 + 0.4·最低`;若一个词的音素**全 uncertain**,
该词整体标 uncertain(不计分、不拦通过)。这样 `delicious` 中段被误杀时,词分由可靠音素决定,不冤枉。

### 2.4 通过判定(放宽 + 解耦对齐噪声)

`passed = 句分 ≥ threshold 且 bad 音素数 ≤ max_bad`
- `max_bad` 默认 **1**(放宽"零 bad"——真人读一句几乎总有 1 个边界音;`uncertain` 不计入 bad)。
- `threshold` 默认建议 **0.7**(GOP 分天花板 ~0.9,旧默认 0.9 不合理)。
- 二者都可配(面板滑杆 + 服务端默认)。

### 2.5 标定来源(展示"分数怎么标的")

GOP 原始 log 后验经 `sigmoid(a·(raw − b))` 映射到 0~1;`a/b/ok_min/warn_min` 存
`calibration.json`(`GOP_CALIBRATION`)。**当前为按少量真机样本手调的临时值**,正式版用
speechocean762 拟合。UI 的"评分细则"说明里标注"当前标定:实验值/正式值"。

## 3. 新增「对齐可靠性」(本设计核心机制)

判断一个音素的低分到底是**真读错**还是**没对齐上**:

- **信号**:① forced_align 给该音素 token 的**对齐置信度**(per-token score);② 该音素时间段内
  canonical 后验的**绝对值**(极低如 < −5 多半是没对齐);③ 该音素的**真实后验峰值是否落在其对齐 span 内**
  (落在 span 外 = 错位)。
- **判定**:满足"对齐置信度低 / 峰值在 span 外 / 后验极低"→ 标 **`uncertain`**(而非 bad)。
- **效果**:`delicious` 中段 L/IH/SH 会被判 uncertain(灰、问号),不再红、不拉低词分、不拦通过;
  而 `think` 的 /θ/→/s/ 是**对齐可靠 + 后验明确偏低 + 有清晰的 actual_ph**,仍判 bad。

> 这是上一轮讨论的"方案 A 对齐置信度降权"的产品化:**不确定就不判错**。需在引擎侧用 forced_align
> 的 score + 峰值位置算出 `align_conf` 与 `reliable`,经契约透出。

## 4. 契约扩展(向下兼容,全 Option/默认)

`PhoneResult` 新增(服务端 `gop.py`/`streaming.py` → toolkit `shadow::PhoneResult` → 前端 types):

| 字段 | 类型 | 说明 |
|---|---|---|
| `t_start` / `t_end` | float? | 该音素对齐时间段(秒),供"在波形上定位"展示 |
| `align_conf` | float? | 对齐置信度 0~1(forced_align score 归一) |
| `reliable` | bool? | 综合判定:false → 前端按 `uncertain` 呈现 |
| `pron_status` | 扩展枚举 | 增加 `uncertain` |

`WordResult` 可加 `pron_status` 扩 `uncertain`。`ScoreResult` 句级 `bad_phone_count` 语义不变
(**只数 reliable 的 bad**)。所有新增字段 `skip_serializing_if None`,老前端忽略即零回归。

## 5. UI 设计(ShadowPanel 明细表)

### 5.1 结构

- **句级**:顶部「句发音分 xx% · 通过/未通过 · 评分细则 ⓘ」。点 ⓘ 展开**评分细则说明**(2.x 的人话版 +
  当前阈值/标定状态)。
- **词级行**:逐词色块(绿/黄/红/灰);点词**展开音素明细表**。
- **音素明细表**(每行一个音素):

| 列 | 内容 |
|---|---|
| 音素 | IPA + ARPAbet(如 `/θ/ TH`) |
| 你的分 | 0~100(进度条 + 数字) |
| 状态 | ok/warn/bad/uncertain 图标+色 |
| 期望→实际 | `bad` 显示 `/θ/ → /s/`;`uncertain` 显示「未对齐/没听准」 |
| 提示 | `hint`(针对性纠音);uncertain 显示「引擎没对齐好,换个安静环境/慢点再读」 |

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
