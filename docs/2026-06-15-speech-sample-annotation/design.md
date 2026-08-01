# zero-desktop 语音识别 · segment 标注样本采集 设计

> 设计文档（**待实现**，2026-06-15）。在「语音识别」每条 segment 上增加一个「标注样本」入口：
> 按标签录入纠正内容（热词替换 / 优化不当的期望文本 / 无需过滤），连同**该段音频快照**一起
> 存为本地样本，供后续做识别/优化的回归测试。**无删除操作**（用户明确不要）。
> 关联调研：[[project_speech_homophone_arch]]（同音字优化架构 + 编排器 per-segment API）。

---

## 1. 背景与动机

中文优化（编排器 `llm.optimize_prompt`）偶有漏网：语气词「啊/哦/嗯」没滤净、同音字没纠对、
或优化改坏原意。用户希望在看到这类 case 时**当场标注成样本**，积累一批带「期望输出」的真实
case，日后用来：

- 回归测试优化 prompt / 热词表的改动（文本对照）；
- 对**音频本身**重测识别（例如识别错的段，换模型/调热词后重跑 ASR 对比）。

## 2. 关键约束（来自现状调研）

1. **segment 不落 zero-desktop 本地库**：`remote.rs` 只把 segment 经 Tauri 事件推给前端，
   编排器（GB10 `:8090`）才入库；本地 `asr_raw_records` 表只服务已弃用的本地 ASR 路径。
   → 样本需新建本地存储。
2. **音频只在编排器留 1 天**：`GET /api/segments/:id/audio` 返回 `audio/wav`，过期 404。
   → 「保留音频」必须在**标注当下**从编排器拉回本地存档，不能事后补。
3. **segment.id = 编排器 DB id**（合并模式下是 chain id）：前端 `Segment.id`/`segment_id` 即编排器
   段 id（`remote.rs` emit 时两者同值），可直接用于 `/api/segments/:id/audio`。
4. **编排器无鉴权**（仅 LAN `127.0.0.1`/内网），http base 由 ws `remote_url` 推导
   （复用 `remote_http_base_from_state`，已有 `speech_fetch_remote_history` 先例）。G10 token 是
   toolkit-server(:8788) 的，与编排器(:8090) 无关，拉音频不带 token。

## 3. 数据模型

### 标签（label）与按标签录入的内容（用户「先选标签、再按标签录入」需求）

| label | 含义 | 录入内容 `correction` | 用途 |
|---|---|---|---|
| `asr_wrong` | ASR 识别错误（原文识别错） | **音频真实文本**（ground-truth 整段转写，**预填当前 `text_raw` 供编辑改对**） | ASR 回归 / WER、换模型/调热词后对**音频**重测对比 |
| `speaker_wrong` | 声纹识别错误（说话人认错，文字可能完全正确） | 正确的说话人（**可留空**，表示只知道这段认错了）；被误判成的人记在 `speaker` 列（migration 0008） | 声纹回归 / 阈值与注册样本调整 |
| `hotword` | 热词/同音字识别错（局部术语） | 正确术语（建议「错词→正确词」，亦可只填正确词） | 补热词表 / 同音字回归；**可勾选同步进编排器 `asr.hotwords`**（见 §4.2） |
| `bad_optimize` | 中文优化不当（语气词没滤净 / 优化过度 / 改坏） | 期望的优化文本（**预填当前 `text_optimized` 供编辑删改**） | 优化 prompt 回归 |
| `ok` | 无需过滤（标为正常正样本） | 无 | 正样本对照 |
| `other` | 其它问题 | 自由文本（走 `note`） | 兜底 |

> `asr_wrong` 与 `hotword` 的区别：`asr_wrong` 录**整段真实文本**（适合对音频跑 ASR 算 WER）；
> `hotword` 只录**出错的术语词**（适合直接补热词表）。两者都强依赖存档音频。

> `speaker_wrong` 与 `asr_wrong` **正交**：前者是说话人归属错，后者是文字错，同一段可能只错一个。
> 声纹错在实际使用中出现频繁且用户此时无需输入任何内容，故在卡片顶部工具栏给了「声纹错」
> **一键快捷按钮**（直接落库 `label=speaker_wrong` + `speaker` 快照，`correction` 留空）；
> 要指明「应该是谁」再走标注面板选同一标签填写。

> 标签集是**建议值**，评审可调整/增删。前端按 label 决定是否显示 `correction` 输入框及其提示语。

