# whisper-turbo 次模型偶发延迟分析（v2 · 已过双模型审核）

> **版本说明**：v1 初稿经两名独立大模型对抗性审核（代码审计向 + ASR 领域向），**服务端诊断主线被推翻**，
> 本 v2 已据审核意见修订。核心变化：
> - ❌ v1 把主慢因归为「whisper 自回归 + 无护栏复读，在 GPU 上时间片争用」——**归错了**。
> - ✅ v2 结论：主慢因是 **① 首段/换入冷启动（懒加载 10-30s、autotune）② Whisper encoder 定长 30s
>   padding 的固定成本 ③ 停止时 flush 的 15s drain**；而「拖累主链」的机制是 **GIL + 主 recognize 同步
>   阻塞事件循环**，不是 CUDA 流争用。「坏段复读」被 `sample_len≈224` 封顶，最坏几百 ms，**是次要因素**。
> - ✅ 对应解法从「解码封顶(B)」改为「**次模型限时(E) + 英文门控(C)**」；编排层润色 gating(A) 仍为可选感知层止血。
>
> 审核轨迹见文末第 10 节。涉及两个仓：
> - **toolkit**（本仓）：ASR 编排层 `orchestrator`（并入 toolkit-server 的 `/api/asr`），只做路由/选型/润色/降级。
> - **streaming-speech**（同机 `D:\git\streaming-speech`）：FunASR 服务（GB10 `:9100`/`:9101`），真正的推理。

---

## 1. 现象

- 场景：桌面端实时会话转写。主模型 `asr.model` = `sensevoice`（默认），另开次模型对比
  `asr.secondary_model` = `whisper-turbo`，`want_secondary=true`。
- 症状：**大部分时候无感，最近偶尔几次明显延迟**。间歇性尖峰，非稳态变慢。
- 诉求：whisper 对**个别英文词**识别质量优于中文向的 sensevoice/paraformer（后者英文常被音译），
  **希望保留 whisper 做英文对比**，目标是消掉偶发尖峰而非换模型。

---

## 2. 数据流与两条链路

```
桌面端 ──WS──▶ toolkit-server /api/asr/stream (orchestrator)
                     │  转发 PCM
                     ▼
              FunASR (:9100 WS)   ← 主模型 recognize()（同步跑在事件循环！）
                     │              + （opt-in）次模型 fan-out（executor 线程）
      segment / secondary 事件回传 orchestrator
                     │
   orchestrator: 立即转发 segment；按需 LLM 润色/翻译；合并模式下双候选整链润色
                     ▼
                  桌面端渲染
```

次模型产出 `secondary` 事件，仅供「并排对比」，**不参与最终采纳的转写主线**（润色/翻译只用主模型文本）。

---

## 3. 服务端：whisper-turbo 的真实开销结构（streaming-speech）— **已修订**

### 3.1 每段音频跑两遍识别（不变）
断句 finalize 后，主模型 `recognize()` 先跑（`app.py:712`）；若 `want_secondary` 且配了次模型，**同一段
PCM 再 fan-out 给次模型**（`app.py:730-738`，`asyncio.create_task(_run_secondary)`，`:734`）。次模型执行走
`run_in_executor(None, recognize_with, ...)`（`app.py:671`）。即：开次模型 = 每段在同一 GPU 上识别两次。

### 3.2 whisper 比主模型重在哪 — **v1 归因已更正**

whisper-turbo 分支见 `recognize_with`（`app.py:352-364`）。真实开销结构（据领域审核修订）：

1. **【更正】encoder 未精简 + 30s 定长 padding 才是固定大头**。
   whisper-large-v3-**turbo** 的精简是把 **decoder 从 32 层砍到 4 层**，**encoder（32 层）一层没省**。
   Whisper 是定长 30s 输入模型：**无论这段是 1.2s 还是 8s，encoder 都按 30s 算力跑一遍前向**。
   SenseVoice/Paraformer 没有这个定长约束，短段就是短算力。**这是「whisper 比主模型重」最本质的结构差异**，
   比 v1 强调的「自回归逐 token」更主导——因为 turbo 恰恰已把自回归那部分压到最轻（4 层 decoder）。
2. **【降级】自回归解码耗时正比于输出 token 数**——成立，但 turbo 的 4 层 decoder 让这一项很轻，短段时
   甚至不如 encoder 固定成本大。不再作为主慢因。
