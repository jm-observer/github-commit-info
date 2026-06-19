/**
 * 英语听力「启动 / 首次进入页面时是否自动播放」偏好。
 *
 * 写 localStorage,跨重启保留;默认 true(保留历史行为)。AnnotationPlayer 在每次
 * 真正跑 init() 时(包括软件启动后首次进英语页 / 切换 annotated <-> all)读取此值
 * 决定要不要 schedule 自动播。fast-path 复用旧单例时跳过 init,与此设置无关。
 */

const KEY = 'english_autostart_on_launch'

export function readAutoStartPref(): boolean {
  if (typeof window === 'undefined') return true
  const v = window.localStorage.getItem(KEY)
  if (v === null) return true // 缺省值
  return v === 'true'
}

export function writeAutoStartPref(v: boolean): void {
  if (typeof window === 'undefined') return
  window.localStorage.setItem(KEY, v ? 'true' : 'false')
}