### 本地新表 `speech_samples`（migration `0005_speech_samples.sql`）

```sql
CREATE TABLE IF NOT EXISTS speech_samples (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  segment_id     INTEGER NOT NULL,   -- 编排器段/链 id（拉音频用）
  session_id     TEXT,               -- 已知则记
  label          TEXT NOT NULL,      -- hotword | bad_optimize | ok | other
  text_raw       TEXT NOT NULL,      -- ASR 原文快照
  text_optimized TEXT,               -- 优化文快照（可能带语气词）
  text_english   TEXT,               -- 英文快照（可选）
  text_secondary TEXT,               -- 次模型快照（可选，有则一并存）
  correction     TEXT,               -- 按 label 录入的纠正内容
  note           TEXT,
  audio_path     TEXT,               -- 本地存档音频路径；空=未存
  audio_status   TEXT NOT NULL,      -- saved | expired | fetch_failed | skipped
  hotword_sync   TEXT,               -- 仅 hotword 标签：added | exists | failed | null
  marked_at      TEXT NOT NULL
);
```

迁移接入：`schema.rs::run_migrations` 末尾**无条件** `execute_batch(include_str!(".../0005_speech_samples.sql"))`
（`CREATE TABLE IF NOT EXISTS` 幂等，无需 column/table 探测）。

### 音频存档路径

`<workspace>/speech_samples/<sample_id>.wav`（`sample_id` = 插入后的 rowid，避免与 segment id 跨会话
冲突）。目录不存在则建。

## 4. 后端（Tauri commands，新文件 `commands/samples.rs`）

`SpeechDatabase`（`db/mod.rs`，已有 settings 范式）新增 `insert_sample` / `list_samples`
（`spawn_blocking` + `Arc<Mutex<Connection>>` 同范式）。命令：

### 4.1 `speech_mark_sample`（核心）

入参：segment 快照（`segment_id`/`session_id`/`text_raw`/`text_optimized`/`text_english`/
`text_secondary`）+ `label` + `correction?` + `note?` + `sync_hotword?`（仅 `hotword` 标签用）。
流程：

a. 先 `insert_sample`（audio_status 暂置 `skipped`）拿到 `sample_id`；
b. 由 `remote_url` 推 http base，`GET {base}/api/segments/{segment_id}/audio`：
   - 200 → 写 `<workspace>/speech_samples/<sample_id>.wav`，更新行 `audio_path` + status=`saved`；
   - 404 → status=`expired`（音频已过期，仅留文本）；
   - 其它/网络错 → status=`fetch_failed`（不报错整体失败，文本样本已存）；
c. 若 `label==hotword && sync_hotword`：执行 §4.2 热词同步，结果写行 `hotword_sync` 字段；
d. 返回最终 `Sample`（含 `audio_status` + `hotword_sync`，供前端提示
   「音频已存档 / 已过期」「已加入热词表 / 已存在 / 同步失败」）。
设计取舍：音频拉取与热词同步均**尽力而为**，失败不回滚文本样本——文本本身即有价值。

### 4.2 热词同步到编排器配置（`hotword` 标签专属）

把录入的**正确术语**追加进编排器 `asr.hotwords`，使其在声学层（Paraformer/Whisper）+ LLM 润色
兜底立即生效（ASR ~15s 热加载、LLM 下条新分段读取）。**先读后写，避免覆盖**（编排器
`asr.hotwords` 是整块 textarea，多端共享）：

1. 取词：`correction` 含 `→`/`->` 则取右侧、否则取整串，trim 为 `term`；
2. `GET {base}/api/config` → 读现有 `asr.hotwords`（换行分隔；按编排器 `parse_hotwords` 同规则
   忽略空行/`#` 注释、每行取首列词面）；
3. 若 `term` 已在词表 → `hotword_sync = "exists"`，不重复写；
4. 否则把 `term` 作为新行 append，`POST {base}/api/config` body `{"asr.hotwords": "<旧文\n+term>"}`
   → 成功 `"added"`，失败/不可达 `"failed"`。

> 同步是**逐条 append**，不删不改既有词；并发覆盖风险靠「读-改-写」窗口小化（非事务，
> 可接受——热词表低频人工编辑）。

### 4.3 其余命令

