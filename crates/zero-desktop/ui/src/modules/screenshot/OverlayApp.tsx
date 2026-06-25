import { Component, ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Dir, Rect, Shape, Tool } from "./types";
import { composePng } from "./compose";
import { useFrozenFrame } from "./hooks/useFrozenFrame";
import SelectionLayer from "./components/SelectionLayer";
import AnnotateCanvas from "./components/AnnotateCanvas";
import Toolbar from "./components/Toolbar";

type Phase = "selecting" | "annotating";

type Drag =
  | { kind: "create"; ox: number; oy: number }
  | { kind: "resize"; dir: Dir; start: Rect; ox: number; oy: number }
  | { kind: "draw" }
  | null;

/** 错误层兜底关窗延时：用户看到原因后才自动 cancel；期间允许 Esc / 点关闭。 */
const ERROR_AUTO_CLOSE_MS = 2500;

/** 顶部 1px 描边：让「窗在但什么都没画」的状态不可能成立——一旦看到这条线，
 *  就知道叠加窗正活着。透明设计本来就靠这种细节维持可感知性。 */
const OVERLAY_VISIBILITY_BORDER = "1px solid rgba(255,255,255,0.18)";

/**
 * 截图叠加窗根组件（设计文档 §3.2）：
 * 冻结帧铺底 → 拖框选区 → 选区内画椭圆/箭头/矩形 → 双击/Enter/完成 提交，Esc 取消。
 *
 * 卡死修复（docs/2026-06-24-screenshot-freeze-fix/design.md §3.1）：
 * - frame 解析失败 / 图片加载失败 / 渲染异常都走 ErrorOverlay，不再返回 null。
 * - 挂载完成后调用 `screenshot_overlay_ready`，告诉 Rust watchdog「前端活着」。
 */
