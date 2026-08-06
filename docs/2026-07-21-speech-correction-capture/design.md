# 语音纠错一键采集（in-place correction capture）设计

> 用户觉得本次中文优化 `O` 有点问题，就在输入框里改好，**手动复制改好的整段文本，按一个专用快捷键**，
> 程序读当前剪贴板拿到 `Y'`，把「我给你的 `O` → 你改好的 `Y'`」连同**原始 ASR `R` 与 segment_ids**
> 一起**原样记录关联**。**本期只采集文本对、不分类、不碰音频**——先攒住干净的成对语料，后续再看能做哪些分类。
>
> **（2026-07-22 定）音频本期整个不做**：`speech_samples` 的音频列留空/`audio_status="skipped"`，
> 取音频（编排器 1 天内逐段拉）连同分类一起后置到 P2。下文凡涉及音频处，均为 P2 内容、P1 跳过。

## 术语锚点（2026-07-25 定，两条线不要再混）

语音数据收集有**两个独立功能**，此前讨论中反复被混为一谈，正式命名如下，后续代码/文档/沟通一律用这两个词：

| 名称 | 代号 | 触发 | 产出物 | 表 | 代码 | 一句话 |
|---|---|---|---|---|---|---|
| **纠错采集** | `correction_capture` | 用户 `Ctrl+C` 后按 `Ctrl+Alt+X` **手动** | **样本**（sample） | `speech_samples` | `capture.rs` | 用户发现某条优化写错了，改好后主动留档，带修正 `Y'` |
| **场景记录** | `scene_log` | 每次语音结果落进外部应用时**自动** | **记录**（record） | `speech_scenes` | `scene_log.rs` | 程序自动记「这段话被送进了哪个 App / 什么窗口标题」，没有 `Y'` |

- 「样本」永远指纠错采集的产物，「记录」永远指场景记录的产物。
- 场景记录采**选项 A（交付时记）**：只记有去处的语音，App 一定准确；代价是不开自动粘贴/自动复制时不记（本文档 §关联待办 会补 A+B 的可能）。
- 本节是两个功能的顶层锚点；场景记录的数据模型细节见 [../2026-07-24-homophone-correction/design.md](../2026-07-24-homophone-correction/design.md)。

### 待办（2026-07-25 排期，均已落地）

1. **场景记录 ↔ 纠错样本关联**（优先）：现在无法区分「用户接受了的交付」与「被改掉的交付」。直接拿场景记录统计表达风格，会把 ASR/LLM 的错误当成用户习惯学进去。数据越攒越多、脏的部分越难剔，故先做。→ `speech_scenes.correction_sample_id`，纠错样本落库后按段号回标。
2. **L2 焦点控件反查**：`GetGUIThreadInfo` 取真正持焦点的控件窗口再反查进程，修 UWP 壳进程（前台记成 `ApplicationFrameHost.exe`）问题，顺带能区分输入框类型。→ 已按 `EnumChildWindows` 反查承载的 CoreWindow 实现（未走 `GetGUIThreadInfo`，效果相同且更省）；「区分输入框类型」仍未做。
3. **`app_title` 纳入统计维度**：标题比 exe 更能反映真实场景（网页标题、文档名），场景记录的聚合应从「按 exe」扩展到「按 title」。→ `SceneStats.top_titles`（原始聚合，不解析归纳）。

### 场景记录的四道数据质量闸（2026-07-25 review 后补齐）

按顺序对应四种会把脏数据记进 `speech_scenes` 的路径，都已在代码里堵上：

1. **待交付内容有保质期 + 内容校验**（`scene_log::PENDING_TTL` = 3 分钟 + worker 侧比对剪贴板现值）。
   auto_copy 写完剪贴板后用户没粘（或用右键菜单粘的，键盘钩子看不见），标记会一直挂着；几小时后
   用户复制自己的东西按 `Ctrl+V`，就会把那段陈年语音文本配上此刻这个无关应用记成一次交付。仅有
   「剪贴板里有待交付内容」这个布尔门控挡不住它——布尔不会自己过期。
2. **auto_paste 记累计全文、覆盖同一行**。合并链下一句话分几次增量打字，逐次各记一行的话
   `text` 就成了一堆半截话，`Retype` 退格擦掉的字符还留在旧行里虚增字数。现由 worker 按
   「会话+段号+模式+内容种类」找到那一行原地覆盖（`repository::update_scene_text`）。
3. **回标限定时间窗 + 会话**。段号是服务端的自增计数器，换服务端 / 重建它的 `app.db` 后会从头
   再来；不设界的话重开的小段号会把几个月前的历史行无声误标成「被改过」。
