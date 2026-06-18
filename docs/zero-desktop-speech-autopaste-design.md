# zero-desktop 语音识别「自动粘贴到焦点」设计

## 0. 定位

zero-desktop 的「StreamSpeech」远程语音识别模块,原本只能把识别结果**写进剪贴板**:
用户在别处的输入框(微信、记事本、浏览器…)等结果出来,再手动 `Ctrl+V` 粘进去。

本设计在此之上增加**自动粘贴**:识别结果一出来,直接「打字」进当前光标所在的输入框,
省掉手动那一下 `Ctrl+V`。

设计的主轴是一个看似简单、实则有坑的问题:

> 一个后台应用,如何把一段文本**安全地**送进「另一个应用的输入框」,既不抢用户的剪贴板,
> 也不破坏「用户当时没聚焦输入框、回头再粘」这个既有习惯?

平台前提:本功能为 **Windows 专属**(依赖 `SendInput`)。非 Windows 编译为 no-op,
zero-desktop 本来就只面向 Windows。

---

## 1. 现状与约束

### 1.1 既有链路(自动复制)

识别走 WebSocket 流式管线,orchestrator 逐段下发 `segment / optimized / translated / secondary`
消息。`optimized`(中文优化稿)与 `translated`(英文翻译)两个分支里,若开了「自动复制」,
会把**时间上相邻的多段拼接成整句**写进剪贴板:

- 拼接器 `AutoCopyAccum` + `next_clipboard_text`:相邻句子音频间隔在 `merge_window_ms` 内则
  续接,并用 `join_dedup` 去除重叠前缀。
- 全局 `Ctrl+V` 观察器(`paste_watch.rs`,`WH_KEYBOARD_LL` 只观察不拦截):用户每段即时粘贴时,
  下次写剪贴板前清空累加器,避免「下一段带上已粘过的前一段」造成重复粘贴。

代码位置:`crates/zero-desktop/src/modules/speech/commands/remote.rs`。

### 1.2 两条必须守住的约束

1. **不抢剪贴板**。剪贴板是用户的公共资源,自动粘贴不应把它清掉/占用。
2. **保留「回头再粘」**。用户识别时光标可能不在任何输入框;结果先进剪贴板,等他回头点进
   输入框再 `Ctrl+V` 拿到**完整全文**——这个能力今天就有,不能因为加了自动粘贴而被破坏。

---

## 2. 关键决策

### 2.1 为什么用「直接打字」而不是「模拟 Ctrl+V」

两条候选路线:

| | 路线 A:写剪贴板 + 模拟 `Ctrl+V` | 路线 B:`SendInput` 直接逐字「打字」 |
|---|---|---|
| 手段 | 改剪贴板内容 → 合成 `Ctrl↓V↓V↑Ctrl↑` | `KEYEVENTF_UNICODE` 逐 UTF-16 码元注入 |
| 剪贴板 | **会被占用/覆盖** | **完全不碰** |
| 兼容性 | 少数应用 `Ctrl+V` 非粘贴(终端等) | 等同键盘输入,兼容面更广 |
| 长文本 | 瞬间贴入 | 略慢(逐字符) |

**选路线 B。** 决定性理由是约束 1.2:

> 自动复制写的是**完整拼接文本**,自动粘贴要的是**逐段增量**。
> 若用路线 A,每段模拟一次 `Ctrl+V`,剪贴板里的整句会被重复贴 N 遍;要避免就得把剪贴板改成
> 只放增量,那「回头 `Ctrl+V` 拿完整全文」又没了。

路线 B 让两条路**彻底解耦**:打字走增量、不消耗剪贴板;剪贴板独立累加完整文本供兜底。
两个需求同时满足。

### 2.2 焦点保护:绝不打进自己窗口(⚠️ 待定)

> **状态:待定。** 下述实现已写进代码,但「是否需要这道守卫、以什么粒度」尚未拍板,
> 保留待评审。路线 B、保留写剪贴板这两条已定,不受本节结论影响。

出发点:自动粘贴只在 zero-desktop **自己不在前台**时才有意义,否则会把识别结果打进自己的界面。

当前实现:`GetForegroundWindow()` → `GetWindowThreadProcessId()` 取前台窗口所属进程,
与 `GetCurrentProcessId()` 比较;相等则**不输入**,直接返回(把内容留给剪贴板兜底)。

待定点:

- 这道守卫只能判断「前台是否属于本进程」,**无法**识别外部窗口里焦点控件是否可编辑
  (见 §4 边界、§7 演进)——它解决的只是「别打进自己 UI」,不是「确保打到了输入框」。
- 取舍:留着是几乎零成本的保险(用户口述时本就聚焦在别处,极少触发);去掉则更简单,但
  zero-desktop 自己在前台时文本会被注入到本应用当前焦点控件,可能产生意外输入。
