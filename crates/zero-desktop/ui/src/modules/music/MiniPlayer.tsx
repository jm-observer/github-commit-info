/**
 * MiniPlayer —— ShellLayout 底栏常驻迷你播放器。
 *
 * 封面 + 标题/歌手 + 播/暂停/上下首 + 进度条（拖动 → music_seek）+ 音量 +
 * repeat/shuffle。全从 usePlayer() 取，任何页面可控。无音频对象。
 */

import { useEffect, useState } from 'react'
import { convertFileSrc } from '@tauri-apps/api/core'
import {
  Music,
  Pause,
  Play,
  Repeat,
  Repeat1,
  Shuffle,
  SkipBack,
  SkipForward,
  Volume2,
} from 'lucide-react'
import { usePlayer } from './PlayerContext'
import type { RepeatMode } from './api/tauri-client'

function fmtTime(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) secs = 0
  const m = Math.floor(secs / 60)
  const s = Math.floor(secs % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
}

function nextRepeat(mode: RepeatMode): RepeatMode {
  return mode === 'off' ? 'all' : mode === 'all' ? 'one' : 'off'
}

export default function MiniPlayer() {
  const {
    status,
    track,
    positionSecs,
    durationSecs,
    volume,
    repeat,
    shuffle,
    format,
    pendingResume,
    resumeLast,
    toggle,
    next,
    prev,
    seek,
    setVolume,
    setRepeat,
    setShuffle,
  } = usePlayer()

  // 拖动进度条时本地接管显示，松手才 commit 到后端（避免事件回跳）。
  const [dragging, setDragging] = useState(false)
  const [dragValue, setDragValue] = useState(0)

  useEffect(() => {
    if (!dragging) setDragValue(positionSecs)
  }, [positionSecs, dragging])

  // 松手/失焦即提交当前拖动值。用 pointer 事件而非 mouseup：触屏/笔一样走这条路，
  // 且指针在拖动中被系统取消（如切窗口）时有 pointercancel 兜底 —— 否则 dragging
  // 会一直为 true，进度条卡在拖动值上不再跟随播放。
  const commitDrag = (el: HTMLInputElement) => {
    if (!dragging) return
    setDragging(false)
    void seek(Number(el.value))
  }

  const playing = status === 'playing'
  const hasTrack = track !== null
  const pos = dragging ? dragValue : positionSecs
  const dur = durationSecs > 0 ? durationSecs : (track?.duration_secs ?? 0)

  const coverSrc = track?.cover_path ? convertFileSrc(track.cover_path) : null

  // 「内嵌 body」形态: 不带外层 h-16/border-t/bg/padding, 由 BottomPlayerBar 统一包裹。
  return (
    <div className="flex h-full w-full items-center gap-3 px-4">
      {/* 封面 + 标题/歌手 */}
      <div className="flex w-56 min-w-0 flex-shrink-0 items-center gap-3">
        <div className="flex h-11 w-11 flex-shrink-0 items-center justify-center overflow-hidden rounded bg-gray-100 dark:bg-gray-800">
          {coverSrc ? (
            <img src={coverSrc} alt="" className="h-full w-full object-cover" />
          ) : (
            <Music size={18} className="text-gray-400" />
          )}
        </div>
        <div className="min-w-0">
          <div className="truncate text-sm font-medium text-gray-900 dark:text-gray-100">
            {track?.title || '未在播放'}
          </div>
          <div className="truncate text-xs text-gray-500 dark:text-gray-400">
            {track?.artist || (hasTrack ? '未知歌手' : '选择曲库开始播放')}
          </div>
        </div>
      </div>

      {/* 传输控制 + 进度条 */}
      <div className="flex flex-1 flex-col items-center gap-1">
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={() => void setShuffle(!shuffle)}
            title={shuffle ? '随机播放：开' : '随机播放：关'}
            className={[
              'rounded p-1 transition-colors',
              shuffle
                ? 'text-blue-500'
                : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-200',
            ].join(' ')}
          >
            <Shuffle size={16} />
          </button>

          <button
            type="button"
            onClick={() => void prev()}
            disabled={!hasTrack}
            title="上一首"
            className="rounded p-1 text-gray-600 transition-colors hover:text-gray-900 disabled:opacity-40 dark:text-gray-300 dark:hover:text-white"
          >
            <SkipBack size={18} />
          </button>

          <button
            type="button"
            onClick={() => (pendingResume ? void resumeLast() : void toggle())}
            disabled={!hasTrack}
            title={playing ? '暂停' : '播放'}
            className="flex h-9 w-9 items-center justify-center rounded-full bg-blue-500 text-white transition-colors hover:bg-blue-600 disabled:opacity-40"
          >
            {playing ? <Pause size={18} /> : <Play size={18} className="ml-0.5" />}
          </button>

          <button
            type="button"
            onClick={() => void next()}
            disabled={!hasTrack}
            title="下一首"
            className="rounded p-1 text-gray-600 transition-colors hover:text-gray-900 disabled:opacity-40 dark:text-gray-300 dark:hover:text-white"
          >
            <SkipForward size={18} />
          </button>

          <button
            type="button"
            onClick={() => void setRepeat(nextRepeat(repeat))}
            title={
              repeat === 'off' ? '循环：关' : repeat === 'all' ? '循环：列表' : '循环：单曲'
            }
            className={[
              'rounded p-1 transition-colors',
              repeat === 'off'
                ? 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-200'
                : 'text-blue-500',
            ].join(' ')}
          >
            {repeat === 'one' ? <Repeat1 size={16} /> : <Repeat size={16} />}
          </button>
        </div>

        <div className="flex w-full max-w-xl items-center gap-2">
          <span className="w-9 text-right text-[11px] tabular-nums text-gray-400">
            {fmtTime(pos)}
          </span>
          <input
            type="range"
            min={0}
            max={dur > 0 ? dur : 1}
            step={0.1}
            value={Math.min(pos, dur > 0 ? dur : 1)}
            disabled={!hasTrack || dur <= 0}
            onChange={e => {
              setDragging(true)
              setDragValue(Number(e.target.value))
            }}
            onPointerUp={e => commitDrag(e.currentTarget)}
            onPointerCancel={e => commitDrag(e.currentTarget)}
            onBlur={e => commitDrag(e.currentTarget)}
            onKeyUp={e => commitDrag(e.currentTarget)}
            className="h-1 flex-1 cursor-pointer accent-blue-500 disabled:cursor-default disabled:opacity-50"
          />
          <span className="w-9 text-[11px] tabular-nums text-gray-400">{fmtTime(dur)}</span>
        </div>
      </div>

      {/* 音量。独占 bit-perfect 下引擎旁路软件音量（不污染原始 PCM），滑块此刻不起作用，
          只记住设置值——必须明确告诉用户，否则表现就是「音量条坏了」。 */}
      <div className="flex w-32 flex-shrink-0 items-center gap-2">
        <Volume2
          size={16}
          className={[
            'flex-shrink-0',
            format?.exclusive ? 'text-amber-500' : 'text-gray-400',
          ].join(' ')}
        />
        <input
          type="range"
          min={0}
          max={1}
          step={0.01}
          value={volume}
          onChange={e => void setVolume(Number(e.target.value))}
          title={
            format?.exclusive
              ? `音量 ${Math.round(volume * 100)}%（独占 bit-perfect 模式下软件音量旁路，此处不生效；请用系统音量，或在音乐页切「共享(兼容)」）`
              : `音量 ${Math.round(volume * 100)}%`
          }
          className={[
            'h-1 flex-1 cursor-pointer accent-blue-500',
            format?.exclusive ? 'opacity-50' : '',
          ].join(' ')}
        />
      </div>
    </div>
  )
}