3. **【降级】语言 auto-detect**（`WHISPER_LANGUAGE=None`，`app.py:43`）——它**复用同一次 encoder 前向**，
   只额外多 **1 个 decoder step**（读 language token logits），≈1 个 token 的时间，**微不足道**。v1 把它列为
   三大开销之一是抓错轻重，v2 降级。
4. **GPU/资源争用**——见 3.3，机制已更正。

### 3.3 「拖累主链」的机制 — **v1 的「CUDA 流时间片争用」已更正为「GIL + 事件循环阻塞」**

关键事实（两名审核独立指出）：
- 主模型 `recognize(seg)` 在 `finalize` 里是**同步阻塞直调，跑在 asyncio 事件循环线程上**（`app.py:712`），
  **没走 executor**。
- 次模型 `recognize_with` 走 `run_in_executor(None, ...)`（`app.py:671`），跑在 ThreadPoolExecutor 工作线程。
- 代码里**没有任何 CUDA stream 管理**，也没有 MPS/多流并行。

因此「次模型拖累主链」**结论成立，但机制不是「CUDA 默认流时间片争用 GPU 算力」，而是**：
1. **GIL 争用**：PyTorch 的 kernel launch、whisper 自回归里的 Python 层循环/张量操作都持 GIL。次模型在
   executor 里跑重解码持 GIL 时，**事件循环线程连「读下一段 PCM / 喂 VAD / 发起下一次 recognize」都推进不了**。
2. **主 recognize 同步阻塞事件循环**：即使不开次模型，主模型每段 recognize 的 CUDA 同步点（取结果
   `.cpu()`/`res[0]`）就已经阻塞事件循环。次模型只是**又加一个抢 GIL/GPU 的竞争者**。
3. **executor backlog**：连续说话时多个 `_run_secondary` 可并发提交到线程池不同线程，**N 段次模型堆积一起
   碾 GPU/GIL**（`sec_tasks` 仅在 flush 时 prune，`app.py:839-847`）。

> **⚠️ 选型影响**：因为主导是 GIL + 事件循环阻塞而非 GPU 算力争用，**「上多 CUDA stream / MPS 让主次并行」
> 解决不了问题**——Python 层 launch 仍抢 GIL、主 recognize 仍同步阻塞 loop。这直接否掉了一类看似合理的方案。

> **独立架构隐患（v1 遗漏）**：主 recognize 同步阻塞事件循环，会**拖累该 asr 实例上所有并发 WS session**
> 的音频读取，不止次模型。且**离线路径 `/transcribe` 特意走了 executor**（`app.py:527`
> `run_in_executor(_transcribe_blocking)`），流式 `finalize` 的主 recognize 却是裸同步——**两条路径对
> recognize 的处理不一致**。这可能是比次模型更根本的问题，次模型只是把它暴露/放大了。

### 3.4 「无防复读护栏」 — **v1 的「关键疑点/主因」降级为「未证实且量级偏小的次要因素」**

事实：opts dict（`app.py:353-360`）确实不含 `compression_ratio_threshold` / `logprob_threshold` /
`no_speech_threshold` / `condition_on_previous_text` / `sample_len`，`beam_size=None`（greedy）。

但审核修正了 v1 的两处：
1. **量级被高估**：Whisper 单窗解码硬顶在 `sample_len`（默认 `n_text_ctx//2 ≈ 224` token，`without_timestamps`
   下更短）。224 token 的复读段在 turbo 4 层 decoder 上大概**几百 ms**，**不是「数秒」**。真出现数秒，更可能
   是冷启动/换入/autotune（见第 5 节），不是 decoder 复读。
2. **护栏归属 + greedy 因果**：那组阈值本就属于 openai-whisper **`transcribe()`** 层（30s 窗 + temperature
   fallback 循环）而非 `DecodingOptions`；「缺 fallback 逻辑 → 坏段无升温逃逸」成立，但**greedy 本身不是复读
   主因**（beam search 同样会复读）。真正防复读的是 temperature fallback，不是 beam vs greedy。
3. **能否加护栏 = 未定**：FunASR 的 whisper wrapper 是否走 transcribe() fallback、`DecodingOptions` 透传哪些
   key、`batch_size_s=0` 的确切语义，**均需进容器读 funasr 源码**（本机无 funasr，仅 Docker 镜像内）。