function OverlayInner() {
  const frame = useFrozenFrame();
  const imgRef = useRef<HTMLImageElement>(null);

  const [phase, setPhase] = useState<Phase>("selecting");
  const [sel, setSel] = useState<Rect | null>(null);
  const [shapes, setShapes] = useState<Shape[]>([]);
  const [draft, setDraft] = useState<Shape | null>(null);

  const [tool, setTool] = useState<Tool>("ellipse");
  const [color, setColor] = useState("#FF3B30");
  const [width, setWidth] = useState(4);

  const [imgError, setImgError] = useState<string | null>(null);

  // 拖拽状态与最新选区/草稿用 ref 读取，避免 window 事件闭包读到旧值。
  const dragRef = useRef<Drag>(null);
  const selRef = useRef<Rect | null>(null);
  const draftRef = useRef<Shape | null>(null);
  const toolRef = useRef(tool);
  const colorRef = useRef(color);
  const widthRef = useRef(width);
  useEffect(() => void (selRef.current = sel), [sel]);
  useEffect(() => void (draftRef.current = draft), [draft]);
  useEffect(() => void (toolRef.current = tool), [tool]);
  useEffect(() => void (colorRef.current = color), [color]);
  useEffect(() => void (widthRef.current = width), [width]);

  // 载入默认颜色/线宽（失败用内置默认）。
  useEffect(() => {
    invoke<{ color: string; line_width: number }>("screenshot_get_settings")
      .then((s) => {
        if (s?.color) setColor(s.color);
        if (s?.line_width) setWidth(s.line_width);
      })
      .catch(() => undefined);
  }, []);

  const cancel = useCallback(() => {
    void invoke("screenshot_cancel").catch(() => undefined);
  }, []);

  const confirm = useCallback(() => {
    const img = imgRef.current;
    const s = selRef.current;
    if (!img || !s || s.w < 2 || s.h < 2) {
      cancel();
      return;
    }
    composePng(img, s, shapes, window.innerWidth, window.innerHeight)
      .then((b64) => invoke("screenshot_commit", { pngBase64: b64 }))
      .catch((e) => {
        // overlay 的 devtools 一般看不到 → 也用 alert 直接弹给用户，便于诊断"按完成无反应"。
        console.error("[overlay] commit failed", e);
        try { alert(`截图提交失败：${String(e)}`); } catch {}
        cancel();
      });
  }, [shapes, cancel]);

  // 键盘：Esc 取消 / Enter 完成 / Ctrl+Z 撤销。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        cancel();
      } else if (e.key === "Enter") {
        e.preventDefault();
        confirm();
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
        e.preventDefault();
        setShapes((arr) => arr.slice(0, -1));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [confirm, cancel]);

  // 全局 move/up：根据 dragRef 更新选区/草稿。仅挂一次。
  useEffect(() => {
    const onMove = (ev: MouseEvent) => {
      const d = dragRef.current;
      if (!d) return;
      const x = clamp(ev.clientX, 0, window.innerWidth);
      const y = clamp(ev.clientY, 0, window.innerHeight);
      if (d.kind === "create") {
        setSel(normRect(d.ox, d.oy, x, y));
      } else if (d.kind === "resize") {
        setSel(resizeRect(d.start, d.dir, x - d.ox, y - d.oy));
      } else if (d.kind === "draw") {
        const cur = draftRef.current;
        if (cur) setDraft({ ...cur, x2: x, y2: y });
      }
    };
    const onUp = () => {
      const d = dragRef.current;
      if (!d) return;
      dragRef.current = null;
      if (d.kind === "create") {
        const s = selRef.current;
        if (s && s.w >= 4 && s.h >= 4) setPhase("annotating");
      } else if (d.kind === "draw") {
        const cur = draftRef.current;
        if (cur && (Math.abs(cur.x2 - cur.x1) > 2 || Math.abs(cur.y2 - cur.y1) > 2)) {
          setShapes((arr) => [...arr, cur]);
        }
        setDraft(null);
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, []);

  const onRootDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    const x = e.clientX;
    const y = e.clientY;
    const s = selRef.current;
    if (phase === "annotating" && s && inside(s, x, y)) {
      // 选区内 → 开画。
      const shape: Shape = {
        tool: toolRef.current,
        x1: x,
        y1: y,
        x2: x,
        y2: y,
        color: colorRef.current,
        width: widthRef.current,
      };
      dragRef.current = { kind: "draw" };
      setDraft(shape);
    } else {
      // 选区外 / 选择阶段 → 重新框选（清空已有标注）。
      if (phase === "annotating") setShapes([]);
      setPhase("selecting");
      setDraft(null);
      dragRef.current = { kind: "create", ox: x, oy: y };
      setSel({ x, y, w: 0, h: 0 });
    }
  };

  const onHandleDown = (dir: Dir, e: React.MouseEvent) => {
    e.stopPropagation();
    const s = selRef.current;
    if (!s) return;
    dragRef.current = { kind: "resize", dir, start: s, ox: e.clientX, oy: e.clientY };
  };

  // frame 解析未完成（首次 effect 前） → 显占位错误层，避免「窗在但空白」。
  if (!frame) {
    return <ErrorOverlay message="叠加窗加载中…" autoCloseMs={null} onClose={cancel} />;
  }

  // 解析失败：先显示错误层，让用户看到原因；用户按 Esc / 点关闭、或短延时后才 cancel。
  if (frame.error) {
    return <ErrorOverlay message={`截图叠加窗参数异常：${frame.error}`} autoCloseMs={ERROR_AUTO_CLOSE_MS} onClose={cancel} />;
  }
  if (imgError) {
    return <ErrorOverlay message={`冻结帧加载失败：${imgError}`} autoCloseMs={ERROR_AUTO_CLOSE_MS} onClose={cancel} />;
  }

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        overflow: "hidden",
        cursor: "crosshair",
        borderTop: OVERLAY_VISIBILITY_BORDER,
      }}
      onMouseDown={onRootDown}
      onDoubleClick={(e) => {
        e.preventDefault();
        confirm();
      }}
    >
      <img
        ref={imgRef}
        src={frame.src}
        crossOrigin="anonymous"
        draggable={false}
        alt="frozen"
        onLoad={() => {
          // 首帧渲染完成 → 通知 Rust watchdog 别误关。失败静默（watchdog 超时自有兜底）。
          void invoke("screenshot_overlay_ready", { sessionId: frame.sessionId }).catch(() => undefined);
        }}
        onError={() => setImgError("asset 协议无法加载冻结帧 PNG")}
        style={{
          position: "absolute",
          inset: 0,
          width: "100%",
          height: "100%",
          pointerEvents: "none",
          display: "block",
        }}
      />

      <SelectionLayer sel={sel} annotating={phase === "annotating"} onHandleDown={onHandleDown} />

      <AnnotateCanvas
        shapes={shapes}
        draft={draft}
        width={window.innerWidth}
        height={window.innerHeight}
      />

      {phase === "annotating" && sel && (
        <Toolbar
          tool={tool}
          onTool={setTool}
          color={color}
          onColor={setColor}
          width={width}
          onWidth={setWidth}
          onUndo={() => setShapes((arr) => arr.slice(0, -1))}
          canUndo={shapes.length > 0}
          onConfirm={confirm}
          onCancel={cancel}
          style={toolbarStyle(sel)}
        />
      )}
    </div>
  );
}