- **`speech_list_samples`** → `Vec<Sample>`（按 `marked_at` 倒序），供样本管理/计数。
- **`speech_export_samples`** → 把全部样本写 `<workspace>/speech_samples/export-<ts>.json`
  （数组，每条含字段 + 音频相对路径），返回该 json 路径；前端给「打开所在文件夹」按钮
  （复用已有 `speech_open_in_folder`）。测试脚本/回归集直接消费该 json + 同目录 wav。

> 命名沿用 `speech_` 前缀（与现有命令一致），在 `main.rs` 的 `invoke_handler` 注册 3 条。
> 数据表 §3 增列 `hotword_sync TEXT`（`added`|`exists`|`failed`|null）记录同步结果。

## 5. 前端

### 5.1 SegmentCard 操作入口

在卡片 hover 操作区（现有「复制中文/复制英文」一排，
[SegmentCard.tsx:111](../../crates/zero-desktop/ui/src/modules/speech/components/SegmentCard.tsx#L111)）
追加一个「标注」按钮（图标 `tag`/`bookmark`）。点击展开一个轻量行内面板（不弹模态）：

另在其左侧追加「声纹错」**快捷按钮**（图标 `user`）：一键落库声纹识别错误样本，不展开面板。

- **标签**下拉：识别错误 / 声纹识别错误 / 热词纠错 / 优化不当 / 正常无需过滤 / 其它；
- **内容**输入（随标签显隐）：
  - 识别错误 → textarea 预填 `text_raw`（ASR 原文），用户改成音频真实文本；
  - 声纹识别错误 → 单行输入「正确的说话人」（可留空），下方小字提示当前识别成谁；
  - 优化不当 → textarea 预填 `text_optimized`，用户删改成期望输出；
  - 热词纠错 → 单行输入「错词→正确词」或正确词；**下方勾选框「同步进热词表（asr.hotwords）」
    默认勾选**（控制是否触发 §4.2）；保存后提示「已加入热词表 / 已存在 / 同步失败」；
  - 正常 → 隐藏；其它 → note；
- **保存**：调 `speech_mark_sample`，成功后按钮变「已标注 ✓」并显示音频状态小字；
- 已标注的 segment 给个角标（本会话内端内记忆即可，MVP 不强求跨会话回显——样本已落库）。

### 5.2 样本入口/导出

语音识别页顶部或 ControlPanel 底部加一个轻量「标注样本（N）」chip + 「导出」按钮
（调 `speech_list_samples` 取计数、`speech_export_samples` 导出）。MVP 可只放「导出」。

### 5.3 API

speech 模块 `api/` 下新增 `SampleAPI`（或并入 `tauri-client.ts`）：`markSample` / `listSamples` /
`exportSamples` + `Sample`/`SampleLabel` 类型。

## 6. 不在本次范围

- 样本的删除/编辑管理界面（用户当前只要「采集 + 导出」；如需再迭代）。
- 跨会话「已标注」状态回显到 SegmentCard（样本已落库，回显非必需）。
- 把音频重测 / 文本回归**测试脚本**本身（本设计只产出样本数据，测试在外部按导出 json 跑）。
- 上传样本到 G10 / 编排器（用户选本地存储；跨仓建表成本高，略）。

## 7. 验收

**功能**
1. 任意 segment hover → 出现「标注」按钮；展开可选标签、按标签录入内容、保存。
2. 标注「优化不当」：内容框预填优化文，编辑后保存；样本入 `speech_samples`，
   `text_optimized`=原优化文、`correction`=期望文。
3. 标注当下若音频未过期：`<workspace>/speech_samples/<id>.wav` 落盘、status=`saved`；
   已过期：status=`expired`、不落 wav、流程不报错。
4. 标注「热词纠错」且勾选同步：编排器 `GET /api/config` 的 `asr.hotwords` 出现该词
   （已存在则不重复）；`hotword_sync` 记 `added`/`exists`；编排器不可达时 `failed` 且样本仍存。
5. 「导出」生成 `export-<ts>.json`，含全部样本 + 音频相对路径，可打开文件夹。

**前端门槛**
5. `npm run build`（`tsc --noEmit && vite build`）通过。

**仓库级门槛**（CLAUDE.md；本次含 Rust 改动，须全绿）
6. `cargo fmt --check` 通过。
7. `cargo clippy -p zero-desktop -- -D warnings` 通过（无 Tauri 工具链则
   `--workspace --exclude toolkit-desktop`）。
8. `cargo test -p zero-desktop` 通过（含新表迁移 + 路径派生的单测）。
