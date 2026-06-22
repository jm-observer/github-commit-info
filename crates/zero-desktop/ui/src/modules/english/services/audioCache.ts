/**
 * audioCache — 英语听力音频缓存策略（标注/全部/听包共用）。
 *
 * 约定：LAN = `http://…:8788`，WAN = `https://…:28080`（见 settings.rs 的派生规则）。
 * - LAN：音频就在同机/局域网，缓存收益极小，且开播前阻塞预缓存只会拖慢起播 → 不缓存，
 *   直接流式播放（AudioPlayerService 命中不到缓存会自动走网络 URL）。
 * - WAN：远程访问，缓存省带宽/延迟 → 后台缓存，但不阻塞起播。
 */

import type { Sentence } from '../types'
import FileCacheManager from './FileCacheManager'

/** 是否需要缓存音频字节。LAN(http) 不缓存、WAN(https) 缓存。 */
export function shouldCacheAudio(apiBaseUrl: string): boolean {
  return apiBaseUrl.startsWith('https')
}

/** 后台静默缓存整批音频，不阻塞起播；cancelRef.current 置 true 即停。单条失败逐条跳过。 */
export async function backgroundCacheAudios(
  sentences: Sentence[],
  apiBaseUrl: string,
  cancelRef: { current: boolean }
): Promise<void> {
  const cacheManager = FileCacheManager.getInstance()
  for (const sentence of sentences) {
    if (cancelRef.current) return
    for (const audio of sentence.audios) {
      if (cancelRef.current) return
      try {
        await cacheManager.downloadAndCache(audio.id, apiBaseUrl)
      } catch (err) {
        if (cancelRef.current) return
        console.error(`后台缓存失败 (ID: ${audio.id}):`, err)
      }
    }
  }
}
