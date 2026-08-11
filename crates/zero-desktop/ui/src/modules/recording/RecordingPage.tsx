import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Video,
  Square,
  Pause,
  Play,
  FolderOpen,
  FolderSearch,
  RefreshCw,
  Trash2,
  Download,
  Settings as SettingsIcon,
  AlertTriangle,
  CheckCircle2,
  Monitor,
  Crop,
} from "lucide-react";
import {
  formatDuration,
  formatSize,
  type FfmpegInfo,
  type RecordingItem,
  type RecordingSettings,
  type RecordingStatus,
} from "./types";

/** 录制中状态轮询间隔（页面上的计时同样以后端为准）。 */
const POLL_MS = 500;

const FPS_CHOICES = [10, 15, 24, 30];
/** CRF 档位：给出人话标签，不让用户去理解 x264 的数字含义。 */
const QUALITY = [
  { crf: 18, label: "高（文件大）" },
  { crf: 23, label: "标准" },
  { crf: 28, label: "省空间" },
];

/** 主窗口「录屏」页：开始/停止入口 + ffmpeg 状态 + 历史录屏列表 + 设置。 */
export default function RecordingPage() {
  const [items, setItems] = useState<RecordingItem[]>([]);
  const [status, setStatus] = useState<RecordingStatus | null>(null);
  const [settings, setSettings] = useState<RecordingSettings | null>(null);
  const [ffmpeg, setFfmpeg] = useState<FfmpegInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [savingSettings, setSavingSettings] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);

  const flash = (msg: string) => {
    setToast(msg);
    window.setTimeout(() => setToast(null), 2200);
  };

  const refresh = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      setItems(await invoke<RecordingItem[]>("recording_list"));
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const detect = useCallback(async (hint: string) => {
    try {
      setFfmpeg(await invoke<FfmpegInfo>("recording_detect_ffmpeg", { pathHint: hint }));
    } catch (e) {
      setFfmpeg({ found: false, path: "", version: "", error: String(e) });
    }
  }, []);

  useEffect(() => {
    void refresh();
    void invoke<RecordingSettings>("recording_get_settings")
      .then(setSettings)
      .catch(() => undefined);
    void detect("");
  }, [refresh, detect]);

  // 录制状态轮询。录制刚结束（recording → idle）时自动刷新列表，新文件立刻出现。
  useEffect(() => {
    let prev: string | null = null;
    const tick = async () => {
      try {
        const s = await invoke<RecordingStatus>("recording_status");
        setStatus(s);
        if (prev && prev !== "idle" && s.state === "idle") void refresh();
        prev = s.state;
      } catch {
        /* 主进程忙，下一轮再说 */
      }
    };
    void tick();
    const id = window.setInterval(tick, POLL_MS);
    return () => window.clearInterval(id);
  }, [refresh]);

  const recording = status && status.state !== "idle";
  const paused = status?.state === "paused";

  const start = async (region: boolean) => {
    if (!ffmpeg?.found) {
      flash("未找到 ffmpeg，请先在设置里指定路径");
      setShowSettings(true);
      return;
    }
    try {
      if (region) {
        // 交给热键那条路径：抓冻结帧 → 叠加窗框选 → 框选完再开录。
        await invoke("recording_start_region");
      } else {
        await invoke("recording_start", { region: null });
      }
    } catch (e) {
      flash(`开始录制失败：${String(e)}`);
    }
  };

  const stop = async () => {
    try {
      const path = await invoke<string>("recording_stop");
      flash(`已保存：${path}`);
      void refresh();
    } catch (e) {
      flash(`停止失败：${String(e)}`);
    }
  };

  const togglePause = async () => {
    try {
      await invoke("recording_set_paused", { paused: !paused });
    } catch (e) {
      flash(String(e));
    }
  };

  const saveSettings = async (next: RecordingSettings) => {
    setSavingSettings(true);
    try {
      await invoke("recording_save_settings", { settings: next });
      setSettings(next);
      void detect(next.ffmpeg_path);
      flash("设置已保存");
    } catch (e) {
      flash(`保存失败：${String(e)}`);
      // 保存失败（多为热键被占用）→ 拉回后端的真实值，别让界面停在没生效的状态上。
      void invoke<RecordingSettings>("recording_get_settings")
        .then(setSettings)
        .catch(() => undefined);
    } finally {
      setSavingSettings(false);
    }
  };

  const act = async (cmd: string, it: RecordingItem, done?: string) => {
    try {
      await invoke(cmd, { path: it.path });
      if (done) flash(done);
      if (cmd === "recording_delete") void refresh();
    } catch (e) {
      flash(String(e));
    }
  };

  return (
    <div className="mx-auto max-w-6xl">
      <div className="mb-5 flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">录屏</h1>
          <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">
            全局热键{" "}
            <kbd className="rounded bg-gray-200 px-1.5 py-0.5 text-xs dark:bg-gray-700">
              {settings?.hotkey ?? "Ctrl+Alt+R"}
            </kbd>{" "}
            开始 / 结束录制；成片存到 recordings 目录（H.264 mp4，编码由 ffmpeg 完成）。
          </p>
        </div>
        <div className="flex items-center gap-2">
          {!recording ? (
            <>
              <button
                onClick={() => void start(true)}
                className="flex items-center gap-2 rounded-md bg-red-500 px-3 py-2 text-sm font-medium text-white hover:bg-red-600"
              >
                <Crop size={16} /> 框选录制
              </button>
              <button
                onClick={() => void start(false)}
                className="flex items-center gap-2 rounded-md bg-gray-100 px-3 py-2 text-sm text-gray-700 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
              >
                <Monitor size={16} /> 录整屏
              </button>
            </>
          ) : (
            <>
              <span className="flex items-center gap-2 rounded-md bg-red-50 px-3 py-2 text-sm font-medium text-red-600 dark:bg-red-950 dark:text-red-300">
                <span
                  className={[
                    "inline-block h-2.5 w-2.5 rounded-full bg-red-500",
                    paused ? "opacity-40" : "animate-pulse",
                  ].join(" ")}
                />
                <span className="tabular-nums">{formatDuration(status?.elapsed_ms ?? 0)}</span>
                {paused && <span className="text-xs opacity-70">已暂停</span>}
              </span>
              <button
                onClick={() => void togglePause()}
                className="flex items-center gap-2 rounded-md bg-gray-100 px-3 py-2 text-sm text-gray-700 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
              >
                {paused ? <Play size={16} /> : <Pause size={16} />} {paused ? "继续" : "暂停"}
              </button>
              <button
                onClick={() => void stop()}
                className="flex items-center gap-2 rounded-md bg-red-500 px-3 py-2 text-sm font-medium text-white hover:bg-red-600"
              >
                <Square size={16} /> 停止
              </button>
            </>
          )}
          <button
            onClick={() => void invoke("recording_open_folder").catch((e) => flash(String(e)))}
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
            title="录屏设置"
            className="rounded-md bg-gray-100 p-2 text-gray-600 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-300 dark:hover:bg-gray-700"
          >
            <SettingsIcon size={16} />
          </button>
        </div>
      </div>

      {/* ffmpeg 状态：没有它录屏就是不可用的，所以不藏在设置里，直接摆在页面顶部。 */}
      {ffmpeg && !ffmpeg.found && (
        <div className="mb-4 flex items-start gap-2 rounded-md border border-amber-300 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-200">
          <AlertTriangle size={16} className="mt-0.5 shrink-0" />
          <div>
            <div className="font-medium">录屏不可用：{ffmpeg.error}</div>
            <button
              onClick={() => setShowSettings(true)}
              className="mt-1 text-xs underline underline-offset-2"
            >
              去设置里指定 ffmpeg 路径
            </button>
          </div>
        </div>
      )}

      {showSettings && settings && (
        <SettingsPanel
          settings={settings}
          ffmpeg={ffmpeg}
          saving={savingSettings}
          onDetect={detect}
          onSave={saveSettings}
        />
      )}

      {err && (
        <div className="mb-4 rounded-md bg-red-50 px-3 py-2 text-sm text-red-600 dark:bg-red-950 dark:text-red-300">
          {err}
        </div>
      )}

      {items.length === 0 && !loading ? (
        <div className="rounded-lg border border-dashed border-gray-300 py-16 text-center text-sm text-gray-400 dark:border-gray-700">
          还没有录屏。按 {settings?.hotkey ?? "Ctrl+Alt+R"} 或点上面的「框选录制」开始。
        </div>
      ) : (
        <div className="divide-y divide-gray-200 overflow-hidden rounded-lg border border-gray-200 dark:divide-gray-700 dark:border-gray-700">
          {items.map((it) => (
            <div key={it.path} className="group flex items-center gap-3 px-3 py-2.5">
              <Video size={18} className="shrink-0 text-gray-400" />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm text-gray-700 dark:text-gray-200" title={it.path}>
                  {it.name}
                </div>
                <div className="text-[11px] text-gray-400">
                  {it.duration_ms > 0 ? formatDuration(it.duration_ms) : "时长未知"}
                  {it.width > 0 && ` · ${it.width}×${it.height}`}
                  {it.fps > 0 && ` · ${it.fps}fps`}
                  {` · ${formatSize(it.size)}`}
                  {` · ${new Date(it.modified_ms).toLocaleString()}`}
                </div>
              </div>
              <div className="flex shrink-0 gap-1 opacity-0 transition-opacity group-hover:opacity-100">
                <button
                  onClick={() => void act("recording_open_file", it)}
                  title="用默认播放器打开"
                  className="rounded p-1.5 text-gray-500 hover:bg-gray-200 hover:text-blue-600 dark:hover:bg-gray-700"
                >
                  <Play size={15} />
                </button>
                <button
                  onClick={() => void act("recording_save_as", it)}
                  title="另存为…"
                  className="rounded p-1.5 text-gray-500 hover:bg-gray-200 hover:text-blue-600 dark:hover:bg-gray-700"
                >
                  <Download size={15} />
                </button>
                <button
                  onClick={() => void act("recording_reveal_in_folder", it)}
                  title="在文件夹中显示"
                  className="rounded p-1.5 text-gray-500 hover:bg-gray-200 hover:text-blue-600 dark:hover:bg-gray-700"
                >
                  <FolderSearch size={15} />
                </button>
                <button
                  onClick={() => {
                    if (window.confirm(`删除 ${it.name}？`)) void act("recording_delete", it, "已删除");
                  }}
                  title="删除"
                  className="rounded p-1.5 text-gray-500 hover:bg-gray-200 hover:text-red-600 dark:hover:bg-gray-700"
                >
                  <Trash2 size={15} />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {toast && (
        <div className="fixed bottom-6 left-1/2 -translate-x-1/2 rounded-md bg-black/80 px-4 py-2 text-sm text-white shadow-lg">
          {toast}
        </div>
      )}
    </div>
  );
}

/** 设置面板：热键 / 帧率 / 画质 / 光标 / 框选 / ffmpeg 路径。 */
function SettingsPanel({
  settings,
  ffmpeg,
  saving,
  onDetect,
  onSave,
}: {
  settings: RecordingSettings;
  ffmpeg: FfmpegInfo | null;
  saving: boolean;
  onDetect: (hint: string) => void;
  onSave: (s: RecordingSettings) => void;
}) {
  const [draft, setDraft] = useState<RecordingSettings>(settings);
  useEffect(() => setDraft(settings), [settings]);

  const set = <K extends keyof RecordingSettings>(k: K, v: RecordingSettings[K]) =>
    setDraft((d) => ({ ...d, [k]: v }));

  return (
    <div className="mb-5 space-y-4 rounded-lg border border-gray-200 p-4 dark:border-gray-700">
      <div className="grid grid-cols-2 gap-4">
        <label className="block">
          <span className="text-xs text-gray-500 dark:text-gray-400">全局热键</span>
          <input
            value={draft.hotkey}
            onChange={(e) => set("hotkey", e.target.value)}
            placeholder="Ctrl+Alt+R"
            className="mt-1 w-full rounded-md border border-gray-300 px-2 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-900"
          />
        </label>

        <label className="block">
          <span className="text-xs text-gray-500 dark:text-gray-400">帧率</span>
          <select
            value={draft.fps}
            onChange={(e) => set("fps", Number(e.target.value))}
            className="mt-1 w-full rounded-md border border-gray-300 px-2 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-900"
          >
            {FPS_CHOICES.map((f) => (
              <option key={f} value={f}>
                {f} fps
              </option>
            ))}
          </select>
        </label>

        <label className="block">
          <span className="text-xs text-gray-500 dark:text-gray-400">画质</span>
          <select
            value={draft.crf}
            onChange={(e) => set("crf", Number(e.target.value))}
            className="mt-1 w-full rounded-md border border-gray-300 px-2 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-900"
          >
            {QUALITY.map((q) => (
              <option key={q.crf} value={q.crf}>
                {q.label}
              </option>
            ))}
          </select>
        </label>

        <label className="block">
          <span className="text-xs text-gray-500 dark:text-gray-400">保存目录（留空用默认）</span>
          <input
            value={draft.save_dir}
            onChange={(e) => set("save_dir", e.target.value)}
            className="mt-1 w-full rounded-md border border-gray-300 px-2 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-900"
          />
        </label>
      </div>

      <div className="flex gap-6">
        <label className="flex items-center gap-2 text-sm text-gray-600 dark:text-gray-300">
          <input
            type="checkbox"
            checked={draft.capture_cursor}
            onChange={(e) => set("capture_cursor", e.target.checked)}
          />
          录入鼠标指针
        </label>
        <label className="flex items-center gap-2 text-sm text-gray-600 dark:text-gray-300">
          <input
            type="checkbox"
            checked={draft.select_region}
            onChange={(e) => set("select_region", e.target.checked)}
          />
          热键触发时先框选区域（关闭则直接录整块屏）
        </label>
      </div>

      <div>
        <span className="text-xs text-gray-500 dark:text-gray-400">
          ffmpeg 路径（留空 = 自动查找 PATH 与常见安装位置）
        </span>
        <div className="mt-1 flex gap-2">
          <input
            value={draft.ffmpeg_path}
            onChange={(e) => set("ffmpeg_path", e.target.value)}
            placeholder={String.raw`例如 C:\ffmpeg\bin\ffmpeg.exe`}
            className="flex-1 rounded-md border border-gray-300 px-2 py-1.5 text-sm dark:border-gray-600 dark:bg-gray-900"
          />
          <button
            onClick={() => onDetect(draft.ffmpeg_path)}
            className="rounded-md bg-gray-100 px-3 py-1.5 text-sm text-gray-700 hover:bg-gray-200 dark:bg-gray-800 dark:text-gray-200 dark:hover:bg-gray-700"
          >
            检测
          </button>
        </div>
        {ffmpeg && (
          <div
            className={[
              "mt-1.5 flex items-start gap-1.5 text-xs",
              ffmpeg.found ? "text-green-600 dark:text-green-400" : "text-amber-600 dark:text-amber-400",
            ].join(" ")}
          >
            {ffmpeg.found ? (
              <CheckCircle2 size={13} className="mt-0.5 shrink-0" />
            ) : (
              <AlertTriangle size={13} className="mt-0.5 shrink-0" />
            )}
            <span className="break-all">
              {ffmpeg.found ? `${ffmpeg.path}　${ffmpeg.version}` : ffmpeg.error}
            </span>
          </div>
        )}
      </div>

      <div className="flex justify-end">
        <button
          onClick={() => onSave(draft)}
          disabled={saving}
          className="rounded-md bg-blue-500 px-4 py-1.5 text-sm font-medium text-white hover:bg-blue-600 disabled:opacity-60"
        >
          保存设置
        </button>
      </div>
    </div>
  );
}
