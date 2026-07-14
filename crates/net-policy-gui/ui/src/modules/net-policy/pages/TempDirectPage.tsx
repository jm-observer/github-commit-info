import { TempDirectControl } from '../components/TempDirectControl'

/** 临时直连页：限时应急直连控制。对应原「5b2. 临时直连」，自包含组件，无需外部状态。 */
export function TempDirectPage() {
  return (
    <div>
      <TempDirectControl />
    </div>
  )
}
