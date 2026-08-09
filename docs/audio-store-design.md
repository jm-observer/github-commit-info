# 音频统一仓库（audio-store）设计

> 状态：定稿 v1.0（§10 决策已锁，可据此进入 Phase A）
> 范围：toolkit-server 新增「音频 blob 仓库」能力；english 后端音频改为持引用；
> zero-desktop 录音替换改走 toolkit。
> 关联仓：`D:\git\toolkit`（能力侧）、`D:\git\english`（消费侧）、`zero-desktop`（录音侧）。

## 1. 背景与目标

当前音频字节在生态里被**复制了多份、归属分散**：

- AudioForge（toolkit）生成逐句 wav，落 `<workspace>/audioforge/<package_id>/NNN.wav`，
  经 `GET /api/web/audio/forge/{package_id}/{file}` 服务。
- english `package.import`（[import.rs](../../english/src/import.rs)）把这些 wav **逐句拉回**，
  复制进自己的 `<workspace>/audio/`，再建 `audio_files` 记录。
- 人声替换音频（原 Tauri 桌面端录制）经 english `/api/sentence/replace-audio` 上传，
  覆盖 english 本地 wav。

**问题**：同一份音频在 toolkit 和 english 各存一份；音频字节的"真相源"散在两个服务、两套存储里；
english 作为学习产品却背着一套文件存储/服务的基础设施。

**目标**：把**音频字节的存储 + 服务 + 写入**收拢成 toolkit 的一个**通用能力**（audio-store），
让 english 只持**引用**，消掉复制冗余。**注意边界**——只搬"音频字节这件基础设施"，
english 的"哪句话配哪条音频"的产品语义留在 english。

### 非目标

- 不动 english 的学习数据模型（customers / 进度 / packages / marks / 句子语义）。
- 不把 english 整体并进 toolkit（见前序结论：english 是对等产品，非工具）。
- 不引入鉴权体系改造（access control 作为未决问题单列，见 §10）。

## 2. 现状对照

### toolkit 侧（[audioforge/](../crates/toolkit-server/src/audioforge/)）

| 项 | 现状 |
|---|---|
| 存储 | `<workspace>/audioforge/<package_id>/NNN.wav` |
| 服务 | `GET /api/web/audio/forge/{package_id}/{file}`，全量读、**无 Range**，防穿越校验 |
| 时长 | 生成时解析 WAV 头（[wav.rs](../crates/toolkit-server/src/audioforge/wav.rs)） |
| 契约 | `manifest_version=1`，逐句 `audio_file`（`001.wav`）+ `duration` |

### english 侧

| 项 | 现状 |
|---|---|
| 存储 | `<workspace>/audio/*.wav`，相对路径存 `audio_files.file_path` |
| 语义键 | `audio_files`：`UNIQUE(data_id, data_type, audio_type)`；`data_type ∈ {word,sentence}`、`audio_type ∈ {male,female}` |
| 服务 | `GET /audio/{audio_id}`（按 `audio_files.id`，Range 流式） |
| 写入 | `POST /api/auth/audio/replace`（admin，按 `audio_id` 换字节）、`POST /api/sentence/replace-audio`（桌面端，校验音频属该句后换字节，保留原路径） |
| 导入 | `package.import` 从 AudioForge 拉 wav → 复制本地 → 建 `audio_files` |

`audio_files` 关键列：`id / data_id / data_type / audio_type / file_path / file_size / duration`。

## 3. 边界划分（核心）

按**语义归属**切，不按文件位置切：

| 东西 | 归属 | 说明 |
|---|---|---|
| 音频字节的存储 / 服务 / 写入（blob） | **toolkit audio-store**（新能力） | 无产品语义，可复用（english / douyin / speech 皆可存） |
| `(data_id, data_type, audio_type)` → 某条音频 的映射 | **留 english** | 学习产品语义，工具中台无家 |
| WAV 时长、字节数等 blob 自身元信息 | toolkit（入库时解析回传） | english 冗余存一份用于展示即可 |

落地形态：english `audio_files.file_path`（本地相对路径）→ 替换为 **toolkit blob 引用 `blob_id`**。
english 不再持有 wav 文件。

## 4. toolkit 侧设计

### 4.1 存储布局

```
<workspace>/audio-store/<id>.wav        # blob 字节，id = 内容寻址短哈希
```

与 `audioforge/`、`knowledge/` 等并列，符合 workspace 布局约定。

### 4.2 id 方案：内容寻址

