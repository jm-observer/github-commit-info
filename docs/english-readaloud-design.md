# English 朗读评测（Read-Aloud）需求 / 设计

> 状态:**⏸ 暂缓(2026-06-26)**——需求与设计已成形,但**暂不落地**。
> 原因:本模式高度依赖「发音评测」的判分质量,而当前评测内核尚不够完善——朗读是「冷读、无范读
> 先验」的检验场景,对评测准确度的要求**比跟读更高**(跟读有范读兜底、更宽容)。评测一旦误判,
> 在「零按钮、空格连读」的全自动体验下会被放大(用户读对被判错、或读错被放过却无从察觉),体验反而更糟。
> **解阻条件**:待 GOP 发音级评测(含真标定 / 真人样本验证,见
> [english-shadow-gop-design.md](english-shadow-gop-design.md) 与
> [gb10-pronunciation-assess-deploy] 的待办)在跟读场景验证扎实、判分稳定后,再回来推进本稿。
> 设计本身评审认为方向成立(纯前端薄叠加 + 一处落库加列),非技术受阻,纯属**等评测成熟度**。
>
> 日期:2026-06-26  ·  范围:`zero-desktop` english 模块 +
> `toolkit-server` shadow 模块(复用,基本不新增后端)
>
> 关联:[english-shadow-design.md](english-shadow-design.md)(跟读 v1:本稿的地基) ·
> [english-shadow-gop-design.md](english-shadow-gop-design.md)(发音级 GOP) ·
> [english-shadow-realtime-design.md](english-shadow-realtime-design.md)(流式 GOP) ·
> [english-shadow-todo.md](english-shadow-todo.md)(跟读优化 TODO)

## 1. 目标与一句话定义

在现有「跟读」之外,加一个**朗读**练习模式:**不播放参考音频**,直接把参考文本摆给用户,
用户**冷读**(看着文本读出来),系统**自动录音、自动评测**并判分/计数——其余(评分内核、采集、落库、
逐词标色、统计)与跟读**完全一致**。

**核心体验:零按钮。** 开启朗读后,用户**唯一的操作就是按空格进下一句**——不点「录音」、不点
「评分」、不点「提交」。看一句、读一句、敲个空格,如此循环(交互详见 §4.1)。

**一句话区别**:

| 模式 | 流程 | 心智 |
|---|---|---|
| **跟读 Shadow**(已有) | 先**听**参考音频 → 模仿着读 → 评测 | 「学」:有范读兜底,降低难度 |
| **朗读 Read-Aloud**(本稿) | **不听**,直接看文本读 → 评测 | 「测/练」:无范读先验,检验真实发音水平 |

明确**不做**:朗读≠默读(silent),参考文本**照常显示**,用户出声读;只是去掉「先放范读音频」
这一步。「看不到原文的纯听写/复述」不在本稿范围。

## 2. 核心洞察:朗读是「去掉播放闸门」的跟读

跟读现状是一条**串行闸门**(见 [english-shadow-design.md](english-shadow-design.md) §3):

```
播放参考音频(播 maxPlayCount 次) → onAwaitShadow → 开麦采集 → 上传评测 → 判分/计数/推进
└──────────────── 朗读砍掉这一段 ────────────────┘
```

评分管线(v1 ASR 文本对齐 / GOP 发音级 / 流式 GOP)**根本不消费参考音频**——它们只拿
「用户录音 + 参考文本 `ref_text`」打分。所以朗读**不需要任何新评分能力**:把「先播放参考」
这一步跳过,直接「显示文本 → 开麦 → 评测」即可。

> **GOP 与朗读是绝配**:GOP 直接对 `ref_text` 的音素序列给发音分,本就不需要范读音频做参照
> (区别于「比对两段音频」的思路)。朗读模式下无范读先验,**恰恰适合用 GOP 测真实发音**;
> v1 ASR 对齐同样适用(只是仍是「内容/可懂度」而非发音标准度,见跟读设计 §6)。

## 3. 关键决策(待拍板,给出推荐)

