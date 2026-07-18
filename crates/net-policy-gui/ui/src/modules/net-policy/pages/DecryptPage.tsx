import { useCallback, useEffect, useState } from 'react'
import {
  Download, Play, Square, Trash2, RefreshCw, ShieldAlert, ShieldCheck, KeyRound, Lock,
} from 'lucide-react'
import {
  NetPolicyAPI,
  DEFAULT_DECRYPT_OPTS,
  type CaStatus,
  type DecryptOpts,
  type DecryptSession,
  type DomainCounters,
  type ProcessCandidate,
} from '../api/tauri-client'
import { btn, Section } from '../uiHelpers'

const READ_CHUNK = 512 * 1024 // 与后端 DECRYPT_READ_MAX_LEN 一致

function stateLabel(s: DecryptSession['state']): string {
  return {
    checking_ca: '校验 CA', preparing: '准备中', decrypting: '解密中',
    stopping: '停止中', finalizing: '收尾中', done: '已完成', failed: '失败',
  }[s]
}

function caStateLabel(s: CaStatus['state']): string {
  return { absent: '未创建', installed: '已信任', broken: '损坏（私钥缺失/指纹不符）' }[s]
}

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/** 每域名计数是否表示"实际拿到了明文"。 */
function anyDecrypted(pd: Record<string, DomainCounters>): boolean {
  return Object.values(pd).some((c) => c.decrypted > 0)
}

/**
 * L4 应用明文页（抓包设计 §17.9）：独立高风险功能。
 * - CA 卡片：创建 / 装信任库（弹 Windows 确认框）/ 移除；显示作用域、owner、指纹、私钥状态。
 * - 会话：精确进程实例 + 必填域名 allowlist；常驻红色"TLS 解密中"；每域名 decrypted/passthrough/
 *   pinned/quic/failed 计数（不只显示总"成功"）；Raw 档红标 + 二次确认。停止按钮始终可见。
 */
