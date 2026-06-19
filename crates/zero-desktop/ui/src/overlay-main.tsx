import React from "react";
import ReactDOM from "react-dom/client";
import OverlayApp from "./modules/screenshot/OverlayApp";

// 叠加窗独立入口（overlay.html），不挂主窗口的路由/布局。
window.addEventListener("error", (e) => {
  console.error("[overlay error]", e.error ?? e.message);
});
window.addEventListener("unhandledrejection", (e) => {
  console.error("[overlay unhandled rejection]", e.reason);
});

ReactDOM.createRoot(document.getElementById("overlay-root")!).render(
  <React.StrictMode>
    <OverlayApp />
  </React.StrictMode>
);
