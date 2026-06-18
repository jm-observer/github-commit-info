import { useCallback, useEffect, useRef, useState } from 'react'
import { RefreshCw, Rocket, CircleDot, ExternalLink, Pencil, Save, X } from 'lucide-react'
import {
  G10DeployAPI,
  openUrl,
  onDeployDone,
  onDeployLog,
  type DeployLog,
  type LocalVersion,
  type PortsResult,
  type ProbeResult,
  type ServiceDef,
} from './api/tauri-client'

// ── 单服务的合并视图状态 ──────────────────────────────────────────────────────

interface Row {
  def: ServiceDef
  probe?: ProbeResult
  local?: LocalVersion
  ports?: PortsResult
  probing: boolean
}

// 漂移判定：优先用 commit 对比——远端编译版（health.commit）与本地编译版（git_hash）都有
// 且不同 → 运行版与本地编译漂移。其次 dirty 时提示本地有未提交改动。
function driftHint(probe?: ProbeResult, local?: LocalVersion): string {
  const remoteCommit = probe?.remote_commit ?? null
  const localHash = local?.git_hash ?? null
  if (remoteCommit && localHash && remoteCommit !== localHash) {
    return '运行版与本地编译有漂移'
  }
  if (local?.dirty) return '本地有未提交改动'
  return ''
}

function StatusDot({ probe, configured }: { probe?: ProbeResult; configured: boolean }) {
  let cls = 'text-gray-400'
  let title = '未探测'
  if (!configured) {
    cls = 'text-gray-300'
    title = '未配置健康端点'
  } else if (probe) {
    if (probe.reachable) {
      cls = 'text-green-500'
      title = `在线 ${probe.latency_ms ?? ''}ms`
    } else {
      cls = 'text-red-500'
      title = probe.error ?? '不可达'
    }
  }
  return <CircleDot size={14} className={cls} aria-label={title} />
}

/** 端口清单 + 实时 TCP 连通性：绿=在监听 / 红=连不上 / 灰=未探测。 */
function PortChips({ def, ports, probing }: { def: ServiceDef; ports?: PortsResult; probing: boolean }) {
  if (def.ports.length === 0) {
    return <span className="text-xs text-gray-400">端口未登记</span>
  }
  const statusOf = (port: number) => ports?.ports.find(p => p.port === port)
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {def.ports.map(p => {
        const st = statusOf(p.port)
        let dot = 'bg-gray-300 dark:bg-gray-600'
        let title = probing ? '探测中…' : '未探测'
        if (st) {
          if (st.open) {
            dot = 'bg-green-500'
            title = `在监听 ${st.latency_ms ?? ''}ms`
          } else {
            dot = 'bg-red-500'
            title = st.error ?? '连不上'
          }
        }
        return (
          <span
            key={p.port}
            title={`${def.host}:${p.port}${p.note ? ` · ${p.note}` : ''} — ${title}`}
            className="inline-flex items-center gap-1 rounded border border-gray-200 bg-gray-50 px-1.5 py-0.5 text-xs dark:border-gray-700 dark:bg-gray-800"
          >
            <span className={`h-1.5 w-1.5 rounded-full ${dot}`} />
            <span className="font-mono text-gray-700 dark:text-gray-200">:{p.port}</span>
            {p.note && <span className="text-gray-400">{p.note}</span>}
          </span>
        )
      })}
    </div>
  )
}

