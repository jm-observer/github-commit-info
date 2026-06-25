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
  /** Rust 侧分配的 session id，前端就绪时回传给 `screenshot_overlay_ready`。 */
  sessionId: number;
  /** 解析失败原因；非空时 OverlayApp 走错误层路径，不要再进入正常截图流程。 */
  error?: string;
}

/** 解析叠加窗 URL 查询串，返回冻结帧信息（首次渲染前为 null；解析后必有值，
 *  失败时 `error` 字段非空）。 */
export function useFrozenFrame(): FrozenFrame | null {
  const [frame, setFrame] = useState<FrozenFrame | null>(null);

  useEffect(() => {
    try {
      const q = new URLSearchParams(window.location.search);
      const framePath = q.get("frame") ?? "";
      const sid = Number(q.get("sid") ?? 0);
      if (!framePath) {
        setFrame({
          src: "",
          framePath: "",
          sessionId: sid,
          geom: { x: 0, y: 0, w: 0, h: 0 },
          error: "缺少冻结帧参数（frame）",
        });
        return;
      }
      setFrame({
        src: convertFileSrc(framePath),
        framePath,
        sessionId: sid,
        geom: {
          x: Number(q.get("x") ?? 0),
          y: Number(q.get("y") ?? 0),
          w: Number(q.get("w") ?? window.innerWidth),
          h: Number(q.get("h") ?? window.innerHeight),
        },
      });
    } catch (e) {
      setFrame({
        src: "",
        framePath: "",
        sessionId: 0,
        geom: { x: 0, y: 0, w: 0, h: 0 },
        error: `解析叠加窗参数失败：${String(e)}`,
      });
    }
  }, []);

  return frame;
}
