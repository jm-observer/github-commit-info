import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // 多入口：主窗口 index.html + 截图叠加窗 overlay.html + 录屏控制条 recorder.html
    // （后两者都是独立 webview）。
    rollupOptions: {
      input: {
        main: "index.html",
        overlay: "overlay.html",
        recorder: "recorder.html",
      },
    },
  },
});
