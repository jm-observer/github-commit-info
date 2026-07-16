import { useCallback, useEffect, useState } from 'react'
import { Download, Play, Square, Trash2, RefreshCw, ShieldAlert } from 'lucide-react'
import {
  NetPolicyAPI,
  type CaptureOpts,
  type CaptureSession,
  type CaptureTarget,
} from '../api/tauri-client'
import { btn, Section } from '../uiHelpers'

const DEFAULT_OPTS: CaptureOpts = { snap_len: 128, file_size_mib: 128, max_secs: 120 }
const READ_CHUNK = 512 * 1024 // 与后端 CAPTURE_READ_MAX_LEN 一致

function stateLabel(s: CaptureSession['state']): string {
  return {
    preparing: '准备中', running: '抓取中', stopping: '停止中', converting: '转换中',
    done: '已完成', failed: '失败', orphaned: '孤立',
  }[s]
}

function targetLabel(t: CaptureTarget): string {
  switch (t.target) {
    case 'all': return '全 TUN'
    case 'process': return `进程 ${t.value.value}`
    case 'domain': return `域名 ${t.value}`
    case 'ip': return `IP ${t.value}`
  }
}

/** 把 base64 解码为 Uint8Array（浏览器原生 atob，避免引入依赖）。 */
function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/**
 * 抓包页（抓包设计 §12）：全 TUN / 定向抓包 + 会话列表 + 停止/删除/保存 pcapng。
 * 高敏感操作——Start 前二次确认；完整包模式额外强调。保存经 CaptureRead 分块 → 浏览器 Blob 下载。
 */
