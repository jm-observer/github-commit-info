import { Plus, Trash2 } from 'lucide-react'
import type { ConnectionsSnapshot, Route, Rule, RuleKind, RuleSet } from '../api/tauri-client'
import { btn, KIND_LABELS } from '../uiHelpers'

/** 规则页的新增表单与用户规则列表。 */
export function RuleList({
  rules,
  newRule,
  setNewRule,
  addRule,
  deleteRule,
  busy,
  curRoute,
  conns,
}: {
  rules: RuleSet
  newRule: Rule
  setNewRule: (r: Rule) => void
  addRule: () => void
  deleteRule: (rule: Rule) => void
  busy: boolean
  curRoute: Route
  conns: ConnectionsSnapshot
}) {
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-gray-500">
        <span>
          命中规则走指定出口；未命中默认{curRoute === 'wg' ? '走海外' : curRoute === 'blackhole' ? '阻断' : '直连'}。
        </span>
        <span className="text-xs text-gray-500">
          活跃：直连 {conns.direct_count} · 海外 {conns.wg_count}
        </span>
      </div>
      <div className="flex flex-wrap items-end gap-2 rounded-lg bg-gray-100 p-3 text-sm dark:bg-gray-800/50">
        <select className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={newRule.kind}
          onChange={e => setNewRule({ ...newRule, kind: e.target.value as RuleKind })}>
          {Object.entries(KIND_LABELS).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
        </select>
        <input className="flex-1 rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" placeholder="值（如 steam.exe / ctrip / example.cn / 1.2.3.0/24）"
          value={newRule.value} onChange={e => setNewRule({ ...newRule, value: e.target.value })}
          onKeyDown={e => e.key === 'Enter' && addRule()} />
        <select className="rounded border px-2 py-1 dark:bg-gray-800 dark:border-gray-700" value={newRule.route}
          onChange={e => setNewRule({ ...newRule, route: e.target.value as Route })}>
          <option value="direct">本地直连</option>
          <option value="wg">走 VPN</option>
          <option value="blackhole">阻断</option>
        </select>
        <button className={btn('primary')} onClick={addRule} disabled={busy}><Plus size={14} /> 添加</button>
      </div>
      <ul className="divide-y divide-gray-200 rounded-lg border border-gray-200 bg-white px-3 text-sm dark:divide-gray-800 dark:border-gray-800 dark:bg-gray-900">
        {rules.rules.length === 0 && (
          <li className="py-2 text-gray-500">
            暂无规则——未命中流量全部{curRoute === 'wg' ? '走海外 VPN' : curRoute === 'blackhole' ? '被阻断（空出口）' : '原样直连（观察）'}。
          </li>
        )}
        {rules.rules.map((r, i) => (
          <li key={i} className="flex items-center gap-2 py-1.5">
            <span className="w-20 text-gray-500">{KIND_LABELS[r.kind]}</span>
            <span className="flex-1 font-mono text-xs">{r.value}</span>
            <span className={`rounded px-1.5 py-0.5 text-xs ${
              r.route === 'direct' ? 'bg-gray-100 text-gray-700' :
              r.route === 'wg' ? 'bg-blue-100 text-blue-800' :
              'bg-red-100 text-red-700'
            }`}>
              {r.route === 'direct' ? '直连' : r.route === 'wg' ? 'VPN' : '阻断'}
            </span>
            <button className="text-gray-400 hover:text-red-600" onClick={() => deleteRule(r)} disabled={busy} title="删除">
              <Trash2 size={14} />
            </button>
          </li>
        ))}
      </ul>
      <p className="mt-2 px-1 text-[11px] text-gray-500 dark:text-gray-400">
        提示：把某个程序设为直连，选「程序名」填可执行名（如 steam.exe）+「本地直连」；
        多个同品牌域名可选「域名关键词」填关键词（如 ctrip）收成一条规则；
        近期有公网连接的进程可在「诊断 → 扫描进程」里查看。
      </p>
    </div>
  )
}
