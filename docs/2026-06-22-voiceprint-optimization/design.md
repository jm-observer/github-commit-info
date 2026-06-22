# 声纹识别 + ASR 控制台两件事的盘点（样本 #13 触发）

> 日期：2026-06-22
> 触发：zero-desktop 语音标注里出现一条 `label=other` 的样本——视频里别人说话的片段；
> 顺带发现 toolkit-server 主面板的「语音」tab 整片空白。

## 一、声纹这条：实际没毛病，借机把现状盘清

### 误以为的「误识别」

最初读法：「这段视频里别人说话的音频被识别成了本人」→ 要靠这条优化声纹。

### 实测结论

- segment 3998 在 orchestrator 里 `"speaker": null`，**系统并没有把这段误归到本人**。
- 把 `13.wav` 调 G10 `:9101/embed` 拿到向量，与本人 embedding 算余弦 = **0.1529**，远低于阈值 `asr.spk_threshold=0.35`（差 0.20）。
- 所以 zero-desktop 上 note 写的「声纹没有匹配，不是本人说话」是**事实陈述**，不是 bug 报告——声纹门控该拒就拒了。

### 单向量声纹库现状（顺带固化）

`speakers(id, name, embedding TEXT CSV, enabled, created_at)`，每人一条向量
（`crates/orchestrator/src/db.rs`）。决策在 FunASR 侧 `finalize()` 完成
（`D:\git\streaming-speech\server\asr\app.py:683-692`）：

```python
spk, score = best_speaker(seg)
gated = GATE_TO_ENROLLED and bool(ENABLED_VPS)
if gated and spk and score < SPK_THRESHOLD: return        # 整段丢弃
speaker = spk if (ENABLED_VPS and spk and score >= SPK_THRESHOLD) else None
await emit_segment(ws, text, beg, end, speaker)
```

观察到的「segment 3998 出现在历史里但 speaker=null」说明 finalize 路径其实走的是
「emit + speaker=None」而非「DROP 全段」。结合代码：

- `best_speaker()` 在嵌入失败时返回 `("", 1.0)` —— `spk=""` 落空，**跳过 DROP**，最终 emit 一条没有 speaker 的段。
- 或者 `gated` 在那一刻是 False（用户后续可能开过 gate=on 后又改回，或 refresh 还没到）。
- 任一路径下，**不算系统出错**。

### 关键约束（决定能/不能怎么优化）

1. **每人单向量**，无负样本、无多向量、无置信度衰减机制。
2. **匹配分数不落库** —— `segments` 只有最终 `speaker` 字符串，没有 similarity；FunASR 实时端日志可能有 `[asr][spk] DROP ...` 行但不结构化。事后无法回溯当时分数。
3. 阈值是**全局**，调高同时影响本人召回。
4. zero-desktop 标注 (`speech_samples.correction` / `note`) 当前是死信，没有任何下游消费。

### 本次的决定：声纹**不动**

- 余弦 0.15 vs 阈值 0.35 → 现有声纹对这类视频对话有 0.20 安全余量。
- 改阈值会缩窄余量但治不了任何已知 bug。
- 重录声纹也救不了不存在的病。
- 故本次声纹库 / 阈值 / enrollment 全部维持原状。

## 二、`/api/asr/` 控制台空白：已修

### 根因

orchestrator 的 console HTML（`crates/orchestrator/src/lib.rs:1400` 起的 `CONSOLE_HTML` 常量）
内所有 `fetch('/api/stats')` / `href="/segment/..."` / `audio.src='/api/segments/.../audio'`
都是绝对路径。

orchestrator **独立 serve 时**这些路径直达自己；toolkit-server **nest 在 `/api/asr` 下**之后：

- iframe 加载 `/api/asr/` → 返回 HTML（这步是对的）
- 内部 `fetch('/api/stats')` → 打到 toolkit-server 根（404，缺 `/api/asr` 前缀）
- 页面 shell 出来但 render() 第一个 fetch 就 404 → 内容区永远空白

### 更正：空白其实是两层 bug，`__PREFIX` 只是第二层

第一次只改了 console 内部的 `__PREFIX`，但部署后仍空白。复查发现**主因在更外层**：

- toolkit-server 主面板「语音」tab 用 iframe 加载 ASR 控制台，`web/app.js` 里
  `f.src = "/api/asr/"`（**带尾斜杠**）。