- **倾向保留**,但等确认。

### 2.3 「再次优化」与「覆盖」:明确放弃覆盖

讨论中考虑过一个问题:同一段落若被**二次优化**(结果可能变化),已经打进输入框的旧文本要不要
覆盖?

结论:**不覆盖**。原因:

- 一旦文本进了别的应用的输入框,我们就**既读不到、也无法可靠定位**它——覆盖只能靠盲发
  `Backspace × N` 删掉再重打,而这要求「用户没移动光标、没接着输入、没有 IME/自动补全改写」,
  缺一个就会误删用户的输入。
- 产品侧确认:前后两次优化的间隔在程序里设得**非常短**,用户不可能在那个窗口里插手;且用户
  习惯后都是等终态再编辑。覆盖的收益极低、风险高。

因此采用**同段去重**:每个段落 id **只自动输入一次**;二次优化只更新剪贴板,不重复打字、也不
覆盖。这同时消除了「同段重复插入」这个真正的 bug 风险。

### 2.4 分隔策略

逐段打字时段与段之间是否补分隔:

- **中文优化模式**:不补空格(中文句间不用空格,且优化稿通常自带句末标点)。
- **英文翻译模式**:续接段(与上一段音频间隔在 `merge_window_ms` 内)前补一个空格,
  使 `Sentence one. Sentence two.` 读起来正常。
- 一段链条的**首段**不补前导分隔。

「是否续接」复用与自动复制相同的 `merge_window_ms` 阈值。

---

## 3. 实现

### 3.1 后端打字原语(`paste_watch.rs`)

新增 `type_text_to_foreground(text) -> bool`:

```
GetForegroundWindow → 非空?
  ↓ 是
GetWindowThreadProcessId 取 pid == GetCurrentProcessId()?
  ↓ 否(前台是别的应用)
对 text.encode_utf16() 每个码元发「按下+抬起」两个 KEYEVENTF_UNICODE 事件
  ↓
SendInput 一次性投递,返回是否全部送达
```

- `wVk = 0` + `KEYEVENTF_UNICODE` 表示字符注入而非虚拟键,`wScan` 携带 UTF-16 码元;代理对
  (emoji 等)按序逐码元发送即可。
- 与 `paste_watch` 自身的 `Ctrl+V` 观察互不干扰:注入的 unicode 事件 `vkCode` 不是 `VK_V`,
  不会误触发粘贴信号。
- 非 Windows 为 no-op。

### 3.2 接入识别流(`remote.rs`)

在 reader 任务内新增与 `copy_acc` 并列的 `AutoPasteState`:

```rust
struct AutoPasteState {
    typed_ids: HashSet<i64>,   // 同段去重:每个 ref 只输入一次
    last_t_end: Option<f64>,   // 上段结束时刻,判定续接(英文是否补空格)
}
```

`optimized` / `translated` 两分支,在写剪贴板之后追加 `auto_paste_segment(...)`:

- 触发条件:`auto_paste == true` **且** `auto_copy_mode` 选中了对应内容
  (`OptimizedZh` → optimized 分支;`English` → translated 分支)。
- `auto_paste_segment` 内:同段去重 → 续接判定 → 拼分隔 → 调 `type_text_to_foreground`。

注意:打字用的是**当段文本 `text`**,不是自动复制那份 `merged`。

### 3.3 配置与持久化

复用 `LlmSettings` 这条既有设置链路,新增一个布尔字段:

- `LlmSettings.auto_paste: bool`(默认 `false`)。
- DTO `CombinedSettings` 同步新增字段。
- SQLite 持久化 key:`llm.auto_paste`(`"1"/"0"`),与 `get/apply/load_llm_settings_from_db`
  对齐。

前端:

- `AppSettings.auto_paste: boolean`(`tauri-client.ts`)。
- `ControlPanel` 新增开关「自动粘贴到焦点」,放在「自动复制 / 合并间隔」一组之后。
- `SpeechPage` 加状态 + 加载 + `handleAutoPasteChange`(改动即 `applySettings` 落库)。

**联动**:开关在「自动复制 = 关闭」时**禁用**——自动粘贴贴的就是自动复制选中的那份内容
(中文优化 / 英文翻译),没选内容就无从粘起。

---

## 4. 行为矩阵

| 录音时前台窗口 | 自动粘贴开关 | 结果 |
|---|---|---|
| 外部应用,焦点在可编辑控件 | 开 | 逐段实时打进光标处;剪贴板仍存完整文本 |
| 外部应用,焦点不可编辑 | 开 | 仍会 `SendInput` 注入,但按键被控件丢弃**或可能触发其快捷键**(见下方边界) |
| zero-desktop 自己在前台 | 开 | **不注入**(进程相同);剪贴板存完整文本,回头 `Ctrl+V` 拿全文 |
| 任意 | 关 | 行为同既有:仅写剪贴板 |

