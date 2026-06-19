import { useEffect, useRef } from "react";
import type { Shape } from "../types";
import { drawShape } from "../draw";

interface Props {
  shapes: Shape[];
  /** 正在拖拽中的临时图形（实时预览）。 */
  draft: Shape | null;
  /** 窗口 CSS 像素尺寸。 */
  width: number;
  height: number;
}

/** 标注图层：铺满整窗的 canvas，画已提交图形 + 草稿。pointer-events:none，事件交给根层。 */
export default function AnnotateCanvas({ shapes, draft, width, height }: Props) {
  const ref = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
    for (const s of shapes) drawShape(ctx, s);
    if (draft) drawShape(ctx, draft);
  }, [shapes, draft, width, height]);

  return (
    <canvas
      ref={ref}
      style={{
        position: "absolute",
        left: 0,
        top: 0,
        width: `${width}px`,
        height: `${height}px`,
        pointerEvents: "none",
      }}
    />
  );
}
