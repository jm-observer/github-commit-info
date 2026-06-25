# 截图热键卡死修复设计

> **症状**：按截图全局热键（默认 `Ctrl+Alt+A`）后，zero-desktop 主窗口看起来「卡死」——
> 鼠标键盘对窗口无任何反应；但同进程外的 ASR（独立后端/独立窗口）仍正常工作。重启 app
> 才能恢复。出现概率不稳定，连按热键、多屏切换、上一次截图未走完时尤其容易触发。
>
> **结论**：现象不是「主线程真挂死」，而是**透明置顶的截图叠加窗口残留在屏幕最上层，
> 拦截了所有鼠标/键盘事件，但又什么都不画**——用户看到的是「主窗罩着一层看不见的玻璃，
> 怎么点都不响应」。ASR 不在这层玻璃下面，所以不受影响。

参考既有模块：[zero-desktop-screenshot-design.md](../zero-desktop-screenshot-design.md)（§3.2 叠加窗口）。

## 1. 根因分析

涉及的关键代码点：

- [crates/zero-desktop/src/modules/screenshot/overlay.rs:33-49](../../crates/zero-desktop/src/modules/screenshot/overlay.rs:33)：
  `WebviewWindowBuilder` 建出的叠加窗 = `decorations=false + always_on_top + skip_taskbar
  + transparent + 铺满目标显示器物理矩形`。**只要这个窗口存在，无论它画不画东西，都会
  全屏拦截输入**。
- [crates/zero-desktop/ui/src/modules/screenshot/OverlayApp.tsx:171](../../crates/zero-desktop/ui/src/modules/screenshot/OverlayApp.tsx:171)：
  `if (!frame) return null;`——`useFrozenFrame()` 解析查询串失败 / asset 协议加载失败时，
  组件返回 `null` 什么都不绘制。窗口仍然在，只是变成「彻底透明、彻底空白」的输入黑洞。
- [crates/zero-desktop/src/modules/screenshot/overlay.rs:20-22](../../crates/zero-desktop/src/modules/screenshot/overlay.rs:20)：
  对已存在的同 label overlay 只做 `let _ = w.close()`（异步排队），随后立即
  `WebviewWindowBuilder::new(app, OVERLAY_LABEL, ...).build()`。旧窗未拆完就建同 label
  新窗，存在竞态——build 可能失败、可能产生异常态窗口。
- [crates/zero-desktop/src/modules/screenshot/mod.rs:52-57](../../crates/zero-desktop/src/modules/screenshot/mod.rs:52)：
  `do_capture` 失败仅 `log::warn!`，**无通知、无任何 UI 反馈**。用户感受 = 「按了没反应」。
- [crates/zero-desktop/src/modules/screenshot/commands.rs:40-57](../../crates/zero-desktop/src/modules/screenshot/commands.rs:40)：
  `do_capture` 全跑在 `run_on_main_thread`，包含 GDI 抓屏 + 同步 PNG 写盘 + webview 建窗，
  4K 多屏时单次主线程耗时可达数百 ms，加剧「卡顿感」。

可触发的故障路径（任一即满足）：

| # | 触发                                                     | 结果                                                |
|---|----------------------------------------------------------|-----------------------------------------------------|
| A | overlay 窗已建出，但 frame 查询串丢失 / 冻结帧文件被占用 / asset URL 加载失败 | 窗口存在但 `OverlayApp` 渲染 null → 全屏隐身玻璃        |
| B | 上一次 commit/cancel 之后 close 未真正销毁（Tauri close 异步） | 残留窗 + 再次按热键 → 同 label rebuild 失败，残留窗滞留 |
| C | 连按热键 / 热键反弹                                         | 多次 `do_capture` 叠加，竞态如上                       |
| D | `do_capture` 中段失败（监视器查询、抓屏、写盘任一）        | 静默 log，用户以为没触发再按 → 走入 A/B/C              |

