import { useCallback, useEffect, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatDuration, type RecordingStatus } from "./types";

/** 状态轮询间隔。计时只到秒，500ms 足够跟手，又不至于每秒唤醒太多次。 */
const POLL_MS = 500;

/**
 * 录制中的悬浮控制条（recorder.html 独立窗口）：红点 + 计时 + 暂停/继续 + 停止 + 丢弃。
 *
 * 计时以 **Rust 侧的 `elapsed_ms` 为准**，不在前端自己 setInterval 累加——暂停、
 * 卡顿、系统休眠都会让前端计数和真实录制时长分家，而这条计时正是用户判断「录够了没」
 * 的唯一依据。
 *
 * 后端状态变成 `idle`（正常停止，或 ffmpeg 中途挂掉）时窗口自行关闭，绝不留一条
 * 停在那儿不动的假控制条。
 */
export default function RecorderBar() {
  const [status, setStatus] = useState<RecordingStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const closeSelf = useCallback(async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().close();
    } catch {
      /* 窗口已被 Rust 侧关掉 */
    }
  }, []);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const s = await invoke<RecordingStatus>("recording_status");
        if (!alive) return;
        setStatus(s);
        // 录制已结束（含 ffmpeg 自行崩溃）→ 控制条没有存在意义了。
        if (s.state === "idle" && !busy) void closeSelf();
      } catch {
        /* 主进程忙，下一轮再说 */
      }
    };
    void tick();
    const id = window.setInterval(tick, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [busy, closeSelf]);

  const paused = status?.state === "paused";

  const togglePause = async () => {
    setBusy(true);
    try {
      await invoke("recording_set_paused", { paused: !paused });
      setErr(null);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const stop = async () => {
    setBusy(true);
    try {
      await invoke("recording_stop");
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
      void closeSelf();
    }
  };

  const discard = async () => {
    setBusy(true);
    try {
      await invoke("recording_discard");
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
      void closeSelf();
    }
  };

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        height: "100%",
        padding: "0 12px",
        boxSizing: "border-box",
        background: "rgba(28,28,30,0.94)",
        borderRadius: 10,
        border: "1px solid rgba(255,255,255,0.12)",
        boxShadow: "0 4px 16px rgba(0,0,0,0.45)",
        color: "#fff",
        fontFamily: "system-ui, -apple-system, sans-serif",
      }}
      // 整条可拖动：控制条难免挡住画面，用户得能把它挪走。按钮是子元素、不带这个
      // 属性，所以点按钮不会被当成拖窗口。
      data-tauri-drag-region
      title={err ?? status?.path ?? ""}
    >
      <span
        style={{
          width: 10,
          height: 10,
          borderRadius: "50%",
          background: paused ? "#8E8E93" : "#FF3B30",
          flex: "0 0 auto",
          // 录制中呼吸闪烁，暂停时静止——余光扫一眼就知道现在录没录。
          animation: paused ? "none" : "recdot 1.2s ease-in-out infinite",
        }}
      />
      <style>{`@keyframes recdot { 0%,100% { opacity: 1 } 50% { opacity: .25 } }`}</style>

      <span
        style={{
          fontVariantNumeric: "tabular-nums",
          fontSize: 15,
          minWidth: 52,
          letterSpacing: 0.5,
        }}
      >
        {formatDuration(status?.elapsed_ms ?? 0)}
      </span>

      <span style={{ flex: 1 }} />

      <button type="button" onClick={togglePause} disabled={busy} style={btn()}>
        {paused ? "继续" : "暂停"}
      </button>
      <button
        type="button"
        onClick={stop}
        disabled={busy}
        style={{ ...btn(), background: "#FF3B30", color: "#fff" }}
      >
        停止
      </button>
      <button type="button" onClick={discard} disabled={busy} style={btn()} title="停止并删除文件">
        丢弃
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
