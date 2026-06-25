/**
 * 记住用户最后停留的英语听力子标签（标注/全部/听包），存 localStorage。
 *
 * 侧栏「英语听力」据此跳回上次的子标签,而不是写死回到「标注」;切到别的功能再点回来
 * 不会丢失当前子标签。EnglishTabs 在每次子标签变化时写入。
 */

const KEY = 'english_last_route'
const VALID = ['/english/annotated', '/english/all', '/english/packages']
const DEFAULT_ROUTE = '/english/annotated'

export function readLastEnglishRoute(): string {
  if (typeof window === 'undefined') return DEFAULT_ROUTE
  const v = window.localStorage.getItem(KEY)
  return v && VALID.includes(v) ? v : DEFAULT_ROUTE
}

export function writeLastEnglishRoute(path: string): void {
  if (typeof window === 'undefined') return
  if (VALID.includes(path)) window.localStorage.setItem(KEY, path)
}