**ASR 不受影响**：ASR daemon 是独立后端进程（GB10:9101 / 同进程 axum 路由），不依赖
zero-desktop 主窗 webview 消息循环，所以 UI「卡」时 ASR 照常响应。

## 2. 设计目标

- **任何失败路径下 overlay 都不能滞留**——「要么显形、要么消失」，绝不允许「透明 + 空白
  + 拦截输入」三件套同时成立。
- **可观测**：所有失败有用户感知（notification）+ 落盘 trace，方便下次复现定位。
- **重入安全**：连按热键、上次未走完 = 静默忽略或聚焦旧窗，不叠加建窗。
- 不引入跨平台行为差异，不改动现有 Rust ↔ React 协议（查询串/命令名/落盘路径）。

## 3. 方案

四道防线，**Rust 侧 ready-ack watchdog（防线 ③）是兜底闸门**——前端任何路径
（JS 没下载、React 没挂、`OverlayApp` 没渲染、asset 协议挂掉）都被它覆盖。前端
防线只在「JS 起来了但业务失败」的子集里生效，因此不能依赖前端单独避免卡死。

### 3.1 防线 ①：overlay 前端「失败即自取消」

`OverlayApp.tsx` 的 `frame` 解析阶段不能再返回 `null` 就完事。

- `useFrozenFrame()` 改为同步解析 + 给出 `error` 字段：查询串缺 `frame` 参数、几何全 0、
  解析异常，统统标错。
- `OverlayApp`：
  - 若 `frame.error`：**先**渲染带可见底色（`rgba(0,0,0,0.25)`）+ 居中错误文案 +
    「关闭(Esc)」按钮的兜底层；用户按 Esc / 点关闭 / 或一个**短延时（~2.5s）兜底**
    后才 `invoke("screenshot_cancel")`。不要在挂载瞬间就 cancel——窗会立刻关掉，
    错误层和按钮就成了死代码，用户也看不到原因（原因走 Rust notification 顶上）。
  - 给最外层 `div` 加一个**极淡可感知底色**（默认状态也可见，例如四角各放一个 8px
    小角标 / 顶部 1px 描边），让「窗在但什么都没画」的状态绝不可能成立。
- `<img>` `onError` → 同上路径（先显示错误层，再延时 cancel）。
- 全局 ErrorBoundary 包住 `OverlayApp`：渲染异常时降级成错误层 + 延时 cancel。

### 3.2 防线 ②：Rust 侧建窗失败 → 必须保证旧窗也清掉

`overlay.rs::open_overlay` 改造：

1. **同步等旧窗拆除**：用 `app.get_webview_window(OVERLAY_LABEL)` 检测到时，先调
   `close()`，然后**轮询等待**（最多 ~300ms，每 ~20ms 一次）直到 `get_webview_window`
   返回 `None`；**超时则放弃本次截图**——通知「上一个截图窗未释放，请稍后重试」+
   尝试 `close()` / 隐藏旧窗，**不复用旧窗**（旧窗本身可能就是故障态隐身玻璃，
   `set_focus` 反而锁定故障）。
2. **build 失败 / 后续 set_position/set_size/show 任一失败时，确保把可能已建出的窗
   close 掉**——`?` 直接抛是不够的，必须捕获 Err 后兜底 `close_overlay`。
3. 给 overlay 装 `WindowEvent::CloseRequested` / `Destroyed` 监听，把残留状态清干净
   （目前不需要状态机，但占位为后续重入加锁留口）。

### 3.3 防线 ③：ready-ack watchdog（兜底闸门）

**核心闸门，覆盖所有「JS/前端没起来」路径**。

- 新增 Tauri command `screenshot_overlay_ready(app)`：前端 `OverlayApp` 一旦
  挂载并完成首帧渲染（`useEffect` 里、`<img onLoad>` 之后），调用一次。