- 但 axum `nest("/api/asr", router)` 把内层 `route("/", console)` 映射到 **`/api/asr`（无尾斜杠）**
  上：`/api/asr` → 200，`/api/asr/` → **404**。
- iframe 拿到 404 → 永远空白。这跟 console 内部的 `__PREFIX` 无关——**控制台根本没被加载**。

所以是两处都要改：

1. **iframe src**（`crates/toolkit-server/web/app.js`）：`/api/asr/` → `/api/asr`（去尾斜杠）。这是空白的直接原因。
2. **console 内部 `__PREFIX`**（orchestrator `lib.rs`）：控制台加载后，其内部 `fetch('/api/stats')`
   等绝对路径要带上 `/api/asr` 前缀才能命中 nest 路由。
3. **console 根导航 `__ROOT`**（orchestrator `lib.rs`）：单条页的「返回管理台」/ tab 回弹链接原本指向
   `__PREFIX + '/'` = `/api/asr/`（同样 404）；改用 `__ROOT = __PREFIX || '/'`（standalone=`/`、
   nested=`/api/asr`，均无尾斜杠 404 风险）。

### 修复（console 内部 `__PREFIX`）

在 `<script>` 顶部注入 `__PREFIX` + 重写 `fetch`：

```js
const __PREFIX=(function(){let p=location.pathname.replace(/\/$/,'');p=p.replace(/\/segment\/\d+$/,'');return p})();
const __origFetch=window.fetch.bind(window);
window.fetch=function(u,o){if(typeof u==='string'&&u.charAt(0)==='/')u=__PREFIX+u;return __origFetch(u,o)};
```

- 独立 serve：`location.pathname='/'` → `__PREFIX=''` → 行为零变化。
- 嵌入 serve：`location.pathname='/api/asr/'` → `__PREFIX='/api/asr'` → 所有以 `/` 开头的 fetch 自动加前缀。
- single-segment 页 `/api/asr/segment/123` → 剥掉 `/segment/123` 后 `__PREFIX='/api/asr'`。

`fetch` 走 monkey-patch 一次性覆盖 `j()`/`doEnroll`/`renderSingleSegment` 全部网络调用。剩下的非
fetch 引用（`href` / `location.href` / `audio.src` / 单文件页的「返回管理台」链接）逐处手改成
`${__PREFIX}/...`，外加 bootstrap 的正则去掉 `^` 锚以兼容嵌套路径。

变更集：`crates/orchestrator/src/lib.rs` ~10 处微调，无 schema/接口改动；`cargo check -p orchestrator` 通过。

### 部署

`pwsh ./deploy-g10.ps1`（默认 `-Service toolkit-server`，交叉编译重启）。**注意 `-SkipBuild` 只复制
旧产物，不会带上本次改动**——首次部署若用了 `-SkipBuild` 会原地踏步（实际踩到过）。部署后浏览器
**硬刷新**（外层 index.html/app.js 有缓存）再看「语音」tab。

curl 验证（部署后）：`/api/asr` → 200 带 `__PREFIX`/`__ROOT`；`/api/asr/api/{stats,history,speakers,asr-config}`
全 200；`web/app.js` 的 iframe src 已是无尾斜杠 `/api/asr`。

## 三、门控漏推 bug：已修（streaming-speech 仓）

### 现象

`gate_to_enrolled=on` 时，**没有匹配到任何已启用声纹的段仍被推送成一条 speaker=null 的记录**
（segment 3998 即此）。严格门控语义下这段应被丢弃、根本不入库。

### 根因

`D:\git\streaming-speech\server\asr\app.py` 的 `finalize()`：

```python
# 旧逻辑
if gated and spk and score < SPK_THRESHOLD:   # ← spk 为真才进 DROP
    return
speaker = spk if (ENABLED_VPS and spk and score >= SPK_THRESHOLD) else None
await emit_segment(...)                        # 没 DROP 的都 emit
```

DROP 分支被 `spk` 真值守卫挡住。而 `best_speaker()` 在**嵌入失败**时返回 `("", 1.0)`（注释自称
"fail open"）——空串 `spk` 是 falsy → 跳过 DROP → 落到 `emit_segment(speaker=None)`。于是
「嵌入算不出来 / 没人匹配上」的段在门控开着时照样推送。

### 修复

