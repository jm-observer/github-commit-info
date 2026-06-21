# orchestrator 独立服务退役 plan（`:8090` → 嵌入 toolkit-server `:8788/api/asr`）

> 状态：**待核验**（2026-06-20 起草）。本文档列出当前已确认事实、待核验事项、迁移步骤与回退方案。
> 动手前请按「核验清单」逐项确认，必要时把结论回填进本文。

---

## 1. 背景与现状

按 [CLAUDE.md](../CLAUDE.md) 的设计，orchestrator（StreamSpeech 语音编排层）已并入
`toolkit-server` 同进程：lib 形态暴露 `router(ctx)` / `init_ctx()`，由 toolkit-server 在
[`crates/toolkit-server/src/lib.rs:87-91`](../crates/toolkit-server/src/lib.rs) 处
`.nest("/api/asr", orchestrator::router(orch_ctx))` 挂载。zero-desktop 新版 shared
settings（`crates/zero-desktop/src/shared/settings.rs`）的 `auto` 派生路径会指向
`:8788/api/asr/stream`。

**但**：用户反馈 `http://192.168.0.68:8090/` 当前仍可访问。也就是说，G10 上还有一个旧的
独立 orchestrator systemd unit 在跑，端口能通。它持有的 `app.db`（声纹/历史/配置/分段音频）
与 toolkit-server 那份是分开的。是否仍有客户端在使用 `:8090`，由 V8（活动连接）和仓内
旧入口（见下方注意框）的清理共同决定，不能假设「没人在用」。

> ⚠️ 仓内已发现至少 4 处仍指向 `:8090` 的入口，与「客户端都已切走」相矛盾，必须在 §5 一并清理：
>
> 1. [`crates/zero-desktop/src/modules/speech/settings.rs:26`](../crates/zero-desktop/src/modules/speech/settings.rs)
>    `DEFAULT_REMOTE_URL = "ws://192.168.0.68:8090/stream"`
> 2. [`crates/zero-desktop/ui/src/modules/speech/api/tauri-client.ts:51`](../crates/zero-desktop/ui/src/modules/speech/api/tauri-client.ts)
>    `DEFAULT_REMOTE_URL = 'ws://192.168.0.68:8090/stream'`
> 3. [`crates/toolkit-server/web/hub.js:13`](../crates/toolkit-server/web/hub.js)
>    `{ id: 'orchestrator', name: 'Orchestrator', port: 8090, … }`
> 4. [`crates/zero-desktop/src/modules/g10_deploy/registry.rs`](../crates/zero-desktop/src/modules/g10_deploy/registry.rs)
>    `orchestrator` 条目（约 215-250 行）
>
> 这些入口不清掉，`:8090` 就有「真实使用方」（用户在 speech 设置面板留着旧 URL、点 hub 旧卡片、
> 在部署面板按一键部署去拉旧 unit），端口退役会引入回归。

本 plan 要解决两件事：

1. 把 `:8090` 那个进程**安全停掉**，让端口真正退役。
2. 把它积累的 `app.db` 数据（如果有）**迁移**到 toolkit-server 的 workspace，避免功能
   语义降级（控制台历史/声纹/配置丢失）。

---

## 2. 已确认事实

| 项 | 证据 |
|---|---|
| orchestrator 已 nest 进 toolkit-server | [`crates/toolkit-server/src/lib.rs:87-91`](../crates/toolkit-server/src/lib.rs) |
| 两边跑的是同一份 lib 代码（功能等价） | [`crates/orchestrator/src/lib.rs:1-3`](../crates/orchestrator/src/lib.rs)：「lib + bin 双形态」 |
| 两边 `app.db` schema 完全一致 | 同一份 [`crates/orchestrator/src/db.rs`](../crates/orchestrator/src/db.rs) 的 `CREATE TABLE IF NOT EXISTS` |
| `deploy-g10.ps1` 已不部署 orchestrator | `deploy-g10.ps1` 第 26-28 / 80 / 172 行注释 |
| zero-desktop 的 shared settings `auto` 派生路径指向 `:8788/api/asr/stream` | `crates/zero-desktop/src/shared/settings.rs` |