- Rust 侧 `open_overlay` 在 `win.show()` 之后启动一个**异步 watchdog**：
  - 注册 token（如 `Arc<AtomicBool>`）入模块级表，键 = overlay label 实例 id；
  - `tauri::async_runtime::spawn` 等待 **~2.5s**；
  - 超时未收到 ready ack（token 仍 false） → `close_overlay` + `notify_capture_failed`
    （「叠加窗未就绪，已自动关闭」）+ 写 trace。
- `screenshot_overlay_ready` 命令把对应 token 置 true，watchdog 看到即跳过关窗。
- `screenshot_commit` / `screenshot_cancel` 也置 token = true（正常路径 watchdog 不打扰）。

这一层兜住的故障：JS chunk 缺失 / asset 协议失败 / `index.html` 路径错 / React
根崩在 mount 之前 / preload 阶段死循环——所有「窗已 show 但前端永远跑不到 ack」
的情况。前端就绪也按这条路径报告，方便统一观测。

### 3.4 防线 ④：`do_capture` 入口重入锁 + 错误通知

`commands.rs::do_capture`：

- 模块级 `static CAPTURE_IN_PROGRESS: AtomicBool = AtomicBool::new(false)`，入口
  `compare_exchange(false, true)`；获取不到 → 直接早返，不报错（连按行为）。
- 出口（无论成功失败）一定 `store(false)`——RAII guard。注意 watchdog 是 spawn 出去
  的异步任务，guard 应在 watchdog 完成后再 reset，或干脆按「建窗成功 → reset；
  失败 → reset」两段，watchdog 自身的失败由其内部 reset。
- **失败路径加通知**：复用 `notify_done` 旁支造一个 `notify_capture_failed(app, &err)`，
  让用户看到「截图失败：<原因>」，不再「按了没反应」。

### 3.5 （可选 P2）抓屏移出主线程

把 GDI 抓屏 + 写盘从主线程拆走，**不是 commands.rs 单文件改动**，要联动调整调用链：

- 入口当前在 [mod.rs:53](../../crates/zero-desktop/src/modules/screenshot/mod.rs:53)：
  热键回调把整段 `do_capture` 塞进 `run_on_main_thread`；命令路径
  [commands.rs:66](../../crates/zero-desktop/src/modules/screenshot/commands.rs:66) 的
  `screenshot_capture` 也是同步 sig。两边都得改：热键回调改成 `spawn` 异步任务，
  命令改 `async fn` 直接 await；`do_capture` 内部拆成「`spawn_blocking` 抓屏+写盘
  → `run_on_main_thread` 建窗」两段。
- **需要先验证**的 Windows 约束：`monitor::monitor_at_cursor`（`GetCursorPos` +
  `MonitorFromPoint`）和 `capture::grab_rgba`（`GetDC(NULL)` + `BitBlt` 桌面 DC）
  在非主线程是否仍工作。GDI 桌面 DC 通常允许任意线程，但实测确认更稳。
- 收益：主线程峰值从「数百 ms」降到「建窗几十 ms」。
- **此项作为 P2 改进，不在第一版**——前 4 道防线已经覆盖卡死本身，主线程短卡顿是
  另一个体验问题，独立 PR 跟进。

### 3.6 防线 ⑤：诊断 trace

`<workspace>/screenshots/.capture-trace.log`：**改 append**（旧的 `.commit-trace.log`
覆盖式只看得到最后一行，本来就是个缺陷，这里不沿用），每行一个阶段。`do_capture`
入口先写一行 `--- session <uuid> ---` 标记一次会话，便于多次截图区分。

为防文件无限增长：超过 ~1MB 时旋转一次（重命名为 `.capture-trace.log.1`，覆盖
上一份旋转文件）。简单 size-based 轮转，不引依赖。

阶段：

