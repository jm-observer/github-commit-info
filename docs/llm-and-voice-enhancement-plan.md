# LLM 中枢收拢 + ASR 中文优化增强 + 本地语音命令（增强计划）

> 状态：**设计 / 实施计划 v1**（2026-06-21 起草）。本仓 issue 驱动，按节落地。
> 与 [voice-command-agent-design.md](voice-command-agent-design.md) 的关系：那篇是**远程通道**
> （zero-desktop ASR → zero agent → tool-calling），本篇做的是**本地轻量通道**——直接在
> zero-desktop 把若干固定语音短语映射成本机操作（如"发送" → 回车）。两条通道**互补**：本地
> 通道零延迟、零网络依赖、覆盖高频小动作；远程通道交给 zero agent 处理复杂意图。

---

## 1. 背景与问题清单

把当前散在多处、相互不通的 LLM 配置 / 提示词收拢；并补齐 ASR 中文优化与本地语音命令两条
能立刻提升体验的链路。

### 1.1 LLM 配置两套并行（最该先拍平）

| | 公共 LLM 层（`toolkit-server/src/llm/mod.rs`） | orchestrator（同进程但自成体系） |
|---|---|---|
| 配置存储 | `toolkit.db` 的 `llm_config` 表（base_url / model / api_key） | `app.db` 的 `config` 表 `vllm.base` / `vllm.model` |
| 环境变量兜底 | `LLM_BASE_URL` / `LLM_MODEL` / `LLM_API_KEY` | `VLLM_BASE` / `VLLM_MODEL` |
| 提示词 | `builtins()`（`douyin_refine` / `chat_summary`） | `app.db` 的 `llm.optimize_prompt` / `llm.translate_prompt`（**不在 builtins**） |
| 客户端 | `toolkit-llm::LlmClient`（统一重试 / 超时） | orchestrator 自行拼 HTTP |
| 热词注入 | 无 | `optimize_prompt_with_hotwords` 拼到 prompt 末尾 |

**后果**：控制台改一处只生效一半；调试 ASR 优化要钻 orchestrator 的"配置"tab；提示词改动
没有版本号 / 哈希溯源。

### 1.2 ASR 中文优化能再上一台阶（不做"风格档"）

今天 prompt 已经做了：上文 20s 窗口、主次模型 A/B 合并、热词同音字纠错。还差：
- **英文 / 代码标识符保护**（`Tauri` 不该被写成"塔里"）
- **数字 / 日期规范化**（中文写法 → 阿拉伯数字 / 标准日期）
- **逐句对齐，不删不增**（防 LLM 自作总结压缩）
- **失败兜底**：vLLM 超时 / 503 时桌面端今天卡在 `optimize_status: running` —— 加超时降级
  直接吐原文 + `status: fallback`，状态明确

> 风格档（口语 / 邮件 / 纪要 / 微信短句）**本期不做**，按用户明确意见排除。

### 1.3 本地语音命令缺位

桌面端今天的 ASR 优化稿只有"进剪贴板 / 自动粘贴 / 显示"三条出路。缺一类"**说几个字就直接
触发一个本机动作**"——典型场景：说完一段话再说"发送"，希望直接敲回车。

---

## 2. 总体方案

按"先地基、再增强、再新功能"的顺序，分三节落地。每节独立可验收。

### 节 A：两套 LLM 配置拢成一套

**改动点**：

1. 在 `llm/mod.rs::builtins()` 新登记两条提示词：
   - `asr_optimize_zh`：占位符 `{HOTWORDS}` / `{CONTEXT}` / `{PRIMARY}` /
     `{SECONDARY}`，默认文本取自 orchestrator 当前 `DEFAULT_OPTIMIZE_PROMPT`
   - `asr_translate`：占位符 `{TEXT}`，默认文本取自 `DEFAULT_TRANSLATE_PROMPT`
2. orchestrator 改读统一配置：
   - 连接：`toolkit-server::llm::resolve_config(toolkit_pool)`（旧 `VLLM_BASE` env 保留 6 个
     月兜底；DB 里的 `vllm.base` / `vllm.model` 标记 `deprecated`，启动若读到则告警并以公共
     LLM 配置为准）
   - 提示词：`resolve_prompt(toolkit_pool, "asr_optimize_zh")`，**渲染时填占位符**——
     热词不再写进 prompt 模板本身，而是渲染时 orchestrator 把热词列表填到 `{HOTWORDS}`
3. 控制台 `/api/web/llm/prompts` 自动列出新两条；删行即恢复内置默认
4. 落产物时记 `prompt_version` + `prompt_hash`（与 douyin 一致）

**不做**：保留 orchestrator 那个"配置"tab 的字段编辑（向后兼容显示），但底部加一行"该配置
已迁至公共 LLM 层，新值请在 ⋯/api/web/llm/config 修改"。

**验收**：在 `/api/web/llm/config` 改 base_url，重启 orchestrator（嵌入式即重启 toolkit-server），
观察 ASR 优化是否生效新配置；在 `/api/web/llm/prompts/asr_optimize_zh` 改文本，ASR 下一段
优化是否使用新文案。

---

### 节 B：ASR 中文优化提示词加固

**改动**仅在 `asr_optimize_zh` 内置默认文本上追加这几条规则：

```
- 英文单词、代码标识符（驼峰 / 蛇形 / 含数字）保持原样，不要意译。
- 数字、日期、金额、版本号统一阿拉伯数字与标准写法（"二零二六年六月" → "2026 年 6 月"，
  "v 一点零" → "v1.0"）。
- 逐句对齐原文，不要合并 / 压缩 / 总结，不要删减信息。
- 仅做润色与同音字纠错；如原文已通顺则原样返回。
```

