import { useState, useEffect, useCallback } from "react";
import { useNavigate } from "react-router-dom";
import { invoke } from "@tauri-apps/api/core";
import {
  LogIn,
  LogOut,
  Activity,
  Search,
  RefreshCw,
  User,
  Wifi,
  WifiOff,
  ChevronDown,
  ChevronUp,
  ExternalLink,
  Info,
} from "lucide-react";

// ============ 类型定义 ============

interface NetStatus {
  mode: "auto" | "lan" | "wan";
  picked: "auto" | "lan" | "wan";
  g10_base: string;
  configured: boolean;
}

interface CookieSummary {
  state: "ok" | "no_login_window";
  hint?: string;
  count?: number;
  has_ms_token?: boolean;
  has_ms_token_any?: boolean;
  names?: string[];
  // 摘要列表（非敏感：name/len/domain 等）
  all?: Array<{
    name: string;
    len: number;
    domain: string;
    path: string;
    http_only: boolean;
    secure: boolean;
  }>;
}

interface PingResult {
  state: "ok" | "unconfigured" | "unreachable" | "http_err";
  status?: number;
  latency_ms?: number;
  server_base?: string;
  server_version?: string;
  error?: string;
}

interface ThsStatus {
  exists: boolean;
  count: number;
  has_required: boolean;
  missing: string[];
  ticket_expires_at?: string;
  ticket_is_session: boolean;
  path: string;
}

// ============ 小组件 ============

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <h2 className="mb-3 text-sm font-semibold uppercase tracking-wider text-gray-500 dark:text-gray-400">
      {children}
    </h2>
  );
}

function StatusBadge({
  ok,
  label,
}: {
  ok: boolean | null;
  label: string;
}) {
  const color =
    ok === null
      ? "bg-gray-300 dark:bg-gray-600"
      : ok
        ? "bg-green-500"
        : "bg-red-500";
  return (
    <span className="flex items-center gap-1.5 text-xs">
      <span className={`inline-block h-2 w-2 rounded-full ${color}`} />
      {label}
    </span>
  );
}

function ActionButton({
  onClick,
  disabled,
  variant = "default",
  children,
  icon: Icon,
}: {
  onClick: () => void;
  disabled?: boolean;
  variant?: "default" | "danger" | "success";
  children: React.ReactNode;
  icon?: React.ElementType;
}) {
  const colors = {
    default:
      "bg-blue-600 hover:bg-blue-700 active:bg-blue-800 text-white disabled:bg-blue-300",
    danger:
      "bg-red-600 hover:bg-red-700 active:bg-red-800 text-white disabled:bg-red-300",
    success:
      "bg-green-600 hover:bg-green-700 active:bg-green-800 text-white disabled:bg-green-300",
  };
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={`flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm transition-colors disabled:cursor-not-allowed ${colors[variant]}`}
    >
      {Icon && <Icon size={14} />}
      {children}
    </button>
  );
}

// ============ G10 配置区（只读，编辑入口在设置页） ============