> 上一版把「zero-desktop 客户端走 `:8788/api/asr/stream`」整体列为已确认事实——这是错的。
> 旧的 speech 模块默认 URL（`settings.rs:26` / `tauri-client.ts:51`）当前仍是
> `ws://192.168.0.68:8090/stream`。只有「shared settings 的 `auto` 派生路径」是新值，
> speech 模块的 `remote_url` 默认值仍待清理（见 §5）。
>
> 同样地，「外网反代也已切换到 `:8788/api/asr/stream`」**仓内没有配置证据**（nginx/Caddy
> 配置不在本仓），此项已降级为 V9 待核验。

---

## 3. 待核验事项（动手前必须逐项确认）

> 用下表的命令收集证据，把答案回填到「结论」列。任何一项不确定都不要进入第 4 节。

| # | 待核验 | 命令 | 结论 |
|---|---|---|---|
| V1 | `:8090` 是否仍在监听 | `curl -m 3 http://192.168.0.68:8090/health` | ✅ 仍在。返回 `{"status":"ok","version":"0.2.1"}` |
| V2 | 是哪个进程在监听 `:8090` | `ssh g10 'ss -lntp \| grep :8090'` | ✅ `orchestrator` pid=1799537（非 toolkit-server） |
| V3 | systemd 是否有 orchestrator unit | `ssh fengqi@g10 'export XDG_RUNTIME_DIR=/run/user/$(id -u); systemctl --user status orchestrator'` | ⚠️ **unit 已 `disabled` 但仍 `active (running)`**——历史上有人 disable 没 stop。启动命令 `orchestrator serve --workspace /home/fengqi/.config/orchestrator` |
| V4 | 老 `app.db` 物理路径 | 看 unit 文件里的 `--workspace` 或 `WorkingDirectory`，或 `lsof -p <pid> \| grep app.db` | `/home/fengqi/.config/orchestrator/app.db`（从 V3 的 `--workspace` 直接得到） |
| V5 | toolkit-server 当前 workspace | 看 toolkit-server unit 的 `--workspace` 参数 | `/home/fengqi/.config/toolkit-server/app.db` |
| V6 | 老库（`:8090`）里有多少数据 | `sqlite3 <old_app.db> 'SELECT (SELECT COUNT(*) FROM speakers), (SELECT COUNT(*) FROM segments), (SELECT COUNT(*) FROM sessions), (SELECT COUNT(*) FROM config), (SELECT COUNT(*) FROM segment_audio);'` | speakers=1, segments=3468, sessions=177, config=10, segment_audio=**0**。最后写入 2026-06-19 01:49 |
| V7 | 新库（toolkit-server workspace）是否真是空库 | 同 V6，目标是 `<toolkit-workspace>/app.db` | **不是空库**。speakers=1, segments=**3635**, sessions=**206**, config=10, segment_audio=**148**。最后写入 2026-06-20 16:59 |
| V8 | `:8090` 旧服务是否还有客户端在用 | `ssh g10 'ss -tnp \| grep :8090'` 看是否有 ESTABLISHED 连接 | 无活跃连接；最新写入停在 18 天前（V6 时间戳印证）。`:8090` 实质已是僵尸进程 |
| V9 | 外网反代（nginx/Caddy 等）是否已指向 `:8788/api/asr/stream` | 在 G10 看反代配置文件，或 `curl -sI https://<外网域名>/api/asr/health` 看路由 | 未单独核验；由 V7 新库 18 天持续增长可间接判断主流量已在 `:8788` |

**关键分支**（按 V6 老库 / V7 新库的数据状态四象限）：