| # | 决策点 | 推荐取值 | 理由 / 备选 |
|---|---|---|---|
| 1 | 朗读的产品形态 | **作为练习「模式」开关**,与跟读并列(`practiceMode: shadow \| readaloud`),共用 ShadowPanel | 不另起一套面板/控制器;UI 仅按模式隐藏「播放参考」与「重听」 |
| 2 | 是否复用评分后端 | **整套复用**,后端零改(评分/采集/落库/契约不动) | 唯一可选的小改是落库加 `mode` 列区分统计(决策 4) |
| 3 | 评分内核优先级 | **优先 GOP**(`GOP_BASE_URL` 配了走发音级);未配回退 v1 ASR | 朗读无范读先验,发音级评测更有价值;回退路径与跟读一致 |
| 4 | 统计是否区分朗读/跟读 | **区分**:`shadow_attempt` / `shadow_stat` 加 `mode`(`shadow`/`readaloud`),幂等 `ALTER TABLE ADD COLUMN`,**不 bump `SCHEMA_VERSION`** | 跟读(有范读)和朗读(冷读)难度不同,混在一起统计会失真;遵循 GOP 的 `detail_json` 加列惯例 |
| 5 | 录音前提 | 朗读**无回声问题**(不外放音频),`auto` VAD + `button` 兜底沿用;**可天然支持流式边读边评而不必戴耳机** | 跟读流式的「回声致命伤 / 戴耳机硬前提」在朗读下不存在(决策见 §6) |
| 6 | 交互模型 | **零按钮全自动**:进句→自动开麦→自动评测→显示结果;用户**唯一操作 = 空格进下一句**(见 §4.1) | 直接满足跟读 TODO-1 的「全自动免手操」诉求,且更彻底;不存在「点录音 / 点评分」 |

## 4. 整体形态与状态机

朗读复用跟读的「闸门接入 AudioPlayerService」机制(`setShadowGate` / `onAwaitShadow` /
`replayCurrent`),差异在**到达单元时不播放参考音频**,且**全程无需任何按钮**:

```
进入一个单元(句/词)
      │
      ▼
显示参考文本(不播音频) ──▶ 自动开麦采集(VAD 自动判停,无「开始录音」按钮)
      │
      ▼  (VAD 判停 / 用户按空格强制结束本句)
自动上传录音 + ref_text → toolkit-server 评测(GOP 优先,回退 v1) ——无「评分」按钮
      │
      ▼
内联显示结果(分数 + 逐词/音素标色)+ success/fail 计数累加
      │
      ▼
等用户按【空格】──▶ 下一个单元(无论通过与否,用户自定节奏)
```

唯一交互见 §4.1。「重听参考 / 开始录音 / 提交评分 / 下一句」这些按钮在朗读全自动模式下**一律不出现**。

### 4.1 唯一交互:空格 = 下一句

开启朗读后,用户的**全部操作就是按空格**。一次完整循环是「**看 → 读 → 空格**」,无限重复:

| 动作 | 谁触发 | 说明 |
|---|---|---|
| 显示文本 | 系统 | 进句即显示,不播音频 |
| 开始录音 | 系统(自动) | 进句即开麦,无需点「录音」 |
| 结束录音 | 系统(VAD 自动判停)**或**用户按空格 | 读完静音 ~900ms 自动停;不想等可直接按空格立即收音 |
| 评测打分 | 系统(自动) | 收音即评,无需点「评分」;评测异步,不卡 UI |
| 进下一句 | **用户按空格** | **唯一手动操作**;用户按自己节奏推进 |

**空格的双重语义(按时机自动判断,用户无感)**:
- 若本句**还在录音**(评分未出)→ 空格 = 立即结束本句录音并在后台评测,**同时**进下一句
  (上一句的分数/计数在后台落定,不阻塞)。
- 若本句**已出分**→ 空格 = 直接进下一句。

> 即用户只需「读完一句敲一下空格」即可一路读下去;愿意等 VAD 自停也行,敲空格只是更快。
> **不设「通过才放行」的闸门**——朗读是用户自定节奏的练习/检验,通过与否都计数留痕,推进权在用户。

**接入点**(均已存在,只加分支):
- `AudioPlayerService` 到达单元时,若 `practiceMode === 'readaloud'`:跳过播放、直接 emit
  `onAwaitShadow`(等效「播放次数=0」)→ `ShadowController` 立即 `captureUtterance({mode:'auto'})`。
- `captureMode` 在朗读全自动下**强制 `auto`**(VAD 判停);`button` 模式不暴露(否则又要点按钮)。
- `autoAdvanceOnPass` 在朗读下**不生效**:推进只认空格,不按通过与否自动跳(决策见 §4.1)。
- 全局空格键监听挂在朗读面板激活期间;失焦/切走时解绑,避免误触。
- `replayCurrent()` 在朗读下语义退化为「重新开麦」,但**默认不暴露重读按钮**(冷读一遍即走,
  想重读就让那一句失败、靠后续复习)。

