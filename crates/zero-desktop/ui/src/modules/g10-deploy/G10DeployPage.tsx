import { useCallback, useEffect, useState } from 'react'
import { RefreshCw, Rocket, CircleDot, ExternalLink, Pencil, Save, X } from 'lucide-react'
import {
  G10DeployAPI,
  openUrl,
  onDeployDone,
  onDeployLog,
  type DeployLog,
  type LocalVersion,
  type NetStatus,
  type ProbeResult,
  type ServiceDef,
} from './api/tauri-client'

// ── 单服务的合并视图状态 ──────────────────────────────────────────────────────

interface Row {
  def: ServiceDef
  probe?: ProbeResult
  local?: LocalVersion
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

// 上次部署时间：后端存 RFC3339 UTC，这里展示为本地时间（YYYY-MM-DD HH:mm）。
function formatDeployedAt(iso?: string | null): string {
  if (!iso) return '从未部署'
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
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

export default function G10DeployPage() {
  const [rows, setRows] = useState<Row[]>([])
  const [warning, setWarning] = useState<string | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  // 当前生效的网络路径：页头展示 + 决定「打开后台」用局域网还是外网地址。
  const [net, setNet] = useState<NetStatus | null>(null)
  const wanOnly = net != null && net.picked !== 'lan'

  /**
   * 当前路径下该服务后台的实际地址。局域网档恒用 `web_url` 直连（不绕公网发夹）；
   * 外网档用 caddy 子域名入口 `web_url_wan`，没有则返回空串 → 按钮置灰。
   */
  const backendUrl = (def: ServiceDef) => (wanOnly ? def.web_url_wan : def.web_url)

  // 本地仓库目录编辑
  const [editingDirName, setEditingDirName] = useState<string | null>(null)
  const [draftDir, setDraftDir] = useState('')
  const [savingDir, setSavingDir] = useState(false)

  // 健康端点编辑（存活探测的依据；https 自签也可填）
  const [editingHealthName, setEditingHealthName] = useState<string | null>(null)
  const [draftHealth, setDraftHealth] = useState('')
  const [savingHealth, setSavingHealth] = useState(false)

  // 后台地址编辑（局域网直连 / 外网 caddy 子域名两条；后者留空 = 该服务无外网入口）
  const [editingWebName, setEditingWebName] = useState<string | null>(null)
  const [draftWeb, setDraftWeb] = useState('')
  const [draftWebWan, setDraftWebWan] = useState('')
  const [savingWeb, setSavingWeb] = useState(false)

  // 环境变量编辑：draftEnv = 草稿 KEY=VAL(+备注) 列表（端口即其中的 <SVC>_BIND 一条）
  const [editingEnvName, setEditingEnvName] = useState<string | null>(null)
  const [draftEnv, setDraftEnv] = useState<{ key: string; value: string; note: string }[]>([])
  const [savingEnv, setSavingEnv] = useState(false)

  // 部署状态（按服务并发）：deployingNames = 正在部署的服务集合；日志/终态按服务名分桶
  const [deployingNames, setDeployingNames] = useState<Set<string>>(new Set())
  const [logsByName, setLogsByName] = useState<Record<string, DeployLog[]>>({})
  const [doneByName, setDoneByName] = useState<Record<string, string>>({})

  // ── 加载清单 + 逐个探测/取本地版本 ────────────────────────────────────────
  const refreshOne = useCallback(async (name: string) => {
    setRows(prev => prev.map(r => (r.def.name === name ? { ...r, probing: true } : r)))
    const [probe, local] = await Promise.all([
      G10DeployAPI.probe(name).catch(e => ({
        name, reachable: false, status: null, remote_version: null,
        latency_ms: null, error: String(e),
      } as ProbeResult)),
      G10DeployAPI.localVersion(name).catch(e => ({
        name, git_hash: null, dirty: false, error: String(e),
      } as LocalVersion)),
    ])
    setRows(prev =>
      prev.map(r => (r.def.name === name ? { ...r, probe, local, probing: false } : r)),
    )
  }, [])

  const loadAll = useCallback(async () => {
    setLoadError(null)
    try {
      G10DeployAPI.netStatus()
        .then(setNet)
        .catch(() => setNet(null))
      const list = await G10DeployAPI.listServices()
      setWarning(list.warning)
      setRows(list.services.map(def => ({ def, probing: true })))
      // 恢复"哪些服务正在部署"的状态（进页/刷新时后端可能仍有部署在跑）
      G10DeployAPI.deployingServices()
        .then(names => setDeployingNames(new Set(names)))
        .catch(() => {})
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
  // 注：StrictMode 下 effect 会跑两遍，cleanup 必须 await Promise 才能拿到 unlisten；
  // 否则第一次订阅会泄漏，导致每条日志触发两次（"Compiling X" 出现两遍即此原因）。
  useEffect(() => {
    let cancelled = false
    const unlistens: Array<() => void> = []
    const track = (p: Promise<() => void>) => {
      p.then(fn => {
        if (cancelled) fn()
        else unlistens.push(fn)
      })
    }

    track(onDeployLog(log => {
      setLogsByName(prev => ({ ...prev, [log.name]: [...(prev[log.name] ?? []), log] }))
    }))

    track(onDeployDone(done => {
      setDeployingNames(prev => {
        const next = new Set(prev)
        next.delete(done.name)
        return next
      })
      setDoneByName(prev => ({
        ...prev,
        [done.name]: done.success
          ? `✅ ${done.name} 部署成功`
          : `❌ ${done.name} 部署失败：${done.error ?? '未知错误'}`,
      }))
      // 部署完成后刷新该服务的连通性/版本
      void refreshOne(done.name)
      // 部署成功时后端已更新「上次部署时间」，重取清单把该服务的 def 同步到面板。
      if (done.success) {
        G10DeployAPI.listServices()
          .then(list => {
            const next = list.services.find(s => s.name === done.name)
            if (next) {
              setRows(prev =>
                prev.map(r => (r.def.name === done.name ? { ...r, def: next } : r)),
              )
            }
          })
          .catch(() => {})
      }
    }))

    return () => {
      cancelled = true
      unlistens.forEach(fn => fn())
    }
  }, [refreshOne])

  const startDeploy = async (name: string) => {
    if (deployingNames.has(name)) {
      window.alert(`${name} 已在部署中，请等待其完成`)
      return
    }
    if (!window.confirm(`确认部署 ${name} 到 G10？将交叉编译 → scp → 重启服务。`)) return
    // 重置该服务的日志/终态（不影响其它正在部署的服务）
    setLogsByName(prev => ({ ...prev, [name]: [] }))
    setDoneByName(prev => {
      const next = { ...prev }
      delete next[name]
      return next
    })
    setDeployingNames(prev => new Set(prev).add(name))
    try {
      await G10DeployAPI.deploy(name)
    } catch (e) {
      setDeployingNames(prev => {
        const next = new Set(prev)
        next.delete(name)
        return next
      })
      window.alert(`部署启动失败：${String(e)}`)
    }
  }

  // 清除某服务的日志块（仅已结束的可清）
  const closeLog = (name: string) => {
    setLogsByName(prev => {
      const next = { ...prev }
      delete next[name]
      return next
    })
    setDoneByName(prev => {
      const next = { ...prev }
      delete next[name]
      return next
    })
  }

  // ── 通用：以当前 rows 为基础替换某服务字段后整体写回覆盖文件 ────────────────
  const saveServicePatch = async (name: string, patch: Partial<ServiceDef>) => {
    const services = rows.map(r => (r.def.name === name ? { ...r.def, ...patch } : r.def))
    await G10DeployAPI.saveServices(services)
    await loadAll()
  }

  // ── 本地仓库目录编辑 ──────────────────────────────────────────────────────
  const startEditDir = (def: ServiceDef) => {
    setEditingDirName(def.name)
    setDraftDir(def.repo_dir)
  }
  const cancelEditDir = () => {
    setEditingDirName(null)
    setDraftDir('')
  }
  const saveDir = async (name: string) => {
    const dir = draftDir.trim()
    if (dir === '') {
      window.alert('仓库目录不能为空')
      return
    }
    setSavingDir(true)
    try {
      await saveServicePatch(name, { repo_dir: dir })
      cancelEditDir()
    } catch (e) {
      window.alert(`保存仓库目录失败：${String(e)}`)
    } finally {
      setSavingDir(false)
    }
  }

  // ── 健康端点编辑 ──────────────────────────────────────────────────────────
  const startEditHealth = (def: ServiceDef) => {
    setEditingHealthName(def.name)
    setDraftHealth(def.health_url)
  }
  const cancelEditHealth = () => {
    setEditingHealthName(null)
    setDraftHealth('')
  }
  const saveHealth = async (name: string) => {
    const url = draftHealth.trim()
    // 允许留空（= 未配置健康端点）；非空时简单校验是 http(s)。
    if (url !== '' && !/^https?:\/\//i.test(url)) {
      window.alert('健康端点需以 http:// 或 https:// 开头（或留空表示不探测）')
      return
    }
    setSavingHealth(true)
    try {
      await saveServicePatch(name, { health_url: url })
      cancelEditHealth()
    } catch (e) {
      window.alert(`保存健康端点失败：${String(e)}`)
    } finally {
      setSavingHealth(false)
    }
  }

  // ── 后台地址编辑 ──────────────────────────────────────────────────────────
  // 后台是浏览器直连，不经 toolkit-server 代发，所以两条路径各存一个地址。改端口 / 换子域名
  // 后在此改，不必编辑 g10-services.json。
  const startEditWeb = (def: ServiceDef) => {
    setEditingWebName(def.name)
    setDraftWeb(def.web_url)
    setDraftWebWan(def.web_url_wan)
  }
  const cancelEditWeb = () => {
    setEditingWebName(null)
    setDraftWeb('')
    setDraftWebWan('')
  }
  const saveWeb = async (name: string) => {
    const lan = draftWeb.trim()
    const wan = draftWebWan.trim()
    // 两条都允许留空（局域网空 = 无后台不显示按钮；外网空 = 外网档置灰）；非空时校验是 http(s)。
    for (const [label, url] of [
      ['局域网后台地址', lan],
      ['外网后台地址', wan],
    ] as const) {
      if (url !== '' && !/^https?:\/\//i.test(url)) {
        window.alert(`${label}需以 http:// 或 https:// 开头（或留空）`)
        return
      }
    }
    setSavingWeb(true)
    try {
      await saveServicePatch(name, { web_url: lan, web_url_wan: wan })
      cancelEditWeb()
    } catch (e) {
      window.alert(`保存后台地址失败：${String(e)}`)
    } finally {
      setSavingWeb(false)
    }
  }

  // ── 环境变量编辑（端口即其中的 <SVC>_BIND 一条） ──────────────────────────
  const startEditEnv = (def: ServiceDef) => {
    setEditingEnvName(def.name)
    setDraftEnv(def.env.map(e => ({ key: e.key, value: e.value, note: e.note })))
  }
  const cancelEditEnv = () => {
    setEditingEnvName(null)
    setDraftEnv([])
  }
  const updateDraftEnv = (i: number, field: 'key' | 'value' | 'note', value: string) => {
    setDraftEnv(prev => prev.map((e, idx) => (idx === i ? { ...e, [field]: value } : e)))
  }
  const addDraftEnv = () => setDraftEnv(prev => [...prev, { key: '', value: '', note: '' }])
  const removeDraftEnv = (i: number) =>
    setDraftEnv(prev => prev.filter((_, idx) => idx !== i))

  const saveEnv = async (name: string) => {
    // 校验：key 非空、形如环境变量名；value 不含逗号（部署链路用逗号分隔多条 -Env）。
    const parsed: { key: string; value: string; note: string }[] = []
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
      parsed.push({ key, value: e.value.trim(), note: e.note.trim() })
    }
    setSavingEnv(true)
    try {
      await saveServicePatch(name, { env: parsed })
      cancelEditEnv()
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
          {net && (
            <p className="mt-0.5 text-xs text-gray-400 dark:text-gray-500">
              连通性经 <b>{net.picked === 'lan' ? '局域网' : '外网'}</b> 的{' '}
              {net.g10_base || '（未配置）'} 代探（toolkit-server 在 G10 本机逐个探健康端点）
            </p>
          )}
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

      {/* 服务卡片列表。wanOnly：当前生效路径是外网 → 「打开后台」不可用（见按钮注释）。 */}
      <div className="space-y-3">
        {rows.map(({ def, probe, local, probing }) => {
          const configured = def.health_url.length > 0
          const canDeploy = def.deploy != null
          const hint = driftHint(probe, local)
          return (
            <div
              key={def.name}
              className="rounded-lg border border-gray-200 p-4 dark:border-gray-800"
            >
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0 flex-1">
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
                    <span className="text-gray-500">
                      上次部署：
                      <span className="font-mono text-gray-800 dark:text-gray-200">
                        {formatDeployedAt(def.last_deployed_at)}
                      </span>
                    </span>
                    {hint && <span className="text-amber-600 dark:text-amber-400">{hint}</span>}
                    {probe?.error && !probe.reachable && (
                      <span className="text-red-500">{probe.error}</span>
                    )}
                  </div>

                  {/* 本地仓库目录（取 git 本地版本 + 部署脚本 cwd 的依据；文件夹移动后在此改） */}
                  <div className="mt-2 flex items-start gap-2">
                    <span className="w-16 flex-shrink-0 pt-0.5 text-xs text-gray-400">本地仓库</span>
                    {editingDirName === def.name ? (
                      <div className="flex items-center gap-1.5">
                        <input
                          type="text"
                          value={draftDir}
                          onChange={e => setDraftDir(e.target.value)}
                          placeholder={`如 D:\\git\\${def.name}`}
                          className="w-80 rounded border border-gray-300 px-1.5 py-0.5 font-mono text-xs dark:border-gray-600 dark:bg-gray-800"
                        />
                        <button
                          type="button"
                          disabled={savingDir}
                          onClick={() => void saveDir(def.name)}
                          className="flex items-center gap-1 rounded bg-blue-500 px-2 py-0.5 text-xs font-medium text-white hover:bg-blue-600 disabled:opacity-50"
                        >
                          <Save size={12} /> {savingDir ? '保存中…' : '保存'}
                        </button>
                        <button
                          type="button"
                          onClick={cancelEditDir}
                          className="rounded px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800"
                        >
                          取消
                        </button>
                      </div>
                    ) : (
                      <div className="flex items-center gap-2">
                        <span
                          title={def.repo_dir}
                          className={[
                            'max-w-[22rem] truncate font-mono text-xs',
                            local?.error ? 'text-red-500' : 'text-gray-700 dark:text-gray-200',
                          ].join(' ')}
                        >
                          {def.repo_dir}
                        </span>
                        {local?.error && (
                          <span className="text-xs text-red-500" title={local.error}>
                            （路径异常）
                          </span>
                        )}
                        <button
                          type="button"
                          onClick={() => startEditDir(def)}
                          title="编辑本地仓库目录"
                          className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800"
                        >
                          <Pencil size={12} />
                        </button>
                      </div>
                    )}
                  </div>

                  {/* 健康端点（存活探测依据；https 自签也可填，探测已放宽证书校验） */}
                  <div className="mt-2 flex items-start gap-2">
                    <span className="w-16 flex-shrink-0 pt-0.5 text-xs text-gray-400">健康端点</span>
                    {editingHealthName === def.name ? (
                      <div className="flex items-center gap-1.5">
                        <input
                          type="text"
                          value={draftHealth}
                          onChange={e => setDraftHealth(e.target.value)}
                          placeholder="http(s)://host:port/health（留空=不探测）"
                          className="w-80 rounded border border-gray-300 px-1.5 py-0.5 font-mono text-xs dark:border-gray-600 dark:bg-gray-800"
                        />
                        <button
                          type="button"
                          disabled={savingHealth}
                          onClick={() => void saveHealth(def.name)}
                          className="flex items-center gap-1 rounded bg-blue-500 px-2 py-0.5 text-xs font-medium text-white hover:bg-blue-600 disabled:opacity-50"
                        >
                          <Save size={12} /> {savingHealth ? '保存中…' : '保存'}
                        </button>
                        <button
                          type="button"
                          onClick={cancelEditHealth}
                          className="rounded px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800"
                        >
                          取消
                        </button>
                      </div>
                    ) : (
                      <div className="flex items-center gap-2">
                        <span
                          title={def.health_url}
                          className="max-w-[22rem] truncate font-mono text-xs text-gray-700 dark:text-gray-200"
                        >
                          {def.health_url || <span className="text-gray-400">未配置</span>}
                        </span>
                        <button
                          type="button"
                          onClick={() => startEditHealth(def)}
                          title="编辑健康端点"
                          className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800"
                        >
                          <Pencil size={12} />
                        </button>
                      </div>
                    )}
                  </div>

                  {/* 后台地址：局域网直连 / 外网 caddy 子域名。当前路径那条加粗，即「打开后台」会跳的。 */}
                  <div className="mt-2 flex items-start gap-2">
                    <span className="w-16 flex-shrink-0 pt-0.5 text-xs text-gray-400">后台地址</span>
                    {editingWebName === def.name ? (
                      <div className="flex flex-col gap-1.5">
                        <div className="flex items-center gap-1.5">
                          <span className="w-12 flex-shrink-0 text-xs text-gray-400">局域网</span>
                          <input
                            type="text"
                            value={draftWeb}
                            onChange={e => setDraftWeb(e.target.value)}
                            placeholder="http(s)://192.168.0.68:port/path（留空=无后台）"
                            className="w-80 rounded border border-gray-300 px-1.5 py-0.5 font-mono text-xs dark:border-gray-600 dark:bg-gray-800"
                          />
                        </div>
                        <div className="flex items-center gap-1.5">
                          <span className="w-12 flex-shrink-0 text-xs text-gray-400">外网</span>
                          <input
                            type="text"
                            value={draftWebWan}
                            onChange={e => setDraftWebWan(e.target.value)}
                            placeholder="https://<svc>.for-memory.site:38788（留空=无外网入口）"
                            className="w-80 rounded border border-gray-300 px-1.5 py-0.5 font-mono text-xs dark:border-gray-600 dark:bg-gray-800"
                          />
                        </div>
                        <div className="flex items-center gap-1.5">
                          <button
                            type="button"
                            disabled={savingWeb}
                            onClick={() => void saveWeb(def.name)}
                            className="flex items-center gap-1 rounded bg-blue-500 px-2 py-0.5 text-xs font-medium text-white hover:bg-blue-600 disabled:opacity-50"
                          >
                            <Save size={12} /> {savingWeb ? '保存中…' : '保存'}
                          </button>
                          <button
                            type="button"
                            onClick={cancelEditWeb}
                            className="rounded px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-800"
                          >
                            取消
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                        <span
                          title={def.web_url || '未配置（无后台）'}
                          className={[
                            'max-w-[22rem] truncate font-mono text-xs',
                            wanOnly
                              ? 'text-gray-400'
                              : 'font-medium text-gray-700 dark:text-gray-200',
                          ].join(' ')}
                        >
                          <span className="mr-1 text-gray-400">局域网</span>
                          {def.web_url || <span className="text-gray-400">未配置</span>}
                        </span>
                        <span
                          title={def.web_url_wan || '未配置（外网档「打开后台」置灰）'}
                          className={[
                            'max-w-[22rem] truncate font-mono text-xs',
                            wanOnly
                              ? 'font-medium text-gray-700 dark:text-gray-200'
                              : 'text-gray-400',
                          ].join(' ')}
                        >
                          <span className="mr-1 text-gray-400">外网</span>
                          {def.web_url_wan || <span className="text-gray-400">未配置</span>}
                        </span>
                        <button
                          type="button"
                          onClick={() => startEditWeb(def)}
                          title="编辑后台地址（局域网 / 外网）"
                          className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800"
                        >
                          <Pencil size={12} />
                        </button>
                      </div>
                    )}
                  </div>

                  {/* 环境变量（安装时注入 systemd unit 的 Environment=；端口即 <SVC>_BIND 一条） */}
                  <div className="mt-2 flex items-start gap-2">
                    <span className="w-16 flex-shrink-0 pt-0.5 text-xs text-gray-400">环境变量</span>
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
                              className="w-48 rounded border border-gray-300 px-1.5 py-0.5 font-mono text-xs dark:border-gray-600 dark:bg-gray-800"
                            />
                            <input
                              type="text"
                              value={e.note}
                              onChange={ev => updateDraftEnv(i, 'note', ev.target.value)}
                              placeholder="备注（可选）"
                              className="w-40 rounded border border-gray-300 px-1.5 py-0.5 text-xs dark:border-gray-600 dark:bg-gray-800"
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
                              title={`${e.key}=${e.value}${e.note ? ` · ${e.note}` : ''}`}
                              className="inline-flex items-center gap-1 rounded border border-gray-200 bg-gray-50 px-1.5 py-0.5 text-xs dark:border-gray-700 dark:bg-gray-800"
                            >
                              <span className="font-mono text-gray-700 dark:text-gray-200">{e.key}</span>
                              <span className="font-mono text-gray-400">=</span>
                              <span className="max-w-[10rem] truncate font-mono text-gray-500">{e.value}</span>
                              {e.note && <span className="text-gray-400">· {e.note}</span>}
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
                  {/* 两条地址都空才算「无后台」不显示按钮；只配了其中一条时按钮在，另一档置灰。 */}
                  {(def.web_url || def.web_url_wan) && (
                    <button
                      type="button"
                      // 后台是浏览器直连，不像健康探测那样能经 toolkit-server 代发，所以按当前路径
                      // 选地址：局域网走 web_url，外网走 caddy 子域名 web_url_wan。后者为空的服务
                      // （如 english 自持 28080 入口未收编进 caddy）外网档仍无处可去 → 置灰。
                      disabled={!backendUrl(def)}
                      onClick={() => void openUrl(backendUrl(def))}
                      title={
                        backendUrl(def)
                          ? `打开后台 ${backendUrl(def)}`
                          : '当前走外网：该服务没有外网入口（未接入 caddy 子域名分流）'
                      }
                      className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm text-gray-600 hover:bg-gray-100 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent dark:text-gray-300 dark:hover:bg-gray-800"
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
                    disabled={!canDeploy || deployingNames.has(def.name)}
                    onClick={() => void startDeploy(def.name)}
                    title={canDeploy ? '交叉编译并部署到 G10' : '该服务暂未接入一键部署（脚本待接入）'}
                    className={[
                      'flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors',
                      canDeploy && !deployingNames.has(def.name)
                        ? 'bg-blue-500 text-white hover:bg-blue-600'
                        : 'cursor-not-allowed bg-gray-100 text-gray-400 dark:bg-gray-800',
                    ].join(' ')}
                  >
                    <Rocket size={14} />
                    {deployingNames.has(def.name) ? '部署中…' : '部署'}
                  </button>
                </div>
              </div>
            </div>
          )
        })}
      </div>

      {/* 部署日志面板：每个（正在 / 刚结束）部署的服务一个独立日志块，支持多服务并发 */}
      {Object.keys(logsByName).length > 0 && (
        <div className="space-y-3">
          {Object.entries(logsByName).map(([name, lines]) => {
            const active = deployingNames.has(name)
            const done = doneByName[name]
            return (
              <div key={name} className="rounded-lg border border-gray-200 dark:border-gray-800">
                <div className="flex items-center justify-between border-b border-gray-200 px-3 py-2 text-sm dark:border-gray-800">
                  <span className="font-medium">
                    部署日志 · {name}
                    {active && <span className="text-blue-500"> （进行中）</span>}
                  </span>
                  <div className="flex items-center gap-2">
                    {done && <span>{done}</span>}
                    {!active && (
                      <button
                        type="button"
                        onClick={() => closeLog(name)}
                        title="清除该日志"
                        className="rounded p-1 text-gray-400 hover:bg-gray-100 hover:text-gray-600 dark:hover:bg-gray-800"
                      >
                        <X size={14} />
                      </button>
                    )}
                  </div>
                </div>
                <div className="max-h-72 overflow-auto bg-gray-950 p-3 font-mono text-xs leading-relaxed text-gray-200">
                  {lines.map((l, i) => (
                    <div key={i} className={l.stream === 'stderr' ? 'text-red-400' : ''}>
                      {l.line}
                    </div>
                  ))}
                </div>
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
