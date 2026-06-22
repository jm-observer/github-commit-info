/**
 * /english/annotated — 标注句子播放页。
 * 由 EnglishBootstrap 包裹，确保 g10_base + customerId 已加载。
 */

import EnglishBootstrap from './EnglishBootstrap'
import EnglishTabs from './components/EnglishTabs'
import AnnotationPlayer from './components/AnnotationPlayer'

export default function EnglishAnnotated() {
  return (
    <EnglishBootstrap>
      <EnglishTabs />
      <AnnotationPlayer autoStart={true} dataSource="annotated" />
    </EnglishBootstrap>
  )
}
