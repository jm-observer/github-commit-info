import type { CSSProperties } from "react";

interface Props {
  /** 选区对应的物理像素尺寸（真正录进视频的分辨率）。 */
  physical: { w: number; h: number };
  onStart: () => void;
  onCancel: () => void;
  style?: CSSProperties;
}

/**
 * 录屏模式的叠加窗工具栏：只有「开始录制 / 取消」+ 一行分辨率提示。
 *
 * 分辨率是显示物理像素而非 CSS 像素——高 DPI 屏上两者能差一倍，用户关心的是
 * 出片分辨率。宽高会被后端向下取偶（yuv420p 要求），这里同步取偶，免得提示的数字
 * 和实际出片差一个像素。
 */
export default function RecordToolbar({ physical, onStart, onCancel, style }: Props) {
  const w = physical.w & ~1;
  const h = physical.h & ~1;
  const tooSmall = w < 16 || h < 16;

  return (
    <div
      style={{
        position: "absolute",
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "6px 10px",
        background: "rgba(28,28,30,0.95)",
        borderRadius: 8,
        boxShadow: "0 2px 8px rgba(0,0,0,0.4)",
        pointerEvents: "auto",
        userSelect: "none",
        color: "#fff",
        fontFamily: "system-ui, -apple-system, sans-serif",
        fontSize: 12,
        ...style,
      }}
      // 防止点工具栏被根层当成「选区外重新框选」。
      onMouseDown={(e) => e.stopPropagation()}
    >
      <span style={{ opacity: 0.75, fontVariantNumeric: "tabular-nums" }}>
        {w} × {h}
      </span>
      <span style={{ width: 1, height: 18, background: "rgba(255,255,255,0.2)" }} />
      <button type="button" onClick={onCancel} style={btn()}>
        取消 (Esc)
      </button>
      <button
        type="button"
        onClick={onStart}
        disabled={tooSmall}
        title={tooSmall ? "区域太小（至少 16×16）" : "开始录制 (Enter)"}
        style={{
          ...btn(),
          background: tooSmall ? "rgba(255,255,255,0.08)" : "#FF3B30",
          color: "#fff",
          cursor: tooSmall ? "not-allowed" : "pointer",
        }}
      >
        ● 开始录制
      </button>
    </div>
  );
}

function btn(): CSSProperties {
  return {
    height: 26,
    padding: "0 10px",
    fontSize: 12,
    color: "#eee",
    background: "rgba(255,255,255,0.10)",
    border: "none",
    borderRadius: 6,
    cursor: "pointer",
  };
}