**结论**：坏段复读是真实但**次要**的因素，且其可治性 gated 在容器核实。**不再作为「主因」。**

---

## 4. 编排层：合并模式下 LLM 润色被压到次模型之后（toolkit）— **审核确认最扎实**

主 segment 收到后**立即转发、绝不被阻塞**（`lib.rs:767-781`）。润色触发条件：

```rust
// crates/orchestrator/src/lib.rs:803-804
let defer_opt_to_secondary = merge_on && hello.want_secondary;
let do_opt_here = hello.want_optimize && !defer_opt_to_secondary;
```

- **合并模式（`merge_window_ms>0`）+ 开次模型**时，主模型润色**推迟到 `secondary` 事件到达**（`lib.rs:957`
  起），用「主链 + 次链」双候选一次性整链润色。设计意图（`lib.rs:799-802`）：避免「先润一版、次模型来了再润
  一版」的翻倍 LLM 调用 + 闪烁覆盖。
- **副作用**：润色后文本落定延迟 = 等 whisper-turbo 返回 + 再等 LLM。**一个仅供对比的次模型，卡住了主输出
  润色的呈现。**
- **非合并模式（`merge_window_ms=0`）**走 `lib.rs:1051` 起逐段 re-polish：**先发主模型润色，次模型到了再覆盖**
  →主输出**不**被次模型 gating。此路径下本节的放大效应不存在。

> 审核补充：合并模式本就每来一个 VAD 段触发一次润色（latest-wins 守卫按主链字符数单调放行，
> `lib.rs:872-880/1003-1011`），故方案 A「翻倍 LLM 调用」实为「每段一次 + latest-wins 收敛」，非精确 2 倍。

> **停止时的 drain**：flush 收尾会 `await asyncio.wait_for(gather(sec_tasks), timeout=15)`（`app.py:841-843`），
> **最长阻塞 15s 才发 `done`**。whisper 慢时这是「停止按钮卡一下」的确定来源。

---

## 5. 偶发主因排序 — **v2 重排（据审核，从源码可确认者优先）**

三个条件同时满足才卡出可感知延迟，任缺其一无感，故间歇。但**主因排序相对 v1 大改**：

1. **【新增·头号嫌疑】首段/换入冷启动**。次模型首次懒加载 ~10-30s（`_ensure_secondary_loaded`，
   `app.py:326-336` + `:664` 注释）；虽有 config 握手预热（`app.py:831-833`）但预热也走 executor 且不阻塞握手
   ——若用户在权重没加载完就说完第一句，第一个 secondary 吃满整个加载时间。另有首段 cuDNN/torch autotune
   benchmark。**最符合「最近偶尔几次、偏会话早期」**，且从源码可确认。热切换 `_build_asr` 在 15s 轮询线程同步
   加载（`_refresh_asr_config`）恰逢说话也会叠加。
2. **【结构性】encoder 30s 定长 padding 固定成本**（见 3.2）。稳态偏慢，遇长段（接近 30s）+ 长解码叠加更明显。
3. **【停止时】flush 的 15s drain**（`app.py:841-843`）——停止偶发卡顿的确定来源。
4. **【放大器】GIL + 主 recognize 阻塞事件循环**（见 3.3）——让上面任一慢因外溢到别的段和 WS 呈现。
5. **【放大器·仅合并模式】编排层润色 gating**（见第 4 节）。
6. **【次要】坏段复读**（见 3.4）——被 sample_len 封顶，量级几百 ms，非主因。

**说话节奏**（连续说 vs 有停顿）决定 1-3 的延迟是否与下一段主 recognize 在 GIL/事件循环上重叠——重叠是
概率性的、依赖 GIL 让渡窗口，比 v1「时间片争用」更微妙。

---

## 6. 结论（v2）

1. **「拖累主链」机制 = GIL + 主 recognize 同步阻塞事件循环 + executor backlog**，非 CUDA 流算力争用。
2. **偶发尖峰头号嫌疑 = 首段/换入冷启动（懒加载/autotune）**，其次 encoder 定长成本、停止时 15s drain；
   坏段复读是次要（被 sample_len 封顶）。
3. **编排层润色 gating（第 4 节）是可感知放大器**，仅在合并模式生效，归因准确、代码可读、最可靠。
4. **动手前必须先采日志证实归因**（第 8 节问题 7）——现有日志缺「解码墙钟耗时」字段，建议先补一条再定位。

