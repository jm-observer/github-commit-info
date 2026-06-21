import { useEffect, useMemo, useRef, useState } from 'react'
import { RefreshCw, Plus, ChevronDown, Folder } from 'lucide-react'
import type { Provider, SessionSummary } from '../api/tauri-client'

interface Props {
  sessions: SessionSummary[]
  claudeId: string
  codexId: string
  onPick: (provider: Provider, id: string) => void
  onRefresh: () => void
  loading: boolean
  /** 立刻新建 Codex 会话（复用所选 Claude 会话的仓库目录）。仅在 newCodexMode=false 时显示。 */
  onNewCodex: () => void
  creatingCodex: boolean
  /** 已选 Claude 会话的项目名（下发给 Codex 选择器作亲和项目）。 */
  claudeProject?: string
  /** 已选 Codex 会话的项目名（下发给 Claude 选择器作亲和项目）。 */
  codexProject?: string
  /** 新建 Codex 模式：勾选后禁用 Codex 下拉，显示设计文档输入，启动时再实际建会话。 */
  newCodexMode: boolean
  setNewCodexMode: (v: boolean) => void
  /** 新建 Codex 模式下的设计文档路径（相对 Claude 会话 cwd 或绝对路径）。 */
  newCodexDesignDoc: string
  setNewCodexDesignDoc: (v: string) => void
}

/** 主展示文本：优先首条用户消息预览（最稳定可认），回退 AI 标题，再回退短 id。 */
function optionLabel(s: SessionSummary): string {
  const body = s.preview?.trim() || s.title?.trim() || s.id.slice(0, 8)
  return `[${s.status}] ${body}`
}

/** 项目名：取 cwd 末段（兼容 / 与 \\ 分隔）；无则空串。 */
function projectName(cwd: string): string {
  const parts = (cwd || '').split(/[/\\]+/).filter(Boolean)
  return parts.length ? parts[parts.length - 1] : ''
}

/** 分词匹配：query 拆成词，每个词都需在 haystack 中出现（顺序无关、部分匹配）。 */
function tokenMatch(haystack: string, query: string): boolean {
  const hay = haystack.toLowerCase()
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every(tok => hay.includes(tok))
}

