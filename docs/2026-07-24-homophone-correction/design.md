# 场景化同音字纠错（homophone correction）设计

> 同一个读音在不同软件里该写成不同的字：在编辑器里说 `hán shù` 是「函数」，在聊天窗口里可能就是「寒暑」。
> 长期目标是让纠错**按场景**进行，且**词表由用户自己的纠错样本自动长出来**。
>
> **但当前只做第一步：数据收集（§1）。** 后面的挖掘 / 替换 / 评估都是设计储备，
> **等收集到的数据能支撑决策后再动**——包括「场景怎么分组」这件事本身。

## 修订记录

| 版本 | 变更 |
|---|---|
| v1（2026-07-24） | 首版 |
| v2（评审 1） | ①「拼音命中即安全」表述过强 → 匹配单位升为词；②评估集**无负样本**、过纠率分母不存在 → §9 重写；③**A 档声学 biasing 在当前架构做不了** → 移出设计 |
| v3（评审 2） | ④**替换执行位置从 orchestrator 移到桌面端交付入口**（长连接下 profile 必然失真，§8.1）；⑤金标须人工确认；⑥diff 拒绝整段 replace；⑦分词降为辅助；⑧统计置信上界作准入；⑨审计落表；⑩依赖需批准，重评估后仅需 1 个 |
| **v4**（2026-07-24 用户定） | **范围收窄：当前只做数据收集**。①`app_profile` 分组**后置**——分组规则应由数据决定，不能先拍脑袋定；②收集期**宽记**：exe / 全路径 / 窗口标题 / 窗口类名 / 交付模式；③`homophone_pairs` 等表**收集期不建**；④原 P1.5 → **P0**，其余全部后置 |

## 0. 设计基调

沿用 [2026-07-21-speech-correction-capture](../2026-07-21-speech-correction-capture/design.md) 的基调：
**自己用的工具，只保正常路径正确 + 绝不把本来对的字改错。**

由此定下长期硬约束（**替换阶段才生效，收集阶段不涉及**）：

> **过纠（把本来正确的字改错）比漏纠严重得多。** 漏纠只是没帮上忙，过纠是主动破坏用户文本，
> 且用户往往在发出去之后才发现。所有取舍一律偏向保守：**宁可不改，不可改错。**

**收集阶段只读不写**（只往自己库里加字段，不碰交付给用户的文本），因此无风险，可直接开工。

---

# 第一部分：当前要做的

## 1. P0 —— 数据收集

### 1.1 为什么先收集，以及为什么「分组」必须后置

原计划在样本上直接落 `app_profile`（`coding` / `chat` / `writing`），由一张手工映射表决定。
**顺序是反的**：你还不知道自己平时都在哪些 app 里说话、各自占比多少、用词差异是否真的成组，
凭什么先定分组？

所以收集期**只记原始事实，不做任何归类**。`app_profile` 这一列**现在不加**——
等数据摊开来看过再定分组维度，那时再加列、回填即可（原始信息都在）。

### 1.2 技术可行性（已验证）

**零新依赖、零新 feature。** [Cargo.toml:63](../../crates/zero-desktop/Cargo.toml:63) 已启用
`Win32_System_Threading` 与 `Win32_UI_WindowsAndMessaging`，
[paste_watch.rs:180](../../crates/zero-desktop/src/modules/speech/paste_watch.rs:180) 现在拿 PID
用的就是这套 API。

| 层 | 做法 | 拿到什么 | 本期 |
|---|---|---|---|
| **L1** | `GetForegroundWindow` → `GetWindowThreadProcessId` → `OpenProcess` + `QueryFullProcessImageNameW` | `Code.exe` + 全路径 | **做** |
| **L1+** | `GetWindowTextW` / `GetClassNameW` | 窗口标题、窗口类名 | **做** |
| L2 | `GetGUIThreadInfo` 取真正持有焦点的控件窗口再反查进程 | 修正 UWP（前台是 `ApplicationFrameHost.exe`）这类 | **不做**，见下 |

