/**
 * MusicPage —— 侧栏「音乐」页（曲库浏览 / 选曲）。
 *
 * 纯控制面：顶部当前文件夹 + 选择/重扫 + 实时格式徽标（来自 music_format_changed）；
 * 主体曲库列表（标题/歌手/时长/封面）+ 搜索过滤；点击 → music_play_queue(列表, index)。
 * 复用 chat-summary / english 的 Tailwind 卡片风格。
 */

import { useEffect, useMemo, useRef, useState } from 'react'
import { convertFileSrc } from '@tauri-apps/api/core'
import { Store } from '@tauri-apps/plugin-store'
import { FolderOpen, Music, Play, RefreshCw, Search, Volume2, XCircle } from 'lucide-react'
import { usePlayer } from './PlayerContext'
import { musicPickFolder, musicScan, type OutputMode, type Track } from './api/tauri-client'

// 曲库持久化：扫描出的 { folder, tracks }，启动直接填充，无需重扫。
const LIBRARY_STORE_FILE = 'music-library.json'
const KEY_LIBRARY = 'library'

interface PersistedLibrary {
  folder: string
  tracks: Track[]
}

function fmtTime(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) secs = 0
  const m = Math.floor(secs / 60)
  const s = Math.floor(secs % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
}

/**
 * 格式徽标文案，如 “FLAC 96kHz/24bit/2ch · 独占 · 无重采样”。
 *
 * 颜色编码（便于定位底噪来源）：
 * - 独占 bit-perfect（exclusive && !resampled）→ 绿色，走原始 PCM 直送路径；
 * - 走了重采样（resampled）→ 琥珀/红色醒目标注，底噪很可能来自此路径。
 */
function FormatBadge() {
  const { format, track } = usePlayer()
  if (!format) return null

  const ext = track?.path?.split('.').pop()?.toUpperCase()
  const khz = (format.sample_rate / 1000)
  const khzStr = Number.isInteger(khz) ? `${khz}kHz` : `${khz.toFixed(1)}kHz`
  const ch = format.channels
  const chStr = ch ? `${ch}ch` : null
  const parts = [
    [ext, [`${khzStr}/${format.bits}bit`, chStr].filter(Boolean).join('/')]
      .filter(Boolean)
      .join(' '),
    format.exclusive ? '独占' : '共享',
    format.resampled ? '已重采样' : '无重采样',
  ]

  const cls = format.resampled
    ? 'bg-red-50 text-red-700 dark:bg-red-900/20 dark:text-red-400'
    : format.exclusive
      ? 'bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400'
      : 'bg-amber-50 text-amber-700 dark:bg-amber-900/20 dark:text-amber-400'

  return (
    <div className="flex flex-col gap-0.5">
      <span
        className={[
          'inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs font-medium',
          cls,
        ].join(' ')}
        title="实际生效的输出格式（采样率/位深/声道 · 独占 vs 共享 · 是否重采样）"
      >
        <Volume2 size={12} />
        {parts.join(' · ')}
      </span>
      {format.resampled && (
        <span className="text-[10px] leading-tight text-red-600/80 dark:text-red-400/80">
          标注「已重采样」时，底噪可能来自设备不支持原始采样率的重采样路径
        </span>
      )}
    </div>
  )
}