**失败兜底**（orchestrator 侧）：

- HTTP 超时 / 5xx → 重试 1 次（指数退避），仍失败则直接把**原文**作为优化稿吐出，
  WS 事件 `optimized` 附 `status: "fallback"`
- 桌面端 UI 在 `optimize_status == "fallback"` 时显示轻量提示（图标 + tooltip：
  "LLM 不可用，已返回原文"），不再显示永久 running

**验收**：
1. 在 prompt 里临时塞一个高混淆短语（"Tauri 的开发"），断网或停 vLLM，桌面端应在 ~3s 内
   收到 fallback 优化稿（= 原文）而非卡 running
2. 恢复 vLLM，下一段优化稿英文保留 `Tauri`、数字写阿拉伯字符

---

### 节 C：本地语音命令（v1：固定短语表 → 本机动作）

**范围**：仅做"短而无歧义"的固定关键词映射，**不接 LLM**。需要复杂自然语言意图时走
[voice-command-agent-design.md](voice-command-agent-design.md) 的远程通道。

**触发点**：桌面端 `speech/commands/remote.rs` 的 `Some("optimized")` 分支——拿到优化稿后，
**先**过命令表，命中则执行动作 + 跳过剪贴板/粘贴；未命中走原有链路。

**命令注册表（最小集）**：

| 命令 | 触发短语（归一化后精确匹配） | 动作 |
|---|---|---|
| `send_enter` | `发送` / `回车` / `确认发送` / `send` / `enter` | 向焦点窗口发一次 VK_RETURN |

**两种匹配位置**（解决"命令通常出现在句末"的情况）：

- **Whole**：整段就是命令（用户单独说"发送"——ASR 通常会因停顿单独成段）。
  执行命令、**不写剪贴板 / 不粘贴**。
- **Tail**：命令挂在正文末尾（用户连说"你好，发送"——ASR 优化稿是 `"你好，发送"`）。
  以分隔符（中英文标点 + 空格 + 全角空格 `，。、；：！？,.;:!? ⎵　`）划界：分隔符前的正文
  走原有剪贴板/粘贴链路；命令**仅在 `auto_paste` 模式下追加** —— 剪贴板模式下用户尚未
  Ctrl+V，提前回车会误提交空内容。
- **连写无分隔符**（如 `"我要发送邮件"`）**不算尾部命令** —— 避免误触发。

> 之后再扩：`new_line`（"换行" → Shift+Enter）、`backspace_one`（"删一个"）、
> `clear_input`（"清空"）、`screenshot`（"截图"——复用现有热键）。每条命令一行注册即可。

**归一化规则**（match 之前对优化稿应用）：
1. 全角 → 半角（句号 / 感叹号等）
2. trim 首尾空白
3. 去掉所有标点（`。！，,.!？?` 等）
4. 大写英文转小写
5. 长度 > 8 字符的优化稿直接跳过（命令短语都很短，长句不会是命令）

**安全护栏**：
- 前台窗口属于本进程时不执行（已有 `type_text_to_foreground` 同款护栏）
- 命中后写一条 INFO 日志：`[voice_cmd] matched={name} text={raw}`
- 在 `LlmSettings` 加 `voice_commands_enabled: bool`（默认 `true`，控制台后续加开关）

**为什么不接 LLM**：
- 这五个短语正确率 100% 字符串匹配，零延迟、零成本
- 任何 LLM 调用都会把"说错触发"的概率变高（同义词、口语化、上下文）
- 复杂意图→远程通道（zero agent），职责分明

**验收**：
1. 在记事本里说一段中文，看到优化稿入剪贴板；再说"发送"，记事本应**敲下一次回车**
   （不应出现"发送"这两个字）
2. 焦点在 zero-desktop 自己的窗口时说"发送"，不应有任何动作（护栏生效）
3. 说"我要发送邮件了"（>8 字符），不应触发命令（应正常进剪贴板）

---

## 3. 落地顺序与依赖

```
节 C（发送 → Enter）── 立刻做，独立可上线，零依赖
    │
    ▼
节 A（LLM 配置收拢）── 中等改动，跨 crate（toolkit-server + orchestrator）
    │
    ▼
节 B（中文优化加固）── 节 A 完成后，prompt 直接在控制台改即可，无需重编
```

节 C 不依赖任何前置改动，本次直接开做。节 A / B 单独排期。

---

## 4. 不在本期范围

- 风格档（口语 / 邮件 / 纪要 / 微信短句）—— 用户明确排除
- 语音命令带参数（"打开 X"、"念 Y"）—— 留给远程通道
- 命令的 GUI 注册 / 编辑 —— v1 用代码常量表
- 唤醒词门控 —— 与 [voice-command-agent-design.md](voice-command-agent-design.md) 合并到那边做

---

## 5. 影响面

| 文件 / 模块 | 节 A | 节 B | 节 C |
|---|---|---|---|
| `crates/toolkit-server/src/llm/mod.rs` | builtins 新增 2 条 | prompt 内置文本 | — |
| `crates/orchestrator/src/lib.rs` | 改读 `resolve_config` / `resolve_prompt` + fallback | 失败降级 | — |
| `crates/zero-desktop/src/modules/speech/paste_watch.rs` | — | — | 新增 `press_enter_to_foreground` |
| `crates/zero-desktop/src/modules/speech/voice_commands.rs`（新） | — | — | 短语表 + 匹配 |
| `crates/zero-desktop/src/modules/speech/commands/remote.rs` | — | — | optimized 分支前置命令分发 |
| `crates/zero-desktop/src/modules/speech/llm_settings.rs` | — | — | 新增 `voice_commands_enabled` |