**L2 的触发条件由数据自己给出**：如果收集到的 `app_exe` 里出现 `ApplicationFrameHost.exe`
或其他明显不是真实应用的宿主进程，说明需要补 L2；没出现就不必做。这正是先收集的价值。

**已知限制（记录在案，不在本期解决）**：浏览器只能拿到 `chrome.exe` / `msedge.exe`，
无法区分网页版飞书与 GitHub——而这恰恰是用词差异最大的一类场景。**唯一的区分手段是窗口标题**，
这也是本期决定记标题的主要原因。

### 1.3 记什么（收集期宽记）

**迁移 0007**（SQL 文件仅作记录，实际 ALTER 在
[`db/schema.rs::run_migrations`](../../crates/zero-desktop/src/modules/speech/db/schema.rs)
里按列存在性守卫执行，沿用 0006 的做法）：

```sql
ALTER TABLE speech_samples ADD COLUMN app_exe       TEXT;  -- "Code.exe"
ALTER TABLE speech_samples ADD COLUMN app_path      TEXT;  -- 全路径，区分同名 exe
ALTER TABLE speech_samples ADD COLUMN app_title     TEXT;  -- 窗口标题
ALTER TABLE speech_samples ADD COLUMN app_class     TEXT;  -- 窗口类名
ALTER TABLE speech_samples ADD COLUMN delivery_mode TEXT;  -- auto_paste | auto_copy
```

**不加 `app_profile`**（§1.1）。

**隐私说明**：`app_title` 会包含聊天对象名、文档名、网页标题。数据只落本地
speech.db，不外传；但**导出 JSON 时会带出来**，分享导出文件前需自行留意。
用户已知情并选择全记。

### 1.4 什么时候取值（唯一需要小心的地方）

**取值时刻 = 实际交付动作发生的那一刻**，不是收到 `optimized` 事件时——
后者只是 LLM 返回的时刻，用户可能在那 1~2 秒里还没切到目标窗口，或（auto_copy 模式下）
根本还没粘贴。

在**真正的交付入口**抓拍一次**不可变**的 `ForegroundApp`，随交付传给 `record_delivery`；
**capture 模块内部不再自查前台窗口**：

```rust
/// 在交付动作发生的那一刻抓拍，之后不可变。
pub struct ForegroundApp {
    pub exe: Option<String>,     // 文件名
    pub path: Option<String>,    // 全路径
    pub title: Option<String>,   // 窗口标题
    pub class: Option<String>,   // 窗口类名
    pub mode: &'static str,      // "auto_paste" | "auto_copy"
}
```

| 交付模式 | 抓拍点 |
|---|---|
| auto_paste | `paste_watch::type_text_to_foreground` **实际执行打字前**（此时已过「前台是本进程则不动」护栏，拿到的必然是外部 app） |
| auto_copy | `PendingClipboardDelivery` 被 `Ctrl+V` **提升为交付的那一刻** |
| 都没开 | 无抓拍，五个字段全空；该样本不参与后续场景分析 |

**只保留最近一次抓拍**（`paste_watch::LAST_DELIVERY`，快照 + 时间戳）：一个 burst 内多次交付
几乎总在同一个 app，且最近一次最接近用户随后按下采集快捷键的时刻。落库时读取——
**读取时刻用户已切窗口也不影响正确性**，存的是交付那一刻的值。
超过配对时间窗（180s）的抓拍视为过期，五列留空**而不是猜一个**。

这个抓拍点**同时是将来 P3 替换的执行点**（§8.1），P0 建好即可复用。

### 1.5 P0 涉及改动

| 文件 | 改动 |
|---|---|
| `crates/zero-desktop/migrations/0007_sample_app_context.sql` | 新增（记录用，实际 DDL 在 schema.rs） |
| `.../speech/db/schema.rs` | 五列按列存在性守卫 ALTER |
| `.../speech/db/repository.rs` | `NewSample` 加五字段，INSERT 带上 |
| `.../speech/paste_watch.rs` | 新增 `foreground_app()`；打字前抓拍 |
| `.../speech/commands/remote.rs` | auto_copy 提升为交付时抓拍 |
| `.../speech/capture.rs` | `Delivered`/`Burst` 携带 `ForegroundApp`；**不再自查前台窗口** |
| `.../speech/commands/export.rs` | 导出带上新字段（无则跳过） |

