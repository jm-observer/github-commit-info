/**
 * PlayerContext —— 音乐播放全局控制面（无 `<audio>`）。
 *
 * 挂在 App 的 ShellLayout 外层，整个生命周期常驻：切路由不卸载 → 事件订阅与
 * 状态不中断（播放本就在后端，UI 卸载也不停）。
 *
 * 职责：
 *  - 启动时 listen 三个事件（state / progress / track_changed）+ 另外两个
 *    （format_changed / error）→ React state。
 *  - 首屏 music_get_state 拉初值（自愈启动竞态）。
 *  - 暴露 play/pause/resume/toggle/seek/next/prev/setVolume… —— 全是 invoke，
 *    UI 无任何本地播放逻辑。
 *  - 已选目录 / 音量 / repeat / shuffle 用 plugin-store 持久化。
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { Store } from '@tauri-apps/plugin-store'
import {
  musicGetState,
  musicNext,
  musicPause,
  musicPlayQueue,
  musicPrev,
  musicResume,
  musicSeek,
  musicSetRepeat,
  musicSetShuffle,
  musicSetVolume,
  musicStop,
  musicToggle,
  onMusicError,
  onMusicFormatChanged,
  onMusicProgress,
  onMusicStateChanged,
  onMusicTrackChanged,
  setOutputMode as invokeSetOutputMode,
  type MusicFormatChanged,
  type OutputMode,
  type PlaybackState,
  type PlaybackStatus,
  type RepeatMode,
  type Track,
} from './api/tauri-client'

const STORE_FILE = 'music-player.json'
const KEY_FOLDER = 'folder'
const KEY_VOLUME = 'volume'
const KEY_REPEAT = 'repeat'
const KEY_SHUFFLE = 'shuffle'
const KEY_OUTPUT_MODE = 'output_mode'
// 上次播放会话：队列（路径数组）+ **实时跟随的当前下标/播放位置** + 当前曲目元信息。
// 启动时后端引擎是空的（无「只加载不播放」命令），靠这两项回填底栏，
// 让播放键冷启动即可点 → 点一下从上次那首、上次那个位置续播。
const KEY_LAST_QUEUE = 'last_queue'
const KEY_LAST_TRACK = 'last_track'
// 会话进度写盘节流：位置每秒都在变，但落盘是文件 IO，5s 一次足够（最多丢 5s 进度）。
const SESSION_PERSIST_INTERVAL_MS = 5000
// 兜底轮询间隔：有曲目时 3s；停止播放且无曲目时降到 15s。
const POLL_ACTIVE_MS = 3000
const POLL_IDLE_MS = 15000

interface LastQueue {
  queue: string[]
  /** 当前曲目在队列中的下标。**随自动切歌实时更新**，不是起播时的那个。 */
  index: number
  /** 当前曲内位置（秒）。老版本存档无此字段 → 视作 0（从曲首续播）。 */
  position_secs?: number
}

interface PlayerContextValue {
  // 状态（事件驱动 + 首屏拉取）
  status: PlaybackStatus
  index: number
  track: Track | null
  positionSecs: number
  durationSecs: number
  volume: number
  repeat: RepeatMode
  shuffle: boolean
  format: MusicFormatChanged | null
  error: string | null

  // 持久化的已选目录（曲库根）
  folder: string | null
  setFolder: (dir: string | null) => void

  // 输出模式（auto=独占 bit-perfect / shared=共享兼容），持久化 + 同步后端
  outputMode: OutputMode
  setOutputMode: (mode: OutputMode) => void

  // 待恢复的上次会话：非 null 表示底栏显示的是回填曲目（尚未真正播放），
  // 此时点播放键应调 resumeLast() 续播，而非 toggle()。
  pendingResume: LastQueue | null
  /** 从上次会话续播（上次那首 + 上次那个位置）。无待恢复会话时是 no-op。 */
  resumeLast: () => Promise<void>

  // 控制（全 invoke）
  play: (paths: string[], start: number) => Promise<void>
  pause: () => Promise<void>
  resume: () => Promise<void>
  toggle: () => Promise<void>
  stop: () => Promise<void>
  seek: (secs: number) => Promise<void>
  next: () => Promise<void>
  prev: () => Promise<void>
  setVolume: (vol: number) => Promise<void>
  setRepeat: (mode: RepeatMode) => Promise<void>
  setShuffle: (on: boolean) => Promise<void>
}

const PlayerContext = createContext<PlayerContextValue | null>(null)

export function usePlayer(): PlayerContextValue {
  const ctx = useContext(PlayerContext)
  if (!ctx) throw new Error('usePlayer 必须在 <MusicPlayerProvider> 内使用')
  return ctx
}