---

## 7. 候选方案与取舍 — **v2 重排（B 降级，E/C 提为首选）**

| 方案 | 位置 | 作用 | 取舍 / 风险 |
|---|---|---|---|
| **E. 次模型限时（超时即弃）** ⭐首选止血 | streaming-speech `_run_secondary` | 给 `run_in_executor(recognize_with)` 套 `asyncio.wait_for(timeout≈1.5s)`，超时丢弃这段对比（记 warning） | **不依赖搞懂 FunASR 内部、不截断正常识别（只是放弃对比）**。钉死次模型最坏占用对下游的影响。对比本就尽力而为，业务可接受 |
| **C. 英文门控** ⭐治本砍量 | 两侧 | 主模型（sensevoice）已出文本 → 统计 ASCII/拉丁字母占比 > 阈值才 fan-out 次模型 | 直接砍 whisper 调用总量=砍争用总量，且**契合「次模型只为英文对比」的业务前提**，性价比最高。判据很轻 |
| **A. 润色不等次模型** | toolkit orchestrator | 合并模式下主模型润色照常立即触发，whisper 到了再 re-polish | 纯感知层止血，**只治第 4 节放大器、不治服务端尖峰**。恢复双次润色 + 一次闪烁 |
| **A′. 带超时的润色** | toolkit orchestrator | 等次模型但设上限，超时先用主模型润 | 比 A 好；超时值别设太短（turbo 长段解码可能 800ms+），建议按次模型历史 P95 或直接 ~1.5s |
| **B. whisper 解码封顶** | streaming-speech FunASR | ~~钉死单段最坏解码时间~~ | **降级/大概率无效**：封 token 数 ≠ 封时间（**encoder 30s padding 封不掉**，不解码也要跑完）；turbo 已默认封在 ~224 token，再压会截断正常长英文句。且能否透传 key 未知 |
| **F. 独立 CUDA stream / 优先级** | FunASR | 让主模型 kernel 插队 | GPU 算力总量不变，**改不了 GIL 争用**，性价比低于 E/C |
| **G. 次模型挪独立进程/GPU** | FunASR | 从根上消 GIL（独立进程）+ GPU 争用（独立 GPU） | 终极方案，工程量大；GB10 单 GPU 时独立进程只消 GIL 不消 GPU 争用 |
| **H. 机会式对比** | FunASR | 仅在 `sec_tasks` 空 + 距上段 > gap 时才 fan-out | 契合「有停顿就无争用」，零依赖，可与 C/E 叠加 |
| **附. 修流式主 recognize 也走 executor** | FunASR | 主 recognize 不再同步阻塞事件循环（对齐离线 `:527`） | 治 3.3 的独立隐患，利好所有并发 session；需评估对 finalize 时序的影响 |
| **D. 换次模型** | 配置 | 切回 sensevoice/paraformer | **用户已否决**（丢 whisper 英文优势） |

**推荐路径**：先 **采日志证实归因**（尤其是否首段冷启动/drain）→ 上 **E（限时止血）+ C（英文门控砍量）**
→ 观察 → 仍需再考虑 **G（独立进程）** 与 **附（主 recognize 走 executor）**。**B 砍掉或降为备选。**

---

## 8. 开放问题清单 + 现有源码能给的答案

1. **FunASR whisper 封装是否等价 transcribe() 的 fallback 逻辑 / 透传哪些 DecodingOptions key /
   `batch_size_s=0` 语义** → **需进容器读 funasr 源码**（本机无 funasr）。决定 3.4 与方案 B 可行性。
2. **能否 `sample_len` 封顶而不截断正常长句** → **需容器核实**；且封 token 封不掉 encoder 固定成本（见 B）。
3. **GPU 争用真实性 / 主 recognize 是否同步直调** → **✅ 源码已答**：主 `recognize` 同步阻塞事件循环
   （`app.py:712`），次模型走 executor（`:671`），**无 CUDA 多流**；串行化主导是 GIL + 事件循环阻塞，不是
   CUDA 流时间片。
4. **偶发是否遗漏因素** → **✅ 源码已补**：首段懒加载 10-30s（`app.py:326/664`）、flush 15s drain
   （`app.py:841-843`）、热切换同步加载（`_refresh_asr_config`）——均从源码可确认，很可能比「复读」更常见。
