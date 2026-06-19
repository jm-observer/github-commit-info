# zero-desktop 截图标注模块设计

> 把「按 PrintScreen 抓全屏 → 裁剪 → 画圈」这一整套手动动作内置进 zero-desktop，做成一个
> 类 Snipaste / 微信截图的**截图标注工具**：全局热键唤起 → 框选 → 标注（椭圆/箭头/矩形）→
> 同时复制到剪贴板并落盘。核心诉求是**抓屏瞬间冻结成一帧静态图**，用户在静态图上裁剪画圈，
> 真实鼠标怎么动都不影响成图——这就解决了「截图时鼠标不能乱动」的痛点。
> （把鼠标光标也画进图里是**可选增强**，实现成本高就放弃，不影响上述核心。）

## 1. 定位与范式

- **形态对齐既有模块**：新增 `screenshot` 模块，沿用本仓「Rust 模块（`src/modules/X`）+ React
  页面（`ui/src/modules/X`）」成对约定；命令注册进 `main.rs` 的 `generate_handler!`，权限补进
  `capabilities/default.json`。
- **职责切分**：Rust 只做平台相关的脏活——①原生抓屏（光标默认不含，可选叠加见 §2）、全局热键、叠加窗口管理、④输出
  （剪贴板 + 落盘）；React 做②框选 + ③canvas 标注 + 合成 PNG。**标注合成放前端**（canvas 直接
  `toBlob`），Rust 不碰图像处理，最省事也最稳。
- **平台**：仅 Windows（与本仓既有 net-policy / speech 一致）。**仅平台相关实现**（`capture.rs` /
  `monitor.rs` 的 GDI 代码）用 `cfg(windows)` 圈起；命令函数本身始终注册，非 Windows 下保留
  `cfg(not(windows))` 的 stub 返回「不支持」错误——这样命令在所有平台都能进 `generate_handler!`、
  非 Windows 也能正常编译，只是调用即报不支持。

## 2. 端到端流程

```
按全局热键（默认 Ctrl+Alt+A）
  → Rust: 定位鼠标所在显示器 → BitBlt 抓该屏全屏位图
          → （可选）GetCursorInfo + DrawIconEx 把光标叠进位图
          → 编码成一帧 PNG（临时文件 / 内存）= 「冻结帧」
  → 弹该屏的无边框 / 置顶 / 铺满 / 透明背景叠加窗口，把冻结帧当背景铺满
  → 用户拖框选区域（半透明遮罩 + 选框 + 尺寸提示）
  → 在选区内画 椭圆 / 箭头 / 矩形（颜色·粗细可调，可撤销）
  → 确认（双击左键 / Enter / 按钮）→ 前端把 选区裁剪 + 标注图层 合成最终 PNG
  → 回传 Rust → 同时：① 写入系统剪贴板  ② 落盘 workspace/screenshots/yyyyMMdd-HHmmss.png
  → 关闭叠加窗口；Esc 任意阶段取消
```

**为什么这样解决「鼠标不能乱动」**：抓到的是触发瞬间的一帧**静态图**，用户之后在这张冻结图上
裁剪画圈，真实光标随便移动都不影响成图——冻结帧本身就是痛点的根因解法，**与光标是否画进图无关**。

**光标进图（可选增强）**：标准 Windows 截屏（`BitBlt` / `Windows.Graphics.Capture`）默认不含光标，
若要画进去需 `GetCursorInfo` 取位置+句柄、`DrawIconEx` 叠到位图。**这是 nice-to-have**：实现/DPI/
自定义光标换算成本偏高时直接放弃，不阻塞核心闭环（见 §8）。

### 便捷交互（细节上车实现时再定，先记目标）

- **双击鼠标左键 = 确认/完成**（等价于 Enter / 工具栏「完成」按钮）。
- `Esc` = 任意阶段取消关窗。
- 框选完成后可继续拖拽八向手柄微调选区，不必重选。
- 其它顺手操作（如 `Ctrl+Z` 撤销、滚轮调粗细、单击选区外重选）实现时按手感增补。

## 3. 架构与文件布局

### 3.1 Rust 侧 `crates/zero-desktop/src/modules/screenshot/`