4. **`session_id` 真的填上了**。此前该列恒为 NULL，既没法消歧也白占一列；现从服务端 `ready`
   帧的 `session_id` 取。

### 开关与隐私

场景记录是**常开**的全量收集，但记的是交付文本 + 窗口标题，标题里可能有聊天对象/文档名/网页
标题这类比语音本身更敏感的信息，故给了独立开关（语音页「场景记录」卡片右上角，设置键
`scene_log.enabled`，默认开）。它**只关落库**：待交付标记照常暂存——那个标记同时是键盘钩子
判断「这一下 `Ctrl+V` 是不是语音交付」的依据，纠错采集的应用抓拍挂在同一个判断上。

### 已知脏数据（修复前产物）

- 场景记录的 auto_copy 抓拍此前无门控：全局任意一次 `Ctrl+V` 都会覆盖抓拍。已加门控（剪贴板里确有待交付语音内容才算数）。修复前落的样本里，`delivery_mode=auto_copy` 且用户实际用 auto_paste 的记录，其应用上下文不可信，分析时排除。
- 上面第 1 道闸补齐前（仅有布尔门控那一版）落的场景记录，可能有个别「陈旧剪贴板内容 + 无关应用」的错配行；`auto_paste` 的记录在第 2 道闸之前是按打字次数分行的半截话，统计字数偏高。两者都以 2026-07-25 为界。

## 0. 设计基调（重要）

**这是用户自己用的工具，用户会自己把控操作。** 只保证正常路径正确 + 绝不破坏用户数据，不为极端并发/误操作做重防御：

- 采集由**专用快捷键**显式触发；程序**不注入按键、从不写剪贴板**（因此天然不破坏用户剪贴板）。
- **不观测每一次 Ctrl+C**（那样太吵）。编辑过程中的复制、选词换词都不理会——只有你按专用快捷键那一下，才读一次剪贴板。
- 偶发一条脏样本、或漏采一条，都可接受（导出时能筛/删；漏了再复制按一次）。不追求 100% 捕获。

## 0.1 交互契约

**采集手势 = 改好 → 自己复制整段 → 按专用快捷键（默认 `Ctrl+Alt+X`，可配）。**
按键时程序读当前剪贴板 = `Y'`，与最近交付配对入库。剪贴板保持不动，你可继续粘贴/发送。

## 1. 背景与现状

- **吐字**：`remote.rs` 在 `optimized`（中文）/`translated`（英文）事件里把 `O` 打进焦点框（auto_paste）或
  写剪贴板（auto_copy）；连续多段会被**合并窗口**拼成一整段。
- **音频（关键事实）**：**本仓不主动保存听写音频**。音频在**编排器(GB10)只留约 1 天**，按 segment id 经
  `GET {http_base}/api/segments/{id}/audio` 取，过期 404。现有 `samples.rs::speech_mark_sample` 就是用这个
  **同一个 segment id 走这条端点**把音频拉下来存档的（`fetch_and_store_audio`）——即采集取音频是**验证过的路径**。
- **样本落库**：`speech_mark_sample` 已实现「写 `speech_samples` + 拉音频存档」，只差触发点不在 UI。
- **表结构**（`0005_speech_samples.sql`）：`label ∈ {…|other}`，三文本列可存 `R`/`O`/`Y'`。
- **设置管线已存在**：`speech_get_settings`/`speech_apply_settings` + `CombinedSettings`。
- **前端现状（校正）**：`listSamples()` 仅定义无调用、无样本列表页；样本目前**只能 `exportSamples` 导出查看**。

## 2. 目标 / 非目标

**目标**：专用快捷键触发 → 读剪贴板拿 `Y'` → 与最近交付配对 → 落库，留住 `R/O/Y'` 三文本 + 该整段音频；
一个总开关能关。

**非目标**：不注入按键/不写剪贴板；不观测 Ctrl+C；不分类；不改 `asr.hotwords`/不训练；不做样本列表页/复核 UI。

## 3. 总体数据流

```
listen ─optimized/translated 逐段─→ 打进焦点框 / 写剪贴板
        │ 同时: 按合并窗口把连续段累积成【当前 burst】
        ▼
  SpeechState.current_burst = { O合并整段, R拼接整段, segment_ids=[所有构成段], started_at, last_at }
  说话停顿(超合并窗口) → 封存当前 burst 进 recent_bursts(ring, 留最近 1~3 个), 开新 burst

用户【改好整段 → 自己复制 → 按专用快捷键】
   ▼
后台 worker:
   1. 读当前剪贴板 = Y'（读不到文本 → 忽略）
   2. 在 {current_burst} ∪ recent_bursts（时间窗内、未采过）里选与 Y' 相似度最高且过闸门的 burst
      无匹配 / Y'==O → 忽略
   3. 插一行样本（label=other, source=copy, audio_status=pending）→ 记 captured 去重
   4. 异步: 对该 burst 的每个 segment_id 逐段拉音频存档 → 回填 audio_status（不阻塞）
   5. 轻量 tray 反馈
```

