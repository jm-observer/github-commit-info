/**
 * AnnotationPlayer — 标注/全量句子播放器（Tailwind 重写，无 AntD）。
 */

import { useState, useEffect, useRef } from 'react'
import { Loader2, AlertCircle, RefreshCw, Music } from 'lucide-react'
import AudioPlayer from './AudioPlayer'
import { AudioPlayerService } from '../services/AudioPlayerService'
import ApiService from '../services/ApiService'
import FileCacheManager from '../services/FileCacheManager'
import EnvConfigService from '../services/EnvConfigService'
import HtmlAudioAdapter from '../adapters/HtmlAudioAdapter'
import type { Sentence } from '../types'
import { Button } from '../../speech/components/ui/Button'
import { readAutoStartPref } from '../autoStartPref'

interface AnnotationPlayerProps {
  autoStart?: boolean
  dataSource?: 'annotated' | 'all'
}

// 跨路由切换 / Strict Mode 挂载的全局会话状态。
// AudioPlayerService 是真单例(其内 HtmlAudioAdapter 持有 `new Audio()`,脱离 React DOM,
// 不随 mount 销毁),所以切菜单回来时不应再 reset/重建——只要 dataSource 没变就直接复用,
// 列表 fetch / 音频缓存 / 自动播放都跳过,播放从离开时的位置继续。
const componentInstanceState = {
  initializedDataSource: null as string | null,
  isInitializing: false,
  sentences: [] as Sentence[],
}