```
screenshot/
  mod.rs                  // 模块入口：注册全局热键、setup hook
  capture.rs              // [cfg(windows)] GDI 抓屏（+ 可选光标叠加）→ PNG bytes
  overlay.rs              // 叠加窗口的创建/定位/销毁（WebviewWindowBuilder）
  output.rs               // 写剪贴板 + 落盘 workspace/screenshots/
  commands.rs             // #[tauri::command] 暴露给前端
  monitor.rs              // [cfg(windows)] 枚举显示器 / 定位鼠标所在屏 + DPI
```

关键 Win32 API（均走已在依赖里的 `windows-sys`）：

| 用途 | API | 需补的 feature |
|---|---|---|
| 抓屏位图 | `GetDC` / `CreateCompatibleDC` / `CreateCompatibleBitmap` / `BitBlt` → `GetDIBits`（或改用 `CreateDIBSection` 直接拿像素指针） | `Win32_Graphics_Gdi` |
| 叠加光标（**可选**） | `GetCursorInfo` / `CopyIcon` / `GetIconInfo` / `DrawIconEx` | `Win32_UI_WindowsAndMessaging`（已开）/ `Win32_Graphics_Gdi` |
| 定位显示器 | `GetCursorPos` / `MonitorFromPoint` / `GetMonitorInfoW` | `Win32_Graphics_Gdi` |
| DPI | `GetDpiForMonitor` / `SetProcessDpiAwareness`（或 manifest） | `Win32_UI_HiDpi` |

> `BitBlt` 只把屏幕拷进一个 GDI 位图**句柄**，拿不到像素 bytes；要得到可编码的 32-bit BGRA
> 像素数据，需在 `BitBlt` 后用 `GetDIBits`（传 `BITMAPINFOHEADER`，`biBitCount=32` /
> `biCompression=BI_RGB`，`biHeight` 取负得 top-down）把位图读进缓冲；或一开始就用
> `CreateDIBSection` 创建带像素指针的 DIB，`BitBlt` 直接写入、省一次拷贝。拿到 BGRA 后翻成
> RGBA，再用轻量 PNG 编码（倾向只引 `png` crate，`image` 较重）。

### 3.2 React 侧 `crates/zero-desktop/ui/src/modules/screenshot/`

```
screenshot/
  OverlayApp.tsx          // 叠加窗口根组件（独立窗口 entry，非主窗口 tab）
  components/
    SelectionLayer.tsx    // 框选交互（拖拽出选区 + 八向 resize + 尺寸标签）
    AnnotateCanvas.tsx    // canvas 标注图层（椭圆/箭头/矩形绘制 + 撤销栈）
    Toolbar.tsx           // 工具栏：工具切换 / 颜色 / 粗细 / 确认 / 取消
  hooks/
    useFrozenFrame.ts     // 拿冻结帧背景 + 屏幕逻辑尺寸
  compose.ts              // 选区裁剪 + 标注层合成 → PNG Blob
```

**叠加窗口是独立 webview**（不是主窗口里的页面）：用 `WebviewWindowBuilder` 新建，加载
`overlay.html` 入口（Vite 多入口），传入冻结帧与屏幕几何。主窗口保持原样。

## 4. 输出契约

- **落盘**：`<workspace>/screenshots/<yyyyMMdd-HHmmss>.png`（workspace = `app_state` 已解析的根，
  默认 `%LOCALAPPDATA%/zero-desktop`）。目录不存在则创建。
- **剪贴板**：写 PNG，复用 `tauri-plugin-clipboard-manager`（`write_image`）。
- **两者同时写**：确认后 Rust 收到 PNG bytes → 先落盘拿到路径 → 再写剪贴板 → 发
  `notification` 提示「已截图（已复制 + 已保存到 …）」。任一失败不阻断另一个，错误进日志 + 通知。

## 5. 依赖与配置改动