## 4. 分类为何后移

`R/O/Y'` + 音频原样存全，将来分类/训练可对历史数据离线跑、可回溯重标；词级改动 diff `O` 与 `Y'` 即可还原，
不必现在做「热词层 vs 优化层」分类。

## 5. 实现

### 5.1 交付按 burst 累积（消化「一段对应多次交付」）

**交付单位 = burst（合并出来的整段），不是单段**——因为用户复制的就是整段，而整段常由多段拼成。

```rust
struct Burst { o_opt: String, r_raw: String, segment_ids: Vec<i64>, session_id: Option<String>,
               started_at: Instant, last_at: Instant, captured: Vec<String> }
// SpeechState:
//   current_burst:  Arc<RwLock<Option<Burst>>>          // 正在累积的整段
//   recent_bursts:  Arc<RwLock<VecDeque<Burst>>>        // 已封存，cap 小（如 3）
//   capture_enabled: 见 §5.5
```

- 每来一段 `optimized`（中文）/`translated`（英文）：若距上段 ≤ 合并窗口 → 追加到 `current_burst`
  （`o_opt` 拼接、`r_raw` 拼接、`segment_ids` 追加）；否则先把 `current_burst` 封存进 `recent_bursts`、再开新的。
  —— 复用 `remote.rs` 现有的 `merge_window_ms` 与拼接逻辑（`join_dedup`）。
- 配对时 `current_burst` 与 `recent_bursts` 都是候选，覆盖「刚说完就采」与「回头改前一段」。

### 5.2 触发：专用快捷键（不注入、不观测 Ctrl+C）

- 注册一个全局快捷键（默认 `Ctrl+Alt+X`）。实现二选一：复用 `paste_watch.rs` 的 `WH_KEYBOARD_LL` 钩子检测该
  组合并**吞掉该组合**（避免落进目标 app），或用 tauri 全局快捷键插件。**倾向复用现有钩子**（无新依赖、能吞键）。
- 命中 → `try_send(())` 给后台 worker。**只读剪贴板、不合成任何键**，故无「注入 Ctrl+C 撞 Alt / 需等释放」问题。
- 单飞：一个原子标志防连按重复；worker 插完样本即清（音频异步不占单飞）。

### 5.3 worker：读剪贴板 + 配对 + 落库

```
Y' = clipboard.read_text()?                            // 读不到文本(图片等) → 忽略
best = ({current_burst} ∪ recent_bursts)
        .filter(|b| now - b.last_at <= TIME_WINDOW && !b.captured.contains(Y'))
        .filter_map(|b| pairs_with_delivery(&b.o_opt, Y', THRESHOLD).map(|s| (b, s)))
        .max_by(similarity)?;                          // 无匹配 → 忽略
if Y' == best.o_opt { return }                         // 没改，无价值
sample_id = insert_sample(R=best.r_raw, O=best.o_opt, Y', label="other", source="copy",
                          segment_ids=best.segment_ids, note=相似度, audio_status="pending")?;
best.captured.push(Y'); clear 单飞;
tokio::spawn(fetch_audio_for_burst(sample_id, best.segment_ids));   // §5.5，异步
```

纯函数（可单测）：

```rust
fn pairs_with_delivery(o: &str, y: &str, threshold: f32) -> Option<f32> {
    let d = levenshtein_chars(o, y) as f32;
    let m = o.chars().count().max(y.chars().count()) as f32;
    let sim = if m == 0.0 { 1.0 } else { 1.0 - d / m };
    (1.0 - sim <= threshold).then_some(sim)     // 默认 0.5（采集期宁松勿严）
}
```

### 5.4 落库

- `insert_sample`：同步、快，插文本行，`audio_status="pending"`。`NewSample` 增补 `source`(`"ui"|"copy"`)、
  `segment_ids`(json)；`segment_id`（既有 NOT NULL 单数列）= 首段 id。

### 5.5 音频：逐段拉取、异步回填（消化「音频到底有没有存」）

**事实**：音频不在本仓，只在编排器留约 1 天，按 segment id 取。采集通常发生在听写后不久 → 音频还在 → 能取。

