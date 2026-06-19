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
// 上次播放会话：队列（路径数组）+ 起始下标 + 当前曲目元信息。
// 启动时后端引擎是空的（无「只加载不播放」命令），靠这两项回填底栏，
// 让播放键冷启动即可点 → 点一下从该曲开头续播。
const KEY_LAST_QUEUE = 'last_queue'
const KEY_LAST_TRACK = 'last_track'

interface LastQueue {
  queue: string[]
  index: number
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
  // 此时点播放键应 play(queue, index) 续播，而非 toggle()。
  pendingResume: LastQueue | null

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
        if (typeof v === 'number') setVolumeState(v)
        if (r === 'off' || r === 'one' || r === 'all') setRepeatState(r)
        if (typeof sh === 'boolean') setShuffleState(sh)
        // 输出模式：读出持久值（默认 auto）→ 同步给后端一次
        const mode: OutputMode = om === 'shared' ? 'shared' : 'auto'
        setOutputModeState(mode)
        void invokeSetOutputMode(mode).catch(e =>
          console.error('[MusicPlayer] 同步输出模式失败:', e),
        )
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
          }),
        )
        unlistens.push(
          await onMusicProgress(p => {
            setPositionSecs(p.position_secs)
            setDurationSecs(p.duration_secs)
          }),
        )
        unlistens.push(
          await onMusicTrackChanged(p => {
            setIndex(p.index)
            setTrack(p.track)
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
            setPositionSecs(0)
            setDurationSecs(lastTrack.duration_secs ?? 0)
          }
        } catch (e) {
          console.error('[MusicPlayer] 上次会话恢复失败:', e)
        }
      }
    }

    void init()

    // 周期兜底拉取后端真值，自愈漏接事件。
    const poll = setInterval(() => {
      musicGetState()
        .then(s => {
          if (mounted) applyState(s)
        })
        .catch(() => {/* 忽略抖动 */})
    }, 3000)

    return () => {
      mounted = false
      clearInterval(poll)
      unlistens.forEach(fn => fn())
    }
  }, [applyState])

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
      // 真正开播：清掉待恢复态，持久化队列供下次启动回填。
      resumingRef.current = false
      setPendingResume(null)
      void persist(KEY_LAST_QUEUE, { queue: paths, index: start } as LastQueue)
      return musicPlayQueue(paths, start)
    },
    [persist],
  )
  const pause = useCallback(() => musicPause(), [])
  const resume = useCallback(() => musicResume(), [])
  const toggle = useCallback(() => musicToggle(), [])
  const stop = useCallback(() => musicStop(), [])
  const seek = useCallback((secs: number) => {
    setPositionSecs(secs) // 即时反馈，随后端 progress 事件校正
    return musicSeek(secs)
  }, [])
  const next = useCallback(() => musicNext(), [])
  const prev = useCallback(() => musicPrev(), [])

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