`id = "aud_" + sm3_short(bytes)`（复用仓内既有 sm3 短哈希工具，参考 `prompt_hash`）。

- **天然去重**：相同字节同一 id，多处引用共享一份存储。
- **替换语义不受影响**：替换 = 存新 blob 得新 id + english 更新引用；旧 blob 若无人引用成孤儿（见 §10 生命周期）。

> 备选：随机 id（如 `new_task_id`）。放弃，因失去去重且无可见收益。

### 4.3 新表（`toolkit.db`）

按仓库约定加进整块 `DDL_V1`（[schema.rs](../crates/toolkit-core/src/schema.rs)），**纯加表、`IF NOT EXISTS` 幂等，
不 bump `SCHEMA_VERSION`**——与 `llm_config` / `shadow_attempt` 等已有加表一致：`migrate()`
每次启动 `execute_batch(DDL_V1)` 都会把缺的表建出；bump 反而不会更新已有 DB 的 `meta.schema_version`，故对纯加表
既无必要也不应做（见 schema.rs 第 88-90 行注释）：

```sql
CREATE TABLE IF NOT EXISTS audio_blob (
  id          TEXT PRIMARY KEY,          -- aud_<sm3short>
  bytes       INTEGER NOT NULL,          -- 字节数
  duration    REAL,                      -- 秒，解析 WAV 头；失败为 NULL
  content_type TEXT NOT NULL DEFAULT 'audio/wav',
  source      TEXT NOT NULL,             -- forge | manual | import | other
  created_at  TEXT NOT NULL              -- now_iso8601()
);
```

> 仅存 blob 自身元信息，**不存任何产品语义**（不知道它属于哪句话）。

### 4.4 路由契约（`/api/web/audio/store`，与 TTS/forge 同前缀）

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/api/web/audio/store?source=<src>` | body = wav 字节（`Content-Type: audio/wav`）。`source` 经 **query 参数**传入（白名单 `forge\|manual\|import\|other`，缺省 `other`）——blob 表无产品语义，来源只能由调用方显式声明。存盘 + 解析时长 + 入库，返回 `{id, bytes, duration}`。内容寻址：同字节重复 POST 幂等返回同 id（命中即不重复落盘、不改已有行 `source`）。 |
| `GET` | `/api/web/audio/store/{id}` | **Range 分块**（见下）。`id` 防穿越白名单校验（照抄 forge 的 `is_safe_segment`）。404 不泄露路径。 |

> **v1 不暴露 `DELETE`**。内容寻址下同一 blob 可能被多处（多句 / 多包）引用，而 store **不持产品引用关系**，
> 显式删除有破坏其它 english 记录的风险；又因 §10 决策 3「容忍孤儿」，删除并非必需。回收能力留到后续做
> 引用计数 / 离线 GC 时再设计，且仅限运维通道，不进 v1 公开面。

实现要点：

- **Range 分块（不引入新依赖）**：用 `std::fs::File` + `seek` 到 Range 起点、`read` **仅请求区间**的字节进
  buffer，返回 `206 Partial Content` + `Content-Range` / `Accept-Ranges: bytes`；无 Range 头则 `200` 全量。
  **内存只受单次 Range 区间大小约束**（客户端按需分块,非整文件读入），既满足 seek 播放、又不引入 `tokio` 的
  `fs`/`io-util` feature 或 `tokio-util`。与现有 forge 路由的 `std::fs` 同步读盘风格一致（forge 是全量，
  store 多一步区间切取）。这是「按区间读」而非连续异步流——wav 句级文件小 MB 量级，无需真流式；若未来出现
  超大音频再评估补依赖换 `ReaderStream`。
- **Range 边界语义（契约，english 反代须原样透传）**：
  - **仅支持单 range**。接受 `bytes=start-end` / `bytes=start-`（到末尾）/ `bytes=-suffix`（末 N 字节）。
  - **multi-range**（逗号分隔）**不支持** → 忽略 Range，按 `200` 全量返回（不报错，保守降级）。
  - **语法无法解析** 的 Range → 同样忽略，按 `200` 全量。
  - **不可满足**（`start >= size`）→ `416 Range Not Satisfiable` + `Content-Range: bytes */{size}`，空 body。
  - 响应头：`206` 必带 `Content-Range: bytes {start}-{end}/{size}` 且 `Content-Length` = 区间长度；
    `200` 必带 `Content-Length` = 全长；两者均带 `Accept-Ranges: bytes` 与 `Content-Type`（`audio/wav`）。
- **上传体积**：store POST 路由单独设 `DefaultBodyLimit::max(64 * 1024 * 1024)`（axum 默认仅 2 MiB，会挡住
  较长 / 高采样率录音）。逐句 wav 实际多在低 MB 量级，64 MiB 留足余量；真出现超大上传再转流式写入（后续优化）。
- 时长解析直接复用 [audioforge/wav.rs](../crates/toolkit-server/src/audioforge/wav.rs)。
- trace：`audio_store_put` / `audio_store_get` 两个 span（trace 未启用时 no-op）。

### 4.5 AudioForge 顺势接入（消掉 english 复制）

让 AudioForge 生成每句 wav 时**同时登记进 audio-store**，并在 manifest 增加逐句 `blob_id`：

- manifest 升 `manifest_version=2`：`ManifestSentence` 增 `blob_id: String`（v1 字段全保留，向后兼容）。
- english `package.import` 改为**直接记 `blob_id`，不再拉字节、不再复制本地**——`import.rs` 的
  `fetch_and_store_audio` 整段删除。

> 这一步是把"复制冗余"彻底消掉的关键。可作为独立阶段（见 §8 Phase C），
> 在仅做 §4.1–4.4 时 english 仍可先用"拉字节→POST store→记 blob_id"的过渡实现。

## 5. english 侧改动

### 5.1 数据模型：引用化

`audio_files` 增一列（增量迁移，english 用 `resource/migrate_*.sql` 套路）：

```sql
ALTER TABLE audio_files ADD COLUMN blob_id VARCHAR(64) NULL
  COMMENT 'toolkit audio-store blob 引用；非空时音频走 toolkit，file_path 作 legacy';