## 5. 复用边界(朗读哪些直接用、哪些改)

| 模块 | 朗读是否复用 | 说明 |
|---|---|---|
| 评分内核(v1 ASR 对齐 / GOP / 流式 GOP) | ✅ 原样 | 不消费参考音频,无需改 |
| toolkit-server `/api/web/shadow/score` + `/stream` 契约 | ✅ 原样 | 可选透传 `mode` 入参用于落库归类(决策 4) |
| 采集 `captureUtterance` / `streamingCapture` | ✅ 原样 | 同一套 VAD/流式采集 |
| `shadow_attempt` / `shadow_stat` 落库 | ✅ 复用,**加 `mode` 列** | 幂等 ALTER,区分朗读/跟读统计 |
| `ShadowPanel` 逐词/音素标色、hint、计数 | ✅ 大部分留 | 仅隐藏「播放参考 / 重听」相关 UI |
| 「标注重点」复用 english 后端 annotate | ✅ 原样 | 与跟读一致 |
| 「先播放参考音频」播放闸门 | ❌ 朗读跳过 | 唯一实质删除项 |
| 偏好 `shadowPrefs` | ⚠️ 扩展 | 加 `practiceMode` 字段(见 §7) |

一句话:**「评分/采集/落库/契约/标色」整套活;朗读只是前端少走一步播放 + 统计多一个维度。**

## 6. 朗读的「流式边读边评」更易落地(可选增强)

跟读做流式(边读边逐词点亮)的头号拦路虎是**回声**:边外放范读边开麦,参考音被录进去污染评测,
必须「戴耳机 / AEC」(见 [realtime 设计](english-shadow-realtime-design.md) 决策 7、风险 §9)。

**朗读模式天生不外放任何音频** → 无回声 → **不必戴耳机**即可直接复用已建成的流式 GOP 链路
(`/api/web/shadow/stream` + `streamScore`)。因此:

- 朗读流式是「**零额外前提**」的体验升级:`streaming` 开 → 边读边逐词点亮 → `end` 触发批量
  finalize 出权威分,与跟读流式同一套代码。
- 建议把**朗读作为流式 GOP 的首选试点场景**(先在无回声的朗读上验证流式体验,再回推跟读)。

## 7. 配置项(设置页)

在 [shadowPrefs.ts](../crates/zero-desktop/ui/src/modules/english/shadow/shadowPrefs.ts) 扩展:

| 项 | 默认 | 说明 |
|---|---|---|
| `practiceMode` | `shadow` | **新增**:`shadow`(先听后读)/ `readaloud`(直接朗读评测) |
| `granularity` | `sentence` | 句 / 词,与跟读共用 |
| `passThreshold` | 0.6 | 命中率 / 发音阈值,与跟读共用(朗读只用于判定 pass 着色/计数,不拦推进) |
| `streaming` | 关 | 边读边逐词点亮;朗读下无戴耳机前提(§6) |

> 朗读不引入「总开关」之外的新顶层开关——它是 english 练习的一个**模式选择**,和跟读互斥单选。
> **朗读下不暴露 `captureMode` / `autoAdvanceOnPass`**:前者强制 `auto`(VAD 自动收音,否则又要点按钮),
> 后者不生效(推进只认空格,见 §4.1)——这两项仍服务于跟读模式。

## 8. 数据模型(toolkit.db,增量加列)

遵循 GOP 的 `detail_json` 加列惯例(幂等 `ALTER TABLE ADD COLUMN`,**不 bump `SCHEMA_VERSION`**,
见 [schema.rs](../crates/toolkit-core/src/schema.rs) §detail_json 注释与
[migrations.rs](../crates/toolkit-core/src/migrations.rs)):

```sql
-- 存量库:幂等补列(新库由 DDL_V1 直接建出,默认 'shadow' 兼容历史数据)
ALTER TABLE shadow_attempt ADD COLUMN mode TEXT NOT NULL DEFAULT 'shadow';  -- 'shadow' | 'readaloud'
ALTER TABLE shadow_stat    ADD COLUMN mode TEXT NOT NULL DEFAULT 'shadow';
```

- `mode` 进 `shadow_stat` 复合主键(与 `customer_id/sentence_id/word_index/kind` 并列),
  让同一单元的朗读、跟读统计**各自独立累加**,互不覆盖。
- `score` 端点新增可选 query `mode`(缺省 `shadow`,**老客户端零回归**);落 `shadow_attempt` 与
  upsert `shadow_stat` 时带上。
