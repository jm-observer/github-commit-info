import React from "react";
import ReactDOM from "react-dom/client";
import RecorderBar from "./modules/recording/RecorderBar";

// 录制控制条独立入口（recorder.html），不挂主窗口的路由/布局。
window.addEventListener("error", (e) => {
  console.error("[recorder error]", e.error ?? e.message);
});
window.addEventListener("unhandledrejection", (e) => {
  console.error("[recorder unhandled rejection]", e.reason);
});

ReactDOM.createRoot(document.getElementById("recorder-root")!).render(
  <React.StrictMode>
    <RecorderBar />
  </React.StrictMode>
);