| 改动 | 文件 | 说明 |
|---|---|---|
| 新增插件 | `Cargo.toml` | `tauri-plugin-global-shortcut = "2"` |
| 补 GDI feature | `Cargo.toml` | `windows-sys` 加 `Win32_Graphics_Gdi` /（如需）`Win32_UI_HiDpi` |
| PNG 编码 | `Cargo.toml` | `png = "0.17"`（或权衡用 `image`） |
| 注册插件 | `main.rs` | `.plugin(tauri_plugin_global_shortcut::Builder::new()...build())` |
| 注册模块 | `src/modules/mod.rs` | `pub mod screenshot;` |
| 注册命令 | `main.rs` `generate_handler!` | 见 §6 |
| 热键注册 | `main.rs` setup | 启动时注册 `Ctrl+Alt+A` → 触发抓屏 |
| 权限 | `capabilities/default.json` | global-shortcut、新窗口、fs 写 screenshots 目录、clipboard write-image |
| Vite 多入口 | `ui/vite.config.*` | 增加 `overlay.html` 入口 |

## 6. Tauri 命令清单（前后端接口）

| 命令 | 方向 | 职责 |
|---|---|---|
| `screenshot_capture` | 热键/UI → Rust | 抓鼠标所在屏（P1 不含光标，光标叠加为 P2 可选）→ 建叠加窗 → 把冻结帧+几何传给叠加窗 |
| `screenshot_commit` | 叠加窗 → Rust | 收最终 PNG bytes → 落盘 + 写剪贴板 → 关窗 + 通知 |
| `screenshot_cancel` | 叠加窗 → Rust | 关闭叠加窗，丢弃 |
| `screenshot_get_settings` | UI → Rust | 取热键 / 默认颜色 / 默认粗细 / 保存目录（P2） |
| `screenshot_save_settings` | UI → Rust | 存设置（P2，走 tauri-plugin-store） |

> 冻结帧传递：小图可 base64 走命令返回；大屏（4K）base64 偏大，倾向写临时 PNG 后用
> `protocol-asset`（已启用）让叠加窗 `<img src=asset://…>` 加载，省内存。

## 7. 分阶段交付

- **P1 — 最小可用闭环**
  抓屏（单显示器=鼠标所在屏，**不含光标**）→ 叠加窗 → 框选 → 画椭圆（单色单粗细）→ 确认
  （双击左键 / Enter）→ 剪贴板 + 落盘双输出 → Esc 取消。热键写死 `Ctrl+Alt+A`。
  光标进图放 P2 视成本决定做不做。
- **P2 — 工具完善**
  箭头 / 矩形；工具栏（颜色 / 粗细）；撤销栈；热键与默认值可配（store）；DPI 缩放打磨；
  确认/取消快捷键。
- **P3 — 增强**
  固定贴图（Snipaste 式钉屏置顶）；多显示器拼屏框选；OCR / 加文字 / 马赛克（按需）。

## 8. 风险与待打磨点

- **DPI 缩放**：高 DPI 下抓屏像素尺寸 ≠ 窗口逻辑尺寸，叠加窗背景对齐与选区坐标换算需按
  monitor 缩放因子处理；建议进程级 DPI-aware（manifest 或 `SetProcessDpiAwareness`），P1 先在
  100% 缩放验证，P2 补全。
- **多屏几何**：P1 限定鼠标所在屏，叠加窗精确定位到该 monitor 的物理矩形（含负坐标的副屏）。
- **热键冲突**：`Ctrl+Alt+A` 与部分输入法/QQ 截图冲突，故 P2 做成可配；注册失败要给明确提示。
- **光标进图（可选）**：`DrawIconEx` 对部分自定义/动画光标可能退化为默认箭头，且高 DPI 下还要
  做光标坐标/尺寸换算。**成本偏高，作为 P2 可选项**——做不顺直接放弃，核心闭环不依赖它。
- **截屏权限**：Windows 普通桌面无需特殊授权；UAC 提权窗口（安全桌面）无法抓，属系统限制。

## 9. 验收要点

- 触发热键后画面定格为按键瞬间的一帧，按键后移动鼠标不改变成图。
- 双击左键 / Enter 可确认完成。
- 框选 + 画圈后确认，剪贴板可在微信/文档直接 `Ctrl+V` 粘出，且
  `<workspace>/screenshots/` 下有对应 PNG。
- Esc 在框选/标注任意阶段都能干净取消、关窗、无残留窗口。
- 100% 与 150% DPI 下选区与成图无明显错位（P2 目标）。