把判断改成「门控开 + 没正向匹配上 → 丢弃」：

```python
matched = bool(spk) and score >= SPK_THRESHOLD   # 嵌入失败(spk="")或低于阈值都算未匹配
if gated and not matched:
    print(f"[asr][spk] DROP unmatched best={spk!r} score={score:.3f} thr={SPK_THRESHOLD} ...")
    return
speaker = spk if matched else None               # 仅门控关时才会走到 None
await emit_segment(ws, text, beg, end, speaker)
```

并把 `best_speaker()` 嵌入失败的返回值从 `("", 1.0)` 改为 `("", -1.0)`（非匹配哨兵，日志更诚实）。

行为变化：

| 场景 | gate=on 旧 | gate=on 新 | gate=off（不变） |
|---|---|---|---|
| 匹配上(≥阈值) | emit + 标注 | emit + 标注 | emit + 标注 |
| 低于阈值 | DROP | DROP | emit, speaker=None |
| **嵌入失败/空** | **emit speaker=None（漏推）** | **DROP** | emit, speaker=None |

代价提示：门控开时，本人若有些**太短/算不出嵌入**的段，现在会被一并丢弃（之前至少能以无名段出现）。
这正是「严格门控」的应有语义，且仅门控开时生效；门控关行为完全不变。`py_compile` 通过。

### 部署（streaming-speech 仓，独立于 toolkit）

FunASR 服务归 streaming-speech 维护，重建走该仓的 `.\scripts\release-server.ps1`（rebuild + up -d asr
docker 容器到 GB10）。**与 toolkit 的 deploy-g10.ps1 是两条独立部署链**。

## 三点五、真正的根因：asr 容器 ORCH_BASE 在 orchestrator 迁出后失效（已修）

> 这才是「视频里别人说话被转写并推送」的根因。前面 §三 的 finalize 漏推 fix 是对的，但在本项
> 修复前一直是**休眠**的——因为门控压根没生效。

### 现象链

部署后查 `:9101/health` 长期 `model=paraformer`、`enrolled_voiceprints=0`，与控制台配置
（`asr.model=sensevoice`、已注册 fengqi 1 条声纹）不符。

### 根因

asr 容器靠 `ORCH_BASE` 每 15s 反向拉取声纹/配置（`app.py:217 _refresh_voiceprints` /
`:234 _refresh_asr_config`，`urlopen(f"{ORCH_BASE}/api/voiceprints")`）。compose **没设 ORCH_BASE**
→ 落到默认 `http://orchestrator:8090`。但 orchestrator 已于 2026-06 迁出容器、并入宿主 toolkit-server
（:8788，路由 nest 在 `/api/asr` 下），`orchestrator` 这个服务名在容器内 **DNS 解析失败**
（实测 `socket.gaierror -3`）。

后果（连锁）：

1. `_refresh_voiceprints` 每轮失败 → `ENABLED_VPS` 恒为 `[]`。
2. `gated = GATE_TO_ENROLLED and bool(ENABLED_VPS)` = `True and False` = **False** → **门控形同关闭**。
3. 于是**所有**语音段（含视频里别人说话）都被转写并以 `speaker=null` 推送 —— 即 segment 3998 的真相。
4. 主模型也从不热切，停在默认 `paraformer`（次模型 sensevoice 是 env 默认才碰巧加载）；阈值/热词也永不更新。

`[asr][cfg] ... gate=on spk_thr=0.35` 日志行有误导性——那些全是 **env 默认值**，不是成功拉取的结果。

### 修复（compose.yaml）

给 asr 服务显式接回宿主 toolkit-server：

```yaml
environment:
  - ORCH_BASE=http://host.docker.internal:8788/api/asr
extra_hosts:
  - "host.docker.internal:host-gateway"
```

- `ORCH_BASE` 末段带 `/api/asr`，使 `{ORCH_BASE}/api/voiceprints` 命中 toolkit-server 的
  nest 路由 `/api/asr/api/voiceprints`（宿主实测 200）。
- 容器经 `host-gateway`（docker 可移植写法）回连宿主；toolkit-server 绑 `0.0.0.0:8788`，
  容器经 bridge 网关 `172.17.0.1:8788` 实测可达。

### 验证（部署后 /health 稳态）

```json
{"status":"ok","model":"sensevoice","spk_threshold":0.35,"gate_to_enrolled":true,"enrolled_voiceprints":1,...}
```

