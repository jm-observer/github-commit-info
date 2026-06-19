// 截图叠加窗共享类型。坐标统一用「窗口 CSS 像素」，合成时再按缩放映射到冻结帧原始分辨率。

export type Tool = "ellipse" | "arrow" | "rect";

/** 八向 resize 手柄方向。 */
export type Dir = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w";

/** 一个标注图形（端点 + 颜色 + 线宽）。 */
export interface Shape {
  tool: Tool;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  color: string;
  width: number;
}

/** 矩形选区（窗口 CSS 像素）。 */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}
