/**
 * /english/packages — 「按包来听」播放页（迁自 mini-program 的按包听音频）。
 * 由 EnglishBootstrap 包裹，确保 g10_base + customerId 已加载。
 */

import EnglishBootstrap from './EnglishBootstrap'
import EnglishTabs from './components/EnglishTabs'
import PackagePlayer from './components/PackagePlayer'

export default function EnglishPackages() {
  return (
    <EnglishBootstrap>
      <EnglishTabs />
      <PackagePlayer autoStart={true} />
    </EnglishBootstrap>
  )
}