```
fetch_audio_for_burst(sample_id, segment_ids):        // 独立异步任务，30s 超时不挡采集
  dir = speech_samples/{sample_id}/
  for seg in segment_ids:
     GET /api/segments/{seg}/audio → 存 dir/{seg}.wav   // 复用 fetch_and_store_audio 的取法
  audio_path = dir; audio_status = 全部到手?"saved" : 有过期?"partial" : "fetch_failed"
```

- **逐段存多个 wav**（`speech_samples/{sample_id}/{seg}.wav`），`audio_path` 指向该文件夹；不做拼接（要拼后面随时能拼）。
- 单段 burst 退化为文件夹里一个 wav，或沿用既有 `{sample_id}.wav` 扁平存法（实现时统一即可）。
- **过期风险**：把文本晾超过 1 天再采 → 对应段音频 404 → `audio_status="expired"`，文本样本照留。
- 想 100% 保音频需「听写时即落地每个 burst 的音频」——代价是没被采集的段也白存，**本期不做**。

### 5.6 设置

- 后端 `capture_enabled: bool` 并入 `CombinedSettings`（get/apply 持久化），默认 `true`；快捷键本期为常量。
- 前端语音设置面板加**一个复选框**「启用纠错一键采集」。

### 5.7 迁移 0006（每列独立守卫）

```rust
if !column_exists(conn, "speech_samples", "source")? {
    conn.execute_batch("ALTER TABLE speech_samples ADD COLUMN source TEXT NOT NULL DEFAULT 'ui';")?;
}
if !column_exists(conn, "speech_samples", "segment_ids")? {
    conn.execute_batch("ALTER TABLE speech_samples ADD COLUMN segment_ids TEXT;")?;
}
```

## 6. 护栏（精简）

| 事项 | 处理 |
|---|---|
| 破坏用户剪贴板 | **从不写剪贴板**，只读文本；非文本忽略。（唯一硬约束，天然满足） |
| 误采（普通复制/编辑中复制） | 只有**专用快捷键**触发才读剪贴板；再叠时间窗 + 相似度闸门 + `Y'==O` 跳过 |
| 一大段对应多次交付 | 交付按 **burst 累积**，带全部 `segment_ids`；配对拿整段比 |
| 音频是否可得 | 采集时逐段从编排器取（1 天内可得，验证过的路径）；过期则文本-only |
| 音频下载占用采集 | 先插样本、音频异步逐段回填 |
| 同段反复复制 | `captured` 去重 + 单飞 |

**刻意不做**：HWND 匹配、剪贴板写协调器、序列号稳定协议、观测每次 Ctrl+C、听写时预存音频。

## 7. 分阶段落地

- **P1（本期）**：burst 累积（`current_burst`/`recent_bursts`）+ 专用快捷键（复用钩子检测组合、吞键、不注入）
  + worker（读剪贴板 → 配 burst → 先插后拉音频）+ 合理性闸门 + 逐段音频异步 + 迁移 0006 + `capture_enabled` 开关
  + 轻量反馈。产物：`speech_samples` 里带 `R/O/Y'`+音频的原始样本，靠 `speech_export_samples` 导出查看。
- **P2**：看数据定分类（diff `O`/`Y'` → 热词层/优化层）+ 样本列表/复核 UI + 阈值/快捷键设置 UI + 音频按需拼接。
- **P3**：热词候选复核后同步 `asr.hotwords`；优化层语料注入优化 prompt few-shot。

## 8. 待定（都不阻塞）

- 音频**逐段存 vs 拼成整段**：倾向逐段存（省事、不丢信息）。
- 相似度阈值 0.5、时间窗、快捷键本期为常量，跑一阵看数据再调。
- P1 是否顺带做「只读样本列表」页（不做则靠导出 JSON）。
- 采集成功反馈：轻量 tray 抖动 vs 「已采集 +计数」toast。

## 9. 涉及文件

- 改：`speech/mod.rs`（`SpeechState` 加 `current_burst`/`recent_bursts`）、
  `commands/remote.rs`（`optimized`/`translated` 里按合并窗口累积/封存 burst）、
  `paste_watch.rs`（加专用组合键检测 + 吞键 + 发信号；不再需要为本功能观测 Ctrl+C）、
  `commands/samples.rs`（拆 `insert_sample`/`fetch_audio_for_burst`；`NewSample` 加 `source`/`segment_ids`）、
  `settings.rs` + `commands/settings.rs`（`CombinedSettings` 加 `capture_enabled`）、
  `db/schema.rs`（挂 0006）、前端设置面板加复选框。
- 新：`speech/capture.rs`（worker + 剪贴板读取 + `pairs_with_delivery` 纯函数）、`migrations/0006_sample_source.sql`。