```
[ts] session <uuid>
[ts] enter
[ts] monitor-ok    | x=.. y=.. w=.. h=..
[ts] capture-ok    | bytes=..
[ts] frame-saved   | <path>
[ts] overlay-built
[ts] overlay-shown
[ts] overlay-ready          ← 防线 ③ 收到 ack
[ts] overlay-ready-timeout  ← 或 watchdog 触发
```

任一阶段失败立即写 `*-fail | <msg>`。`.commit-trace.log` 同步改成 append（顺手修
既有缺陷）。

## 4. 改动清单

| 文件 | 改动 |
|---|---|
| `crates/zero-desktop/ui/src/modules/screenshot/hooks/useFrozenFrame.ts` | 新增 `error` 字段；查询串缺失/异常时填错。 |
| `crates/zero-desktop/ui/src/modules/screenshot/OverlayApp.tsx` | 失败兜底层（可见底色 + 文案 + 关闭按钮 + 短延时兜底 cancel，**不在挂载瞬间 cancel**）；最外层 div 添加可感知底色；`<img onError>` 走错误层；ErrorBoundary 包裹；挂载完成调用 `screenshot_overlay_ready`。 |
| `crates/zero-desktop/src/modules/screenshot/overlay.rs` | `open_overlay`：等待旧窗销毁后再建新窗，超时**放弃本次截图**并尝试关旧窗（不复用）；后续步骤失败必兜底 `close_overlay`；启动 ready-ack watchdog。 |
| `crates/zero-desktop/src/modules/screenshot/commands.rs` | `do_capture` 入口加重入锁；新增 `screenshot_overlay_ready` 命令；失败弹 notification；写 append 模式 `.capture-trace.log`；既有 `.commit-trace.log` 同步改 append + 体积旋转。 |
| `crates/zero-desktop/src/modules/screenshot/mod.rs` | 注册新命令 `screenshot_overlay_ready`。 |
| （P2 独立 PR）`mod.rs` + `commands.rs` | 抓屏 + 写盘移出主线程：热键回调改 spawn 异步，`screenshot_capture` 改 `async`，先在 Windows 验证 GDI 桌面 DC 跨线程 OK。 |

预估代码量：**前后端合计 < 200 行**（含 watchdog + trace 旋转），纯增量、不动协议。

## 5. 验收

复现路径（任一），改前必现 / 改后必不卡死：

1. 按住热键不放 / 1 秒内连按 5 次 → 主窗仍可响应；最多看到 1 个 overlay 或一条「截图失败」通知。
2. 手动把 `<workspace>/screenshots/.frozen-frame.png` 文件占用（PowerShell 开句柄），按热键
   → overlay 不再「隐身卡死」，要么显示错误提示并可 Esc 退出，要么直接 notification 报错不开窗。
3. 走 commit 流程一次后立刻再按热键 → 不出现 build 冲突 / 残留窗。
4. 副屏 / 多 DPI 切换鼠标到不同屏按热键 → 正常。

辅助验收（专门验 watchdog）：

5. **临时把 `overlay.html` 改成空文件**（或在 OverlayApp 顶层 `throw new Error("test")`）
   → 按热键 → ~2.5s 内自动关窗 + notification「叠加窗未就绪」+ `.capture-trace.log`
   末尾有 `overlay-ready-timeout`。
6. 连续 5 次截图，看 `.capture-trace.log` 是否累计 5 段完整 trace（append 验证）。

辅助：截图目录下 `.capture-trace.log` + `.commit-trace.log` 两条 trace（均 append）
应覆盖完整链路。

## 6. 不做的事

- 不重做截图模块、不改交互/快捷键/合成协议。
- 不把 overlay 改成「Rust 原生窗」——`tauri-plugin-global-shortcut` + WebviewWindow 的
  组合本身没问题，问题在错误处理。
- ready-ack watchdog 仅在「show() 后未就绪」窗口期生效，**不做长期 watchdog 线程
  定时巡检残留窗**——前 4 道防线已封死所有已知卡死路径。
