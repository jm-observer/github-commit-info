import type { Rect, Shape } from "./types";
import { drawShape } from "./draw";

/**
 * 选区裁剪 + 标注合成 → PNG base64（不含 data: 前缀）。
 *
 * 坐标系：选区/标注都在「窗口 CSS 像素」；冻结帧 `<img>` 的 naturalWidth/Height 是显示器
 * 物理像素。按 `natural / displayed` 求缩放，把裁剪与标注映射回原始分辨率，成图即全分辨率
 * （因此对高 DPI 也成立，不止 100%）。
 */
export async function composePng(
  img: HTMLImageElement,
  sel: Rect,
  shapes: Shape[],
  displayW: number,
  displayH: number
): Promise<string> {
  const scaleX = img.naturalWidth / displayW;
  const scaleY = img.naturalHeight / displayH;
  const outW = Math.max(1, Math.round(sel.w * scaleX));
  const outH = Math.max(1, Math.round(sel.h * scaleY));

  const canvas = document.createElement("canvas");
  canvas.width = outW;
  canvas.height = outH;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("无法创建合成画布");

  // 裁剪：从冻结帧对应区域绘制到成图。
  ctx.drawImage(
    img,
    sel.x * scaleX,
    sel.y * scaleY,
    sel.w * scaleX,
    sel.h * scaleY,
    0,
    0,
    outW,
    outH
  );

  // 标注：平移到选区局部坐标后按缩放绘制（线宽随缩放一并放大，保持比例）。
  ctx.save();
  ctx.scale(scaleX, scaleY);
  ctx.translate(-sel.x, -sel.y);
  for (const s of shapes) drawShape(ctx, s);
  ctx.restore();

  const blob = await new Promise<Blob | null>((resolve) =>
    canvas.toBlob(resolve, "image/png")
  );
  if (!blob) throw new Error("合成 PNG 失败");
  const bytes = new Uint8Array(await blob.arrayBuffer());
  return base64FromBytes(bytes);
}

/** Uint8Array → base64（分块避免 String.fromCharCode 爆栈）。 */
function base64FromBytes(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}