| V6 老库 | V7 新库 | 走哪条路径 |
|---|---|---|
| 空 | 空 | [§4.1 轻量路径](#41-轻量路径老库为空时)：直接 disable 旧服务 |
| 有数据 | 空 | [§4.2 完整路径](#42-完整路径老库有数据且新库为空时)：cp 覆盖 |
| 空 | 有数据 | [§4.1 轻量路径](#41-轻量路径老库为空时)：保留新库即可，跳过迁库 |
| 有数据 | 有数据 | [§4.3 合并路径](#43-合并路径两边均有业务数据时-需人工决策)：人工决策合并 SQL |

**本次实际落点（2026-06-20 核验 + 决策）**：`有数据 | 有数据` 象限 → §4.3。
进一步在 §4.3 内决策为「**放弃老库**」分支（不做 SQL 合并）。判据：

- 新库 segments(3635) > 老库(3468)、新库 sessions(206) > 老库(177)、新库 segment_audio(148) vs 老库 0、
  新库 config 含老库没有的 `main` 热词、新库最后写入 2026-06-20 vs 老库 2026-06-19。
- 也就是说 `:8090` 已经是 18 天没人喂的僵尸进程，新库才是权威源；
  合并 segments 涉及 19 天前重叠时间窗去重 + 主键重映射，工程成本远超价值。
- 声纹库两边各 1 条，假设是同一人（最坏让用户重录一条，可忽略）。

其他分支：

- 如果 V8 显示**还有客户端在用 `:8090`** → 停机前需先排查是谁还在连，可能是某个旧版桌面端
  或脚本写死了地址。本次 V8 无活跃连接，跳过。
- 如果 V9 显示外网反代仍指向 `:8090` → 必须先改反代后再退役端口，否则外网识别会断。
  本次 V9 未单独核验，但 V7 新库 18 天持续增长 → 间接证据「主流量已在 `:8788`」。

---

## 4. 迁移步骤

> **重要**：以下命令都用「目标用户身份」执行 `systemctl --user`，**不要加 `sudo`**。
> `sudo systemctl --user …` 会切到 root 的 user manager，停/启的根本不是 fengqi 的 unit。
>
> 即使以 fengqi 身份 ssh 进 G10，**非交互 / 远端调用场景**仍可能拿不到 user manager 的
> `XDG_RUNTIME_DIR`（本仓 `deploy-g10.ps1` 在重启服务时也显式 export 了该变量）。所以**所有
> 远程 `systemctl --user` 命令都显式带上**：
>
> ```bash
> export XDG_RUNTIME_DIR=/run/user/$(id -u)
> systemctl --user …
> ```
>
> 若需 root 登录后切到 fengqi：
> `sudo -u fengqi XDG_RUNTIME_DIR=/run/user/$(id -u fengqi) systemctl --user …`。

### 4.1 轻量路径（老库为空时）

**前置**：先完成 [§5.1 端口退役前必须发布的客户端入口清理](#51-端口退役前必须发布的客户端入口清理)，
且新版 zero-desktop 已发布到使用者机器（speech 设置迁移逻辑必须先生效）。否则停掉 `:8090` 后，
仍持有旧 `remote_url` 的桌面端会连不上 ASR。

```bash
ssh fengqi@g10 'export XDG_RUNTIME_DIR=/run/user/$(id -u); systemctl --user disable --now orchestrator'
```

验证：`curl http://192.168.0.68:8090/` 应该 connect refused。然后进
[§5.2 端口退役后的文档 / 可选清理](#52-端口退役后的文档--可选清理) 收尾。

### 4.2 完整路径（老库有数据且新库为空时）

> 前提：V6 老库有业务数据，**V7 新库基本为空**。两者 schema 一致，cp 直接覆盖即可。
> 若 V7 显示新库也有业务数据，**不要走本路径**，转 §4.3 合并路径。
>
> ⚠️ **顺序敏感**：与 §4.1 不同，本路径不能「先发布 §5.1 让客户端切到 `:8788`，再 cp」。
> 客户端一旦切到 `:8788` 就会向新库写入 segments / segment_audio / sessions；后续 `cp "$OLD" "$NEW"`
> 会**整库覆盖**，把这些新写入的数据擦掉，V7 「新库为空」的前提也就过期了。
>
> 正确做法：**冻结写入 → 迁库 → 起新库 → 验证 → 再发布客户端 → 确认流量切走 → 退役 `:8090`**。
> [§5.1](#51-端口退役前必须发布的客户端入口清理) 的代码改动可以提前合入和构建（产物先囤着），
> 但「发布到使用者机器」这一步必须放进维护窗口里 Step 7 的位置。

进入维护窗口后按时序：

```bash
ssh fengqi@g10
export XDG_RUNTIME_DIR=/run/user/$(id -u)

# Step 1：冻结两边写入——同时停 :8090 和 toolkit-server，确保 cp 期间没有 sqlite 进程持有连接
systemctl --user stop orchestrator
systemctl --user stop toolkit-server

# Step 2：定位两份 app.db（按 V4 / V5 的结论替换路径）
OLD_DB=/path/from/V4/app.db
NEW_DB=/path/from/V5/app.db

# Step 2.5（强制）：服务停掉后，重新核验 V6 / V7 计数。
# §3 的 V7 是核验时刻的快照,核验到 Step 1 之间 toolkit-server 仍在运行,可能已写入
# 新 segments / sessions / segment_audio——继续 cp 会整库覆盖这些新写入,丢数据。
sqlite3 "$OLD_DB" 'SELECT "OLD",
  (SELECT COUNT(*) FROM speakers),
  (SELECT COUNT(*) FROM segments),
  (SELECT COUNT(*) FROM sessions),
  (SELECT COUNT(*) FROM config),
  (SELECT COUNT(*) FROM segment_audio);'
sqlite3 "$NEW_DB" 'SELECT "NEW",
  (SELECT COUNT(*) FROM speakers),
  (SELECT COUNT(*) FROM segments),
  (SELECT COUNT(*) FROM sessions),
  (SELECT COUNT(*) FROM config),
  (SELECT COUNT(*) FROM segment_audio);'
# 判定：新库（NEW 行）必须仍是 §3 V7 时的「基本为空」状态。
#   - 仍为空 → 继续 Step 3。
#   - 已有任何业务行（segments / sessions / segment_audio 非零，或 config 出现非默认 key）
#     → **立即停止本节**：先 systemctl --user start toolkit-server 把服务恢复（保住已有新
#       写入），再转 §4.3 合并路径走人工决策。**禁止继续执行下面的 cp**。

# Step 3：备份 toolkit-server 现有 app.db（即便重新核验仍为空也备份，回滚兜底）
cp "$NEW_DB" "${NEW_DB}.bak.$(date +%Y%m%d-%H%M%S)"

# Step 4：拷贝
cp "$OLD_DB" "$NEW_DB"
# 权限对齐（toolkit-server 跑哪个用户就归谁；通常和备份的 owner 一致）
ls -l "${NEW_DB}.bak."*  # 参照备份的 owner

# Step 5：先起 toolkit-server，:8788/api/asr 提供服务；此时 :8090 仍是停的（不再启动）
systemctl --user start toolkit-server

# Step 6：在 http://192.168.0.68:8788/api/asr 控制台验证迁移数据齐全：
#   - 说话人列表（speakers）
#   - 历史分段（segments）+ 音频回放（segment_audio）
#   - 配置 tab 的热词 / prompt（config）
```

**Step 6 通过后**，把 §5.1 的新版 zero-desktop **发布**给使用者并让他们重启桌面端（首次启动
触发 DB 旧 URL 迁移 + 切到 `:8788/api/asr/stream`）。在使用者机器上：

- 确认 speech 设置面板里 `remote_url` 已经是新地址（不再是 `:8090`）。
- 发起一次实测识别，从 toolkit-server 日志确认请求落在 `:8788/api/asr/stream`。

**确认客户端流量已切走 `:8788` 后**才执行：

```bash
# Step 7：彻底退役 :8090 独立服务
systemctl --user disable orchestrator
# 可选：rm ~/.config/systemd/user/orchestrator.service（保留也无害，只是不会再起来）

# Step 8：确认端口已死
ss -lntp | grep :8090   # 应该无输出
```

退役完成后进 [§5.2](#52-端口退役后的文档--可选清理) 收尾。

### 4.3 合并路径（两边均有业务数据时；需人工决策）

> 前提：V6 与 V7 **都**有 segments / segment_audio / speakers 等业务数据。
> schema 一致**不**等于可以 cp 覆盖：
>
> - `segments.id` / `speakers.id` 是 `INTEGER PRIMARY KEY AUTOINCREMENT`，两边各自从 1
>   开始递增，**主键范围必然重叠**——cp 会让新库已有数据丢失。
> - `segment_audio.segment_id INTEGER PRIMARY KEY`（**非 AUTOINCREMENT，与 `segments.id`
>   绑定**）。跨库合并时必须随 segment id 一起重映射，否则音频会指向错误的 segment；
>   做不到精确重映射就只能放弃其中一侧的音频。
> - `session_id` 在两边各自生成，跨库可能撞值。
> - `config` 表是 key→value 单行覆盖语义，新库改过的配置 vs 老库改过的配置需要逐 key
>   决策保留哪边。
>
> 因此本路径**不**在本 plan 里给死命令。请把 V6 / V7 的实际计数（每张表分别多少行）发给我，
> 我根据「谁是权威源」「哪张表两边都有用户写入」补一段 `INSERT … SELECT … WHERE NOT EXISTS`
> 的合并 SQL；也可能决策结果是「以新库为准、放弃老库数据」（如果老库只是历史残留，例如最近
> 一次实际使用是几个月前）。

合并 / 决策完成后，**同 §4.2 一样要走「冻结写入 → 合并 → 起新库验证 → 再发布客户端 → 确认
流量切走 → 退役 `:8090`」的顺序**：合并 SQL 必须在两边服务都 stop 的维护窗口里执行（避免
toolkit-server 一边接受新写入一边被并发合并的竞态），客户端发布同样放在合并验证通过之后，
最后再按 §4.2 的 Step 7 / Step 8 disable 旧 unit。

#### 4.3.A 本次决策：放弃老库（不做 SQL 合并）

> 2026-06-20 已决策。判据见 §3 「本次实际落点」段。

由于没有合并写入，不需要维护窗口，**不必停 toolkit-server**（新库一直保持可用）。
仅需安全停掉 `:8090` + 归档老库 + 完成 §5.1 客户端入口清理。

`:8090` 的特殊状态简化了 disable 步骤——V3 显示 unit 已经是 `disabled` 但 `active (running)`，
所以**只需 `stop`，不需要再 `disable`**：

```bash
ssh fengqi@g10
export XDG_RUNTIME_DIR=/run/user/$(id -u)

# Step 1：停掉残留的 :8090 进程
systemctl --user stop orchestrator

# Step 2：确认端口已死
ss -lntp | grep :8090   # 应该无输出
curl -m 3 http://192.168.0.68:8090/health   # 应该 connect refused

# Step 3：归档老库（不 rm，留个保险）
cd /home/fengqi/.config/orchestrator
mv app.db app.db.archived-2026-06-20
# 同目录其他文件（如 WAL / SHM）一并归档
[ -f app.db-wal ] && mv app.db-wal app.db-wal.archived-2026-06-20
[ -f app.db-shm ] && mv app.db-shm app.db-shm.archived-2026-06-20

# Step 4：移除 unit 文件（disable 状态已生效，但删文件可保证彻底）
[ -f ~/.config/systemd/user/orchestrator.service ] && rm ~/.config/systemd/user/orchestrator.service
systemctl --user daemon-reload
```

之后照常走 [§5.1](#51-端口退役前必须发布的客户端入口清理) 客户端入口清理 → [§5.2](#52-端口退役后的文档--可选清理) 文档收尾。

注：本分支不存在「先发布客户端导致新库被覆盖」的风险（因为不动新库），所以 §5.1 的发布
**可以照走轻量路径节奏**（立即合入、构建、部署、发布），不必排进维护窗口。

---

## 5. 仓内代码 / 文档清理

清理拆成两批，**顺序不可颠倒**：

- **§5.1** 是退役 `:8090` 的**安全前置**：必须先合入、构建、发布到目标使用者机器，否则停掉
  `:8090` 后旧入口仍指向死端口（桌面端连不上、hub 点击 404、部署面板甚至会一键重启那个已
  被退役的 unit）。**例外**：走 §4.2 / §4.3 这种有数据迁移的路径时，「发布给使用者」这一步
  要推迟到 cp/合并完成、toolkit-server 起来验证之后做（详见 §4.2 的顺序说明），避免迁库时
  被新写入污染。
- **§5.2** 是退役 `:8090` 之后的文档收尾与可选 bin 退役，对运行链路无影响。

### 5.1 端口退役前必须发布的客户端入口清理

| 文件 | 修改 |
|---|---|
| `crates/zero-desktop/src/modules/speech/settings.rs:26` | `DEFAULT_REMOTE_URL` 从 `ws://192.168.0.68:8090/stream` 改为新址（按 V9 结论：局域网 `ws://192.168.0.68:8788/api/asr/stream` 或外网反代）。同步检查 `remote_url_presets` 默认值。 |
| `crates/zero-desktop/ui/src/modules/speech/api/tauri-client.ts:51` | 同上，`DEFAULT_REMOTE_URL` 改为新址，与 Rust 侧保持一致。 |
| `crates/zero-desktop/ui/src/modules/speech/components/ControlPanel.tsx:249` | UI 文案「GB10 管理台 (http://&lt;server&gt;:8090/) 调整」改为指向 `:8788/api/asr` 控制台。否则用户照文案打开旧地址就会撞 connect refused。 |
| `crates/zero-desktop/src/modules/speech/settings.rs::load_remote_settings_from_db` | **必须**：补一段 settings 迁移逻辑——当 DB 里 `remote.url` 仍是旧默认 `ws://192.168.0.68:8090/stream` 时，自动改写为新 `DEFAULT_REMOTE_URL` 并回写 DB（同步过滤 `remote.url_presets` 里残留的旧值）。原因：当前 `load_remote_settings_from_db`（约 267-290 行）只在 DB 缺失时才用默认值；DB 已持久化旧 URL 的用户**不会自动获得新默认**，光改常量等于没改。 |
| `crates/zero-desktop/src/modules/g10_deploy/registry.rs` | 删除 `builtin()` 里 orchestrator 那一条（约 215-250 行）。部署面板上的「一键部署 orchestrator」会去重新构建 + scp + restart 一个已退役的 unit，必须在停端口前清掉。 |
| `crates/toolkit-server/web/hub.js:13` | 删除 `{ id: 'orchestrator', name: 'Orchestrator', port: 8090, … }` 这条入口卡片。语音控制台改走 toolkit-server 主面板的「语音」tab（见下一行），hub 本身不需要再单列入口。 |
| `crates/toolkit-server/web/{index.html,app.js}` | **主面板加「语音」tab**：`<nav id="tabs">` 加 `<button data-tab="asr">语音</button>`；`<main>` 末尾加 `<section id="tab-asr"><iframe id="asr-frame" ...></iframe></section>`；`app.js` 在 tab 切换逻辑里给 `asr` 加懒加载（首次切换才设 `iframe.src='/api/asr/'`，避免每开主面板就建 WS）。后端 `/api/asr/*` 路由不动（API + console HTML 仍由 orchestrator lib 提供，主面板用 iframe 嵌入）。 |

**发布动作清单**（不止「重启」——`hub.js` 是 `include_str!` 进 toolkit-server 二进制的静态资源，
见 [`crates/toolkit-server/src/static_assets.rs:13`](../crates/toolkit-server/src/static_assets.rs)，
**只重启进程不会生效**；同时 G10 上若存在 `<workspace>/web/` 目录，`ServeDir` 优先级高于嵌入
版本，旧的 `web/hub.js` 会继续覆盖嵌入新版）：

1. **zero-desktop 改动（speech 默认 URL + DB 迁移 + g10_deploy registry）** → 构建新版 zero-desktop
   安装包，分发给使用者并让其重启桌面端。
2. **toolkit-server 改动（hub.js）** → 必须重新构建 toolkit-server 二进制并部署到 G10
   （`pwsh ./deploy-g10.ps1 -Service toolkit-server`），单独重启不行。
3. **检查 G10 上 `<workspace>/web/` 是否存在**：
   - 存在 → 同步该目录里的 `hub.js`（删旧 `:8090` 卡片那行），或者**直接删整个 `web/` 目录**
     让 toolkit-server 回退到嵌入版本（推荐：与 deploy 一致，没有「线上 web/ 漂移于源码」的隐患）。
     之后 `systemctl --user restart toolkit-server`。
   - 不存在 → toolkit-server 自动用嵌入版本，无需额外操作；上面 Step 2 的 deploy 已经把新二进
     制（含新 hub.js）放上去了。
4. **验证**：`curl -s http://192.168.0.68:8788/hub.js | grep -c "port: 8090"` 应为 0。

**发布时机分两种情况**：

- 走 [§4.1 轻量路径](#41-轻量路径老库为空时)：上面 4 步**立即执行**完，在使用者机器上确认
  `remote_url` 已是新地址 → 才执行 §4.1 的 disable。
- 走 [§4.2 完整路径](#42-完整路径老库有数据且新库为空时) 或 [§4.3 合并路径](#43-合并路径两边均有业务数据时-需人工决策)：
  上面 Step 1（zero-desktop 构建）和 Step 2 / Step 3 的「构建」「同步/删 web/」**代码改动可以
  提前合入和构建产物**，但**「分发给使用者 + 在 G10 启动新版 toolkit-server」必须推迟到维护
  窗口内 cp/合并完成、toolkit-server 起来并验证之后**。否则客户端先切到新库写入、再被 cp 整
  库覆盖会丢数据。具体节奏照 §4.2 的 Step 1–8 走（其中 Step 5「起 toolkit-server」就要起改过
  hub.js 的新二进制）。

### 5.2 端口退役后的文档 / 可选清理

| 文件 | 修改 |
|---|---|
| `CLAUDE.md` | 检查并修正所有提到「orchestrator 是宿主 systemd 服务」「`:8090`」「`-Service orchestrator` 部署」的措辞，统一为「已并入 `:8788/api/asr`」 |
| `crates/orchestrator/Cargo.toml` | 评估是否删除 `[[bin]]` 段——独立 bin 还要不要保留作本地调试用途？（建议保留作调试用，物理删除收益小） |
| `crates/orchestrator/src/main.rs` | 若决定退役 bin，则删除此文件 |

---

## 6. 回退方案

整个过程的可逆点：

- **迁库前**：什么都没动，直接放弃即可。
- **迁库后但 toolkit-server 异常**：停 toolkit-server，把备份 `app.db.bak.*` 拷回去，再起。
- **`:8090` 退役后发现还有客户端在用**：
  `ssh fengqi@g10 'export XDG_RUNTIME_DIR=/run/user/$(id -u); systemctl --user enable --now orchestrator'`
  暂时拉回来（但此时它的 `app.db` 已经是停机时刻的快照，可能与 toolkit-server 数据
  不一致——所以这是「应急」不是「常态」，要尽快把客户端切换过去）。

---

## 7. 风险与开放问题

- **V4 路径找不到**：如果 `:8090` 那个进程的 unit 文件不在标准位置，需要从 `/proc/<pid>/cmdline`
  和 `/proc/<pid>/cwd` 反推 workspace。
- **bin 是否保留**：CLAUDE.md 说 bin 仅作独立调试用途。本 plan 默认保留；如果你倾向「物理
  退役 = 删 bin」，需要追加一节评估对调试链路的影响。
- **`:8090` 是不是只是 toolkit-server 自己开的另一个端口**：极小概率，但需要 V2 的
  `ss -lntp` 输出确认到底是 `orchestrator` 还是 `toolkit-server` 在监听——如果是后者，
  那说明 toolkit-server 有意外的额外监听，本 plan 不适用。

---

## 8. 决策记录

| 日期 | 决策 | 谁 | 备注 |
|---|---|---|---|
| 2026-06-20 | 起草本 plan | claude + fengqi | 待第 3 节核验完成后再决定走轻量 / 完整路径 |
| 2026-06-20 | 按 Codex 复核意见修订 | claude | sudo systemctl 修正、补 segment_audio、合并路径独立成 §4.3、外网反代降级为 V9、补 hub.js 清理项 |
| 2026-06-20 | 按 Codex 第二轮意见再修订 | claude | 删除「客户端都已切走」与「没人在用 :8090」两条无证据断言；speech 模块旧 `:8090` 默认 URL 明确进 §5 清理；分支表补「老库空+新库有数据」象限；§4.3 `segment_audio` schema 描述按真实结构修正；§5 hub.js 修改说明加上「只删卡片不增 path 入口」 |
| 2026-06-20 | 按 Codex 第三轮意见再修订 | claude | 修正 §3 四象限表格列数错位；删 §1 重复的 hub.js 注意框；「至少 3 处」→「至少 4 处」；V3 / §4.1 / §4.2 / §6 所有远程 `systemctl --user` 显式 export `XDG_RUNTIME_DIR` |
| 2026-06-20 | 按 Codex 第四轮意见再修订 | claude | §5 拆成 §5.1（端口退役前必须发布的客户端入口清理，含 speech DB 旧 URL 迁移）+ §5.2（退役后文档收尾）；§4.1 / §4.2 / §4.3 入口处显式加「先做 §5.1 再 disable」前置 |
| 2026-06-20 | 按 Codex 第五轮意见再修订 | claude | §4.2 重排为「冻结写入 → 迁库 → 起新库验证 → 再发布客户端 → 确认流量切走 → 退役」八步顺序，消除「先发布客户端导致新库被 cp 覆盖」的数据丢失窗口；§4.3 同步约束；§5.1 发布时机按 §4.1 / §4.2 区分 |
| 2026-06-20 | 按 Codex 第六轮意见再修订 | claude | §4.2 增 Step 2.5：停服务后强制重新核验 V6/V7 计数；若新库已不为空则立即退出 §4.2 转 §4.3，禁止继续 cp，消除「V7 核验 → 维护窗口期间新库被新写入」的窗口 |
| 2026-06-20 | 按 Codex 第七轮意见再修订 | claude | §5.1 发布说明展开为 4 步动作清单：明确 hub.js 是 `include_str!` 进二进制（必须重新构建并 `deploy-g10.ps1 -Service toolkit-server`，单纯重启无效）；说明 G10 `<workspace>/web/` 若存在会经 `ServeDir` 覆盖嵌入版本（同步或删之）；末尾给 `curl + grep` 验证命令 |
| 2026-06-20 | 回填核验结果 + 决策放弃老库 | fengqi | §3 V1–V9 回填具体值（含 V3「disabled 但 active」、V6/V7 计数）；分支判定为「有 ⨯ 有」→ §4.3，并在 §4.3 内进一步决策「放弃老库」；新增 §4.3.A 给「只 stop 不需 disable + 归档 app.db + 删 unit 文件」的简化退役步骤；§5.1 补 ControlPanel.tsx:249 UI 文案清理 |
| 2026-06-20 | 入口集成方案选定 + §5.1 落地 | fengqi + claude | 选方案 A：主面板加「语音」tab iframe 嵌入 `/api/asr/`（不动 orchestrator router 路径）；§5.1 五条全部代码改动落地：`index.html`/`app.js` 加 tab + 懒加载、`hub.js` 删 orchestrator 卡片、`speech/settings.rs` DEFAULT_REMOTE_URL 改新址 + 新增 LEGACY_REMOTE_URL + `load_remote_settings_from_db` 加旧值自动回写迁移、`tauri-client.ts` 默认 URL 同步、`ControlPanel.tsx` 文案改 toolkit-server 主面板、`g10_deploy/registry.rs` 删 orchestrator ServiceDef 并把模块注释从 "7 个服务" 改为 "6 个" |