5. **编排 gating 影响面 / 桌面端默认是否合并模式** → 「合并+次模型才 gating」**✅ 准确**（`lib.rs:803-804`）；
   非合并走 `lib.rs:1051` re-polish、主输出不被 gating；**「桌面端默认 merge_window_ms 值」需查 zero-desktop
   前端**（由客户端 hello 帧传入，`lib.rs:653-654`），不在被审两文件内。
6. **方案 A 取舍量化 / A′ 超时值** → 属运行时/产品权衡，源码给不出，需实测 LLM 单次延迟与体感。
7. **日志能否证实复读/超长解码** → **⚠️ 打折**：`[asr][seg] ... dur=..ms`（`app.py:645`）的 `dur` 是**音频
   时长**，**不是解码墙钟耗时**；`[asr][sec] ... text=..`（`app.py:676`）能看复读文本但无耗时。**建议先在
   `_run_secondary` 加 `time.monotonic()` 前后差的解码耗时日志**，否则归因验证不充分。

---

## 9. 关键代码索引

**toolkit / orchestrator**（`crates/orchestrator/src/lib.rs`）
- `:296` `asr.model` 默认 sensevoice；`:299` `asr.secondary_model` 默认 paraformer
- `:653-654` `merge_on = merge_window_ms > 0`（值由客户端 hello 传入）
- `:767-781` 主 segment 立即转发（不被 LLM 阻塞）
- `:803-804` `defer_opt_to_secondary = merge_on && want_secondary`（润色 gating 判据）
- `:872-880 / 1003-1011` 合并模式 latest-wins 守卫
- `:909-1034` secondary 事件 + 合并模式双候选整链润色
- `:1051-` 非合并模式逐段 re-polish（主输出不被 gating）
- `:1515-1518` 控制台 `asr.model` / `asr.secondary_model` 选项与 hint

**streaming-speech / FunASR**（`server/asr/app.py`）
- `:39-43` whisper 目录 env + `WHISPER_LANGUAGE`（默认 auto-detect）
- `:315-337` `_ensure_secondary_loaded`（懒加载 10-30s）
- `:340-370` `recognize_with`（次模型；whisper 分支 DecodingOptions `:352-364`，无护栏 key）
- `:527` 离线 `/transcribe` 走 `run_in_executor(_transcribe_blocking)`（**对比流式的反例**）
- `:611-639` `recognize`（主模型）
- `:645` `[asr][seg] ... dur=..ms`（dur=音频时长，非解码耗时）
- `:659-690` `_run_secondary`（executor `:671` + emit secondary `:676`）
- `:693-738` `finalize`（主模型同步 `recognize` `:712` + fan-out 次模型 `:730-738`）
- `:839-847` flush drain：`wait_for(gather(sec_tasks), timeout=15)`

---

## 10. 审核轨迹

- **v1 初稿**：作者据两仓源码分析，主线为「whisper 自回归+无护栏复读 → GPU 时间片争用 → 主链变慢；治本靠
  解码封顶(B)」。作者已在 v1 用 ⚠️ 框与开放问题标注不确定性。
- **审核 1（代码审计向）**：逐条核对 25 处 `文件:行号` 引用，**全部可核对、无张冠李戴**（仅 finalize 范围
  693-734 应为 693-738 的小误）。指出 3.2 机制归错层（GIL+事件循环阻塞非 CUDA 流）、3.4 主因下得过早、第 5
  节遗漏懒加载/drain 两个源码可确认的偶发来源、主 recognize 阻塞事件循环的独立隐患、日志缺解码耗时字段。
- **审核 2（ASR 领域向）**：更正 turbo 慢因（encoder 未精简+30s padding 才是大头，非自回归）、auto-detect
  被高估（仅 1 decoder step）、复读量级被高估（sample_len≈224 封顶几百 ms，非数秒）、方案 B 无效（封 token 不
  封时间）、补方案 E/C/G/H，重排偶发主因（冷启动为首）。
- **v2（本文）**：采纳上述两份意见修订。两名审核**独立地**在「机制是 GIL 而非 CUDA 流」「复读非主因、冷启动
  才是」「第 4 节最扎实」三点上一致，可信度较高。**仍需进容器核实第 8 节问题 1/2，并先采日志证实归因后再动手。**

*文档状态：v2，服务端主线已修订。方案 E/C 为推荐止血，动手前先补解码耗时日志 + 采样验证冷启动假设。*
