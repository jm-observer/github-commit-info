/**
 * playerSession — 记录当前 AudioPlayerService 单例归哪个入口所有。
 *
 * AudioPlayerService 是真单例（内部 HtmlAudioAdapter 持 `new Audio()`，脱离 React DOM）。
 * 标注(annotated) / 全部(all) / 听包(pkg:<id>) 三个入口都会接管它。各入口切菜单回来时会走
 * 「fast-path」直接挂回 UI 而不重建单例——但如果中途别的入口已经接管了单例，fast-path 就会
 * 误用不属于自己的单例（UI 显示 A 包、实际在放 B 标注）。owner 标记让各入口在 fast-path 前
 * 先确认单例仍归自己，否则回退到完整重建。
 *
 * owner 取值：`'annotated'` / `'all'` / `pkg:<packageId>`。
 */
export const playerSession = { owner: null as string | null }