export default function G10DeployPage() {
  const [rows, setRows] = useState<Row[]>([])
  const [warning, setWarning] = useState<string | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)

  // 端口编辑：editingName = 正在编辑端口的服务，draftPorts = 草稿端口列表
  const [editingName, setEditingName] = useState<string | null>(null)
  const [draftPorts, setDraftPorts] = useState<{ port: string; note: string }[]>([])
  const [savingPorts, setSavingPorts] = useState(false)

  // 环境变量编辑：editingEnvName = 正在编辑环境变量的服务，draftEnv = 草稿 KEY=VAL 列表
  const [editingEnvName, setEditingEnvName] = useState<string | null>(null)
  const [draftEnv, setDraftEnv] = useState<{ key: string; value: string }[]>([])
  const [savingEnv, setSavingEnv] = useState(false)

  // 部署日志面板
  const [deployingName, setDeployingName] = useState<string | null>(null)
  const [logLines, setLogLines] = useState<DeployLog[]>([])
  const [doneMsg, setDoneMsg] = useState<string | null>(null)
  const logEndRef = useRef<HTMLDivElement | null>(null)

  // ── 加载清单 + 逐个探测/取本地版本 ────────────────────────────────────────
  const refreshOne = useCallback(async (name: string) => {
    setRows(prev => prev.map(r => (r.def.name === name ? { ...r, probing: true } : r)))
    const [probe, local, ports] = await Promise.all([
      G10DeployAPI.probe(name).catch(e => ({
        name, reachable: false, status: null, remote_version: null,
        latency_ms: null, error: String(e),
      } as ProbeResult)),
      G10DeployAPI.localVersion(name).catch(e => ({
        name, git_hash: null, dirty: false, error: String(e),
      } as LocalVersion)),
      G10DeployAPI.probePorts(name).catch(() => undefined),
    ])
    setRows(prev =>
      prev.map(r => (r.def.name === name ? { ...r, probe, local, ports, probing: false } : r)),
    )
  }, [])

  const loadAll = useCallback(async () => {
    setLoadError(null)
    try {
      const list = await G10DeployAPI.listServices()
      setWarning(list.warning)
      setRows(list.services.map(def => ({ def, probing: true })))
      // 并发探测每个服务
      await Promise.all(list.services.map(s => refreshOne(s.name)))
    } catch (e) {
      setLoadError(String(e))
    }
  }, [refreshOne])

  useEffect(() => {
    void loadAll()
  }, [loadAll])

  // ── 订阅部署事件 ──────────────────────────────────────────────────────────
  useEffect(() => {
    let unlistenLog: (() => void) | null = null
    let unlistenDone: (() => void) | null = null

    onDeployLog(log => {
      setLogLines(prev => [...prev, log])
    }).then(fn => { unlistenLog = fn })

    onDeployDone(done => {
      setDeployingName(null)
      setDoneMsg(
        done.success
          ? `✅ ${done.name} 部署成功`
          : `❌ ${done.name} 部署失败：${done.error ?? '未知错误'}`,
      )
      // 部署完成后刷新该服务的连通性/版本
      void refreshOne(done.name)
    }).then(fn => { unlistenDone = fn })

    return () => {
      unlistenLog?.()
      unlistenDone?.()
    }
  }, [refreshOne])

  // 日志自动滚到底
  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [logLines])

  const startDeploy = async (name: string) => {
    if (deployingName) {
      window.alert('已有部署正在进行，请等待完成')
      return
    }
    if (!window.confirm(`确认部署 ${name} 到 G10？将交叉编译 → scp → 重启服务。`)) return
    setLogLines([])
    setDoneMsg(null)
    setDeployingName(name)
    try {
      await G10DeployAPI.deploy(name)
    } catch (e) {
      setDeployingName(null)
      window.alert(`部署启动失败：${String(e)}`)
    }
  }

  // ── 端口编辑 ──────────────────────────────────────────────────────────────
  const startEditPorts = (def: ServiceDef) => {
    setEditingName(def.name)
    setDraftPorts(def.ports.map(p => ({ port: String(p.port), note: p.note })))
  }
  const cancelEditPorts = () => {
    setEditingName(null)
    setDraftPorts([])
  }
  const updateDraftPort = (i: number, field: 'port' | 'note', value: string) => {
    setDraftPorts(prev => prev.map((p, idx) => (idx === i ? { ...p, [field]: value } : p)))
  }
  const addDraftPort = () => setDraftPorts(prev => [...prev, { port: '', note: '' }])
  const removeDraftPort = (i: number) =>
    setDraftPorts(prev => prev.filter((_, idx) => idx !== i))

  const savePorts = async (name: string) => {
    // 校验端口（1–65535 整数）
    const parsed: { port: number; note: string }[] = []
    for (const p of draftPorts) {
      const trimmed = p.port.trim()
      if (trimmed === '') continue // 空行忽略
      const n = Number(trimmed)
      if (!Number.isInteger(n) || n < 1 || n > 65535) {
        window.alert(`端口 "${p.port}" 非法（需 1–65535 整数）`)
        return
      }
      parsed.push({ port: n, note: p.note.trim() })
    }
    // 以当前 rows 的完整清单为基础，仅替换目标服务的 ports，整体写回覆盖文件。
    const services = rows.map(r =>
      r.def.name === name ? { ...r.def, ports: parsed } : r.def,
    )
    setSavingPorts(true)
    try {
      await G10DeployAPI.saveServices(services)
      cancelEditPorts()
      await loadAll() // 重新加载（读覆盖文件）+ 重新探测
    } catch (e) {
      window.alert(`保存端口失败：${String(e)}`)
    } finally {
      setSavingPorts(false)
    }
  }

  // ── 环境变量编辑 ──────────────────────────────────────────────────────────
  const startEditEnv = (def: ServiceDef) => {
    setEditingEnvName(def.name)
    setDraftEnv(def.env.map(e => ({ key: e.key, value: e.value })))
  }
  const cancelEditEnv = () => {
    setEditingEnvName(null)
    setDraftEnv([])
  }
  const updateDraftEnv = (i: number, field: 'key' | 'value', value: string) => {
    setDraftEnv(prev => prev.map((e, idx) => (idx === i ? { ...e, [field]: value } : e)))
  }
  const addDraftEnv = () => setDraftEnv(prev => [...prev, { key: '', value: '' }])
  const removeDraftEnv = (i: number) =>
    setDraftEnv(prev => prev.filter((_, idx) => idx !== i))

  const saveEnv = async (name: string) => {
    // 校验：key 非空、形如环境变量名；value 不含逗号（部署链路用逗号分隔多条 -Env）。
    const parsed: { key: string; value: string }[] = []
    const seen = new Set<string>()
    for (const e of draftEnv) {
      const key = e.key.trim()
      if (key === '') continue // 空行忽略
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) {
        window.alert(`环境变量名 "${e.key}" 非法（需字母/下划线开头，仅含字母数字下划线）`)
        return
      }
      if (seen.has(key)) {
        window.alert(`环境变量名 "${key}" 重复`)
        return
      }
      if (e.value.includes(',')) {
        window.alert(`环境变量 "${key}" 的值不能含逗号（部署链路以逗号分隔多条）`)
        return
      }
      seen.add(key)
      parsed.push({ key, value: e.value.trim() })
    }
    // 以当前 rows 的完整清单为基础，仅替换目标服务的 env，整体写回覆盖文件。
    const services = rows.map(r =>
      r.def.name === name ? { ...r.def, env: parsed } : r.def,
    )
    setSavingEnv(true)
    try {
      await G10DeployAPI.saveServices(services)
      cancelEditEnv()
      await loadAll()
    } catch (e) {
      window.alert(`保存环境变量失败：${String(e)}`)
    } finally {
      setSavingEnv(false)
    }
  }

  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">G10 部署管理</h1>
          <p className="text-sm text-gray-500 dark:text-gray-400">
            D:\git 下部署到 G10 的服务：连通性 · 本地编译版 vs 远端运行版 · 一键交叉编译部署
          </p>
        </div>
        <button
          type="button"
          onClick={() => void loadAll()}
          className="flex items-center gap-1.5 rounded-md bg-gray-100 px-3 py-1.5 text-sm hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700"
        >
          <RefreshCw size={14} /> 刷新全部
        </button>
      </header>

      {loadError && (
        <div className="rounded-md bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-950 dark:text-red-300">
          加载清单失败：{loadError}
        </div>
      )}
      {warning && (
        <div className="rounded-md bg-amber-50 px-3 py-2 text-sm text-amber-700 dark:bg-amber-950 dark:text-amber-300">
          {warning}
        </div>
      )}

      {/* 服务卡片列表 */}
      <div className="space-y-3">
        {rows.map(({ def, probe, local, ports, probing }) => {
          const configured = def.health_url.length > 0
          const canDeploy = def.deploy != null
          const hint = driftHint(probe, local)
          return (
            <div
              key={def.name}
              className="rounded-lg border border-gray-200 p-4 dark:border-gray-800"
            >
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <StatusDot probe={probe} configured={configured} />
                    <span className="font-medium">{def.label}</span>
                    {def.remote_service && (
                      <span className="rounded bg-gray-100 px-1.5 py-0.5 text-xs text-gray-500 dark:bg-gray-800">
                        {def.remote_service}
                      </span>
                    )}
                  </div>
                  <p className="mt-0.5 truncate text-xs text-gray-500 dark:text-gray-400">
                    {def.note}
                  </p>

                  {/* 版本对比：远端运行版(semver) · 远端编译版(commit) · 本地编译版(commit) */}
                  <div className="mt-2 flex flex-wrap gap-x-5 gap-y-1 text-xs">
                    <span className="text-gray-500">
                      远端运行版：
                      <span className="font-mono text-gray-800 dark:text-gray-200">
                        {probe?.remote_version ?? (probing ? '…' : '—')}
                      </span>
                    </span>
                    <span className="text-gray-500">
                      远端编译版：
                      <span className="font-mono text-gray-800 dark:text-gray-200">
                        {probe?.remote_commit ?? (probing ? '…' : '—')}
                      </span>
                    </span>
                    <span className="text-gray-500">
                      本地编译版：
                      <span className="font-mono text-gray-800 dark:text-gray-200">
                        {local?.git_hash ?? (probing ? '…' : '—')}
                        {local?.dirty ? '*' : ''}
                      </span>
                    </span>
                    {hint && <span className="text-amber-600 dark:text-amber-400">{hint}</span>}
                    {probe?.error && !probe.reachable && (
                      <span className="text-red-500">{probe.error}</span>
                    )}
                  </div>

                  {/* 端口清单 + 实时连通性（可编辑） */}
                  <div className="mt-2 flex items-start gap-2">
                    <span className="pt-0.5 text-xs text-gray-400">端口</span>
                    {editingName === def.name ? (
                      <div className="flex flex-col gap-1.5">
                        {draftPorts.map((p, i) => (
                          <div key={i} className="flex items-center gap-1.5">
                            <input
                              type="number"
                              min={1}
                              max={65535}
                              value={p.port}
                              onChange={e => updateDraftPort(i, 'port', e.target.value)}
                              placeholder="端口"
                              className="w-20 rounded border border-gray-300 px-1.5 py-0.5 text-xs dark:border-gray-600 dark:bg-gray-800"
                            />
                            <input
                              type="text"
                              value={p.note}
                              onChange={e => updateDraftPort(i, 'note', e.target.value)}
                              placeholder="用途说明"
                              className="w-44 rounded border border-gray-300 px-1.5 py-0.5 text-xs dark:border-gray-600 dark:bg-gray-800"
                            />
                            <button
                              type="button"
                              onClick={() => removeDraftPort(i)}
                              title="删除该端口"
                              className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-red-500 dark:hover:bg-gray-800"
                            >
                              <X size={12} />
                            </button>
                          </div>
                        ))}
                        <div className="flex items-center gap-2">
                          <button
                            type="button"
                            onClick={addDraftPort}
                            className="rounded border border-dashed border-gray-300 px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-50 dark:border-gray-600 dark:hover:bg-gray-800"
                          >
                            + 添加端口
                          </button>
                          <button
                            type="button"
                            disabled={savingPorts}
                            onClick={() => void savePorts(def.name)}
                            className="flex items-center gap-1 rounded bg-blue-500 px-2 py-0.5 text-xs font-medium text-white hover:bg-blue-600 disabled:opacity-50"
                          >
                            <Save size={12} /> {savingPorts ? '保存中…' : '保存'}
                          </button>
                          <button
                            type="button"
                            onClick={cancelEditPorts}
                            className="rounded px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800"
                          >
                            取消
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="flex items-center gap-2">
                        <PortChips def={def} ports={ports} probing={probing} />
                        <button
                          type="button"
                          onClick={() => startEditPorts(def)}
                          title="编辑端口"
                          className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800"
                        >
                          <Pencil size={12} />
                        </button>
                      </div>
                    )}
                  </div>

                  {/* 环境变量（安装时注入 systemd unit 的 Environment=；仅 deploy 脚本支持 -Env 时生效） */}
                  <div className="mt-2 flex items-start gap-2">
                    <span className="pt-0.5 text-xs text-gray-400">环境变量</span>
                    {editingEnvName === def.name ? (
                      <div className="flex flex-col gap-1.5">
                        {draftEnv.map((e, i) => (
                          <div key={i} className="flex items-center gap-1.5">
                            <input
                              type="text"
                              value={e.key}
                              onChange={ev => updateDraftEnv(i, 'key', ev.target.value)}
                              placeholder="KEY"
                              className="w-40 rounded border border-gray-300 px-1.5 py-0.5 font-mono text-xs dark:border-gray-600 dark:bg-gray-800"
                            />
                            <span className="text-xs text-gray-400">=</span>
                            <input
                              type="text"
                              value={e.value}
                              onChange={ev => updateDraftEnv(i, 'value', ev.target.value)}
                              placeholder="值（不含逗号）"
                              className="w-56 rounded border border-gray-300 px-1.5 py-0.5 font-mono text-xs dark:border-gray-600 dark:bg-gray-800"
                            />
                            <button
                              type="button"
                              onClick={() => removeDraftEnv(i)}
                              title="删除该变量"
                              className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-red-500 dark:hover:bg-gray-800"
                            >
                              <X size={12} />
                            </button>
                          </div>
                        ))}
                        <div className="flex items-center gap-2">
                          <button
                            type="button"
                            onClick={addDraftEnv}
                            className="rounded border border-dashed border-gray-300 px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-50 dark:border-gray-600 dark:hover:bg-gray-800"
                          >
                            + 添加变量
                          </button>
                          <button
                            type="button"
                            disabled={savingEnv}
                            onClick={() => void saveEnv(def.name)}
                            className="flex items-center gap-1 rounded bg-blue-500 px-2 py-0.5 text-xs font-medium text-white hover:bg-blue-600 disabled:opacity-50"
                          >
                            <Save size={12} /> {savingEnv ? '保存中…' : '保存'}
                          </button>
                          <button
                            type="button"
                            onClick={cancelEditEnv}
                            className="rounded px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800"
                          >
                            取消
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="flex flex-wrap items-center gap-1.5">
                        {def.env.length === 0 ? (
                          <span className="text-xs text-gray-400">未配置</span>
                        ) : (
                          def.env.map(e => (
                            <span
                              key={e.key}
                              title={`${e.key}=${e.value}`}
                              className="inline-flex items-center gap-1 rounded border border-gray-200 bg-gray-50 px-1.5 py-0.5 font-mono text-xs dark:border-gray-700 dark:bg-gray-800"
                            >
                              <span className="text-gray-700 dark:text-gray-200">{e.key}</span>
                              <span className="text-gray-400">=</span>
                              <span className="max-w-[12rem] truncate text-gray-500">{e.value}</span>
                            </span>
                          ))
                        )}
                        {!canDeploy && def.env.length > 0 && (
                          <span className="text-xs text-amber-500" title="该服务未接入一键部署，环境变量不会被注入">
                            （未接入部署，不生效）
                          </span>
                        )}
                        <button
                          type="button"
                          onClick={() => startEditEnv(def)}
                          title="编辑环境变量"
                          className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800"
                        >
                          <Pencil size={12} />
                        </button>
                      </div>
                    )}
                  </div>
                </div>

                {/* 操作 */}
                <div className="flex flex-shrink-0 items-center gap-2">
                  {def.web_url && (
                    <button
                      type="button"
                      onClick={() => void openUrl(def.web_url)}
                      title={`打开后台 ${def.web_url}`}
                      className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm text-gray-600 hover:bg-gray-100 dark:text-gray-300 dark:hover:bg-gray-800"
                    >
                      <ExternalLink size={14} /> 打开后台
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => void refreshOne(def.name)}
                    title="刷新连通性/版本"
                    className="rounded-md p-1.5 text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800"
                  >
                    <RefreshCw size={14} className={probing ? 'animate-spin' : ''} />
                  </button>
                  <button
                    type="button"
                    disabled={!canDeploy || deployingName != null}
                    onClick={() => void startDeploy(def.name)}
                    title={canDeploy ? '交叉编译并部署到 G10' : '该服务暂未接入一键部署（脚本待接入）'}
                    className={[
                      'flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors',
                      canDeploy && deployingName == null
                        ? 'bg-blue-500 text-white hover:bg-blue-600'
                        : 'cursor-not-allowed bg-gray-100 text-gray-400 dark:bg-gray-800',
                    ].join(' ')}
                  >
                    <Rocket size={14} />
                    {deployingName === def.name ? '部署中…' : '部署'}
                  </button>
                </div>
              </div>
            </div>
          )
        })}
      </div>

      {/* 部署日志面板 */}
      {(deployingName || logLines.length > 0 || doneMsg) && (
        <div className="rounded-lg border border-gray-200 dark:border-gray-800">
          <div className="flex items-center justify-between border-b border-gray-200 px-3 py-2 text-sm dark:border-gray-800">
            <span className="font-medium">
              部署日志 {deployingName ? `· ${deployingName}（进行中）` : ''}
            </span>
            {doneMsg && <span>{doneMsg}</span>}
          </div>
          <div className="max-h-72 overflow-auto bg-gray-950 p-3 font-mono text-xs leading-relaxed text-gray-200">
            {logLines.map((l, i) => (
              <div key={i} className={l.stream === 'stderr' ? 'text-red-400' : ''}>
                {l.line}
              </div>
            ))}
            <div ref={logEndRef} />
          </div>
        </div>
      )}
    </div>
  )
}