export default function MusicPage() {
  const { folder, setFolder, play, track: current, outputMode, setOutputMode } = usePlayer()

  const [tracks, setTracks] = useState<Track[]>([])
  const [query, setQuery] = useState('')
  const [scanning, setScanning] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const libStoreRef = useRef<Store | null>(null)

  // 持久化曲库到 music-library.json（folder + tracks）。
  const persistLibrary = async (dir: string, list: Track[]) => {
    try {
      const store = libStoreRef.current ?? (await Store.load(LIBRARY_STORE_FILE))
      libStoreRef.current = store
      await store.set(KEY_LIBRARY, { folder: dir, tracks: list } as PersistedLibrary)
      await store.save()
    } catch (e) {
      console.error('[Music] 曲库持久化失败:', e)
    }
  }

  // 扫描（手动刷新 / 选新文件夹）→ 覆盖内存 + 持久化存储。
  const scan = async (dir: string) => {
    setScanning(true)
    setError(null)
    try {
      const list = await musicScan(dir)
      setTracks(list)
      void persistLibrary(dir, list)
    } catch (e: any) {
      setError(typeof e === 'string' ? e : (e?.message ?? String(e)))
      setTracks([])
    } finally {
      setScanning(false)
    }
  }

  // 启动：从 music-library.json 读出上次曲库，直接填充（无需重扫）。
  useEffect(() => {
    let mounted = true
    void (async () => {
      try {
        const store = await Store.load(LIBRARY_STORE_FILE)
        libStoreRef.current = store
        const lib = await store.get<PersistedLibrary>(KEY_LIBRARY)
        if (!mounted) return
        if (lib && Array.isArray(lib.tracks)) {
          setTracks(lib.tracks)
          // folder 由 PlayerContext 自身持久化；若缺失则回填，便于「重新扫描」可用。
          if (lib.folder && !folder) setFolder(lib.folder)
        }
      } catch (e) {
        console.error('[Music] 曲库加载失败:', e)
      }
    })()
    return () => {
      mounted = false
    }
    // 仅启动加载一次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const pickFolder = async () => {
    try {
      const dir = await musicPickFolder()
      if (dir) {
        setFolder(dir)
        await scan(dir)
      }
    } catch (e: any) {
      setError(typeof e === 'string' ? e : (e?.message ?? String(e)))
    }
  }

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (!q) return tracks
    return tracks.filter(
      t =>
        t.title.toLowerCase().includes(q) ||
        t.artist.toLowerCase().includes(q) ||
        t.album.toLowerCase().includes(q),
    )
  }, [tracks, query])

  // 点击播放：把当前过滤后的列表作为队列，传该曲在列表里的下标。
  const playAt = (i: number) => {
    const paths = filtered.map(t => t.path)
    void play(paths, i)
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      {/* 顶部：标题 + 文件夹 + 选择/重扫 + 格式徽标 */}
      <div className="flex flex-shrink-0 flex-col gap-3">
        <div className="flex items-center justify-between gap-3">
          <h1 className="text-xl font-semibold">音乐</h1>
          <FormatBadge />
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => void pickFolder()}
            className="flex items-center gap-2 rounded-md bg-blue-500 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-600"
          >
            <FolderOpen size={15} />
            选择文件夹
          </button>
          <button
            type="button"
            onClick={() => folder && void scan(folder)}
            disabled={!folder || scanning}
            className="flex items-center gap-2 rounded-md border border-gray-300 px-3 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-100 disabled:opacity-50 dark:border-gray-600 dark:text-gray-200 dark:hover:bg-gray-800"
          >
            <RefreshCw size={15} className={scanning ? 'animate-spin' : ''} />
            重新扫描
          </button>
          <span className="truncate text-xs text-gray-500 dark:text-gray-400" title={folder ?? ''}>
            {folder ?? '未选择曲库文件夹'}
          </span>
        </div>

        {/* 输出模式选择器 */}
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <label
              htmlFor="music-output-mode"
              className="text-xs font-medium text-gray-600 dark:text-gray-300"
            >
              输出：
            </label>
            <select
              id="music-output-mode"
              value={outputMode}
              onChange={e => setOutputMode(e.target.value as OutputMode)}
              className="rounded-md border border-gray-300 bg-white px-2 py-1 text-xs text-gray-700 outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
            >
              <option value="auto">自动(独占)</option>
              <option value="shared">共享(兼容)</option>
            </select>
          </div>
          <span className="text-[10px] leading-tight text-gray-400">
            44.1kHz 在部分设备独占会加速失真，可切「共享(兼容)」（重采样、音量可调）。
          </span>
        </div>
      </div>

      {/* 搜索 */}
      <div className="relative max-w-sm flex-shrink-0">
        <Search size={15} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
        <input
          value={query}
          onChange={e => setQuery(e.target.value)}
          placeholder="搜索标题 / 歌手 / 专辑…"
          className="w-full rounded-md border border-gray-300 bg-white py-2 pl-9 pr-3 text-sm outline-none focus:border-blue-400 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-100"
        />
      </div>

      {error && (
        <div className="flex flex-shrink-0 items-start gap-2 rounded-md bg-red-50 px-3 py-2 text-xs text-red-600 dark:bg-red-900/20 dark:text-red-400">
          <XCircle size={13} className="mt-0.5 flex-shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {/* 曲库列表（独立滚动区，顶部控制 / 底部统计常驻不动） */}
      <section className="min-h-0 flex-1 overflow-auto rounded-md border border-gray-200 dark:border-gray-700">
        {filtered.length === 0 ? (
          <div className="flex flex-col items-center gap-2 px-4 py-12 text-sm text-gray-400">
            <Music size={28} className="opacity-50" />
            {scanning
              ? '扫描中…'
              : folder
                ? query
                  ? '没有匹配的曲目'
                  : '该文件夹未找到音频文件'
                : '请选择一个曲库文件夹'}
          </div>
        ) : (
          <ul className="divide-y divide-gray-100 dark:divide-gray-800">
            {filtered.map((t, i) => {
              const isCurrent = current?.path === t.path
              const coverSrc = t.cover_path ? convertFileSrc(t.cover_path) : null
              return (
                <li
                  key={t.path}
                  onDoubleClick={() => playAt(i)}
                  className={[
                    'group flex items-center gap-3 px-3 py-2 transition-colors',
                    isCurrent
                      ? 'bg-blue-50 dark:bg-blue-900/20'
                      : 'hover:bg-gray-50 dark:hover:bg-gray-800/50',
                  ].join(' ')}
                >
                  <button
                    type="button"
                    onClick={() => playAt(i)}
                    title="播放"
                    className="relative flex h-10 w-10 flex-shrink-0 items-center justify-center overflow-hidden rounded bg-gray-100 dark:bg-gray-800"
                  >
                    {coverSrc ? (
                      <img src={coverSrc} alt="" className="h-full w-full object-cover" />
                    ) : (
                      <Music size={16} className="text-gray-400" />
                    )}
                    <span className="absolute inset-0 flex items-center justify-center bg-black/40 opacity-0 transition-opacity group-hover:opacity-100">
                      <Play size={16} className="ml-0.5 text-white" />
                    </span>
                  </button>

                  <div className="min-w-0 flex-1">
                    <div
                      className={[
                        'truncate text-sm',
                        isCurrent
                          ? 'font-medium text-blue-700 dark:text-blue-300'
                          : 'text-gray-900 dark:text-gray-100',
                      ].join(' ')}
                    >
                      {t.title || '未知标题'}
                    </div>
                    <div className="truncate text-xs text-gray-500 dark:text-gray-400">
                      {t.artist || '未知歌手'}
                      {t.album ? ` · ${t.album}` : ''}
                    </div>
                  </div>

                  <span className="flex-shrink-0 text-xs tabular-nums text-gray-400">
                    {fmtTime(t.duration_secs)}
                  </span>
                </li>
              )
            })}
          </ul>
        )}
      </section>

      <p className="flex-shrink-0 text-xs text-gray-400">
        共 {filtered.length} 首{query && tracks.length !== filtered.length ? `（已过滤，全部 ${tracks.length} 首）` : ''}。
        点击封面或双击行播放；播放、解码、输出全部在后端原生引擎完成（非浏览器音频）。
      </p>
    </div>
  )
}
