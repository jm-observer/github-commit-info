/**
 * PackagePlayer — 「按包来听」播放器（迁自 mini-program 的 package-player）。
 *
 * 与 AnnotationPlayer 复用同一套播放底座（AudioPlayerService 单例 + AudioPlayer UI +
 * ShadowPanel + FileCacheManager），区别在于数据来源是「音频包」：先 package.list 拉包列表，
 * 用户选包后 package.sentences 拉该包句子，再走同样的缓存 + 单例初始化 + 自动播放流程。
 * 单例归属用 playerSession.owner 标记，避免与标注/全部入口交叉接管单例。
 */

import { useState, useEffect, useRef } from 'react'
import { Loader2, AlertCircle, RefreshCw, Music, SkipForward } from 'lucide-react'
import AudioPlayer from './AudioPlayer'
import ShadowPanel from '../shadow/ShadowPanel'
import { AudioPlayerService } from '../services/AudioPlayerService'
import ApiService from '../services/ApiService'
import FileCacheManager from '../services/FileCacheManager'
import EnvConfigService from '../services/EnvConfigService'
import HtmlAudioAdapter from '../adapters/HtmlAudioAdapter'
import { playerSession } from '../services/playerSession'
import { shouldCacheAudio, backgroundCacheAudios } from '../services/audioCache'
import type { Package, Sentence } from '../types'
import { Button } from '../../speech/components/ui/Button'
import { readAutoStartPref } from '../autoStartPref'

const LAST_PACKAGE_KEY = 'english_pkg_last_selected_package_id'

// 跨路由切换 / Strict Mode 挂载的全局会话状态（同 AnnotationPlayer 纪律，但按 packageId 记）。
const packageInstanceState = {
  initializedPackageId: null as number | null,
  isInitializing: false,
  sentences: [] as Sentence[],
}

function errMsg(err: any): string {
  return (
    err?.message ||
    (typeof err === 'string' ? err : null) ||
    (() => { try { return JSON.stringify(err) } catch { return String(err) } })()
  )
}