function G10SettingsSection() {
  const navigate = useNavigate();
  const [g10Base, setG10Base] = useState<string>("");
  const [picked, setPicked] = useState<string>("");

  useEffect(() => {
    invoke<NetStatus>("net_resolve_status")
      .then((s) => {
        setG10Base(s.configured ? s.g10_base : "");
        setPicked(s.picked === "lan" ? "局域网" : s.picked === "wan" ? "外网" : "");
      })
      .catch((e) => console.error("load net status:", e));
  }, []);

  return (
    <div className="rounded-lg border border-blue-100 bg-blue-50 p-4 dark:border-blue-900/30 dark:bg-blue-950/20">
      <div className="mb-3 flex items-center gap-2">
        <Info size={14} className="text-blue-500" />
        <span className="text-xs text-blue-700 dark:text-blue-400">
          G10 base / token 已统一在设置页配置，此处仅展示当前值。
        </span>
      </div>
      <div className="space-y-2">
        <div>
          <span className="text-xs text-gray-500 dark:text-gray-400">当前生效 G10：</span>
          <span className="ml-1 text-sm font-mono text-gray-800 dark:text-gray-200">
            {g10Base
              ? <>{g10Base}{picked && <span className="ml-1 text-xs text-blue-600 dark:text-blue-400">（{picked}）</span>}</>
              : <span className="text-yellow-600 dark:text-yellow-400">未配置</span>}
          </span>
        </div>
        <div>
          <span className="text-xs text-gray-500 dark:text-gray-400">Auth Token：</span>
          <span className="ml-1 text-sm text-gray-500 dark:text-gray-400">（已隐藏）</span>
        </div>
      </div>
      <button
        onClick={() => navigate("/settings")}
        className="mt-3 flex items-center gap-1.5 rounded-md border border-blue-300 bg-white px-3 py-1.5 text-xs text-blue-700 hover:bg-blue-50 dark:border-blue-700 dark:bg-gray-800 dark:text-blue-400 dark:hover:bg-gray-700"
      >
        <ExternalLink size={12} />
        去设置页修改
      </button>
    </div>
  );
}

// ============ 抖音登录操作区 ============

