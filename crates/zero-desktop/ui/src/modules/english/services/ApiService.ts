/**
 * ApiService — 调 G10 API 的 HTTP 客户端。
 *
 * 改造点（相对原仓）：
 * - apiBase 从 EnvConfigService.getConfig() 取（getConfig 内部调 english_get_g10_base）。
 * - 若 g10_base 为空，getConfig() 抛错，ApiService 不 fallback 到任何 hardcode URL。
 * - 无 AntD 依赖。
 */

import { fetch } from '@tauri-apps/plugin-http'
import { invoke } from '@tauri-apps/api/core'
import type { ApiRequest, ApiResponse } from '../types'
import EnvConfigService from './EnvConfigService'
import FileCacheManager from './FileCacheManager'

class ApiService {
  private static instance: ApiService
  private constructor() {}

  static getInstance(): ApiService {
    if (!ApiService.instance) ApiService.instance = new ApiService()
    return ApiService.instance
  }

  async request<T = any>(method: string, params?: Record<string, any>): Promise<ApiResponse<T>> {
    const envConfig = await EnvConfigService.getInstance().getConfig()
    const apiBaseUrl = envConfig.apiBaseUrl

    const requestData: ApiRequest = { method, params: params || {} }

    try {
      const response = await fetch(`${apiBaseUrl}/api`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(requestData)
      })

      if (!response.ok) {
        throw new Error(`HTTP ${response.status}: ${response.statusText}`)
      }

      const result = await response.json() as ApiResponse<T>
      if (result.code !== 0) {
        throw new Error(result.msg || '请求失败')
      }
      return result
    } catch (error) {
      console.error('[ApiService] 请求失败:', error)
      throw error
    }
  }

  async getAnnotatedSentences() {
    const customerId = await EnvConfigService.getInstance().getCustomerId()
    if (!customerId) {
      throw new Error('customer_id 未配置，请在设置页配置 customer_id')
    }
    return this.request('sentences.annotated', { customer_id: customerId })
  }

  async getAllSentences() {
    // 带 customer_id 才能回显「全部」视图里的标注/报错/已读状态（否则报错感叹号
    // 只是会话内乐观更新，刷新/重启后丢失）。缺 customer_id 时退化为不带（全 false）。
    const customerId = await EnvConfigService.getInstance().getCustomerId()
    return this.request('sentences.all', customerId ? { customer_id: customerId } : {})
  }

  /** 取音频包列表（「按包来听」）。对应 mini-program 的 package.list。 */
  async getPackages() {
    return this.request<import('../types').Package[]>('package.list')
  }

  /** 取指定包内的句子（带 customer_id 以回显标注/报错状态）。对应 package.sentences。 */
  async getPackageSentences(packageId: number) {
    const customerId = await EnvConfigService.getInstance().getCustomerId()
    if (!customerId) throw new Error('customer_id 未配置，请在设置页配置 customer_id')
    return this.request('package.sentences', { customer_id: customerId, package_id: packageId })
  }

  async annotateSentence(sentenceId: number, annotationText: string) {
    const customerId = await EnvConfigService.getInstance().getCustomerId()
    if (!customerId) throw new Error('customer_id 未配置，请在设置页配置 customer_id')
    return this.request('sentence.annotate', {
      customer_id: customerId,
      sentence_id: sentenceId,
      annotation_text: annotationText
    })
  }

  async unannotateSentence(sentenceId: number) {
    const customerId = await EnvConfigService.getInstance().getCustomerId()
    if (!customerId) throw new Error('customer_id 未配置，请在设置页配置 customer_id')
    return this.request('sentence.unannotate', { customer_id: customerId, sentence_id: sentenceId })
  }

  async reportError(objectType: string, objectId: number, labelType = 'error') {
    const customerId = await EnvConfigService.getInstance().getCustomerId()
    if (!customerId) throw new Error('customer_id 未配置，请在设置页配置 customer_id')
    return this.request('sentence.reportError', {
      customer_id: customerId, object_type: objectType, object_id: objectId, label_type: labelType
    })
  }

  async unreportError(objectType: string, objectId: number, labelType = 'error') {
    const customerId = await EnvConfigService.getInstance().getCustomerId()
    if (!customerId) throw new Error('customer_id 未配置，请在设置页配置 customer_id')
    return this.request('sentence.unreportError', {
      customer_id: customerId, object_type: objectType, object_id: objectId, label_type: labelType
    })
  }

  async downloadAudio(audioId: number): Promise<ArrayBuffer> {
    const envConfig = await EnvConfigService.getInstance().getConfig()
    const response = await fetch(`${envConfig.apiBaseUrl}/audio/${audioId}`, { method: 'GET' })
    if (!response.ok) throw new Error(`HTTP ${response.status}: ${response.statusText}`)
    return response.arrayBuffer()
  }

  /** 查询音色库（经 Tauri 后端代理 → toolkit-server /voices）。回传上游原始 JSON。 */
  async getVoices(): Promise<any> {
    return invoke<any>('english_tts_voices')
  }

  /**
   * 生成 TTS 预览：后端调 TTS 把 WAV 落盘到缓存目录，返回绝对路径。
   * 不写任何业务数据，仅供试听。
   */
  async previewTts(text: string, voiceId: string, speed?: number): Promise<string> {
    return invoke<string>('english_tts_preview', { text, voiceId, speed })
  }

  /**
   * 确认替换：把上一步预览的 WAV 上传到 english 后端，原子地改句子文本 + 换当前音频。
   * previewPath 必须是 previewTts 返回的路径。
   */
  async replaceSentenceAudio(
    sentenceId: number,
    audioId: number,
    text: string,
    previewPath: string
  ): Promise<void> {
    await invoke('english_replace_sentence_audio', { sentenceId, audioId, text, previewPath })
  }

  async clearSentencesCache(): Promise<void> {
    try {
      const cacheManager = FileCacheManager.getInstance()
      await cacheManager.removeStringCache('api_cache_sentences_annotated')
      await cacheManager.removeStringCache('api_cache_sentences_all')
    } catch (error) {
      console.error('[ApiService] 清除缓存失败:', error)
    }
  }
}

export default ApiService