export default function PackagePlayer({ autoStart = true }: { autoStart?: boolean }) {
  const [packages, setPackages] = useState<Package[]>([])
  const [pkgLoading, setPkgLoading] = useState(false)
  const [selectedPackageId, setSelectedPackageId] = useState<number | null>(null)

  const [loading, setLoading] = useState(false)
  const [loadingText, setLoadingText] = useState('正在加载...')
  const [error, setError] = useState<string | null>(null)
  const [sentences, setSentences] = useState<Sentence[]>([])
  const [initialized, setInitialized] = useState(false)

  const autoPlayStartedRef = useRef(false)
  const backgroundDownloadCancelRef = useRef(false)

  useEffect(() => {
    let cancelled = false

    const run = async () => {
      try {
        setPkgLoading(true)
        setError(null)
        const resp = await ApiService.getInstance().getPackages()
        if (cancelled) return
        const list: Package[] = (resp.data as Package[]) || []
        setPackages(list)
        setPkgLoading(false)

        if (list.length === 0) {
          setError('没有可用的音频包')
          return
        }

        // 恢复上次选择的包。
        const savedRaw = localStorage.getItem(LAST_PACKAGE_KEY)
        const saved = savedRaw ? Number(savedRaw) : NaN
        const restore = list.find((p) => p.id === saved)?.id
        if (restore == null) return // 没有上次记录 → 等用户手动选

        setSelectedPackageId(restore)

        // fast-path：同一包且单例仍归本「听包」入口所有 → 直接挂回 UI，不停播放、不重拉。
        if (
          packageInstanceState.initializedPackageId === restore &&
          packageInstanceState.sentences.length > 0 &&
          playerSession.owner === `pkg:${restore}`
        ) {
          setSentences(packageInstanceState.sentences)
          setInitialized(true)
          return
        }

        // 正在初始化中（Strict Mode 双 mount）→ 直接等，避免建出两个单例同时播。
        if (packageInstanceState.isInitializing) return

        void initPackage(restore)
      } catch (err: any) {
        if (cancelled) return
        setPkgLoading(false)
        setError(errMsg(err))
      }
    }

    void run()

    // unmount：只取消后台下载，不停播放——单例继续在后台跑，切菜单回来无缝接续。
    return () => {
      cancelled = true
      backgroundDownloadCancelRef.current = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const initPackage = async (pkgId: number) => {
    if (packageInstanceState.isInitializing) return
    packageInstanceState.isInitializing = true
    backgroundDownloadCancelRef.current = false
    autoPlayStartedRef.current = false
    setInitialized(false)
    setError(null)
    setSentences([])

    try {
      setLoading(true)
      setLoadingText('正在获取包内句子...')

      // 切包前先把残留单例的音停掉（HtmlAudioAdapter 的 <audio> 脱离 DOM，不主动 stop 会继续响）。
      try { AudioPlayerService.getInstance().stopAudio() } catch { /* not yet initialized */ }

      const resp = await ApiService.getInstance().getPackageSentences(pkgId)
      const list: Sentence[] = (resp.data as Sentence[]) || []
      if (list.length === 0) throw new Error('该包没有句子')
      setSentences(list)

      setLoadingText('正在初始化播放器...')
      await new Promise((r) => setTimeout(r, 100))
      await initAudioPlayer(list)

      // 缓存不阻塞起播：LAN 直接流式不缓存，WAN 后台静默缓存整批。
      const envConfig = await EnvConfigService.getInstance().getConfig()
      if (shouldCacheAudio(envConfig.apiBaseUrl)) {
        backgroundDownloadCancelRef.current = false
        void backgroundCacheAudios(list, envConfig.apiBaseUrl, backgroundDownloadCancelRef)
      }

      setLoading(false)
      setInitialized(true)
      packageInstanceState.initializedPackageId = pkgId
      packageInstanceState.sentences = list
      playerSession.owner = `pkg:${pkgId}`
      localStorage.setItem(LAST_PACKAGE_KEY, String(pkgId))
    } catch (err: any) {
      console.error('听包播放器初始化失败:', err)
      setLoading(false)
      setError(errMsg(err) || '初始化失败（无详情）')
    } finally {
      packageInstanceState.isInitializing = false
    }
  }

  const initAudioPlayer = async (sentencesList: Sentence[]) => {
    if (!sentencesList.length) throw new Error('没有可播放的句子')

    // 「启动自动播放」只在本次会话首次进入英语听力时生效一次；切包属于"换歌单"，载入后保持暂停。
    const hadInstance = AudioPlayerService.hasInstance()

    const envConfig = await EnvConfigService.getInstance().getConfig()
    const cacheManager = FileCacheManager.getInstance()
    // 销毁所有历史 adapter（含失联幽灵）后再建新的，同 AnnotationPlayer。
    HtmlAudioAdapter.destroyAll()
    const audioAdapter = new HtmlAudioAdapter()

    try { AudioPlayerService.getInstance().stopAudio() } catch { /* no prior instance */ }
    AudioPlayerService.resetInstance()
    const audioService = AudioPlayerService.getInstance(audioAdapter, cacheManager, envConfig)

    audioService.setSentences(sentencesList)
    audioService.setMaxPlayCount(4)
    audioService.resetPlayer()

    if (autoStart && readAutoStartPref() && !hadInstance && !autoPlayStartedRef.current) {
      autoPlayStartedRef.current = true
      setTimeout(() => {
        try { void audioService.playCurrentAudio() }
        catch (err) { console.error('自动播放失败:', err); autoPlayStartedRef.current = false }
      }, 500)
    }
  }

  const handleSelectPackage = (pkgId: number) => {
    if (Number.isNaN(pkgId)) return
    if (pkgId === selectedPackageId && initialized) return
    setSelectedPackageId(pkgId)
    void initPackage(pkgId)
  }

  // 「下一集」：按列表顺序切到下一个包（末尾回到第一个）。
  const handleNextPackage = () => {
    if (packages.length === 0) return
    const idx = packages.findIndex((p) => p.id === selectedPackageId)
    const next = packages[(idx + 1) % packages.length]
    if (next) handleSelectPackage(next.id)
  }

  const handleReload = () => {
    packageInstanceState.initializedPackageId = null
    packageInstanceState.sentences = []
    try { AudioPlayerService.getInstance().stopAudio() } catch { /* ignore */ }
    setInitialized(false)
    setSentences([])
    setError(null)
    if (selectedPackageId != null) void initPackage(selectedPackageId)
  }

  const selectedPackage = packages.find((p) => p.id === selectedPackageId) || null

  return (
    <div className="flex flex-col gap-4">
      {/* 包选择条 */}
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm text-gray-500 dark:text-gray-400">音频包</span>
        <select
          value={selectedPackageId ?? ''}
          disabled={pkgLoading || packages.length === 0}
          onChange={(e) => handleSelectPackage(Number(e.target.value))}
          className="min-w-[14rem] rounded-md border border-gray-300 bg-white px-3 py-1.5 text-sm text-gray-900 disabled:opacity-60 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-100"
        >
          <option value="" disabled>
            {pkgLoading ? '正在加载包列表...' : '请选择一个音频包'}
          </option>
          {packages.map((p) => (
            <option key={p.id} value={p.id}>
              {p.title}
              {typeof p.sentence_count === 'number' ? `（${p.sentence_count} 句）` : ''}
            </option>
          ))}
        </select>
        <Button
          variant="outline"
          size="sm"
          disabled={packages.length === 0}
          onClick={handleNextPackage}
        >
          <SkipForward size={14} />
          下一集
        </Button>
      </div>

      {selectedPackage?.description && (
        <p className="text-sm text-gray-500 dark:text-gray-400">{selectedPackage.description}</p>
      )}

      {/* 加载中 */}
      {loading && (
        <div className="flex flex-col items-center justify-center gap-3 py-16 text-gray-500 dark:text-gray-400">
          <Loader2 size={32} className="animate-spin" />
          <span className="text-sm">{loadingText}</span>
        </div>
      )}

      {/* 错误 */}
      {error && !loading && (
        <div className="flex flex-col items-center gap-3 rounded-lg bg-red-50 p-6 dark:bg-red-900/20">
          <div className="flex items-center gap-2 text-red-600 dark:text-red-400">
            <AlertCircle size={20} />
            <span className="font-medium">加载失败</span>
          </div>
          <p className="text-sm text-red-500 dark:text-red-400">{error}</p>
          {selectedPackageId != null && (
            <Button variant="outline" size="sm" onClick={handleReload}>
              <RefreshCw size={14} />
              重试
            </Button>
          )}
        </div>
      )}

      {/* 未选包提示 */}
      {!loading && !error && !initialized && selectedPackageId == null && !pkgLoading && packages.length > 0 && (
        <div className="rounded-md bg-gray-50 px-3 py-8 text-center text-sm text-gray-500 dark:bg-gray-800/50 dark:text-gray-400">
          请选择一个音频包开始播放
        </div>
      )}

      {/* 已就绪 */}
      {!loading && !error && initialized && selectedPackage && (
        <div className="flex items-center gap-2 rounded-md bg-green-50 px-3 py-2 text-sm text-green-700 dark:bg-green-900/20 dark:text-green-400">
          <Music size={16} />
          <span>
            「{selectedPackage.title}」已就绪 — 共 {sentences.length} 个句子
          </span>
        </div>
      )}

      {/* 播放器 UI（单例已初始化后渲染） */}
      {initialized && (
        <>
          <AudioPlayer showAnnotation={true} showReport={true} showOptions={true} />
          <ShadowPanel />
        </>
      )}
    </div>
  )
}