`enrolled_voiceprints=1` + `gate=on` → `gated=True` → **门控首次真正生效**；model 也热切到
sensevoice。结合 §三 的 finalize fix：视频/他人语音（cos 0.15 < 0.35）现在会被 DROP，不再推送。

### 顺带修的冒烟误报（release-server.ps1）

- 新增 asr `/health` GET 端点（`app.py`，返回 model/threshold/gate/voiceprint 数，不触发模型推理）。
- `release-server.ps1` 冒烟从死端口 `:8090/api/stats` 改为轮询 `:9101/health`（最多 ~40s，
  容错 FunASR 模型加载耗时），不再每次假报失败。

## 四、后续工程化（不在本次范围，记账）

- **持久化匹配分数**：`segments` 增加 `speaker_score REAL`；让 FunASR 在 `emit_segment` 时一并把
  `score` 透传（即便 speaker=None 也带分数，便于后期统计阈值是否合理）。**这是把"事后无法判断当时
  发生了什么"这件事彻底解决的最小改动**。
- **多向量声纹库**：`speaker_embeddings(speaker_id, embedding, source, created_at)`，匹配取 Top-K
  最大或均值。一人多场景（耳麦/外放/嘈杂）能各贡献一条；本次因为只有一条声纹也用不上。
- **标注 → 反馈闭环**：zero-desktop `speech_mark_sample` 把 `label=other` 加上「这段不是 X」的
  反向信号推给 orchestrator（新增 `POST /api/asr/api/speakers/negative-sample`），后端写入待重训
  队列或做轻量统计。当前 `correction`/`note` 是死信。
- **新增 `speaker_wrong` 标签**：现有 `asr_wrong | hotword | bad_optimize | ok | other` 没有
  「说话人错」类，用 `other + note 文字判断` 不可机读；新增显式标签更利于闭环。
- **`gate_to_enrolled` 时 emit 与 DROP 的语义**：当前若 `best_speaker` 返回 `("", 1.0)` 仍会
  emit；这与「严格门控」的直觉不一致。建议在 streaming-speech 侧把 emb 失败/为空时也归到 DROP。

## 执行记录

- 2026-06-22 21:32:52  样本 #13 标注（label=other）。
- 2026-06-22 22:15      cos(sample13.wav, fengqi) = 0.1529；当前阈值 0.35；判定声纹不动。
- 2026-06-22 22:30      orchestrator console HTML 注入 `__PREFIX` 修复 nest 路径，`cargo check` 通过；
                        待 G10 重部署后验证。
- 2026-06-22 22:45      streaming-speech finalize() 门控漏推 bug 修复（嵌入失败/未匹配在 gate=on 时
                        改为 DROP），`py_compile` 通过。
- 2026-06-22 23:00      release-server.ps1 部署 asr：finalize fix 上线（容器内 grep 确认）。发现
                        /health 显示 voiceprints=0、model=paraformer，与配置不符 → 深挖。
- 2026-06-22 23:10      定位真因：asr 容器 ORCH_BASE 默认 http://orchestrator:8090 在迁出后 DNS 解析
                        失败，声纹恒空 → 门控形同关闭 → 所有语音被推送。新增 /health 端点 + 修冒烟。
- 2026-06-22 23:14      compose 加 ORCH_BASE=http://host.docker.internal:8788/api/asr + host-gateway，
                        重部署。/health 稳态：model=sensevoice、voiceprints=1、gate=on → 门控首次真正
                        生效。冒烟轮询 /health 通过（发布完成）。整链修复闭环。
- 2026-06-22 23:17      用户报「语音」tab 仍空白。查得部署的 toolkit-server 是 -SkipBuild 旧产物，
                        无 __PREFIX。全量 deploy 后仍空白 → 复查发现真因：iframe src `/api/asr/`
                        带尾斜杠被 axum nest 404。
- 2026-06-22 23:24      修 web/app.js iframe src 去尾斜杠 + orchestrator __ROOT 根导航，cargo check 通过，
                        deploy-g10.ps1 全量重编译重启。curl 验证 /api/asr 200 + 内部调用全 200。
                        待用户硬刷新确认。

[db]: ../../crates/orchestrator/src/db.rs
[finalize]: file:///D:/git/streaming-speech/server/asr/app.py