```

过渡期 `file_path`（本地）与 `blob_id`（toolkit）并存：`blob_id` 非空 → 走 toolkit；否则走 legacy 本地文件。
迁移完成后（§8 Phase E）可弃用 `file_path` 的本地语义。

### 5.2 服务：`GET /audio/{audio_id}` 改为反代

- `blob_id` 非空：english 反代 `GET {TOOLKIT_BASE_URL}/api/web/audio/store/{blob_id}`，
  **透传 Range / Content-Range / 状态码**。客户端（小程序 / 网页）**零改动**——URL 不变，仍打 english 同源。
- `blob_id` 为空：走现有本地文件逻辑（兼容存量未迁数据）。

> 选反代而非 302 重定向：保持同源、规避客户端 CORS / 鉴权细节、可平滑下线。代价是多一跳（GB10 内网，可接受）。
> 详见 §10 未决问题（直连 vs 反代）。

### 5.3 写入：新增"记引用"端点，旧字节上传端点过渡期保留

- **新增**：替换端点接受"已存入 store 的 `blob_id`"——english 只更新 `audio_files.blob_id` / `duration` /
  `file_size`，保留原有"音频须属该句"的校验语义（[admin.rs](../../english/src/admin.rs) `replace_sentence_audio`）。
  zero-desktop 走这条（先 POST store 得 blob_id，再调此端点，见 §6）。
- **保留(不硬切)**：现有字节上传端点 `POST /api/auth/audio/replace`、`POST /api/sentence/replace-audio`
  过渡期**继续可用**——内部改为"收字节 → POST `/api/web/audio/store` → 得 `blob_id` → 写引用"，
  对旧调用方（admin 后台、任何遗留客户端）行为不变，避免过渡期被断。Phase E 回归后再视情况下线或指向新端点。

### 5.4 导入：见 §4.5

`import.rs` 在 Phase C 后不再复制字节，仅按 manifest `blob_id` 写引用。

## 6. zero-desktop 侧（录音替换的新流向）

桌面端已由 zero-desktop 取代，且本就直连 toolkit-server。人声替换新链路：

```
zero-desktop 录音(wav)
  → POST /api/web/audio/store            (toolkit, 得 blob_id)
  → 调 english 替换 RPC(sentence_id, audio_type, blob_id)   (english 仅更新引用)