export function CapturePage() {
  const [sessions, setSessions] = useState<CaptureSession[]>([])
  const [opts, setOpts] = useState<CaptureOpts>(DEFAULT_OPTS)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  const [confirmOpen, setConfirmOpen] = useState(false)

  const refresh = useCallback(async () => {
    try {
      setSessions(await NetPolicyAPI.captureList())
      setErr(null)
    } catch (e) {
      setErr(String(e))
    }
  }, [])

  useEffect(() => {
    void refresh()
    // 有运行中会话时轮询对齐（时间上限到时 agent 自动 stop）。
    const t = setInterval(() => void refresh(), 3000)
    return () => clearInterval(t)
  }, [refresh])

  const fullPacket = opts.snap_len === 0

  async function doStart() {
    setConfirmOpen(false)
    setBusy(true)
    setErr(null)
    try {
      // MVP UI 先支持全 TUN；定向抓包由「流量」页行操作发起（传 process/domain target）。
      await NetPolicyAPI.captureStart({ target: 'all' }, opts)
      await refresh()
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  async function stop(id: string) {
    setBusy(true)
    try { await NetPolicyAPI.captureStop(id); await refresh() }
    catch (e) { setErr(String(e)) }
    finally { setBusy(false) }
  }

  async function del(id: string) {
    setBusy(true)
    try { await NetPolicyAPI.captureDelete(id); await refresh() }
    catch (e) { setErr(String(e)) }
    finally { setBusy(false) }
  }

  async function save(sess: CaptureSession) {
    setBusy(true)
    setErr(null)
    try {
      const parts: Uint8Array[] = []
      let offset = 0
      for (;;) {
        const chunk = await NetPolicyAPI.captureRead(sess.id, offset, READ_CHUNK)
        const bytes = b64ToBytes(chunk.data_base64)
        parts.push(bytes)
        offset += bytes.length
        if (chunk.eof || bytes.length === 0) break
      }
      const blob = new Blob(parts as BlobPart[], { type: 'application/octet-stream' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = sess.file_name ?? `${sess.id}.pcapng`
      a.click()
      URL.revokeObjectURL(url)
    } catch (e) {
      setErr(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="space-y-6">
      <Section
        title="新建抓包"
        description="抓取整个 TUN 的限时快照，导出 Wireshark 可打开的 pcapng"
        right={
          <button className={btn('ghost')} onClick={() => void refresh()} disabled={busy}>
            <RefreshCw size={14} /> 刷新
          </button>
        }
      >
        <div className="grid grid-cols-1 gap-3 text-sm sm:grid-cols-3">
          <label className="flex flex-col gap-1">
            截断长度（字节，0=完整包）
            <input
              type="number"
              className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
              value={opts.snap_len}
              onChange={(e) => setOpts({ ...opts, snap_len: Number(e.target.value) })}
            />
          </label>
          <label className="flex flex-col gap-1">
            容量上限（MiB，16–512）
            <input
              type="number"
              className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
              value={opts.file_size_mib}
              onChange={(e) => setOpts({ ...opts, file_size_mib: Number(e.target.value) })}
            />
          </label>
          <label className="flex flex-col gap-1">
            时长上限（秒，10–600）
            <input
              type="number"
              className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
              value={opts.max_secs}
              onChange={(e) => setOpts({ ...opts, max_secs: Number(e.target.value) })}
            />
          </label>
        </div>
        {fullPacket && (
          <div className="flex items-start gap-2 rounded-md bg-red-100 px-3 py-2 text-xs text-red-800 dark:bg-red-950 dark:text-red-300">
            <ShieldAlert size={14} className="mt-0.5 shrink-0" />
            <span>
              <b>完整包模式（高敏感）</b>：pcapng 可能包含 Cookie、Authorization、明文 HTTP、DNS 查询及业务载荷。
              HTTPS/QUIC 内容仍是密文。导出后请自行妥善保管。
            </span>
          </div>
        )}
        <button className={btn('primary')} onClick={() => setConfirmOpen(true)} disabled={busy}>
          <Play size={14} /> 开始全 TUN 抓包
        </button>
        <p className="text-[11px] text-gray-400 dark:text-gray-500">
          按进程 / 域名定向抓包请在「流量」页对应行发起（开始时按当前连接端点冻结，新连接不自动纳入）。
          LAN 与未入 TUN 的 IPv6 不在本次抓包范围。
        </p>
      </Section>

      {err && (
        <div className="rounded-md bg-amber-100 px-3 py-2 text-xs text-amber-800 dark:bg-amber-950 dark:text-amber-300">
          {err}
        </div>
      )}

      <Section title="抓包会话" description="仅同时允许一个进行中的会话">
        {sessions.length === 0 ? (
          <p className="py-6 text-center text-sm text-gray-500">暂无抓包会话。</p>
        ) : (
          <div className="space-y-2">
            {sessions.map((s) => (
              <div
                key={s.id}
                className="flex flex-wrap items-center gap-3 rounded-md border border-gray-200 px-3 py-2 text-sm dark:border-gray-800"
              >
                <span className="font-mono text-xs text-gray-500">{s.id.slice(0, 12)}…</span>
                <span className="rounded bg-gray-100 px-1.5 py-0.5 text-xs dark:bg-gray-800">
                  {stateLabel(s.state)}
                </span>
                <span>{targetLabel(s.target)}</span>
                {s.endpoint_count > 0 && (
                  <span className="text-xs text-gray-500">{s.endpoint_count} 端点</span>
                )}
                {s.bytes != null && (
                  <span className="text-xs text-gray-500">{(s.bytes / 1024).toFixed(0)} KB</span>
                )}
                {s.error && (
                  <span className="text-xs text-red-600" title={s.error.message}>
                    [{s.error.kind}]
                  </span>
                )}
                <div className="ml-auto flex gap-2">
                  {s.state === 'running' && (
                    <button className={btn('ghost')} onClick={() => void stop(s.id)} disabled={busy}>
                      <Square size={13} /> 停止
                    </button>
                  )}
                  {s.state === 'done' && (
                    <button className={btn('primary')} onClick={() => void save(s)} disabled={busy}>
                      <Download size={13} /> 保存 pcapng
                    </button>
                  )}
                  <button
                    className={btn('danger')}
                    onClick={() => void del(s.id)}
                    disabled={busy || s.state === 'running'}
                    title={s.state === 'running' ? '请先停止' : '删除'}
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
                {s.known_limits.length > 0 && (
                  <ul className="w-full list-disc pl-5 text-[11px] text-gray-400 dark:text-gray-500">
                    {s.known_limits.map((k, i) => (
                      <li key={i}>{k}</li>
                    ))}
                  </ul>
                )}
              </div>
            ))}
          </div>
        )}
      </Section>

      {confirmOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
          <div className="w-full max-w-md space-y-4 rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
            <h3 className="flex items-center gap-2 text-sm font-semibold">
              <ShieldAlert size={16} className="text-amber-500" /> 确认开始抓包
            </h3>
            <ul className="space-y-1 text-sm text-gray-600 dark:text-gray-300">
              <li>目标：全 TUN（所有受管出站流量）</li>
              <li>截断：{fullPacket ? '完整包（高敏感）' : `每包前 ${opts.snap_len} 字节`}</li>
              <li>时长上限：{opts.max_secs} 秒</li>
              <li>容量上限：{opts.file_size_mib} MiB（循环覆盖最旧）</li>
            </ul>
            <p className="text-xs text-gray-500">
              抓包不解密 HTTPS/QUIC；不改变路由与网络姿态。
            </p>
            <div className="flex justify-end gap-2">
              <button className={btn('ghost')} onClick={() => setConfirmOpen(false)}>取消</button>
              <button className={btn('primary')} onClick={() => void doStart()}>确认开始</button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