**P0 无新依赖、不碰交付文本、不建新表**，可直接开工。

---

# 第二部分：设计储备（等数据支撑，勿提前实现）

> 以下为 v1~v3 的分析结果，结论仍然有效，但**都在 P0 数据之后**。
> 尤其注意：分组维度、是否真的需要替换、替换值不值得做，都应由 P0 的数据回答。

## 2. 背景与现状

### 2.1 热词现状

- **只有一张全局表**：`asr.hotwords`（orchestrator app.db config 键），
  [`resolve_hotwords`](../../crates/orchestrator/src/lib.rs:141) 取原文，
  [`parse_hotwords`](../../crates/orchestrator/src/lib.rs:146) 抽词面。
- **两处消费**：①声学层（ASR 服务周期性拉 `/api/asr-config`）；
  ②LLM 润色 prompt 末尾（[`optimize_prompt_with_hotwords`](../../crates/orchestrator/src/lib.rs:199)）。
- **写入通道**：桌面端
  [`sync_hotword_to_orchestrator`](../../crates/zero-desktop/src/modules/speech/commands/samples.rs:177)
  经 `/api/config` append 进 `asr.hotwords`，仅 `label="hotword"` 触发。

### 2.2 关键架构事实一：声学热词是全局轮询配置

ASR 服务**周期性拉** `/api/asr-config`（[lib.rs:1174](../../crates/orchestrator/src/lib.rs:1174)），
约 15s 热生效；orchestrator 从不主动推。

推论：**「按当前前台应用下发声学热词」在本仓改不动**——按窗口切换改全局键，既有 ~15s 延迟，
又会跨会话互相污染。要做 per-session 声学 biasing 必须改 **streaming-speech 仓**。跨仓课题，不在范围内。

### 2.3 关键架构事实二：WS 是长连接，`hello` 只构造一次

[remote.rs:561](../../crates/zero-desktop/src/modules/speech/commands/remote.rs:561) 的 `hello` 帧
**在重连循环之外构造一次**，[:575](../../crates/zero-desktop/src/modules/speech/commands/remote.rs:575)
的重连循环每次复用同一字符串。

推论：**任何放在 `hello` 里的场景信息，整个会话期间都不更新，连断线重连也不刷新。**
**这否决了「在 orchestrator 侧做场景化替换」的整条路线**，详见 §8.1。

### 2.4 关键架构事实三：`→` 已被现存代码用作语法

[samples.rs:413](../../crates/zero-desktop/src/modules/speech/commands/samples.rs:413) 的现存测试：

```
extract_hotword_term("旧菜盒子 → 韭菜盒子") == "韭菜盒子"
```

`→` 已是「取右边词面」的既定语法。**任何把 `wrong→right` 写进 `asr.hotwords` 系列键的做法，
都会被这段既存逻辑吃掉左半边。** 因此**绝不复用该键承载替换规则**（v2 曾如此设计，已废）。

### 2.5 样本现状

