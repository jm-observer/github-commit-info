import { useCallback, useEffect, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { Camera, FolderOpen, RefreshCw, Copy, Trash2, X, Settings as SettingsIcon } from "lucide-react";

interface HistoryItem {
  name: string;
  path: string;
  modified_ms: number;
  size: number;
  width: number;
  height: number;
}

interface ScreenshotSettings {
  hotkey: string;
  color: string;
  line_width: number;
  save_dir: string;
}

const COLORS = ["#FF3B30", "#FFCC00", "#34C759", "#0A84FF", "#FFFFFF", "#1C1C1E"];
const WIDTHS = [2, 4, 6, 10];

/** 主窗口「截图」页：立即截图入口 + 历史截图画廊（来自 <workspace>/screenshots）。 */
export default function ScreenshotPage() {
  const [items, setItems] = useState<HistoryItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [preview, setPreview] = useState<HistoryItem | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [settings, setSettings] = useState<ScreenshotSettings | null>(null);
  const [savingSettings, setSavingSettings] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const list = await invoke<HistoryItem[]>("screenshot_list_history");
      setItems(list);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    void invoke<ScreenshotSettings>("screenshot_get_settings")
      .then(setSettings)
      .catch(() => undefined);
    // 截图叠加窗关闭后主窗口重新获得焦点 → 自动刷新出现新图。
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

  const flash = (msg: string) => {
    setToast(msg);
    window.setTimeout(() => setToast(null), 1800);
  };

  const capture = async () => {
    try {
      await invoke("screenshot_capture");
    } catch (e) {
      flash(`截图失败：${String(e)}`);
    }
  };

  const openFolder = async () => {
    try {
      await invoke("screenshot_open_folder");
    } catch (e) {
      flash(`打开文件夹失败：${String(e)}`);
    }
  };

  const copy = async (it: HistoryItem) => {
    try {
      await invoke("screenshot_copy_to_clipboard", { path: it.path });
      flash("已复制到剪贴板");
    } catch (e) {
      flash(`复制失败：${String(e)}`);
    }
  };

  const saveSettings = async () => {
    if (!settings) return;
    setSavingSettings(true);
    try {
      await invoke("screenshot_save_settings", { settings });
      flash("设置已保存，热键即时生效");
      setShowSettings(false);
    } catch (e) {
      flash(`保存失败：${String(e)}`);
    } finally {
      setSavingSettings(false);
    }
  };

  const remove = async (it: HistoryItem) => {
    if (!window.confirm(`删除 ${it.name}？此操作不可恢复。`)) return;
    try {
      await invoke("screenshot_delete", { path: it.path });
      setItems((arr) => arr.filter((x) => x.path !== it.path));
      if (preview?.path === it.path) setPreview(null);
    } catch (e) {
      flash(`删除失败：${String(e)}`);
    }
  };

  return (
    <div className="mx-auto max-w-6xl">
      <div className="mb-5 flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">截图</h1>
          <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">
            全局热键 <kbd className="rounded bg-gray-200 px-1.5 py-0.5 text-xs dark:bg-gray-700">Ctrl+Alt+A</kbd>{" "}
            随手截图；成图自动进剪贴板并存到 screenshots 目录。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => void capture()}
            className="flex items-center gap-2 rounded-md bg-blue-500 px-3 py-2 text-sm font-medium text-white hover:bg-blue-600"
          >
            <Camera size={16} /> 立即截图
          </button>
          <button
            onClick={() => void openFolder()}
            className="flex items-center gap-2 rounded-md bg-gray-100 px-3 py-2 text-sm text-gray-700 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
          >
            <FolderOpen size={16} /> 打开文件夹
          </button>
          <button
            onClick={() => void refresh()}
            disabled={loading}
            className="flex items-center gap-2 rounded-md bg-gray-100 px-3 py-2 text-sm text-gray-700 hover:bg-gray-200 disabled:opacity-60 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
          >
            <RefreshCw size={16} className={loading ? "animate-spin" : ""} /> 刷新
          </button>
          <button
            onClick={() => setShowSettings((v) => !v)}
            title="截图设置"
            className={[
              "flex items-center gap-2 rounded-md px-3 py-2 text-sm",
              showSettings
                ? "bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300"
                : "bg-gray-100 text-gray-700 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700",
            ].join(" ")}
          >
            <SettingsIcon size={16} /> 设置
          </button>
        </div>
      </div>

      {showSettings && settings && (
        <div className="mb-5 rounded-lg border border-gray-200 bg-gray-50 p-4 dark:border-gray-800 dark:bg-gray-900">
          <div className="grid gap-4 sm:grid-cols-2">
            {/* 热键 */}
            <label className="block">
              <span className="mb-1 block text-sm font-medium">全局热键</span>
              <input
                type="text"
                value={settings.hotkey}
                onChange={(e) => setSettings({ ...settings, hotkey: e.target.value })}
                placeholder="Ctrl+Alt+A"
                className="w-full rounded-md border border-gray-300 bg-white px-3 py-1.5 text-sm dark:border-gray-700 dark:bg-gray-800"
              />
              <span className="mt-1 block text-xs text-gray-400">
                组合如 <code>Ctrl+Alt+A</code> / <code>Shift+PrintScreen</code> / <code>Ctrl+F2</code>，保存即生效。
              </span>
            </label>

            {/* 保存目录（只读） */}
            <label className="block">
              <span className="mb-1 block text-sm font-medium">保存目录</span>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={settings.save_dir}
                  readOnly
                  className="w-full rounded-md border border-gray-300 bg-gray-100 px-3 py-1.5 text-sm text-gray-500 dark:border-gray-700 dark:bg-gray-800"
                />
                <button
                  onClick={() => void openFolder()}
                  className="shrink-0 rounded-md bg-gray-200 px-3 text-sm hover:bg-gray-300 dark:bg-gray-700 dark:hover:bg-gray-600"
                >
                  打开
                </button>
              </div>
            </label>

            {/* 默认颜色 */}
            <div>
              <span className="mb-1 block text-sm font-medium">默认标注颜色</span>
              <div className="flex items-center gap-2">
                {COLORS.map((c) => (
                  <button
                    key={c}
                    onClick={() => setSettings({ ...settings, color: c })}
                    title={c}
                    style={{ background: c }}
                    className={[
                      "h-6 w-6 rounded-full border",
                      settings.color.toUpperCase() === c
                        ? "ring-2 ring-blue-500 ring-offset-1 dark:ring-offset-gray-900"
                        : "border-gray-300 dark:border-gray-600",
                    ].join(" ")}
                  />
                ))}
              </div>
            </div>

            {/* 默认线宽 */}
            <div>
              <span className="mb-1 block text-sm font-medium">默认线宽</span>
              <div className="flex items-center gap-2">
                {WIDTHS.map((w) => (
                  <button
                    key={w}
                    onClick={() => setSettings({ ...settings, line_width: w })}
                    className={[
                      "flex h-8 w-10 items-center justify-center rounded-md border",
                      settings.line_width === w
                        ? "border-blue-500 bg-blue-50 dark:bg-blue-950"
                        : "border-gray-300 dark:border-gray-600",
                    ].join(" ")}
                  >
                    <span
                      style={{ height: w }}
                      className="w-5 rounded-full bg-gray-600 dark:bg-gray-300"
                    />
                  </button>
                ))}
              </div>
            </div>
          </div>

          <div className="mt-4 flex justify-end gap-2">
            <button
              onClick={() => setShowSettings(false)}
              className="rounded-md bg-gray-100 px-3 py-1.5 text-sm hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700"
            >
              取消
            </button>
            <button
              onClick={() => void saveSettings()}
              disabled={savingSettings}
              className="rounded-md bg-blue-500 px-4 py-1.5 text-sm font-medium text-white hover:bg-blue-600 disabled:opacity-60"
            >
              {savingSettings ? "保存中…" : "保存"}
            </button>
          </div>
        </div>
      )}

      {err && (
        <div className="mb-4 rounded-md bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950 dark:text-red-300">
          {err}
        </div>
      )}

      {items.length === 0 && !loading ? (
        <div className="flex h-64 flex-col items-center justify-center rounded-lg border border-dashed border-gray-300 text-gray-400 dark:border-gray-700">
          <Camera size={32} />
          <p className="mt-2 text-sm">还没有截图，按 Ctrl+Alt+A 或点「立即截图」</p>
        </div>
      ) : (
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
          {items.map((it) => (
            <div
              key={it.path}
              className="group relative overflow-hidden rounded-lg border border-gray-200 bg-gray-50 dark:border-gray-800 dark:bg-gray-900"
            >
              <button
                onClick={() => setPreview(it)}
                className="block aspect-video w-full overflow-hidden bg-[repeating-conic-gradient(#e5e7eb_0_25%,#f3f4f6_0_50%)] bg-[length:16px_16px]"
                title="点击放大"
              >
                <img
                  src={convertFileSrc(it.path)}
                  alt={it.name}
                  className="h-full w-full object-contain"
                  loading="lazy"
                />
              </button>
              <div className="flex items-center justify-between px-2 py-1.5">
                <div className="min-w-0">
                  <div className="truncate text-xs text-gray-600 dark:text-gray-300" title={it.name}>
                    {it.name}
                  </div>
                  <div className="text-[10px] text-gray-400">
                    {it.width}×{it.height} · {fmtSize(it.size)}
                  </div>
                </div>
                <div className="flex shrink-0 gap-1 opacity-0 transition-opacity group-hover:opacity-100">
                  <button
                    onClick={() => void copy(it)}
                    title="复制到剪贴板"
                    className="rounded p-1 text-gray-500 hover:bg-gray-200 hover:text-blue-600 dark:hover:bg-gray-700"
                  >
                    <Copy size={15} />
                  </button>
                  <button
                    onClick={() => void remove(it)}
                    title="删除"
                    className="rounded p-1 text-gray-500 hover:bg-gray-200 hover:text-red-600 dark:hover:bg-gray-700"
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* 放大预览 */}
      {preview && (
        <div
          className="fixed inset-0 z-50 flex flex-col bg-black/80 p-6"
          onClick={() => setPreview(null)}
        >
          <div className="mb-3 flex items-center justify-between text-white" onClick={(e) => e.stopPropagation()}>
            <span className="text-sm">
              {preview.name} · {preview.width}×{preview.height}
            </span>
            <div className="flex gap-2">
              <button
                onClick={() => void copy(preview)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-white/25"
              >
                <Copy size={15} /> 复制
              </button>
              <button
                onClick={() => void remove(preview)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-red-500"
              >
                <Trash2 size={15} /> 删除
              </button>
              <button
                onClick={() => setPreview(null)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-white/25"
              >
                <X size={15} /> 关闭
              </button>
            </div>
          </div>
          <img
            src={convertFileSrc(preview.path)}
            alt={preview.name}
            className="min-h-0 flex-1 object-contain"
            onClick={(e) => e.stopPropagation()}
          />
        </div>
      )}

      {toast && (
        <div className="fixed bottom-6 left-1/2 -translate-x-1/2 rounded-md bg-gray-900 px-4 py-2 text-sm text-white shadow-lg dark:bg-gray-100 dark:text-gray-900">
          {toast}
        </div>
      )}
    </div>
  );
}

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
