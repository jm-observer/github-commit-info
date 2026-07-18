import { useCallback, useEffect, useState } from 'react'
import { Check, Gauge, RefreshCw, Save } from 'lucide-react'
import { NetPolicyAPI, type ProxyNode, type ProxySubscription, type ProxySubscriptions } from '../api/tauri-client'
import { useNetPolicyController } from '../NetPolicyController'
import { useNetPolicyProbe } from '../ProbeContext'
import { btn, Section } from '../uiHelpers'

export function ProxySubscriptionsPage() {
  const { settings, setSettings, busy, saveSettings } = useNetPolicyController()
  const { status } = useNetPolicyProbe()
  const [nodes, setNodes] = useState<ProxyNode[]>([])
  const [nodesError, setNodesError] = useState<string | null>(null)
  const [nodesLoading, setNodesLoading] = useState(false)
  const [testing, setTesting] = useState<string | null>(null)
  const [selectedNode, setSelectedNode] = useState<string | null>(null)
  const [subscriptionBusy, setSubscriptionBusy] = useState(false)
  const [saveState, setSaveState] = useState<'idle' | 'saving' | 'saved' | 'failed'>('idle')

  const refreshNodes = useCallback(async () => {
    if (!status?.mihomo_running) {
      setNodes([])
      setNodesError('引擎未运行；保存设置并在概览中启动策略后可读取节点。')
      return
    }
    setNodesLoading(true)
    setNodesError(null)
    try {
      setNodes(await NetPolicyAPI.getProxyNodes())
    } catch (error) {
      setNodesError(String(error).replace(/^Error:\s*/i, ''))
    } finally {
      setNodesLoading(false)
    }
  }, [status?.mihomo_running])

  useEffect(() => {
    void refreshNodes()
  }, [refreshNodes])

  const refreshSubscription = async () => {
    setSubscriptionBusy(true)
    setNodesError(null)
    try {
      await NetPolicyAPI.egressRefreshSubscription('proxy')
      await refreshNodes()
    } catch (error) {
      setNodesError(`刷新订阅失败：${String(error).replace(/^Error:\s*/i, '')}`)
    } finally {
      setSubscriptionBusy(false)
    }
  }

  const selectNode = async (name: string) => {
    setSelectedNode(name)
    setNodesError(null)
    try {
      await NetPolicyAPI.egressSelectNode('proxy', name)
    } catch (error) {
      setNodesError(`切换节点失败：${String(error).replace(/^Error:\s*/i, '')}`)
    } finally {
      setSelectedNode(null)
    }
  }

  if (!settings) {
    return <p className="py-8 text-center text-sm text-gray-500">正在读取代理订阅配置…</p>
  }

  const subscriptions: ProxySubscriptions = settings.proxy_subscriptions ?? { first: null, second: null, active: null }
  const listeners = settings.local_proxy ?? { socks_port: 7891, http_port: 7890 }
  const setSubscriptions = (patch: Partial<ProxySubscriptions>) => {
    setSettings({ ...settings, proxy_subscriptions: { ...subscriptions, ...patch } })
  }
  const updateSubscription = (slot: 0 | 1, patch: Partial<ProxySubscription>) => {
    const key = slot === 0 ? 'first' : 'second'
    const current = subscriptions[key]
    setSubscriptions({
      [key]: {
        name: current?.name ?? `订阅 ${slot + 1}`,
        url: current?.url ?? '',
        interval_secs: current?.interval_secs ?? 3600,
        ...patch,
      },
    })
  }

  const save = async () => {
    setSaveState('saving')
    const ok = await saveSettings()
    setSaveState(ok ? 'saved' : 'failed')
    if (ok) void refreshNodes()
  }

  const testNode = async (name: string) => {
    setTesting(name)
    try {
      const tested = await NetPolicyAPI.testProxyNode(name)
      setNodes((current) => current.map((node) => node.name === name ? tested : node))
    } catch (error) {
      setNodesError(`测速 ${name} 失败：${String(error).replace(/^Error:\s*/i, '')}`)
    } finally {
      setTesting(null)
    }
  }

  const testAll = async () => {
    for (const node of nodes.slice(0, 50)) {
      await testNode(node.name)
    }
  }

  return (
    <div className="space-y-7">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className={`text-xs ${saveState === 'failed' ? 'text-red-600' : saveState === 'saved' ? 'text-emerald-600' : 'text-gray-500'}`}>
          {saveState === 'saving'
            ? '正在保存并同步运行配置…'
            : saveState === 'saved'
              ? '已保存；运行配置已同步。'
              : saveState === 'failed'
                ? '保存失败；配置未确认生效。'
                : `当前状态：${status?.mihomo_running ? '引擎运行中，保存后自动热加载' : '引擎未运行，保存后下次应用生效'}`}
        </div>
        <button className={btn('primary')} disabled={busy || saveState === 'saving'} onClick={() => void save()}>
          <Save size={14} /> 保存代理设置
        </button>
      </div>

      <details open className="rounded-lg border border-gray-200 p-4 dark:border-gray-800">
        <summary className="cursor-pointer text-sm font-medium">本地代理端口</summary>
        <p className="mt-1 text-xs text-gray-500">仅监听本机；供浏览器、命令行和其他应用显式连接。</p>
        <div className="mt-4">
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <label className="flex flex-col gap-1 text-sm">
            SOCKS5 端口
            <input
              type="number"
              min={1}
              max={65535}
              className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
              value={listeners.socks_port}
              onChange={(event) => setSettings({
                ...settings,
                local_proxy: { ...listeners, socks_port: Number(event.target.value) },
              })}
            />
            <span className="text-xs text-gray-500">连接地址：127.0.0.1:{listeners.socks_port}，支持 TCP/UDP。</span>
          </label>
          <label className="flex flex-col gap-1 text-sm">
            HTTP/HTTPS CONNECT 端口
            <input
              type="number"
              min={1}
              max={65535}
              className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
              value={listeners.http_port}
              onChange={(event) => setSettings({
                ...settings,
                local_proxy: { ...listeners, http_port: Number(event.target.value) },
              })}
            />
            <span className="text-xs text-gray-500">
              连接地址：127.0.0.1:{listeners.http_port}；HTTPS 请求通过 HTTP CONNECT 隧道转发。
            </span>
          </label>
        </div>
        </div>
      </details>

      <details open className="rounded-lg border border-gray-200 p-4 dark:border-gray-800">
        <summary className="cursor-pointer text-sm font-medium">代理订阅</summary>
        <p className="mt-1 text-xs text-gray-500">支持两条 mihomo/Clash provider 订阅；激活项可作为独立默认出口。</p>
        <div className="mt-4">
        {[0, 1].map((slot) => {
          const typedSlot = slot as 0 | 1
          const key = typedSlot === 0 ? 'first' : 'second'
          const subscription = subscriptions[key]
          return (
            <div key={slot} className="rounded-lg border border-gray-200 p-4 dark:border-gray-700">
              <div className="mb-3 flex items-center justify-between gap-3">
                <div className="text-sm font-medium">订阅 {slot + 1}</div>
                <button
                  type="button"
                  className={btn()}
                  onClick={() => setSubscriptions({
                    [key]: null,
                    active: subscriptions.active === slot ? null : subscriptions.active,
                  })}
                >
                  清空
                </button>
              </div>
              <div className="grid grid-cols-1 gap-3 text-sm sm:grid-cols-3">
                <label className="flex flex-col gap-1">
                  名称
                  <input
                    className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
                    value={subscription?.name ?? ''}
                    onChange={(event) => updateSubscription(typedSlot, { name: event.target.value })}
                    placeholder={`订阅 ${slot + 1}`}
                  />
                </label>
                <label className="flex flex-col gap-1 sm:col-span-2">
                  订阅 URL（HTTP/HTTPS 下载地址，不是代理端口）
                  <input
                    className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
                    value={subscription?.url ?? ''}
                    onChange={(event) => updateSubscription(typedSlot, { url: event.target.value })}
                    placeholder="https://example.com/subscription"
                  />
                </label>
                <label className="flex flex-col gap-1">
                  更新间隔（秒）
                  <input
                    type="number"
                    min={60}
                    max={86400}
                    className="rounded border px-2 py-1 dark:border-gray-700 dark:bg-gray-800"
                    value={subscription?.interval_secs ?? 3600}
                    onChange={(event) => updateSubscription(typedSlot, { interval_secs: Number(event.target.value) })}
                  />
                </label>
              </div>
              <label className="mt-3 flex items-center gap-2 text-sm">
                <input
                  type="radio"
                  name="active-proxy-subscription"
                  checked={subscriptions.active === slot}
                  disabled={!subscription?.url}
                  onChange={() => setSubscriptions({ active: typedSlot })}
                />
                设为当前激活订阅
              </label>
            </div>
          )
        })}
        <p className="text-xs text-gray-500">
          激活后可在概览中选择“代理订阅”作为默认出口；WireGuard 上游代理也可以复用该节点组。
        </p>
        </div>
      </details>

      <Section
        title="订阅节点与测速"
        description={nodes.length ? `已加载 ${nodes.length} 个节点` : '保存并应用激活订阅后读取'}
        right={(
          <div className="flex gap-2">
            <button className={btn()} disabled={nodesLoading || testing !== null || subscriptionBusy} onClick={() => void refreshSubscription()}>
              <RefreshCw size={14} className={subscriptionBusy ? 'animate-spin' : ''} /> 刷新订阅
            </button>
            <button className={btn()} disabled={nodesLoading || testing !== null || subscriptionBusy} onClick={() => void refreshNodes()}>
              <RefreshCw size={14} className={nodesLoading ? 'animate-spin' : ''} /> 刷新节点
            </button>
            <button className={btn()} disabled={nodes.length === 0 || testing !== null} onClick={() => void testAll()}>
              <Gauge size={14} /> 全部测速
            </button>
          </div>
        )}
      >
        {nodesError && <p className="text-xs text-amber-700 dark:text-amber-300">{nodesError}</p>}
        {!nodesError && nodes.length === 0 && <p className="text-sm text-gray-500">暂无节点数据。</p>}
        {nodes.length > 0 && (
          <div className="max-h-80 overflow-y-auto rounded border border-gray-200 dark:border-gray-800">
            {nodes.map((node) => (
              <div key={node.name} className="flex items-center gap-3 border-b border-gray-100 px-3 py-2 text-sm last:border-b-0 dark:border-gray-800">
                <span className={`h-2 w-2 rounded-full ${node.alive ? 'bg-emerald-500' : 'bg-gray-400'}`} title={node.alive ? '可用' : '未验证'} />
                <span className="min-w-0 flex-1 truncate" title={node.name}>{node.name}</span>
                <span className="text-xs text-gray-500">{node.type || 'Proxy'}</span>
                <span className="w-16 text-right font-mono text-xs text-gray-600 dark:text-gray-300">
                  {testing === node.name ? '测速中…' : node.delay_ms != null ? `${node.delay_ms} ms` : '未测速'}
                </span>
                <button className={btn()} disabled={testing !== null || selectedNode !== null} onClick={() => void selectNode(node.name)}>
                  {selectedNode === node.name ? <RefreshCw size={13} className="animate-spin" /> : node.alive ? <Check size={13} /> : null}
                  {node.alive ? '使用' : '选择'}
                </button>
                <button className={btn()} disabled={testing !== null || selectedNode !== null} onClick={() => void testNode(node.name)}>测速</button>
              </div>
            ))}
          </div>
        )}
      </Section>
    </div>
  )
}