- 三文本已在采集：`text_raw`(R) / `text_optimized`(O) / `correction`(Y')
  （[capture.rs](../../crates/zero-desktop/src/modules/speech/capture.rs)）。
- **无负样本**：采集只在用户主动修改后触发，落库的全是「用户认为需要改」的样本。

## 3. 问题拆解：三类错，只治中间那类

| 类型 | 例 | 判据 | 处理 |
|---|---|---|---|
| **A. 声学听错** | 环境噪声、吞字 | `R` 与 `Y'` **读音不等价** | 词表救不了；**丢弃** |
| **B. 选字错（真同音字）** | 「函数」→「涵数」、人名「张三」→「章三」 | `O` 与 `Y'` **读音等价、汉字不同** | ← **唯一目标** |
| **C. 口误 / 事后改写** | 说完觉得不通顺重写 | 读音差异大、长度变化大 | 噪声；**丢弃** |

**支点**：读音等价这一条规则能把 B 类从 diff 里精确切出来。
**但它只保证挖掘输入干净，不保证** ①这条修改确实是同音纠错（→ §6.3 人工确认）
②替换时机正确（→ §8）。

## 4. 场景分层（分组维度待 P0 数据决定）

```
全局词表          到处都对的词
场景词表(profile) 由 P0 数据归纳，不预设
```

**命中优先级：场景词 > 全局词**；未归类的归 `default` 只用全局词。
**分组规则不在此预设**——这正是 P0 要回答的问题。

## 5. 数据模型（收集期不建）

### 5.1 同音对候选表

```sql
CREATE TABLE IF NOT EXISTS homophone_pairs (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  wrong        TEXT NOT NULL,      -- 完整词
  right        TEXT NOT NULL,      -- 完整词
  reading      TEXT NOT NULL,      -- 归一化读音键
  match_kind   TEXT NOT NULL,      -- exact | polyphone | fuzzy（§6.2）
  scope        TEXT NOT NULL,      -- profile 名，或 "*" = 全局
  hits         INTEGER NOT NULL,
  ctx_samples  TEXT,
  status       TEXT NOT NULL,      -- pending | approved | rejected
  enabled      INTEGER NOT NULL,
  first_seen   TEXT NOT NULL,
  last_seen    TEXT NOT NULL,
  UNIQUE(wrong, right, scope)
);
```

**结构化字段一律留表，不序列化进任何 config 文本键**（§2.4 的教训）。

### 5.2 scope 规则

- 挖掘**只产出具体 profile 的候选**，绝不产出 `"*"`；
- **`"*"` 只能由用户 approve 时显式选择**（`current` / `global` / `reject`）；
- **禁止自动提升为全局**——「到处都出现」恰恰可能说明它是常见词，误替代价更大；
- 冲突：场景词优先；同一 scope 内一个 `wrong` 对多个 `right` 时**一律不替换**。

### 5.3 替换审计表

```sql
CREATE TABLE IF NOT EXISTS homophone_replacements (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  segment_ids TEXT, profile TEXT NOT NULL,
  input_text  TEXT NOT NULL, output_text TEXT NOT NULL,
  pair_ids    TEXT,
  action      TEXT NOT NULL,      -- replaced | skipped
  reason      TEXT,               -- ambiguous | boundary | over_limit | disabled
  created_at  TEXT NOT NULL
);
```

**必须落表而非只写日志**，否则无法与 §9 评估关联。命中但未替换也要记——
它是判断闸门过严/过松的唯一依据。

## 6. 挖掘

**分层**：`speech/homophone.rs` 纯函数（输入 `(O, Y', scope)` → 候选，无 IO，全单测）
+ `repository.rs`/command 负责 IO。

### 6.1 diff 定义

按**字符**的 LCS/Myers diff。**只接受同时满足以下全部条件的 `replace` 块**，
其余（含所有 `insert`/`delete`）一律丢弃：

1. 两侧**字符数相同**；
2. 两侧**读音串等长**且逐位可比；
3. 两侧**纯汉字**（不含标点、空白、拉丁字母、数字）；
4. **前后都必须存在 `equal` 块**——**整段 `replace` 一律拒绝**，哪怕它同时位于首尾；
5. 两侧长度均 **≤ 6 字**。

**反例甲**：`O:请输入函数参数` / `Y':请输入函数字段参数` —— 这是 `insert("字段")` 而非 replace，
丢弃。朴素逐字符配对会把 `参数` 与 `字段` 错位配对，产出完全错误的「同音对」。

**反例乙**：整段被重写、恰好首尾即全文 —— v2 的「或位于文本首尾」会放行，v3 起堵死。

**丢弃是廉价的**：样本源源不断，宁可丢掉一批可疑的，也不能放一条脏对进词表。

### 6.2 读音归一与 `match_kind`

| `match_kind` | 判据 | 处置 |
|---|---|---|
| `exact` | 两侧**带声调读音串完全一致**，涉及字**均非多音字**（或读音可唯一确定） | **只有它自动进候选** |
| `polyphone` | 仅在「读音集合交集非空」的放宽下等价 | 只导出给人工看 |
| `fuzzy` | 需模糊音归并（`zh/z`、`ch/c`、`sh/s`、`n/l`、`an/ang`、`in/ing`、`en/eng`）或忽略声调 | 只导出给人工看 |

**必须分级的原因**：「行、重、长、地、得」这类多音字只要任一读音相同就会被判等；
模糊音同理（「是/四」「南/兰」）。

**入表门槛**：同一 `(wrong, right, scope)` 且 `exact`，累计 `hits >= 2` 才置 `pending`。

### 6.3 人工确认既是入表闸，也是金标来源

**读音等价 ≠ 同音纠错。** 用户的读音等价修改也可能是改专名、改用词习惯、把 LLM 的措辞
改回自己的说法。因此挖掘**只产候选**；**只有经人工 approve 的对才既进词表、又计入金标**。

## 7. 依赖（**需用户明确批准后方可引入**）

[AGENTS.md:69](../../AGENTS.md:69)：**「未经用户明确同意，不得添加新依赖」**。

| 依赖 | 用途 | 结论 |
|---|---|---|
| `similar` | 字符级 diff | **不引入**。自写 LCS 几十行；[capture.rs](../../crates/zero-desktop/src/modules/speech/capture.rs) 的 `similarity` 即先例 |
| `jieba-rs` | 分词 | **不引入**。§8.2 把分词降为可选辅助后不再是安全闸门；省掉 ~5MB 词典与版本漂移 |
| **`pinyin`** | 汉字 → 带声调读音、多音字 | **需引入，待批准**。汉字读音表无法自写。批准前需确认：许可证、体积、多音字覆盖、aarch64 交叉编译 |

**挖掘阶段在依赖决策落定前不开工。P0 不涉及任何依赖。**

## 8. 纠正的执行

### 8.1 执行位置：桌面端交付入口

**v2 的方案（orchestrator 侧替换）不成立**，因为 §2.3 长连接期间 profile 不更新。
更根本的原因是——即使改协议让每个事件都带 profile 也解决不了：

> **profile 是「交付时刻」的属性，而 orchestrator 侧替换必然发生在「交付之前」**——
> 它必须**预测**用户接下来会粘到哪个窗口。用户完全可能在 LLM 处理的 1~2 秒内切走。
> **这个预测在原理上就做不准。**

| 方案 | 判定 |
|---|---|
| 每个 `optimized` 事件带 profile | **不采纳**。只是把不准的预测挪个地方 |
| 切换前台应用即重建 WS 会话 | **不采纳**。会打断录音 |
| **桌面端在实际交付入口替换** | **采纳**。那一刻前台窗口是确定的，**不需要预测** |

**连带简化**：orchestrator 侧改动归零；不需跨进程下发词表；§2.4 的 `→` 冲突一并消失；
替换点与 P0 的抓拍点**是同一处**。

**已知取舍**：直接用 orchestrator web 控制台（不经桌面端）的场景享受不到替换。接受。

### 8.2 匹配方式：pair 字符串优先，分词只作辅助

**不把分词作为唯一安全闸门**（技术词/人名/项目代号会被错切，词典版本变化会让历史结果漂移，
`wrong` 与 `right` 的切分边界还可能不一致）：

1. **主判据**：`approved pair` 的 `wrong` 作**完整字符串**查找；
2. **边界校验（轻规则，无依赖）**：命中位置**前后相邻字符不得为汉字**——
   若相邻仍是汉字，可能切开了更长的词（如「内涵数据」中的「涵数」），**放弃**；
3. **辅助校验（可选）**：若将来引入分词，仅用于**追加**否决，不用于**放行**；
4. **记录实际边界与算法版本**，便于日后识别旧产物。

单字词合法（人名「张/章」正是核心目标）；排除的是「切开更长词」。

### 8.3 替换闸门（全部满足才替换，任一不满足即整段放弃）

1. 只用 `approved` + `enabled=1` + `exact` 的对；
2. 通过 §8.2 的字符串 + 相邻汉字边界校验；
3. **唯一性**：同一 scope 内该 `wrong` 只对应一个 `right`，**一对多一律不替换**；
4. 单次交付替换次数上限（如 3 次），超过则**整段跳过**——异常多的替换通常意味着识别整体崩了；
5. **每次命中都写审计表**（§5.3），含 `skipped` 及原因。**不做回滚**（文本已进外部窗口，无落点）。

## 9. 评估

**v1 的洞**：过纠率的分母（「全部本来正确的位置」）**没有数据来源**，
因为 `speech_samples` 只在用户主动修改后落库。

### 9.1 负样本

**唯一干净的负样本 = 用户显式确认「这段无需修改」的 `O == Y'` 样本**
（采集快捷键加「确认无误」变体，落库 `label="ok"`）。

**不采纳「从正常交付文本按比例抽样」**：抽样文本**没有金标**，
拿它当负样本等于默认「用户没改 = 完全正确」，而现实是用户经常懒得改。

**没有负样本就没有过纠率，没有过纠率就不准上线替换。**

### 9.2 金标口径

**金标 = 经 §6.3 人工 approve 的同音位**，不是全部读音等价位——
未经确认的读音等价修改可能是改专名/改习惯，计入会让两个指标同时失真。

### 9.3 划分与指标

- 测试集按 `(scope, pair)` **分层**划出，**同一词对不得同时出现在挖掘集与测试集**；
- **测试集一旦固定不得反复调参**；

  | 指标 | 定义 | 要求 |
  |---|---|---|
  | 纠错率 | 改对的金标同音位 / 全部**人工确认的**金标同音位 | 越高越好，无硬门槛 |
  | **过纠率** | 负样本中被改动的词位 / 负样本中全部本来正确的词位 | 见 §9.4 |
  | baseline | **完全不替换**时的上述指标 | 必须同时报告 |

### 9.4 统计准入（点估计不够）

- **判据**：过纠率的 **95% 置信上界（Wilson 或 Clopper–Pearson）< 0.5%**；
- **观测单位是样本（burst）不是词位**——同段内词位高度相关，当独立观测会**低估方差**；
- **量级预期**：零误替时上界 ≈ `3/n`，压到 0.5% 以下需 **n ≥ 600 条独立负样本**。
  按日常使用估算，攒够需要相当一段时间——**这是替换阶段短期内到不了的现实原因**。

## 10. 后续分期

| 阶段 | 内容 | 准入门槛 |
|---|---|---|
| **P0** | **数据收集（§1）** | 无。**当前唯一在做的** |
| P1 | 看数据定分组维度，加 `app_profile` 并回填 | P0 数据量足以看出模式 |
| P2a | `homophone.rs` 纯函数 + 候选导出 | **`pinyin` 依赖获批**（§7） |
| P2b | 负样本采集 + approve/scope command | — |
| P2c | 建评估集，跑 baseline | 负样本量满足 §9.4 |
| P3 | 桌面端交付入口的确定性词级替换 | ①§8.2/8.3 闸门就位；②过纠率 95% 置信上界 < 0.5% 且已报 baseline；③审计表就位。**三者缺一不上线** |
| P4 | LLM 受限候选 | 看 P3 收益，可能不做 |
| — | A 档声学 biasing | 依赖 streaming-speech 跨仓改造，独立立项；须与替换**分别评估** |

**已定的取舍**：

1. **不自动上线词表**——自动上线一旦挖出一条脏对，会持续、静默地破坏后续所有文本。
2. **不自动提升全局 scope**（§5.2）。
3. **不在 orchestrator 侧替换**（§8.1）。
4. **A/C 两类错直接丢弃，不留桶**。
5. **一个 burst 跨应用不特殊处理**，取该 burst 期间**最近一次**交付的抓拍（§1.4）。
6. **不做样本复核 UI**，沿用 export JSON 路线。
7. **不做替换回滚**，只留审计表。
8. **分组维度由数据决定，不预设**（v4）。

## 11. 已知风险

| 风险 | 缓解 |
|---|---|
| 浏览器内场景无法区分 | 记窗口标题（§1.3）；效果待 P0 数据验证 |
| UWP 宿主进程掩盖真实应用 | P0 数据里若出现 `ApplicationFrameHost.exe` 再补 L2（§1.2） |
| 窗口标题含隐私 | 只落本地；导出前自行留意（§1.3） |
| 读音相同但语境不该替换 | 字符串 + 边界 + 唯一性闸门 + 人工 approve + 置信上界；**无法根除，靠评估量化** |
| 多音字/模糊音伪候选 | `match_kind` 三级，只有 `exact` 自动进候选 |
| diff 错位 / 整段重写混入 | 五条 replace 块准入（§6.1） |
| 金标含非同音纠错 | 金标必须人工 approve（§9.2） |
| 负样本不足 → 评估失真 | §9.4 置信上界 + 样本量门槛 |

## 12. 评审已消化

### 评审 1（v1 → v2）

| 意见 | 处置 |
|---|---|
| 「拼音命中即可限制过纠」不成立 | **接受**，改为「限制改动范围、不保证改动正确性」。**「支持回滚」不采纳**——无落点 |
| `diff_chars` 未定义、会错位 | **接受**。**「至少一个词而非单字符」澄清**：单字词合法 |
| 评估集没有负样本 | **接受，v1 硬伤**。**「按比例抽样」不采纳**——无金标 |
| 下发没有闭环 | **接受**（v3 因替换移到桌面端而整体消失） |
| `"*"` 规则未定义 | **接受**（§5.2） |
| 前台窗口取值时刻竞态 | **接受**（§1.4） |
| 多音字交集产生伪候选 | **接受**（§6.2 三级） |
| A 档并非低风险 | **接受且加码**：A 档在当前架构**根本做不了**（§2.2） |
| 纯函数与流程矛盾 | **接受**（§6 拆两层） |

### 评审 2（v2 → v3）

| 意见 | 处置 |
|---|---|
| `hello.app_profile` 与长连接冲突 | **接受，阻断问题**。查证发现更糟：`hello` 在重连循环外构造一次（§2.3）。选**「桌面端交付后替换」**——另两案都在预测原理上不可预测的东西（§8.1） |
| pair 压成热词文本丢失结构 | **接受，有实锤**：`extract_hotword_term` 已用 `→` 作语法（§2.4）。采纳上一条后 pair 不再进任何 config 键 |
| 挖掘输入含非同音纠错样本 | **接受**（§6.3 + §9.2：人工 approve 既是入表闸也是金标来源） |
| 首尾条件仍放过整段重写 | **接受**（§6.1 第 4 条 + 长度上限） |
| `jieba-rs` 边界不稳定 | **接受**（§8.2 pair 作完整字符串、分词仅辅助 → jieba 可完全不引入） |
| 缺统计准入标准 | **接受**（§9.4）。**另补**：词位非独立观测，须按样本聚类，否则低估方差 |
| 审计日志无落点 | **接受**（§5.3 落表，`skipped` 及原因一并记） |
| 依赖需遵守 AGENTS.md | **接受，v2 违规**（§7：仅 `pinyin` 需批准，另两个不引入） |

### 用户定案（v3 → v4）

| 决策 | 处置 |
|---|---|
| 「先做数据收集，别一次铺开」 | **接受**。原 P1.5 提为 P0 并收窄，其余全部移入「设计储备」 |
| 「分组不能先拍脑袋」 | **接受**。`app_profile` 列后置，收集期只记原始事实（§1.1） |
| 「标题可以记，前期尽量全记」 | **接受**。五字段宽记：exe / 全路径 / 标题 / 类名 / 交付模式（§1.3），隐私影响已说明 |
