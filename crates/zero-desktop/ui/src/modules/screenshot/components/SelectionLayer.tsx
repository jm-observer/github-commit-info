import type { Dir, Rect } from "../types";

interface Props {
  /** 当前选区（null = 尚未框选，整屏压暗）。 */
  sel: Rect | null;
  /** 是否进入标注阶段（显示八向手柄）。 */
  annotating: boolean;
  /** 拖拽某个手柄微调选区。 */
  onHandleDown: (dir: Dir, e: React.MouseEvent) => void;
}

const DIM = "rgba(0,0,0,0.45)";
const HANDLES: Dir[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

/** 框选可视层：整屏压暗 + 选区高亮（box-shadow 抠洞）+ 边框 + 尺寸标签 + 八向手柄。 */
export default function SelectionLayer({ sel, annotating, onHandleDown }: Props) {
  // 未框选：整屏压暗，提示用户拖拽。
  if (!sel) {
    return (
      <div
        style={{
          position: "absolute",
          inset: 0,
          background: DIM,
          pointerEvents: "none",
        }}
      />
    );
  }

  return (
    <>
      {/* 选区框：用超大 spread 的 box-shadow 把区域外整屏压暗，区域内保持原图。 */}
      <div
        style={{
          position: "absolute",
          left: sel.x,
          top: sel.y,
          width: sel.w,
          height: sel.h,
          boxShadow: `0 0 0 100000px ${DIM}`,
          border: "1px solid #0A84FF",
          pointerEvents: "none",
        }}
      />
      {/* 尺寸标签 */}
      <div
        style={{
          position: "absolute",
          left: sel.x,
          top: Math.max(0, sel.y - 22),
          padding: "1px 6px",
          fontSize: 12,
          lineHeight: "18px",
          color: "#fff",
          background: "rgba(10,132,255,0.9)",
          borderRadius: 3,
          pointerEvents: "none",
          whiteSpace: "nowrap",
        }}
      >
        {Math.round(sel.w)} × {Math.round(sel.h)}
      </div>
      {/* 八向手柄（仅标注阶段）：拖拽微调选区。 */}
      {annotating &&
        HANDLES.map((dir) => {
          const pos = handlePos(sel, dir);
          return (
            <div
              key={dir}
              onMouseDown={(e) => onHandleDown(dir, e)}
              style={{
                position: "absolute",
                left: pos.left - 5,
                top: pos.top - 5,
                width: 10,
                height: 10,
                background: "#fff",
                border: "1px solid #0A84FF",
                borderRadius: 2,
                cursor: cursorFor(dir),
                pointerEvents: "auto",
              }}
            />
          );
        })}
    </>
  );
}

function handlePos(r: Rect, dir: Dir): { left: number; top: number } {
  const midX = r.x + r.w / 2;
  const midY = r.y + r.h / 2;
  const left =
    dir.includes("w") ? r.x : dir.includes("e") ? r.x + r.w : midX;
  const top =
    dir.includes("n") ? r.y : dir.includes("s") ? r.y + r.h : midY;
  return { left, top };
}

function cursorFor(dir: Dir): string {
  switch (dir) {
    case "n":
    case "s":
      return "ns-resize";
    case "e":
    case "w":
      return "ew-resize";
    case "nw":
    case "se":
      return "nwse-resize";
    default:
      return "nesw-resize";
  }
}