/** updated_at(ISO8601) → 本地化短时间；无法解析则原样返回。 */
function shortTime(iso: string): string {
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return iso
  return new Date(t).toLocaleString(undefined, {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

/**
 * updated_at(ISO8601) → 相对时间（对齐桌面端观感）；无法解析则原样返回。
 * <60s "刚刚" / <60min "N 分" / <24h "N 小时" / <30d "N 天" / 否则 "N 月"。
 * 下拉打开时按当前时刻计算一次即可（瞬时交互，无需定时刷新）。
 */
function relativeTime(iso: string): string {
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return iso
  const diffMs = Date.now() - t
  const sec = Math.max(0, Math.floor(diffMs / 1000))
  if (sec < 60) return '刚刚'
  const min = Math.floor(sec / 60)
  if (min < 60) return `${min} 分`
  const hour = Math.floor(min / 60)
  if (hour < 24) return `${hour} 小时`
  const day = Math.floor(hour / 24)
  if (day < 30) return `${day} 天`
  const month = Math.floor(day / 30)
  return `${month} 月`
}

const UNKNOWN_PROJECT = '未知项目'

interface Group {
  /** 分组键（项目名，空 cwd → UNKNOWN_PROJECT）。 */
  project: string
  /** 是否为亲和项目（与对侧已选会话同项目）。 */
  affinity: boolean
  /** 组内会话，已按 updated_at 倒序。 */
  items: SessionSummary[]
}

/**
 * 先筛后分组：把已筛会话按 projectName(cwd) 分组。
 * - 组内按 updated_at 倒序；组间按组内最新会话 updated_at 倒序。
 * - 空 cwd → UNKNOWN_PROJECT，恒排末尾。
 * - affinityProject 非空时，该组置顶并标记 affinity。
 */
function groupSessions(filtered: SessionSummary[], affinityProject?: string): Group[] {
  const byProject = new Map<string, SessionSummary[]>()
  for (const s of filtered) {
    const proj = projectName(s.cwd) || UNKNOWN_PROJECT
    const arr = byProject.get(proj)
    if (arr) arr.push(s)
    else byProject.set(proj, [s])
  }
  const groups: Group[] = []
  for (const [project, items] of byProject) {
    items.sort((a, b) => (a.updated_at < b.updated_at ? 1 : a.updated_at > b.updated_at ? -1 : 0))
    groups.push({ project, affinity: !!affinityProject && project === affinityProject, items })
  }
  const latest = (g: Group) => g.items[0]?.updated_at ?? ''
  groups.sort((a, b) => {
    // 亲和组置顶。
    if (a.affinity !== b.affinity) return a.affinity ? -1 : 1
    // 未知项目恒排末尾。
    const aUnknown = a.project === UNKNOWN_PROJECT
    const bUnknown = b.project === UNKNOWN_PROJECT
    if (aUnknown !== bUnknown) return aUnknown ? 1 : -1
    // 其余按组内最新倒序。
    return latest(a) < latest(b) ? 1 : latest(a) > latest(b) ? -1 : 0
  })
  return groups
}

/**
 * 可输入过滤的会话下拉框（替代原生 select）。
 * - 输入框即筛选框：键入文本按 title/id/status 模糊匹配。
 * - 选项按 `updated_at` 倒序（最新在最前）。
 * - 点击选项选中；Esc / 点击外部关闭；回车选中当前列表首项。
 */
function SideSelect({
  label,
  provider,
  sessions,
  value,
  onPick,
  affinityProject,
  disabled,
  disabledHint,
}: {
  label: string
  provider: Provider
  sessions: SessionSummary[]
  value: string
  onPick: (provider: Provider, id: string) => void
  /** 对侧已选会话的项目名（同项目联动）；空表示对侧未选，退化为纯 L1。 */
  affinityProject?: string
  /** 禁用：输入框不可编辑、下拉不弹出；用于"新建 Codex 模式"等延迟创建场景。 */
  disabled?: boolean
  /** 禁用时的占位文案（替代输入框内容）。 */
  disabledHint?: string
}) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const rootRef = useRef<HTMLDivElement>(null)

  // 仅本侧 provider，按 updated_at 倒序（最新在最前）。
  const sorted = useMemo(
    () =>
      sessions
        .filter(s => s.provider === provider)
        .slice()
        .sort((a, b) => (a.updated_at < b.updated_at ? 1 : a.updated_at > b.updated_at ? -1 : 0)),
    [sessions, provider],
  )

  const selected = sorted.find(s => s.id === value)

  // 打开且已键入时按 query 分词过滤（跨 预览/标题/项目/状态/id）；否则展示全部（倒序）。
  const filtered = useMemo(() => {
    const q = query.trim()
    if (!open || !q) return sorted
    return sorted.filter(s =>
      tokenMatch(`${s.preview} ${s.title} ${projectName(s.cwd)} ${s.status} ${s.id}`, q),
    )
  }, [sorted, query, open])

  // 先筛后分组（空组自动隐藏）；亲和项目非空时该组置顶。
  const groups = useMemo(
    () => groupSessions(filtered, affinityProject),
    [filtered, affinityProject],
  )

  // 是否需要在亲和组之后插入"其他项目"分隔（仅当确有亲和组且其后还有别的组）。
  const hasAffinityGroup = !!affinityProject && groups.some(g => g.affinity)

  // 点击组件外部关闭下拉。
  useEffect(() => {
    if (!open) return
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', onDown)
    return () => document.removeEventListener('mousedown', onDown)
  }, [open])

  const pick = (id: string) => {
    onPick(provider, id)
    setOpen(false)
    setQuery('')
  }

  // 关闭态展示已选会话标签；打开态展示用户输入。禁用时退化为占位提示。
  const display = disabled ? disabledHint ?? '' : open ? query : selected ? optionLabel(selected) : ''

  return (
    // min-w-0 让 flex-1 真正约束子项宽度。
    <div ref={rootRef} className="relative flex min-w-0 flex-1 flex-col gap-1">
      <label className="text-xs text-gray-500 dark:text-gray-400">{label}</label>
      <div className="relative">
        <input
          value={display}
          placeholder={disabled ? '' : '— 选择 / 输入筛选 —'}
          disabled={disabled}
          onChange={e => {
            if (disabled) return
            setQuery(e.target.value)
            if (!open) setOpen(true)
          }}
          onFocus={() => {
            if (disabled) return
            setOpen(true)
            setQuery('')
          }}
          onKeyDown={e => {
            if (disabled) return
            if (e.key === 'Escape') {
              setOpen(false)
              ;(e.target as HTMLInputElement).blur()
            } else if (e.key === 'Enter' && open && filtered.length > 0) {
              e.preventDefault()
              pick(filtered[0].id)
              ;(e.target as HTMLInputElement).blur()
            }
          }}
          className={`w-full overflow-hidden text-ellipsis whitespace-nowrap rounded-md border border-gray-300 bg-white px-2 py-1.5 pr-7 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100 ${
            disabled ? 'cursor-not-allowed bg-gray-50 italic text-gray-500 dark:bg-gray-900/40 dark:text-gray-400' : ''
          }`}
        />
        <ChevronDown
          size={14}
          className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-gray-400"
        />
        {open && !disabled && (
          <ul className="absolute left-0 top-full z-20 mt-1 max-h-64 w-max min-w-full max-w-[32rem] overflow-auto rounded-md border border-gray-200 bg-white py-1 shadow-lg dark:border-gray-600 dark:bg-gray-800">
            <li
              // mousedown 而非 click：抢在 input blur 之前触发，避免列表先被关闭。
              onMouseDown={e => {
                e.preventDefault()
                pick('')
              }}
              className="cursor-pointer px-2 py-1.5 text-sm text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700"
            >
              — 清除选择 —
            </li>
            {groups.length === 0 && (
              <li className="px-2 py-1.5 text-sm text-gray-400">无匹配会话</li>
            )}
            {groups.map((g, gi) => {
              // 亲和组之后、第一个非亲和组之前插入"其他项目"弱色分隔。
              const showOtherDivider = hasAffinityGroup && !g.affinity && groups[gi - 1]?.affinity
              return (
                <li key={g.project} className="list-none">
                  {showOtherDivider && (
                    <div className="px-2 pb-1 pt-2 text-[10px] font-medium uppercase tracking-wide text-gray-400 dark:text-gray-500">
                      其他项目
                    </div>
                  )}
                  {/* 组头：📁 + 项目名，sticky 弱色不可点击；亲和组带"匹配当前选择"徽标。 */}
                  <div className="sticky top-0 z-10 flex items-center gap-1.5 bg-white px-2 py-1 text-xs font-medium text-gray-500 dark:bg-gray-800 dark:text-gray-400">
                    <Folder size={12} className="shrink-0 text-gray-400" />
                    <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
                      {g.project}
                    </span>
                    {g.affinity && (
                      <span className="shrink-0 rounded bg-blue-50 px-1 py-0.5 text-[10px] font-normal text-blue-500 dark:bg-blue-900/30 dark:text-blue-300">
                        匹配当前选择
                      </span>
                    )}
                  </div>
                  <ul>
                    {g.items.map(s => (
                      <li
                        key={s.id}
                        onMouseDown={e => {
                          e.preventDefault()
                          pick(s.id)
                        }}
                        title={`${shortTime(s.updated_at)}${s.title ? ` · ${s.title}` : ''}`}
                        className={`flex cursor-pointer items-center justify-between gap-2 px-2 py-1.5 text-sm hover:bg-gray-100 dark:hover:bg-gray-700 ${
                          s.id === value ? 'bg-blue-50 dark:bg-gray-700/60' : ''
                        } ${hasAffinityGroup && !g.affinity ? 'opacity-70' : ''}`}
                      >
                        <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
                          {optionLabel(s)}
                        </span>
                        <span className="shrink-0 text-xs text-gray-400">
                          {relativeTime(s.updated_at)}
                        </span>
                      </li>
                    ))}
                  </ul>
                </li>
              )
            })}
          </ul>
        )}
      </div>
    </div>
  )
}

export function SessionPairPicker({
  sessions,
  claudeId,
  codexId,
  onPick,
  onRefresh,
  loading,
  onNewCodex,
  creatingCodex,
  claudeProject,
  codexProject,
  newCodexMode,
  setNewCodexMode,
  newCodexDesignDoc,
  setNewCodexDesignDoc,
}: Props) {
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-end gap-3">
        <SideSelect
          label="Claude Code 会话"
          provider="claude"
          sessions={sessions}
          value={claudeId}
          onPick={onPick}
          affinityProject={codexProject}
        />
        <SideSelect
          label={newCodexMode ? 'Codex 会话（启动时新建）' : 'Codex 会话'}
          provider="codex"
          sessions={sessions}
          value={codexId}
          onPick={onPick}
          affinityProject={claudeProject}
          disabled={newCodexMode}
          disabledHint={
            newCodexMode
              ? claudeId
                ? '将在「启动复核循环」时按设计文档新建一个 Codex 会话'
                : '请先选 Claude 会话；启动时会新建 Codex'
              : undefined
          }
        />
        {!newCodexMode && (
          <button
            onClick={onNewCodex}
            disabled={creatingCodex || !claudeId}
            title={claudeId ? '立刻新建一个空 Codex 会话（不附文档；消耗 codex 额度）' : '请先选择 Claude 会话以确定仓库目录'}
            className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:hover:bg-gray-800"
          >
            <Plus size={14} className={creatingCodex ? 'animate-spin' : ''} />
            {creatingCodex ? '新建中…' : '新建 Codex'}
          </button>
        )}
        <button
          onClick={onRefresh}
          disabled={loading}
          title="刷新会话清单"
          className="flex items-center gap-1.5 rounded-md border border-gray-300 px-3 py-1.5 text-sm hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:hover:bg-gray-800"
        >
          <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
          刷新
        </button>
      </div>

      <div className="flex items-center gap-3 text-xs">
        <label
          className="flex items-center gap-1.5 text-gray-700 dark:text-gray-200"
          title="勾选后 Codex 下拉禁用，启动时按下方设计文档新建一个 Codex 会话（establishing 阶段就声明 VERDICT / ASK_USER 契约）。"
        >
          <input
            type="checkbox"
            checked={newCodexMode}
            onChange={e => setNewCodexMode(e.target.checked)}
          />
          新建 Codex 会话（启动时建，把设计文档喂进去）
        </label>
        {newCodexMode && (
          <input
            type="text"
            value={newCodexDesignDoc}
            onChange={e => setNewCodexDesignDoc(e.target.value)}
            placeholder="设计文档路径（仓内相对或绝对；启动时随 establishing prompt 一起喂给新 Codex 会话）"
            className="flex-1 rounded-md border border-gray-300 bg-white px-2 py-1 text-xs outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
          />
        )}
      </div>
    </div>
  )
}