export default function AnnotationPlayer({ autoStart = true, dataSource = 'annotated' }: AnnotationPlayerProps) {
  const [loading, setLoading] = useState(false)
  const [loadingText, setLoadingText] = useState(dataSource === 'annotated' ? '正在加载标注句子...' : '正在加载所有句子...')
  const [error, setError] = useState<string | null>(null)
  const [sentences, setSentences] = useState<Sentence[]>([])
  const [initialized, setInitialized] = useState(false)
  const initStartedRef = useRef(false)
  const autoPlayStartedRef = useRef(false)
  const backgroundDownloadCancelRef = useRef(false)

  useEffect(() => {
    // 同 dataSource 已经初始化过 → 切菜单回来,直接挂回单例 UI,不停播放、不重拉列表。
    if (
      componentInstanceState.initializedDataSource === dataSource &&
      componentInstanceState.sentences.length > 0
    ) {
      setSentences(componentInstanceState.sentences)
      setInitialized(true)
      setLoading(false)
      setError(null)
      return  // 注意:不返回 cleanup,unmount 时不停播放。
    }

    // 任意 dataSource 正在初始化中 → 直接等(防 Strict Mode 双 mount 触发两次 init,
    // 否则会建出两个 HtmlAudioAdapter 同时播 → "两个声音" 的根因)。这里**不**要求
    // initializedDataSource === dataSource,因为首次 mount 时 init 还没完成,
    // initializedDataSource 还是 null,加 dataSource 比较守卫就失效了。
    if (componentInstanceState.isInitializing) {
      return
    }

    // dataSource 真的变了(切 annotated <-> all)或首次 → 停旧播放,做全套初始化。
    // 显式 stopAudio() 兜底:即便上一次 init 因故漏调 stop,这里把残留 adapter 的音也停掉。
    try { AudioPlayerService.getInstance().stopAudio() } catch { /* not yet initialized */ }

    componentInstanceState.isInitializing = true
    backgroundDownloadCancelRef.current = false
    initStartedRef.current = false
    autoPlayStartedRef.current = false
    setInitialized(false)
    setSentences([])
    setError(null)

    void init().finally(() => { componentInstanceState.isInitializing = false })

    // unmount cleanup:只取消后台下载,**不**停播放——单例继续在后台跑,切菜单回来无缝接续。
    return () => {
      backgroundDownloadCancelRef.current = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataSource])

  const init = async () => {
    if (initStartedRef.current) return
    initStartedRef.current = true

    try {
      setLoading(true)
      setLoadingText(dataSource === 'annotated' ? '正在获取标注句子列表...' : '正在获取所有句子列表...')
      setError(null)

      const sentencesList = await loadSentences()
      await cacheAudioFiles(sentencesList)

      setLoadingText('正在初始化播放器...')
      await new Promise(resolve => setTimeout(resolve, 100))
      await initAudioPlayer(sentencesList)

      setLoading(false)
      setInitialized(true)
      // 标记初始化完成 + 缓存列表,下次同 dataSource mount 走 fast-path 直接复用。
      componentInstanceState.initializedDataSource = dataSource
      componentInstanceState.sentences = sentencesList
    } catch (err: any) {
      console.error('播放器初始化失败:', err)
      setLoading(false)
      const detail =
        err?.message ||
        (typeof err === 'string' ? err : null) ||
        (() => { try { return JSON.stringify(err) } catch { return String(err) } })()
      setError(detail || '初始化失败（无详情）')
    }
  }

  const loadSentences = async (): Promise<Sentence[]> => {
    setLoadingText(dataSource === 'annotated' ? '正在获取标注句子列表...' : '正在获取所有句子列表...')
    const apiService = ApiService.getInstance()
    const response = dataSource === 'annotated'
      ? await apiService.getAnnotatedSentences()
      : await apiService.getAllSentences()

    const list: Sentence[] = response.data || []
    setSentences(list)

    if (list.length === 0) {
      throw new Error(dataSource === 'annotated' ? '没有标注句子' : '没有句子')
    }
    return list
  }

  const cacheAudioFiles = async (sentencesList: Sentence[]) => {
    if (!sentencesList.length) return

    // EnvConfig 用于缓存 manager（apiBaseUrl 备用）
    const envConfig = await EnvConfigService.getInstance().getConfig()
    const cacheManager = FileCacheManager.getInstance()

    const isAllMode = dataSource === 'all'
    const primary = isAllMode ? sentencesList.slice(0, 50) : sentencesList
    const remaining = isAllMode ? sentencesList.slice(50) : []

    let cachedCount = 0
    const totalCount = primary.reduce((acc, s) => acc + s.audios.length, 0)

    for (const sentence of primary) {
      for (const audio of sentence.audios) {
        setLoadingText(`正在缓存音频... (${cachedCount + 1}/${totalCount})`)
        try {
          await cacheManager.downloadAndCache(audio.id, envConfig.apiBaseUrl)
          cachedCount++
        } catch (err) {
          console.error(`缓存音频失败 (ID: ${audio.id}):`, err)
        }
      }
    }

    if (remaining.length > 0) {
      backgroundDownloadCancelRef.current = false
      cacheRemainingAudioFiles(remaining, cacheManager, envConfig.apiBaseUrl).catch(err => {
        if (!backgroundDownloadCancelRef.current) console.error('后台下载出错:', err)
      })
    }
  }

  const cacheRemainingAudioFiles = async (
    remainingSentences: Sentence[],
    cacheManager: FileCacheManager,
    apiBaseUrl: string
  ) => {
    if (backgroundDownloadCancelRef.current) return
    let count = 0
    for (const sentence of remainingSentences) {
      if (backgroundDownloadCancelRef.current) return
      for (const audio of sentence.audios) {
        if (backgroundDownloadCancelRef.current) return
        try {
          await cacheManager.downloadAndCache(audio.id, apiBaseUrl)
          count++
          if (count % 10 === 0) console.log(`后台下载进度: ${count}`)
        } catch (err) {
          if (backgroundDownloadCancelRef.current) return
          console.error(`后台缓存失败 (ID: ${audio.id}):`, err)
        }
      }
    }
  }

  const initAudioPlayer = async (sentencesList: Sentence[]) => {
    if (!sentencesList.length) throw new Error('没有可播放的句子')

    const envConfig = await EnvConfigService.getInstance().getConfig()
    const cacheManager = FileCacheManager.getInstance()
    const audioAdapter = new HtmlAudioAdapter()

    // 创建新单例前先把残留单例的 audio 显式停掉。HtmlAudioAdapter 的 <audio> 是
    // `new Audio()` 脱离 DOM 的元素,resetInstance 只断 service 引用,旧 <audio> 还在
    // 内存里继续响——必须主动 stop。
    try { AudioPlayerService.getInstance().stopAudio() } catch { /* no prior instance */ }
    AudioPlayerService.resetInstance()
    const audioService = AudioPlayerService.getInstance(audioAdapter, cacheManager, envConfig)

    audioService.setSentences(sentencesList)
    audioService.setMaxPlayCount(4)
    audioService.resetPlayer()

    // prop 上的 autoStart 是默认值;用户偏好(localStorage)进一步覆盖,关掉就不自动播。
    if (autoStart && readAutoStartPref() && !autoPlayStartedRef.current) {
      autoPlayStartedRef.current = true
      setTimeout(() => {
        try { void audioService.playCurrentAudio() }
        catch (err) { console.error('自动播放失败:', err); autoPlayStartedRef.current = false }
      }, 500)
    }
  }

  const handleReload = () => {
    // 主动重试 = 用户明确要求重建,清掉 fast-path 缓存 + 停旧播放。
    componentInstanceState.initializedDataSource = null
    componentInstanceState.sentences = []
    try { AudioPlayerService.getInstance().stopAudio() } catch { /* ignore */ }
    setInitialized(false)
    setSentences([])
    setError(null)
    initStartedRef.current = false
    autoPlayStartedRef.current = false
    void init()
  }

  return (
    <div className="flex flex-col gap-4">
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
          <Button variant="outline" size="sm" onClick={handleReload}>
            <RefreshCw size={14} />
            重试
          </Button>
        </div>
      )}

      {/* 已初始化 */}
      {!loading && !error && initialized && (
        <div className="flex items-center gap-2 rounded-md bg-green-50 px-3 py-2 text-sm text-green-700 dark:bg-green-900/20 dark:text-green-400">
          <Music size={16} />
          <span>
            {dataSource === 'annotated' ? '标注播放' : '音频播放'}已就绪 —
            共 {sentences.length} {dataSource === 'annotated' ? '个标注句子' : '个句子'}
          </span>
        </div>
      )}

      {/* 播放器 UI（只在 AudioPlayerService 已初始化后渲染） */}
      {initialized && (
        <AudioPlayer showAnnotation={true} showReport={true} showOptions={true} />
      )}
    </div>
  )
}