> 焦点判断只到「前台窗口是否属于本进程」这一层(见 §2.2 / §7):**无法识别外部窗口里焦点
> 控件是否可编辑**。因此「不注入」仅在前台是 zero-desktop 自己时成立;前台是别的应用时一律
> 注入,落点是否生效取决于那个应用。

**已知边界(不特殊处理)**:

- **注入落点不可控**:前台是非输入类的外部窗口时,逐字 `SendInput` 仍会发出,可能什么都不
  发生,也**可能被识别为快捷键**误触发该应用功能。要规避就需探测目标控件可编辑性(§7),
  当前靠用户「需要时才开开关」来约束使用场景。
- **前半程未聚焦 + 后半程才聚焦**:前半程的段落没被打进输入框(只进了剪贴板),后半程的段落
  已自动打入。此时**不能**靠 `Ctrl+V` 只补前面——剪贴板里是**完整全文**,粘上去会把已打入的
  后半程内容重复一遍。实际补救只能是手动清掉后再 `Ctrl+V` 全文,或接受输入框里只有后半程。
  特殊处理它就要回到「盲插旧文本」的麻烦,收益不抵复杂度。

---

## 5. 受影响文件

后端:
- `crates/zero-desktop/Cargo.toml`(`windows-sys` 增 `Win32_System_Threading`)
- `crates/zero-desktop/src/modules/speech/paste_watch.rs`(`type_text_to_foreground` + `win::type_text`)
- `crates/zero-desktop/src/modules/speech/commands/remote.rs`(`AutoPasteState` / `auto_paste_segment` + 两分支接入)
- `crates/zero-desktop/src/modules/speech/llm_settings.rs`(`auto_paste` 字段)
- `crates/zero-desktop/src/modules/speech/settings.rs`(DTO / get / apply / load 持久化 `llm.auto_paste`)

前端:
- `crates/zero-desktop/ui/src/modules/speech/api/tauri-client.ts`(`AppSettings.auto_paste`)
- `crates/zero-desktop/ui/src/modules/speech/components/ControlPanel.tsx`(开关 UI)
- `crates/zero-desktop/ui/src/modules/speech/SpeechPage.tsx`(状态 / 加载 / 持久化)

---

## 6. 验收

- 自动化:`cargo check -p zero-desktop`、`npm run build`(含 `tsc --noEmit`)、
  `cargo test -p zero-desktop --bin zero-desktop remote`(9 个既有用例不回归)均通过。
- 真机(需 Windows + 桌面端启动,无法走浏览器预览):
  1. 「自动复制」选中文优化或英文翻译 → 打开「自动粘贴到焦点」。
  2. 光标放到记事本 / 微信输入框,说一句话 → 文本应自动出现。
  3. 切回 zero-desktop 自己窗口再说一句 → 不应打进界面;切到别处 `Ctrl+V` 应拿到完整文本。

---

## 7. 后续可演进(暂不做)

- **二次优化的「定稿后再贴」**:若上游协议未来能标记 `is_final`,可改为「只在终态触发自动
  粘贴」,既避免覆盖也拿到最终结果。当前因间隔极短、覆盖风险高而采用「首段即贴、不覆盖」。
- **快捷键 / 热字串触发**:目前为「识别即贴」,未来可加「按下某键才把缓冲贴出」的半自动模式。
- **目标可编辑性探测**:当前仅判断「前台是否本进程」,无法识别前台窗口里焦点控件是否可编辑;
  靠用户自行在需要时开启开关来规避。

---

## 8. 附:启动时自动识别开关

与自动粘贴一同加入的一个独立小开关,**与自动粘贴无耦合**,只是共用同一条设置链路。

- **行为**:打开后,语音页就绪(`getSettings` 加载完 + `store.isInitialized` + 设备列表非空)
  且当前未在录音时,**自动调用 `startRecording`**,省去手动点「开始」。靠 `autoStartFiredRef`
  保证一次会话只触发一次;若进程已在录音则跳过。
- **配置**:`LlmSettings.auto_start`(默认 `false`)→ SQLite key `ui.auto_start`;DTO /
  get / apply / load 与既有字段对齐。
- **UI**:`ControlPanel` 底部开关组「启动时自动识别」,不随「自动复制」联动禁用。
- **受影响文件**:后端 `llm_settings.rs` / `settings.rs`;前端 `tauri-client.ts`(`AppSettings.auto_start`)、
  `ControlPanel.tsx`(开关)、`SpeechPage.tsx`(状态 / 加载 / `handleAutoStartChange` / 自动启动 effect)。