export function DecryptPage() {
  const [ca, setCa] = useState<CaStatus | null>(null)
  const [sessions, setSessions] = useState<DecryptSession[]>([])
  const [candidates, setCandidates] = useState<ProcessCandidate[]>([])
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)

  // Start 表单
  const [pid, setPid] = useState<number | null>(null)
  const [domainsText, setDomainsText] = useState('')
  const [opts, setOpts] = useState<DecryptOpts>(DEFAULT_DECRYPT_OPTS)
  const [confirmOpen, setConfirmOpen] = useState(false)

  const refresh = useCallback(async () => {
    try {
      const [c, l] = await Promise.all([NetPolicyAPI.decryptCaStatus(), NetPolicyAPI.decryptList()])
      setCa(c)
      setSessions(l)
      setErr(null)
    } catch (e) {
      setErr(String(e))
    }
  }, [])

  useEffect(() => {
    void refresh()
    NetPolicyAPI.listProcessCandidates().then(setCandidates).catch(() => {})
    const t = setInterval(() => void refresh(), 3000)
    return () => clearInterval(t)
  }, [refresh])

  const installed = ca?.state === 'installed' && !!ca.owner_sid
  const raw = opts.redact_profile === 'raw'
  const activeSession = sessions.find((s) => s.state === 'decrypting' || s.state === 'preparing')

  async function run<T>(fn: () => Promise<T>) {
    setBusy(true)
    setErr(null)
    try { await fn(); await refresh() }
    catch (e) { setErr(String(e)) }
    finally { setBusy(false) }
  }

  function parseDomains(): string[] {
    return domainsText.split(/[\s,]+/).map((d) => d.trim().toLowerCase()).filter(Boolean)
  }

  async function doStart() {
    setConfirmOpen(false)
    const cand = candidates.find((c) => c.pid === pid)
    if (!cand) { setErr('请先选择目标进程'); return }
    const domains = parseDomains()
    if (domains.length === 0) { setErr('请至少填写一个 allowlist 域名'); return }
    await run(() =>
      NetPolicyAPI.decryptStart(
        // created_at_100ns=0：agent 在启动时按 PID 重读进程创建时间与路径（防 PID 复用，§17.5）。
        { process: { pid: cand.pid, created_at_100ns: 0, path: cand.path }, domains },
        opts,
      ),
    )
  }

  async function save(sess: DecryptSession, artifact: 'http_jsonl' | 'manifest') {
    setBusy(true)
    setErr(null)
    try {
      const parts: Uint8Array[] = []
      let offset = 0
      for (;;) {
        const chunk = await NetPolicyAPI.decryptRead(sess.id, artifact, offset, READ_CHUNK)
        const bytes = b64ToBytes(chunk.data_base64)
        parts.push(bytes)
        offset += bytes.length
        if (chunk.eof || bytes.length === 0) break
      }
      const blob = new Blob(parts as BlobPart[], { type: 'application/octet-stream' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = artifact === 'http_jsonl' ? `${sess.id}-http.jsonl` : `${sess.id}-manifest.json`
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
      {/* 原理与风险说明 */}
      <div className="flex items-start gap-2 rounded-md border border-amber-300 bg-amber-50 px-4 py-3 text-xs text-amber-900 dark:border-amber-800 dark:bg-amber-950/40 dark:text-amber-200">
        <ShieldAlert size={16} className="mt-0.5 shrink-0" />
        <div className="space-y-1">
          <div className="font-semibold">应用明文（TLS 解密）是独立的高风险诊断功能</div>
          <p>
            它主动终止客户端 TLS，用专用调试 CA 动态签发叶子证书，能看到 HTTP 请求/响应、Cookie、
            Authorization、表单与 WebSocket 消息，也会改变 TLS 握手与协议特征。仅用于你拥有或明确获授权分析的
            设备与流量。默认脱敏核心凭据；QUIC/被 pinning 的连接不会被解密，会诚实标注。
          </p>
        </div>
      </div>

      {err && (
        <div className="rounded-md bg-amber-100 px-3 py-2 text-xs text-amber-800 dark:bg-amber-950 dark:text-amber-300">
          {err}
        </div>
      )}

      {/* CA 卡片 */}
      <Section
        title="专用调试 CA"
        description="仅装入当前用户 CurrentUser\Root；私钥经 DPAPI 加密留在服务端，永不出管道"
        right={
          <button className={btn('ghost')} onClick={() => void refresh()} disabled={busy}>
            <RefreshCw size={14} /> 刷新
          </button>
        }
      >
        <div className="flex flex-wrap items-center gap-3 text-sm">
          <span className={`inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs ${
            installed ? 'bg-green-100 text-green-700 dark:bg-green-950/40 dark:text-green-300'
              : ca?.state === 'broken' ? 'bg-red-100 text-red-700 dark:bg-red-950/40 dark:text-red-300'
              : 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-300'
          }`}>
            {installed ? <ShieldCheck size={13} /> : <KeyRound size={13} />}
            {ca ? caStateLabel(ca.state) : '…'}
          </span>
          {installed && !activeSession && (
            <span className="text-xs text-gray-500">已信任，未拦截</span>
          )}
          {ca?.thumbprint && (
            <span className="font-mono text-[11px] text-gray-500" title={ca.thumbprint}>
              指纹 {ca.thumbprint.slice(0, 16)}…
            </span>
          )}
          {ca?.owner_sid && <span className="text-[11px] text-gray-400">owner {ca.owner_sid}</span>}
          {ca?.store_scope && <span className="text-[11px] text-gray-400">scope {ca.store_scope}</span>}
        </div>
        <div className="flex flex-wrap gap-2">
          {ca?.state === 'absent' && (
            <button className={btn('primary')} onClick={() => void run(NetPolicyAPI.decryptCaCreate)} disabled={busy}>
              <KeyRound size={14} /> 创建调试 CA
            </button>
          )}
          {ca?.state !== 'absent' && !installed && (
            <button className={btn('primary')} onClick={() => void run(NetPolicyAPI.decryptCaInstall)} disabled={busy}>
              <ShieldCheck size={14} /> 安装到信任库（弹 Windows 确认框）
            </button>
          )}
          {ca?.state !== 'absent' && (
            <button className={btn('danger')} onClick={() => void run(NetPolicyAPI.decryptCaRemove)} disabled={busy || !!activeSession}>
              <Trash2 size={14} /> 移除 CA 与信任
            </button>
          )}
        </div>
      </Section>

      {/* 活跃会话红色横幅 */}
      {activeSession && (
        <div className="flex items-center gap-2 rounded-md bg-red-600 px-4 py-3 text-sm font-semibold text-white">
          <Lock size={16} className="shrink-0 animate-pulse" />
          正在 TLS 解密（{activeSession.target.domains.join(', ')}）——
          {activeSession.opts.redact_profile === 'raw' ? ' Raw 原文模式' : ' 已脱敏'}
          <button
            className="ml-auto rounded bg-white/20 px-3 py-1 text-xs hover:bg-white/30"
            onClick={() => void run(() => NetPolicyAPI.decryptStop(activeSession.id))}
            disabled={busy}
          >
            <Square size={12} className="mr-1 inline" /> 立即停止
          </button>
        </div>
      )}

      {/* 新建会话 */}
      <Section title="新建解密会话" description="精确进程实例 + 必填域名 allowlist；开始时冻结，新连接不自动纳入">
        <div className="grid grid-cols-1 gap-3 text-sm sm:grid-cols-2">
          <label className="flex flex-col gap-1">
            目标进程
            <select
              className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
              value={pid ?? ''}
              onChange={(e) => setPid(e.target.value ? Number(e.target.value) : null)}
            >
              <option value="">— 选择当前活跃进程 —</option>
              {candidates.map((c) => (
                <option key={`${c.pid}-${c.path}`} value={c.pid}>
                  {c.name}（PID {c.pid}）
                </option>
              ))}
            </select>
          </label>
          <label className="flex flex-col gap-1">
            域名 allowlist（逗号/换行分隔，最多 32）
            <textarea
              className="rounded border px-2 py-1 font-mono text-xs dark:border-gray-700 dark:bg-gray-800"
              rows={2}
              placeholder="api.example.com&#10;example.com"
              value={domainsText}
              onChange={(e) => setDomainsText(e.target.value)}
            />
          </label>
          <label className="flex flex-col gap-1">
            时长上限（秒，10–300）
            <input
              type="number"
              className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
              value={opts.max_secs}
              onChange={(e) => setOpts({ ...opts, max_secs: Number(e.target.value) })}
            />
          </label>
          <div className="flex flex-col gap-1.5 text-xs">
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={opts.capture_bodies}
                onChange={(e) => setOpts({ ...opts, capture_bodies: e.target.checked })} />
              采集正文（默认只记方法/URL/状态/头）
            </label>
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={opts.force_tcp_for_quic}
                onChange={(e) => setOpts({ ...opts, force_tcp_for_quic: e.target.checked })} />
              逼 QUIC 回退 TCP（阻目标进程+域名的 UDP/443，改变应用行为）
            </label>
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={raw}
                onChange={(e) => setOpts({ ...opts, redact_profile: e.target.checked ? 'raw' : 'default' })} />
              <span className={raw ? 'font-semibold text-red-600' : ''}>Raw 原文模式（保留凭据明文）</span>
            </label>
          </div>
        </div>
        {raw && (
          <div className="flex items-start gap-2 rounded-md bg-red-100 px-3 py-2 text-xs text-red-800 dark:bg-red-950 dark:text-red-300">
            <ShieldAlert size={14} className="mt-0.5 shrink-0" />
            <span><b>Raw 模式</b>：不脱敏，将保留 Cookie、Authorization、token 等原文，且保留时限更短。请确保有必要。</span>
          </div>
        )}
        <button
          className={btn('primary')}
          onClick={() => setConfirmOpen(true)}
          disabled={busy || !installed || !!activeSession}
          title={!installed ? '请先创建并安装调试 CA' : activeSession ? '已有会话进行中' : ''}
        >
          <Play size={14} /> 开始解密
        </button>
        {!installed && (
          <p className="text-[11px] text-gray-400">需先创建并安装调试 CA 才能开始解密。</p>
        )}
      </Section>

      {/* 会话列表 + 每域名计数 */}
      <Section title="解密会话" description="每域名分别显示 已解密 / 透传 / 被 pinning / QUIC / 失败">
        {sessions.length === 0 ? (
          <p className="py-6 text-center text-sm text-gray-500">暂无解密会话。</p>
        ) : (
          <div className="space-y-3">
            {sessions.map((s) => (
              <div key={s.id} className="rounded-md border border-gray-200 px-3 py-2 text-sm dark:border-gray-800">
                <div className="flex flex-wrap items-center gap-3">
                  <span className="font-mono text-xs text-gray-500">{s.id.slice(0, 12)}…</span>
                  <span className="rounded bg-gray-100 px-1.5 py-0.5 text-xs dark:bg-gray-800">{stateLabel(s.state)}</span>
                  <span className="text-xs">{s.target.domains.join(', ')}</span>
                  {s.opts.redact_profile === 'raw' && (
                    <span className="rounded bg-red-600 px-1.5 py-0.5 text-[10px] font-semibold text-white">RAW</span>
                  )}
                  {s.error && <span className="text-xs text-red-600" title={s.error.message}>[{s.error.kind}]</span>}
                  <div className="ml-auto flex gap-2">
                    {(s.state === 'decrypting' || s.state === 'preparing') && (
                      <button className={btn('ghost')} onClick={() => void run(() => NetPolicyAPI.decryptStop(s.id))} disabled={busy}>
                        <Square size={13} /> 停止
                      </button>
                    )}
                    {s.state === 'done' && (
                      <>
                        <button className={btn('primary')} onClick={() => void save(s, 'http_jsonl')} disabled={busy}>
                          <Download size={13} /> http.jsonl
                        </button>
                        <button className={btn('ghost')} onClick={() => void save(s, 'manifest')} disabled={busy}>
                          manifest
                        </button>
                      </>
                    )}
                    <button
                      className={btn('danger')}
                      onClick={() => void run(() => NetPolicyAPI.decryptDelete(s.id))}
                      disabled={busy || s.state === 'decrypting' || s.state === 'preparing'}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </div>
                {Object.keys(s.per_domain).length > 0 && (
                  <table className="mt-2 w-full text-[11px]">
                    <thead className="text-gray-400">
                      <tr>
                        <th className="text-left font-normal">域名</th>
                        <th className="font-normal">已解密</th>
                        <th className="font-normal">透传</th>
                        <th className="font-normal">被 pinning</th>
                        <th className="font-normal">QUIC</th>
                        <th className="font-normal">失败</th>
                      </tr>
                    </thead>
                    <tbody className="font-mono">
                      {Object.entries(s.per_domain).map(([d, c]) => (
                        <tr key={d}>
                          <td className="text-left font-sans text-gray-600 dark:text-gray-300">{d}</td>
                          <td className="text-center text-green-600">{c.decrypted}</td>
                          <td className="text-center text-gray-500">{c.passthrough}</td>
                          <td className="text-center text-amber-600">{c.pinned}</td>
                          <td className="text-center text-gray-500">{c.quic}</td>
                          <td className="text-center text-red-600">{c.failed}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
                {s.state === 'done' && !anyDecrypted(s.per_domain) && (
                  <p className="mt-1 text-[11px] text-gray-400">
                    未产生明文（可能全部透传/被 pinning/QUIC，或目标进程期间无匹配流量）。
                  </p>
                )}
              </div>
            ))}
          </div>
        )}
      </Section>

      {/* Start 二次确认 */}
      {confirmOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4">
          <div className="w-full max-w-md space-y-4 rounded-lg bg-white p-5 shadow-xl dark:bg-gray-900">
            <h3 className="flex items-center gap-2 text-sm font-semibold">
              <ShieldAlert size={16} className={raw ? 'text-red-500' : 'text-amber-500'} /> 确认开始解密
            </h3>
            <ul className="space-y-1 text-sm text-gray-600 dark:text-gray-300">
              <li>目标进程 PID：{pid ?? '—'}</li>
              <li>域名 allowlist：{parseDomains().join(', ') || '—'}</li>
              <li>时长上限：{opts.max_secs} 秒</li>
              <li>正文：{opts.capture_bodies ? '采集' : '仅元数据'}</li>
              <li className={raw ? 'font-semibold text-red-600' : ''}>
                脱敏：{raw ? 'Raw 原文（保留凭据）' : '默认（核心凭据强制脱敏）'}
              </li>
              {opts.force_tcp_for_quic && <li className="text-amber-600">将阻断目标 UDP/443 逼 QUIC 回退</li>}
            </ul>
            <div className="flex justify-end gap-2">
              <button className={btn('ghost')} onClick={() => setConfirmOpen(false)}>取消</button>
              <button className={btn(raw ? 'danger' : 'primary')} onClick={() => void doStart()}>确认开始</button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
