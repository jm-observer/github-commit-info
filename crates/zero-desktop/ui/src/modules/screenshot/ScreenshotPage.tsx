import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { Camera, FolderOpen, RefreshCw, Copy, Trash2, X, Settings as SettingsIcon, FolderSearch, Download, Star, ChevronLeft, ChevronRight } from "lucide-react";

interface HistoryItem {
  name: string;
  path: string;
  modified_ms: number;
  size: number;
  width: number;
  height: number;
  /** 已收藏 = 永久保留。 */
  starred: boolean;
}

/** 已收藏区折叠阈值：超过这个数量先只显示前 N 张，避免把「最近截图」挤出首屏。 */
const STARRED_COLLAPSE = 12;

interface ScreenshotSettings {
  hotkey: string;
  color: string;
  line_width: number;
  save_dir: string;
  delay_secs: number;
}

const COLORS = ["#FF3B30", "#FFCC00", "#34C759", "#0A84FF", "#FFFFFF", "#1C1C1E"];
const WIDTHS = [2, 4, 6, 10];
const DELAYS = [0, 3, 5, 10];

/** 主窗口「截图」页：立即截图入口 + 历史截图画廊（来自 <workspace>/screenshots）。 */
export default function ScreenshotPage() {
  const [items, setItems] = useState<HistoryItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // 预览上下文：打开预览时快照当前分区的路径序列 + 位置，用于上/下一张切换。
  // 存 path 而非对象，收藏状态变化、列表刷新后仍能重新解析到最新的 item。
  const [previewCtx, setPreviewCtx] = useState<{ paths: string[]; index: number } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [starredExpanded, setStarredExpanded] = useState(false);
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

  // 分区：收藏的永久保留，单独一段置顶；其余按时间倒序。
  const starred = useMemo(() => items.filter((it) => it.starred), [items]);
  const recent = useMemo(() => items.filter((it) => !it.starred), [items]);
  // 「最近截图」再按天分组（收藏区是精选、量少，不分组，保持纯时间倒序）。
  const recentGroups = useMemo(() => groupByDay(recent), [recent]);

  /** 当前预览的图（按 path 从最新 items 里解析；已被删除则为 null）。 */
  const preview = useMemo(() => {
    if (!previewCtx) return null;
    const path = previewCtx.paths[previewCtx.index];
    return items.find((it) => it.path === path) ?? null;
  }, [previewCtx, items]);

  /** 打开预览：快照该分区的顺序，之后在这份快照里翻页。 */
  const openPreview = (list: HistoryItem[], it: HistoryItem) => {
    const paths = list.map((x) => x.path);
    setPreviewCtx({ paths, index: Math.max(0, paths.indexOf(it.path)) });
  };

  /** 翻页（-1 上一张 / +1 下一张）。到头即停，不循环。 */
  const step = useCallback((delta: number) => {
    setPreviewCtx((ctx) => {
      if (!ctx) return ctx;
      const next = ctx.index + delta;
      if (next < 0 || next >= ctx.paths.length) return ctx;
      return { ...ctx, index: next };
    });
  }, []);

  // 预览态下的键盘操作：← / → 翻页，Esc 关闭。
  useEffect(() => {
    if (!previewCtx) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPreviewCtx(null);
      else if (e.key === "ArrowLeft") step(-1);
      else if (e.key === "ArrowRight") step(1);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [previewCtx, step]);

  // 滚轮翻页节流：一次滚动手势会连发几十个 wheel 事件，不节流会一口气翻到底。
  const wheelAt = useRef(0);
  const onWheel = (e: React.WheelEvent) => {
    const now = Date.now();
    if (now - wheelAt.current < 250) return;
    wheelAt.current = now;
    step(e.deltaY > 0 ? 1 : -1);
  };

  const capture = async () => {
    try {
      await invoke("screenshot_capture");
      if (settings && settings.delay_secs > 0) {
        flash(`将在 ${settings.delay_secs} 秒后自动截图`);
      }
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

  const reveal = async (it: HistoryItem) => {
    try {
      await invoke("screenshot_reveal_in_folder", { path: it.path });
    } catch (e) {
      flash(`定位文件失败：${String(e)}`);
    }
  };

  const saveAs = async (it: HistoryItem) => {
    try {
      const dest = await invoke<string | null>("screenshot_save_as", { path: it.path });
      flash(dest ? `已另存到 ${dest}` : "已取消保存");
    } catch (e) {
      flash(`另存失败：${String(e)}`);
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

  /** 切换收藏。乐观更新（预览层按 path 解析，自动跟着变），失败回滚并提示。
   *  预览态下改收藏不影响当前翻页序列——序列是打开时的快照。 */
  const toggleStar = async (it: HistoryItem) => {
    const next = !it.starred;
    const apply = (starred: boolean) =>
      setItems((arr) => arr.map((x) => (x.path === it.path ? { ...x, starred } : x)));
    apply(next);
    try {
      await invoke("screenshot_set_starred", { path: it.path, starred: next });
    } catch (e) {
      apply(it.starred);
      flash(`${next ? "收藏" : "取消收藏"}失败：${String(e)}`);
    }
  };

  const remove = async (it: HistoryItem) => {
    if (!window.confirm(`删除 ${it.name}？此操作不可恢复。`)) return;
    try {
      await invoke("screenshot_delete", { path: it.path });
      setItems((arr) => arr.filter((x) => x.path !== it.path));
      // 预览序列里同步剔除：停留在同一位置（即原来的下一张），越界则退到上一张，空了就关闭。
      setPreviewCtx((ctx) => {
        if (!ctx) return ctx;
        const at = ctx.paths.indexOf(it.path);
        if (at < 0) return ctx;
        const paths = ctx.paths.filter((p) => p !== it.path);
        if (paths.length === 0) return null;
        const shifted = at < ctx.index ? ctx.index - 1 : ctx.index;
        const index = Math.min(shifted, paths.length - 1);
        return { paths, index };
      });
    } catch (e) {
      flash(`删除失败：${String(e)}`);
    }
  };

  // 收藏多了先只显示两行，别把「最近截图」挤出首屏。
  const visibleStarred = starredExpanded ? starred : starred.slice(0, STARRED_COLLAPSE);

  /** 画廊卡片。`list` = 所在分区，决定放大后左右翻页的范围。 */
  const renderCard = (it: HistoryItem, list: HistoryItem[]) => (
    <div
      key={it.path}
      className="group relative overflow-hidden rounded-lg border border-gray-200 bg-gray-50 dark:border-gray-800 dark:bg-gray-900"
    >
      <button
        onClick={() => openPreview(list, it)}
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
      {/* 收藏是高频动作，星标常驻显示，不藏在 hover 里 */}
      <button
        onClick={() => void toggleStar(it)}
        title={it.starred ? "取消收藏" : "收藏（永久保留）"}
        className={[
          "absolute right-1.5 top-1.5 rounded-full p-1.5 transition-colors",
          it.starred
            ? "bg-black/40 text-yellow-400 hover:bg-black/60"
            : "bg-black/25 text-white/70 hover:bg-black/50 hover:text-yellow-300",
        ].join(" ")}
      >
        <Star size={15} fill={it.starred ? "currentColor" : "none"} />
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
            onClick={() => void saveAs(it)}
            title="另存为…"
            className="rounded p-1 text-gray-500 hover:bg-gray-200 hover:text-blue-600 dark:hover:bg-gray-700"
          >
            <Download size={15} />
          </button>
          <button
            onClick={() => void reveal(it)}
            title="在文件夹中显示"
            className="rounded p-1 text-gray-500 hover:bg-gray-200 hover:text-blue-600 dark:hover:bg-gray-700"
          >
            <FolderSearch size={15} />
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
  );

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

            {/* 延迟截图 */}
            <div>
              <span className="mb-1 block text-sm font-medium">延迟截图</span>
              <div className="flex items-center gap-2">
                {DELAYS.map((d) => (
                  <button
                    key={d}
                    onClick={() => setSettings({ ...settings, delay_secs: d })}
                    className={[
                      "flex h-8 min-w-10 items-center justify-center rounded-md border px-2 text-sm",
                      settings.delay_secs === d
                        ? "border-blue-500 bg-blue-50 text-blue-700 dark:bg-blue-950 dark:text-blue-300"
                        : "border-gray-300 text-gray-600 dark:border-gray-600 dark:text-gray-300",
                    ].join(" ")}
                  >
                    {d === 0 ? "立即" : `${d}s`}
                  </button>
                ))}
              </div>
              <span className="mt-1 block text-xs text-gray-400">
                触发后倒计时再抓屏，用于抓右键菜单等瞬时内容（触发热键会先把菜单关掉）。
              </span>
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
        <div className="space-y-6">
          {/* 已收藏区：为空时整段不渲染，不留空标题 */}
          {starred.length > 0 && (
            <section>
              <div className="mb-2 flex items-center justify-between">
                <h2 className="flex items-center gap-1.5 text-sm font-medium text-gray-700 dark:text-gray-200">
                  <Star size={15} className="text-yellow-500" fill="currentColor" />
                  已收藏
                  <span className="text-gray-400">({starred.length})</span>
                  <span className="ml-1 text-xs font-normal text-gray-400">永久保留</span>
                </h2>
                {starred.length > STARRED_COLLAPSE && (
                  <button
                    onClick={() => setStarredExpanded((v) => !v)}
                    className="text-xs text-blue-600 hover:underline dark:text-blue-400"
                  >
                    {starredExpanded ? "收起" : `展开全部 ${starred.length} 张`}
                  </button>
                )}
              </div>
              <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
                {visibleStarred.map((it) => renderCard(it, starred))}
              </div>
            </section>
          )}

          {/* 最近截图区（未收藏） */}
          <section>
            <h2 className="mb-2 flex items-center gap-1.5 text-sm font-medium text-gray-700 dark:text-gray-200">
              最近截图
              <span className="text-gray-400">({recent.length})</span>
            </h2>
            {recent.length === 0 ? (
              <p className="rounded-lg border border-dashed border-gray-300 px-3 py-6 text-center text-sm text-gray-400 dark:border-gray-700">
                没有未收藏的截图
              </p>
            ) : (
              <div className="space-y-4">
                {recentGroups.map((g) => (
                  <div key={g.key}>
                    <div className="mb-1.5 flex items-center gap-2 text-xs text-gray-400">
                      <span>{g.label}</span>
                      <span className="text-gray-300 dark:text-gray-600">{g.items.length}</span>
                      <span className="h-px flex-1 bg-gray-200 dark:bg-gray-800" />
                    </div>
                    {/* 翻页序列仍是整个「最近截图」区：放大后能连续翻过日期边界 */}
                    <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
                      {g.items.map((it) => renderCard(it, recent))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>
        </div>
      )}

      {/* 放大预览：点图片以外任意处关闭；← / → 或滚轮上下切换 */}
      {preview && previewCtx && (
        <div
          className="fixed inset-0 z-50 flex flex-col bg-black/80 p-6"
          onClick={() => setPreviewCtx(null)}
          onWheel={onWheel}
        >
          <div className="mb-3 flex items-center justify-between text-white" onClick={(e) => e.stopPropagation()}>
            <span className="text-sm">
              {preview.name} · {preview.width}×{preview.height}
              <span className="ml-2 text-white/60">
                {previewCtx.index + 1} / {previewCtx.paths.length}
              </span>
            </span>
            <div className="flex gap-2">
              <button
                onClick={() => void toggleStar(preview)}
                className={[
                  "flex items-center gap-1 rounded px-2 py-1 text-sm",
                  preview.starred ? "bg-yellow-500/90 text-black hover:bg-yellow-400" : "bg-white/15 hover:bg-white/25",
                ].join(" ")}
              >
                <Star size={15} fill={preview.starred ? "currentColor" : "none"} />{" "}
                {preview.starred ? "已收藏" : "收藏"}
              </button>
              <button
                onClick={() => void copy(preview)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-white/25"
              >
                <Copy size={15} /> 复制
              </button>
              <button
                onClick={() => void saveAs(preview)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-white/25"
              >
                <Download size={15} /> 另存为
              </button>
              <button
                onClick={() => void reveal(preview)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-white/25"
              >
                <FolderSearch size={15} /> 在文件夹中显示
              </button>
              <button
                onClick={() => void remove(preview)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-red-500"
              >
                <Trash2 size={15} /> 删除
              </button>
              <button
                onClick={() => setPreviewCtx(null)}
                className="flex items-center gap-1 rounded bg-white/15 px-2 py-1 text-sm hover:bg-white/25"
              >
                <X size={15} /> 关闭
              </button>
            </div>
          </div>

          <div className="relative flex min-h-0 flex-1 items-center justify-center">
            {previewCtx.index > 0 && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  step(-1);
                }}
                title="上一张（←）"
                className="absolute left-0 z-10 rounded-full bg-white/15 p-2 text-white hover:bg-white/30"
              >
                <ChevronLeft size={24} />
              </button>
            )}
            <img
              src={convertFileSrc(preview.path)}
              alt={preview.name}
              className="max-h-full max-w-full object-contain"
              onClick={(e) => e.stopPropagation()}
            />
            {previewCtx.index < previewCtx.paths.length - 1 && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  step(1);
                }}
                title="下一张（→）"
                className="absolute right-0 z-10 rounded-full bg-white/15 p-2 text-white hover:bg-white/30"
              >
                <ChevronRight size={24} />
              </button>
            )}
          </div>
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

/** 当天 0 点的毫秒时间戳（本地时区）。 */
function startOfDay(ms: number): number {
  const d = new Date(ms);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** 日期小标题：今天 / 昨天 / M月D日（跨年补年份）。 */
function dayLabel(dayStart: number): string {
  const today = startOfDay(Date.now());
  if (dayStart === today) return "今天";
  if (dayStart === today - 86400000) return "昨天";
  const d = new Date(dayStart);
  const md = `${d.getMonth() + 1}月${d.getDate()}日`;
  return d.getFullYear() === new Date().getFullYear() ? md : `${d.getFullYear()}年${md}`;
}

/** 按天分组（输入已是时间倒序，分组后组间、组内都保持倒序）。 */
function groupByDay(list: HistoryItem[]): { key: number; label: string; items: HistoryItem[] }[] {
  const groups: { key: number; label: string; items: HistoryItem[] }[] = [];
  for (const it of list) {
    const key = startOfDay(it.modified_ms);
    const last = groups[groups.length - 1];
    if (last && last.key === key) last.items.push(it);
    else groups.push({ key, label: dayLabel(key), items: [it] });
  }
  return groups;
}

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