- `stats` 端点回读时按 `mode` 区分返回(或入参 `mode` 过滤),前端按当前模式回填计数。

> 若评审认为「朗读/跟读统计不必分家」,决策 4 可降级为不加列(共用一套计数)——但默认推荐分开,
> 否则「冷读失败」会拉低「跟读成功率」,难度维度被抹平。

## 9. 前端改动清单(zero-desktop)

1. `shadowPrefs`:加 `practiceMode`;设置页加「跟读 / 朗读」单选。
2. `AudioPlayerService`:到达单元时按 `practiceMode` 决定「播放参考 N 次」或「跳过播放直接
   `onAwaitShadow`」;`replayCurrent` 在朗读下=重新开麦。**跟读路径逐字节不变(零回归)**。
3. `ShadowController`(或面板内逻辑):朗读下 `onAwaitShadow` → 立即 `captureUtterance({mode:'auto'})`
   → `done` resolve 后自动 `scoreShadow` → 内联展示。**全程无按钮触发**。
4. **空格键交互**:朗读面板激活期间监听 `keydown`(Space):录音中 → 强制 `stop()` + 后台评测 +
   立即 `nextSentence()`;已出分 → 直接 `nextSentence()`。失焦/卸载时解绑(§4.1)。
5. `ShadowPanel`:朗读模式**隐藏整排手动按钮**(开始/停止/重读/重听/评分/下一句),仅显示文本 +
   实时状态(「录音中… / 评估中… / 结果」)+ 一行提示「读完按空格进下一句」;分数/标色/计数复用。
6. `ShadowService.scoreShadow` / `streamScore`:透传 `mode`(Tauri 命令加一个入参,默认 `shadow`)。
7. `fetchShadowStats`:按当前 `practiceMode` 回读对应统计。

## 10. 风险与取舍

- **冷读对初学者偏难**:没有范读先验,生词易读错 → 失败率高、易挫败。对策:朗读定位为「进阶/检验」
  场景;UI 提供「切回跟读」快捷出口;可设朗读阈值略宽容。
- **v1 ASR 仍只评「内容」不评「发音」**:朗读下若 `GOP_BASE_URL` 未配,回退 v1,用户读音不准但
  ASR 听懂仍判过——需在 UI 说明「发音级评测需 GOP 后端」(与跟读一致,GOP 未配回退 v1)。
- **统计语义**:朗读与跟读分开计数(决策 4)是默认,但需在统计页清晰标注两条曲线,避免用户困惑。
- **空文本/超长句**:朗读直接面对文本,需保证 `ref_text` 非空、过长句给录音窗口上限(沿用
  `recordWindowMs` / `maxMs`)。
- **零回归底线**:`practiceMode === 'shadow'` 时所有行为(播放、闸门、落库、统计)与今天**完全一致**;
  朗读是纯叠加路径。

## 11. 分阶段落地

- **Phase 1(MVP,纯前端 + 一处落库加列)**
  1. `shadowPrefs` 加 `practiceMode` + 设置页单选。
  2. `AudioPlayerService` 朗读分支(跳过播放直接进闸门)+ `ShadowPanel` 隐藏范读相关 UI。
  3. `shadow_attempt`/`shadow_stat` 加 `mode` 列(幂等 ALTER)+ score/stats 端点透传 `mode`。
  4. 验收:开朗读 → 不播音频、直接显示文本并自动开麦 → 读完(VAD 自停或按空格)自动出分标色 →
     **全程除空格外不碰任何按钮**,空格一路读下去 → 计数与跟读分开累加、刷新后仍在。
- **Phase 2(朗读流式边读边评)**:在朗读上复用流式 GOP(§6),**无戴耳机前提**先把流式体验在
  朗读场景验证扎实,再回推跟读流式。
- **Phase 3(体验打磨)**:朗读专属难度提示 / 阈值;朗读 vs 跟读统计对比页(同一单元两种模式的
  成功率对照,反映「脱离范读后的真实水平差距」)。

## 12. 与跟读 / GOP / 流式的关系(一句话)

朗读**不新增任何评分能力**,是跟读链路「去掉播放闸门 + 统计加一个 `mode` 维度」的薄前端叠加;
评分仍走 GOP(优先)/ v1(回退)/ 流式 GOP(可选,且朗读下无回声前提更易落地),输出契约、落库、
逐词/音素标色与跟读完全一致,`practiceMode=shadow` 时对现有跟读零回归。