/** 错误兜底层：可见底色 + 文案 + 关闭按钮；可选短延时后自动 cancel。
 *  ⚠️ 不在挂载瞬间 cancel——窗会立刻关掉，用户根本看不到原因。 */
function ErrorOverlay({
  message,
  autoCloseMs,
  onClose,
}: {
  message: string;
  autoCloseMs: number | null;
  onClose: () => void;
}) {
  useEffect(() => {
    if (autoCloseMs == null) return;
    const t = window.setTimeout(onClose, autoCloseMs);
    return () => window.clearTimeout(t);
  }, [autoCloseMs, onClose]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.45)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        color: "#fff",
        fontFamily: "system-ui, -apple-system, sans-serif",
        zIndex: 99999,
      }}
    >
      <div
        style={{
          maxWidth: 520,
          padding: "20px 28px",
          background: "rgba(20,20,20,0.85)",
          border: "1px solid rgba(255,255,255,0.15)",
          borderRadius: 8,
          boxShadow: "0 8px 32px rgba(0,0,0,0.4)",
          textAlign: "center",
        }}
      >
        <div style={{ fontSize: 14, lineHeight: 1.6, marginBottom: 16 }}>{message}</div>
        <button
          type="button"
          onClick={onClose}
          style={{
            padding: "6px 16px",
            background: "#FF3B30",
            color: "#fff",
            border: "none",
            borderRadius: 4,
            cursor: "pointer",
            fontSize: 13,
          }}
        >
          关闭 (Esc)
        </button>
        {autoCloseMs != null && (
          <div style={{ fontSize: 11, opacity: 0.6, marginTop: 10 }}>
            {Math.round(autoCloseMs / 1000)} 秒后自动关闭
          </div>
        )}
      </div>
    </div>
  );
}

/** React 渲染异常兜底：避免 throw 一路冒到根 → 整个窗白屏但仍 always-on-top。 */
class OverlayErrorBoundary extends Component<{ children: ReactNode }, { err: Error | null }> {
  state = { err: null as Error | null };
  static getDerivedStateFromError(err: Error) {
    return { err };
  }
  componentDidCatch(err: Error) {
    console.error("[overlay] render crash", err);
  }
  render() {
    if (this.state.err) {
      return (
        <ErrorOverlay
          message={`截图叠加窗渲染异常：${this.state.err.message}`}
          autoCloseMs={ERROR_AUTO_CLOSE_MS}
          onClose={() => void invoke("screenshot_cancel").catch(() => undefined)}
        />
      );
    }
    return this.props.children;
  }
}

export default function OverlayApp() {
  return (
    <OverlayErrorBoundary>
      <OverlayInner />
    </OverlayErrorBoundary>
  );
}

// ---- 几何辅助（窗口 CSS 像素） ----

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function normRect(x1: number, y1: number, x2: number, y2: number): Rect {
  return {
    x: Math.min(x1, x2),
    y: Math.min(y1, y2),
    w: Math.abs(x2 - x1),
    h: Math.abs(y2 - y1),
  };
}

function inside(r: Rect, x: number, y: number): boolean {
  return x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h;
}

function resizeRect(r: Rect, dir: Dir, dx: number, dy: number): Rect {
  let left = r.x;
  let top = r.y;
  let right = r.x + r.w;
  let bottom = r.y + r.h;
  if (dir.includes("w")) left += dx;
  if (dir.includes("e")) right += dx;
  if (dir.includes("n")) top += dy;
  if (dir.includes("s")) bottom += dy;
  return normRect(left, top, right, bottom);
}

/** 工具栏定位：选区下方贴右；空间不足则移到选区内上方。 */
function toolbarStyle(sel: Rect): React.CSSProperties {
  const below = sel.y + sel.h + 8;
  const fitsBelow = below + 40 < window.innerHeight;
  return {
    top: fitsBelow ? below : Math.max(8, sel.y + sel.h - 44),
    left: clamp(sel.x, 8, Math.max(8, window.innerWidth - 360)),
  };
}