export function MusicPlayerProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<PlaybackStatus>('stopped')
  const [index, setIndex] = useState(0)
  const [track, setTrack] = useState<Track | null>(null)
  const [positionSecs, setPositionSecs] = useState(0)
  const [durationSecs, setDurationSecs] = useState(0)
  const [volume, setVolumeState] = useState(1)
  const [repeat, setRepeatState] = useState<RepeatMode>('off')
  const [shuffle, setShuffleState] = useState(false)
  const [format, setFormat] = useState<MusicFormatChanged | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [folder, setFolderState] = useState<string | null>(null)
  const [outputMode, setOutputModeState] = useState<OutputMode>('auto')
  const [pendingResume, setPendingResume] = useState<LastQueue | null>(null)

  const storeRef = useRef<Store | null>(null)
  // 与 applyState（[] 依赖、闭包固定）共享的待恢复标志，避免周期兜底拉取
  // 用后端空快照覆盖掉回填的曲目展示。
  const resumingRef = useRef(false)
  // 当前播放会话的实时镜像（队列 + 下标 + 位置）。放 ref 而非 state：位置每 250ms 变一次，
  // 进 state 会带着整棵树重渲染，而它的唯一用途是定时写盘。
  const sessionRef = useRef<LastQueue | null>(null)

  // 应用后端权威快照到本地 state。
  const applyState = useCallback((s: PlaybackState) => {
    setStatus(s.status)
    setVolumeState(s.volume)
    setRepeatState(s.repeat)
    setShuffleState(s.shuffle)
    // 待恢复态下，后端是空快照（stopped/null）；保留回填的曲目，等用户点播放再续播。
    if (resumingRef.current && s.track === null && s.status === 'stopped') return
    setIndex(s.index)
    setTrack(s.track)
    setPositionSecs(s.position_secs)
    setDurationSecs(s.duration_secs)
  }, [])

  const persist = useCallback(async (key: string, value: unknown) => {
    const store = storeRef.current
    if (!store) return
    try {
      await store.set(key, value)
      await store.save()
    } catch (e) {
      console.error('[MusicPlayer] 持久化失败:', key, e)
    }
  }, [])

  /** 把会话镜像落盘（队列 + 当前下标 + 当前位置）。无会话时不写，免得覆盖掉有效存档。 */
  const persistSession = useCallback(() => {
    const s = sessionRef.current
    if (!s || s.queue.length === 0) return
    void persist(KEY_LAST_QUEUE, s)
  }, [persist])

  // ── 持久化加载 + 事件订阅 + 首屏拉取 ───────────────────────────────────────
  useEffect(() => {
    let mounted = true
    const unlistens: Array<() => void> = []

    async function init() {
      // 1) plugin-store 读已选目录 / 音量 / repeat / shuffle
      try {
        const store = await Store.load(STORE_FILE)
        storeRef.current = store
        const f = await store.get<string>(KEY_FOLDER)
        const v = await store.get<number>(KEY_VOLUME)
        const r = await store.get<RepeatMode>(KEY_REPEAT)
        const sh = await store.get<boolean>(KEY_SHUFFLE)
        const om = await store.get<OutputMode>(KEY_OUTPUT_MODE)
        if (!mounted) return
        if (f) setFolderState(f)
        if (r === 'off' || r === 'one' || r === 'all') setRepeatState(r)
        if (typeof sh === 'boolean') setShuffleState(sh)
        // 输出模式：读出持久值（默认 auto）→ 同步给后端一次
        const mode: OutputMode = om === 'shared' ? 'shared' : 'auto'
        setOutputModeState(mode)
        void invokeSetOutputMode(mode).catch(e =>
          console.error('[MusicPlayer] 同步输出模式失败:', e),
        )
        // 音量/循环/随机：**必须 await 同步给后端**再往下走。后端引擎冷启动是默认值
        // （音量 1.0 / off / 关），而下面第 3 步的 musicGetState 会用后端快照覆盖本地
        // state——不先推给后端，读出来的持久值转头就被冲掉（音量重启即回满格的原因）。
        // 命令通道是 FIFO，这些命令先入队，Snapshot 必在其后处理，拿到的就是新值。
        if (typeof v === 'number') {
          setVolumeState(v)
          await musicSetVolume(v).catch(e =>
            console.error('[MusicPlayer] 同步音量失败:', e),
          )
        }
        if (r === 'off' || r === 'one' || r === 'all') {
          await musicSetRepeat(r).catch(e =>
            console.error('[MusicPlayer] 同步循环模式失败:', e),
          )
        }
        if (typeof sh === 'boolean') {
          await musicSetShuffle(sh).catch(e =>
            console.error('[MusicPlayer] 同步随机模式失败:', e),
          )
        }
      } catch (e) {
        console.error('[MusicPlayer] store 加载失败:', e)
      }

      // 2) 订阅事件
      try {
        unlistens.push(
          await onMusicStateChanged(p => {
            setStatus(p.status)
            setIndex(p.index)
            setTrack(p.track)
            if (sessionRef.current && p.index >= 0) sessionRef.current.index = p.index
          }),
        )
        unlistens.push(
          await onMusicProgress(p => {
            setPositionSecs(p.position_secs)
            setDurationSecs(p.duration_secs)
            if (sessionRef.current) sessionRef.current.position_secs = p.position_secs
          }),
        )
        unlistens.push(
          await onMusicTrackChanged(p => {
            setIndex(p.index)
            setTrack(p.track)
            // 切歌（含 gapless 自动推进）是重要节点：立刻更新镜像并落盘，
            // 不等 5s 定时器——否则重启回到的是「上次手动点播的那首」而非真正听到的那首。
            if (sessionRef.current && p.index >= 0) {
              sessionRef.current.index = p.index
              sessionRef.current.position_secs = 0
              persistSession()
            }
          }),
        )
        unlistens.push(
          await onMusicFormatChanged(p => {
            setFormat(p)
          }),
        )
        unlistens.push(
          await onMusicError(p => {
            setError(p.message)
          }),
        )
      } catch (e) {
        console.error('[MusicPlayer] 事件订阅失败:', e)
      }

      // 3) 首屏拉初值（自愈启动竞态）
      let backendHasTrack = false
      try {
        const s = await musicGetState()
        if (mounted) applyState(s)
        backendHasTrack = s.track !== null
      } catch {
        /* 后端尚未就绪则忽略，后续事件会补 */
      }

      // 4) 后端冷启动无曲目时，回填上次会话 → 底栏可见曲目、播放键可点。
      //    点播放键时（见 MiniPlayer）走 play(queue,index) 从该曲开头续播。
      if (mounted && !backendHasTrack) {
        try {
          const store = storeRef.current
          const lastQueue = await store?.get<LastQueue>(KEY_LAST_QUEUE)
          const lastTrack = await store?.get<Track>(KEY_LAST_TRACK)
          if (
            mounted &&
            lastQueue &&
            Array.isArray(lastQueue.queue) &&
            lastQueue.queue.length > 0 &&
            lastTrack
          ) {
            resumingRef.current = true
            setPendingResume(lastQueue)
            setTrack(lastTrack)
            setIndex(lastQueue.index)
            setPositionSecs(lastQueue.position_secs ?? 0)
            setDurationSecs(lastTrack.duration_secs ?? 0)
          }
        } catch (e) {
          console.error('[MusicPlayer] 上次会话恢复失败:', e)
        }
      }
    }

    void init()

    return () => {
      mounted = false
      unlistens.forEach(fn => fn())
    }
  }, [applyState, persistSession])

  // 空闲判定（停止播放且无曲目）。放 ref 供轮询闭包读最新值，不必重建定时器。
  const idleRef = useRef(false)
  idleRef.current = status === 'stopped' && track === null

  // 周期兜底拉取后端真值，自愈漏接事件。
  // 每次拉取都要经引擎线程回传快照（Snapshot 命令），空闲时按 3s 敲门纯属白白唤醒它 ——
  // 故用递归 setTimeout 按当前状态定间隔，而非固定 setInterval。
  useEffect(() => {
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined

    const tick = async () => {
      try {
        const s = await musicGetState()
        if (!cancelled) applyState(s)
      } catch {
        /* 忽略抖动 */
      }
      if (cancelled) return
      timer = setTimeout(tick, idleRef.current ? POLL_IDLE_MS : POLL_ACTIVE_MS)
    }

    timer = setTimeout(tick, POLL_ACTIVE_MS)
    return () => {
      cancelled = true
      if (timer) clearTimeout(timer)
    }
  }, [applyState])

  // 会话进度定时落盘（下标随切歌已即时写，这里主要是曲内位置）。
  useEffect(() => {
    const timer = setInterval(persistSession, SESSION_PERSIST_INTERVAL_MS)
    return () => clearInterval(timer)
  }, [persistSession])

  // 当前曲目变化时持久化元信息（供下次启动回填底栏）。
  useEffect(() => {
    if (track) void persist(KEY_LAST_TRACK, track)
  }, [track, persist])

  // ── 已选目录 ───────────────────────────────────────────────────────────────
  const setFolder = useCallback(
    (dir: string | null) => {
      setFolderState(dir)
      void persist(KEY_FOLDER, dir)
    },
    [persist],
  )

  // ── 输出模式 ─────────────────────────────────────────────────────────────────
  const setOutputMode = useCallback(
    (mode: OutputMode) => {
      setOutputModeState(mode)
      void persist(KEY_OUTPUT_MODE, mode)
      void invokeSetOutputMode(mode).catch(e =>
        console.error('[MusicPlayer] 设置输出模式失败:', e),
      )
    },
    [persist],
  )

  // ── 控制方法（全 invoke，乐观更新本地控件态再以后端事件为准） ───────────────
  const play = useCallback(
    (paths: string[], start: number) => {
      // 真正开播：清掉待恢复态，建立会话镜像（此后由事件回调实时跟随）并落盘。
      resumingRef.current = false
      setPendingResume(null)
      sessionRef.current = { queue: paths, index: start, position_secs: 0 }
      persistSession()
      return musicPlayQueue(paths, start)
    },
    [persistSession],
  )

  /**
   * 从上次会话续播：起播上次那首，再 seek 回上次的位置。
   *
   * 两条命令连发即可——引擎命令通道是 FIFO，`Seek` 必在 `PlayQueue` 的 `start_current`
   * 完成之后才被处理，不存在「seek 到还没建好的解码器」的竞态。
   */
  const resumeLast = useCallback(async () => {
    const last = pendingResume
    if (!last || last.queue.length === 0) return
    const pos = last.position_secs ?? 0
    await play(last.queue, last.index)
    if (pos > 0) {
      sessionRef.current = { queue: last.queue, index: last.index, position_secs: pos }
      await musicSeek(pos)
    }
  }, [pendingResume, play])
  const pause = useCallback(() => musicPause(), [])
  const resume = useCallback(() => musicResume(), [])
  const toggle = useCallback(() => musicToggle(), [])
  const stop = useCallback(() => {
    sessionRef.current = null // 显式停止 = 放弃会话，下次启动不再回填
    return musicStop()
  }, [])
  const seek = useCallback(
    (secs: number) => {
      setPositionSecs(secs) // 即时反馈，随后端 progress 事件校正
      // 待恢复态：后端队列还是空的，seek 过去没有意义——改成挪动「续播起点」，
      // 让用户拖完进度条再点播放就从那里开始。
      if (pendingResume) {
        setPendingResume({ ...pendingResume, position_secs: secs })
        return Promise.resolve()
      }
      return musicSeek(secs)
    },
    [pendingResume],
  )

  // 待恢复态下后端没有队列，next/prev 直接发命令是空操作（点了没反应）。
  // 先把上次会话装回后端再执行，语义即「从上次那首的下/上一首开始」。
  const next = useCallback(async () => {
    if (pendingResume) await resumeLast()
    return musicNext()
  }, [pendingResume, resumeLast])
  const prev = useCallback(async () => {
    if (pendingResume) await resumeLast()
    return musicPrev()
  }, [pendingResume, resumeLast])

  const setVolume = useCallback(
    (vol: number) => {
      setVolumeState(vol)
      void persist(KEY_VOLUME, vol)
      return musicSetVolume(vol)
    },
    [persist],
  )

  const setRepeat = useCallback(
    (mode: RepeatMode) => {
      setRepeatState(mode)
      void persist(KEY_REPEAT, mode)
      return musicSetRepeat(mode)
    },
    [persist],
  )

  const setShuffle = useCallback(
    (on: boolean) => {
      setShuffleState(on)
      void persist(KEY_SHUFFLE, on)
      return musicSetShuffle(on)
    },
    [persist],
  )

  const value = useMemo<PlayerContextValue>(
    () => ({
      status,
      index,
      track,
      positionSecs,
      durationSecs,
      volume,
      repeat,
      shuffle,
      format,
      error,
      folder,
      setFolder,
      outputMode,
      setOutputMode,
      pendingResume,
      resumeLast,
      play,
      pause,
      resume,
      toggle,
      stop,
      seek,
      next,
      prev,
      setVolume,
      setRepeat,
      setShuffle,
    }),
    [
      status,
      index,
      track,
      positionSecs,
      durationSecs,
      volume,
      repeat,
      shuffle,
      format,
      error,
      folder,
      setFolder,
      outputMode,
      setOutputMode,
      pendingResume,
      resumeLast,
      play,
      pause,
      resume,
      toggle,
      stop,
      seek,
      next,
      prev,
      setVolume,
      setRepeat,
      setShuffle,
    ],
  )

  return <PlayerContext.Provider value={value}>{children}</PlayerContext.Provider>
}
