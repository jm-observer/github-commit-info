import type { CSSProperties } from "react";
import type { Tool } from "../types";

interface Props {
  tool: Tool;
  onTool: (t: Tool) => void;
  color: string;
  onColor: (c: string) => void;
  width: number;
  onWidth: (w: number) => void;
  onUndo: () => void;
  canUndo: boolean;
  onConfirm: () => void;
  onCancel: () => void;
  style?: CSSProperties;
}

const TOOLS: { key: Tool; label: string }[] = [
  { key: "ellipse", label: "○" },
  { key: "arrow", label: "↗" },
  { key: "rect", label: "▭" },
];
const COLORS = ["#FF3B30", "#FFCC00", "#34C759", "#0A84FF", "#FFFFFF", "#1C1C1E"];
const WIDTHS = [2, 4, 6, 10];

/** 标注工具栏：工具切换 / 颜色 / 粗细 / 撤销 / 完成 / 取消。 */
export default function Toolbar(props: Props) {
  const {
    tool,
    onTool,
    color,
    onColor,
    width,
    onWidth,
    onUndo,
    canUndo,
    onConfirm,
    onCancel,
    style,
  } = props;

  return (
    <div
      style={{
        position: "absolute",
        display: "flex",
        alignItems: "center",
        gap: 8,
        padding: "6px 8px",
        background: "rgba(28,28,30,0.95)",
        borderRadius: 8,
        boxShadow: "0 2px 8px rgba(0,0,0,0.4)",
        pointerEvents: "auto",
        userSelect: "none",
        ...style,
      }}
      // 防止点击工具栏被根层当成「选区外重选」。
      onMouseDown={(e) => e.stopPropagation()}
    >
      {TOOLS.map((t) => (
        <button
          key={t.key}
          onClick={() => onTool(t.key)}
          style={btnStyle(tool === t.key)}
          title={t.key}
        >
          {t.label}
        </button>
      ))}

      <span style={sep} />

      {COLORS.map((c) => (
        <button
          key={c}
          onClick={() => onColor(c)}
          title={c}
          style={{
            width: 18,
            height: 18,
            borderRadius: "50%",
            background: c,
            border: color === c ? "2px solid #fff" : "1px solid #555",
            cursor: "pointer",
            padding: 0,
          }}
        />
      ))}

      <span style={sep} />

      {WIDTHS.map((w) => (
        <button key={w} onClick={() => onWidth(w)} style={btnStyle(width === w)}>
          <span
            style={{
              display: "inline-block",
              width: 16,
              height: w,
              background: width === w ? "#0A84FF" : "#ccc",
              borderRadius: w,
            }}
          />
        </button>
      ))}

      <span style={sep} />

      <button onClick={onUndo} disabled={!canUndo} style={btnStyle(false, !canUndo)}>
        撤销
      </button>
      <button onClick={onCancel} style={btnStyle(false)}>
        取消
      </button>
      <button onClick={onConfirm} style={{ ...btnStyle(false), background: "#0A84FF", color: "#fff" }}>
        完成
      </button>
    </div>
  );
}

const sep: CSSProperties = {
  width: 1,
  height: 18,
  background: "rgba(255,255,255,0.2)",
};

function btnStyle(active: boolean, disabled = false): CSSProperties {
  return {
    minWidth: 28,
    height: 26,
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    padding: "0 6px",
    fontSize: 14,
    color: active ? "#fff" : "#ddd",
    background: active ? "#0A84FF" : "rgba(255,255,255,0.08)",
    border: "none",
    borderRadius: 5,
    cursor: disabled ? "not-allowed" : "pointer",
    opacity: disabled ? 0.4 : 1,
  };
}