```

这条 **zero-desktop 新链路**里 english 不再经手音频字节，replace 退化为一次引用更新。
（区别于 §5.3 的 **legacy 兼容链路**：旧字节上传端点过渡期仍由 english 收字节并内部转存 store，
至 Phase E 才彻底不经手字节。）

## 7. 存量迁移

一次性脚本（english 侧，复用其 DB 连接；不单做 toolkit 子命令，见 §10 决策 5）：

1. 遍历 `audio_files` 中 `blob_id IS NULL` 的行；
2. 读 `file_path` 本地 wav → `POST /api/web/audio/store` → 得 `blob_id`；
3. `UPDATE audio_files SET blob_id=?, duration=COALESCE(duration, <store返回>) WHERE id=?`；
4. 内容寻址保证**幂等**（重跑同一文件得同 id，不重复落盘）；
5. 本地 wav **暂不删**，全量校验通过后（抽样比对 store 服务字节）再清，作为回滚保险（§9）。

体量：english 现网"几千句"，逐句一次 POST，分钟级可跑完。

## 8. 阶段拆分

| 阶段 | 内容 | 可独立验收 |
|---|---|---|
| **A** | toolkit audio-store 能力：`audio_blob` 表 + 两路由（POST 入库 / GET Range 分块）+ 时长解析 + trace | ✅ 单测 + curl 存取一条 wav |
| **B** | english 引用化：`blob_id` 列迁移 + `/audio/{id}` 反代 + **新增** blob_id 替换端点 + **legacy 字节上传端点（`/api/auth/audio/replace`、`/api/sentence/replace-audio`）内部桥接转存 store**（§5.3，过渡兼容不可漏） | ✅ 一条句子音频走 toolkit 播放成功 + 旧字节端点仍可用 |
| **C** | AudioForge 登记进 store + manifest v2 `blob_id`；`import.rs` 删复制逻辑 | ✅ 新导入包零本地 wav，引用直达 store |
| **D** | 存量迁移脚本：旧 `audio_files` 回填 `blob_id` | ✅ 全量迁移 + 抽样比对 |
| **E** | 清理：删 english 本地 `audio/`、弃用 `file_path` 本地语义 | ✅ 回归后下线 legacy 路径 |

A、B 即可跑通"新音频走 toolkit"的端到端；C 消复制；D、E 收尾。

## 9. 过渡与回滚

- 全程**双轨**：`blob_id` 非空走 toolkit，空走本地。任一阶段可停在双轨态。
- 回滚：english 侧只要不删本地 wav、不弃 `file_path`，把"`blob_id` 优先"开关关掉即回到现状。
- store 内容寻址 + 幂等，重复迁移无副作用。

## 10. 决策（已锁）

1. **音频访问控制**：audio-store **仅对内网开放**，不自带 token；由 english 反代（§5.2）把门，
   沿用 english 现有鉴权。对外暴露口径不变。
2. **manifest v2 时机**（§4.5）：**先过渡后消复制**。Phase A/B 用 english "拉字节 → POST store → 记 blob_id"
   的过渡实现快速跑通；manifest v2（AudioForge 直接写 `blob_id`）放 Phase C 一并落，届时删 english 复制逻辑。
3. **孤儿回收**：**v1 不自动删、不暴露 `DELETE`，容忍孤儿**（内容寻址下孤儿仅占盘、无正确性风险；
   且同一 blob 可能被多处引用，store 不持引用关系，公开删除会误伤）。引用计数 / 离线 GC 留作后续，
   届时仅限运维通道。
4. **直连 vs 反代**（§5.2）：**过渡期一律反代**，客户端（小程序/网页）零改动、保持同源；
   是否改直连 toolkit 留作后续优化，不进本期。
5. **存量迁移执行方**：**english 一次性脚本**（贴近 `audio_files` / 本地 `audio/` 现状，复用 english 的 DB 连接）；
   toolkit 侧只需保证 `POST /store` 幂等可重跑。不为此单做 toolkit 子命令。

---

## 附：关键文件索引

toolkit：
- [audioforge/routes.rs](../crates/toolkit-server/src/audioforge/routes.rs) — 现有 forge 下载路由（防穿越样板）
- [audioforge/wav.rs](../crates/toolkit-server/src/audioforge/wav.rs) — WAV 时长解析（复用）
- [audioforge/manifest.rs](../crates/toolkit-server/src/audioforge/manifest.rs) — manifest 契约（v2 加 blob_id）
- [toolkit-core/src/schema.rs](../crates/toolkit-core/src/schema.rs) — `DDL_V1` + `SCHEMA_VERSION`

english（`D:\git\english`）：
- `src/import.rs` — `package.import`（Phase C 删复制）
- `src/admin.rs` — `replace_audio_file` / `replace_sentence_audio`（改收 blob_id）
- `src/main.rs` — `/audio/{audio_id}` 路由（改反代）、replace 端点
- `resource/init_database.sql` — `audio_files` 定义（加 `blob_id` 迁移）