function DouyinLoginSection() {
  const [loading, setLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  async function handleOpen() {
    setLoading("open");
    setError(null);
    setMsg(null);
    try {
      await invoke("cookie_open_douyin_login");
      setMsg("抖音登录窗已打开");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(null);
    }
  }

  async function handleClose() {
    setLoading("close");
    setError(null);
    setMsg(null);
    try {
      await invoke("cookie_close_douyin_login");
      setMsg("抖音登录窗已关闭");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(null);
    }
  }

  return (
    <div className="rounded-lg border border-gray-200 p-4 dark:border-gray-700">
      <SectionTitle>抖音登录</SectionTitle>
      <div className="flex flex-wrap items-center gap-2">
        <ActionButton
          onClick={handleOpen}
          disabled={loading !== null}
          icon={LogIn}
        >
          {loading === "open" ? "打开中…" : "打开抖音登录窗"}
        </ActionButton>
        <ActionButton
          onClick={handleClose}
          disabled={loading !== null}
          variant="danger"
          icon={LogOut}
        >
          {loading === "close" ? "关闭中…" : "关闭"}
        </ActionButton>
      </div>
      {msg && (
        <p className="mt-2 text-xs text-green-600 dark:text-green-400">{msg}</p>
      )}
      {error && <p className="mt-2 text-xs text-red-500">{error}</p>}
    </div>
  );
}

// ============ 同花顺登录操作区 ============

function ThsLoginSection() {
  const [loading, setLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [status, setStatus] = useState<ThsStatus | null>(null);
  const [showPath, setShowPath] = useState(false);

  const loadStatus = useCallback(async () => {
    try {
      const s = await invoke<ThsStatus>("cookie_ths_status");
      setStatus(s);
    } catch (e) {
      console.error("ths_status:", e);
    }
  }, []);

  useEffect(() => {
    loadStatus();
  }, [loadStatus]);

  async function handleOpen() {
    setLoading("open");
    setError(null);
    setMsg(null);
    try {
      await invoke("cookie_open_ths_login");
      setMsg("同花顺登录窗已打开");
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(null);
    }
  }

  async function handleClose() {
    setLoading("close");
    setError(null);
    setMsg(null);
    try {
      await invoke("cookie_close_ths_login");
      setMsg("同花顺登录窗已关闭");
      await loadStatus();
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(null);
    }
  }

  return (
    <div className="rounded-lg border border-gray-200 p-4 dark:border-gray-700">
      <SectionTitle>同花顺登录</SectionTitle>
      <div className="flex flex-wrap items-center gap-2">
        <ActionButton
          onClick={handleOpen}
          disabled={loading !== null}
          icon={LogIn}
        >
          {loading === "open" ? "打开中…" : "打开同花顺登录窗"}
        </ActionButton>
        <ActionButton
          onClick={handleClose}
          disabled={loading !== null}
          variant="danger"
          icon={LogOut}
        >
          {loading === "close" ? "关闭中…" : "关闭"}
        </ActionButton>
        <ActionButton onClick={loadStatus} icon={RefreshCw}>
          刷新状态
        </ActionButton>
      </div>
      {msg && (
        <p className="mt-2 text-xs text-green-600 dark:text-green-400">{msg}</p>
      )}
      {error && <p className="mt-2 text-xs text-red-500">{error}</p>}
      {status && (
        <div className="mt-3 text-xs text-gray-600 dark:text-gray-400">
          <div className="flex items-center gap-3">
            <StatusBadge ok={status.has_required} label="登录态" />
            <span>共 {status.count} 条 cookie</span>
          </div>
          {!status.has_required && status.missing.length > 0 && (
            <p className="mt-1 text-amber-600 dark:text-amber-400">
              缺少: {status.missing.join(", ")}
            </p>
          )}
          {status.ticket_expires_at && (
            <p className="mt-1">ticket 过期: {status.ticket_expires_at}</p>
          )}
          {status.ticket_is_session && (
            <p className="mt-1 text-amber-600 dark:text-amber-400">
              ticket 为 session cookie（关浏览器即失效）
            </p>
          )}
          <button
            className="mt-1 flex items-center gap-1 text-gray-400 hover:text-gray-600"
            onClick={() => setShowPath((v) => !v)}
          >
            {showPath ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
            路径
          </button>
          {showPath && (
            <p className="break-all font-mono text-xs text-gray-400">
              {status.path}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

// ============ G10 ping 区 ============

function PingSection() {
  const [result, setResult] = useState<PingResult | null>(null);
  const [loading, setLoading] = useState(false);

  async function handlePing() {
    setLoading(true);
    try {
      const r = await invoke<PingResult>("cookie_ping_server");
      setResult(r);
    } catch (e) {
      setResult({ state: "unreachable", error: String(e) });
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="rounded-lg border border-gray-200 p-4 dark:border-gray-700">
      <SectionTitle>G10 连通性</SectionTitle>
      <div className="flex flex-wrap items-center gap-3">
        <ActionButton onClick={handlePing} disabled={loading} icon={Activity}>
          {loading ? "检测中…" : "Ping G10"}
        </ActionButton>
        {result && (
          <span className="flex items-center gap-1.5 text-xs">
            {result.state === "ok" ? (
              <>
                <Wifi size={14} className="text-green-500" />
                <span className="text-green-600 dark:text-green-400">
                  OK {result.latency_ms}ms
                  {result.server_version ? ` · v${result.server_version}` : ""}
                </span>
              </>
            ) : (
              <>
                <WifiOff size={14} className="text-red-500" />
                <span className="text-red-500">
                  {result.state === "unconfigured"
                    ? "未配置 G10 base"
                    : result.state === "http_err"
                      ? `HTTP ${result.status}`
                      : result.error ?? "连接失败"}
                </span>
              </>
            )}
          </span>
        )}
      </div>
    </div>
  );
}

// ============ Cookie 状态诊断区 ============

function CookieInspectSection() {
  const [data, setData] = useState<CookieSummary | null>(null);
  const [loading, setLoading] = useState(false);
  const [expanded, setExpanded] = useState(false);

  async function handleInspect() {
    setLoading(true);
    try {
      const r = await invoke<CookieSummary>("cookie_inspect_cookies");
      setData(r);
    } catch (e) {
      setData({ state: "no_login_window", hint: String(e) });
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="rounded-lg border border-gray-200 p-4 dark:border-gray-700">
      <SectionTitle>Cookie 状态诊断</SectionTitle>
      <ActionButton onClick={handleInspect} disabled={loading} icon={Search}>
        {loading ? "读取中…" : "诊断当前 Cookie"}
      </ActionButton>
      {data && (
        <div className="mt-3 text-xs">
          {data.state === "no_login_window" ? (
            <p className="text-amber-600 dark:text-amber-400">
              {data.hint ?? "请先打开抖音登录窗"}
            </p>
          ) : (
            <div className="space-y-1 text-gray-600 dark:text-gray-400">
              <div className="flex items-center gap-4">
                <StatusBadge ok={true} label={`已读取 ${data.count} 条`} />
                <StatusBadge
                  ok={data.has_ms_token_any ?? false}
                  label={
                    data.has_ms_token_any
                      ? "msToken 已就绪"
                      : "msToken 缺失（非必需）"
                  }
                />
              </div>
              <button
                className="flex items-center gap-1 text-gray-400 hover:text-gray-600"
                onClick={() => setExpanded((v) => !v)}
              >
                {expanded ? <ChevronUp size={12} /> : <ChevronDown size={12} />}
                查看 cookie 清单（{data.names?.length ?? 0} 项，不含原文）
              </button>
              {expanded && data.all && (
                <div className="mt-1 max-h-48 overflow-y-auto rounded-md bg-gray-50 p-2 dark:bg-gray-900">
                  <table className="w-full text-xs">
                    <thead>
                      <tr className="text-gray-400">
                        <th className="pb-1 pr-3 text-left font-medium">
                          name
                        </th>
                        <th className="pb-1 pr-3 text-right font-medium">
                          len
                        </th>
                        <th className="pb-1 text-left font-medium">domain</th>
                      </tr>
                    </thead>
                    <tbody>
                      {data.all.map((c, i) => (
                        <tr key={i} className="border-t border-gray-100 dark:border-gray-800">
                          <td className="py-0.5 pr-3 font-mono">{c.name}</td>
                          <td className="py-0.5 pr-3 text-right text-gray-400">
                            {c.len}
                          </td>
                          <td className="py-0.5 text-gray-400">{c.domain}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ============ 博主解析同步区 ============

function TrackCreatorSection() {
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<unknown>(null);
  const [error, setError] = useState<string | null>(null);

  async function handleTrack() {
    setLoading(true);
    setError(null);
    setResult(null);
    try {
      const r = await invoke("cookie_track_current_creator");
      setResult(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="rounded-lg border border-gray-200 p-4 dark:border-gray-700">
      <SectionTitle>解析并同步博主</SectionTitle>
      <p className="mb-3 text-xs text-gray-500 dark:text-gray-400">
        在抖音登录窗中打开博主主页后点击，将当前 URL 解析为博主写入 G10。
      </p>
      <ActionButton onClick={handleTrack} disabled={loading} icon={User}>
        {loading ? "解析中…" : "解析当前博主"}
      </ActionButton>
      {result !== null && (
        <pre className="mt-2 max-h-32 overflow-auto rounded-md bg-gray-50 p-2 text-xs dark:bg-gray-900">
          {JSON.stringify(result as object, null, 2)}
        </pre>
      )}
      {error && <p className="mt-2 text-xs text-red-500">{error}</p>}
    </div>
  );
}

// ============ 主页面 ============

export default function CookiePage() {
  return (
    <div className="mx-auto max-w-2xl space-y-4">
      <div>
        <h1 className="text-xl font-semibold">Cookie 采集</h1>
        <p className="mt-1 text-sm text-gray-500 dark:text-gray-400">
          管理抖音 / 同花顺登录态，同步 Cookie 到 G10 server。bridge 固定监听{" "}
          <code className="rounded bg-gray-100 px-1 dark:bg-gray-800">
            127.0.0.1:28788
          </code>
          。
        </p>
      </div>

      <G10SettingsSection />
      <PingSection />
      <DouyinLoginSection />
      <CookieInspectSection />
      <TrackCreatorSection />
      <ThsLoginSection />
    </div>
  );
}
