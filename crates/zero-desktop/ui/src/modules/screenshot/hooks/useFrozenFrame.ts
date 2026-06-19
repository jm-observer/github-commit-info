import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";

/** 冻结帧 + 目标显示器几何（来自 overlay.html 的查询串，由 Rust overlay.rs 写入）。 */
export interface FrozenFrame {
  /** 经 asset 协议可加载的冻结帧 URL。 */
  src: string;
  /** 冻结帧 PNG 的绝对路径（原始）。 */
  framePath: string;
  /** 目标显示器物理矩形（像素）。 */
  geom: { x: number; y: number; w: number; h: number };
}

/** 解析叠加窗 URL 查询串，返回冻结帧信息（未就绪时为 null）。 */
export function useFrozenFrame(): FrozenFrame | null {
  const [frame, setFrame] = useState<FrozenFrame | null>(null);

  useEffect(() => {
    const q = new URLSearchParams(window.location.search);
    const framePath = q.get("frame") ?? "";
    if (!framePath) return;
    setFrame({
      src: convertFileSrc(framePath),
      framePath,
      geom: {
        x: Number(q.get("x") ?? 0),
        y: Number(q.get("y") ?? 0),
        w: Number(q.get("w") ?? window.innerWidth),
        h: Number(q.get("h") ?? window.innerHeight),
      },
    });
  }, []);

  return frame;
}
