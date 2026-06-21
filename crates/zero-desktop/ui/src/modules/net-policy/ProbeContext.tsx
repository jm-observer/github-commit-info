import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import {
  NetPolicyAPI,
  type Status,
  type VerifyReport,
  type ConnectionsSnapshot,
} from './api/tauri-client'

/**
 * 网络策略「本机现状」**全局** probe 上下文。
 *
 * 设计动机：原本 `CurrentStateSection` 在挂载时（也即切到「网络策略」菜单时）才跑 verify 探测
 * （api.ipify.org + DNS + 控制器，~1s 的开销），每次切页都要等。把 probe 提到 App 根，所有页面共享：
 *  - **App 启动即并行跑一次** verify + status + conns（fire-and-forget），用户切过来时数据已就位。
 *  - status + conns 在 provider 里**持续 3s 后台轮询**，跨页面持续（之前在 NetPolicyPage 里轮询，
 *    离开页面就停，再回去要重新拉）。
 *  - verify 只在挂载 + 显式 `runVerify()` 时跑（避免每分钟戳一次 api.ipify）。
 *
 * 失败语义：每个 probe 失败不抛错、不改变已有缓存值（避免页面闪烁回空）。
 */

const EMPTY_CONNS: ConnectionsSnapshot = {
  available: false,
  total: 0,
  wg_count: 0,
  direct_count: 0,
  other_count: 0,
  by_process: {},
  connections: [],
}

interface ProbeContextValue {
  status: Status | null
  conns: ConnectionsSnapshot
  verify: VerifyReport | null
  exitIp: string | null
  exitIpAt: string | null
  verifyUpdatedAt: string | null
  /** verify 正在跑（用于按钮 loading）。 */
  probing: boolean
  /** 重拉 status + conns（便宜）。 */
  refreshFast: () => void
  /** 重跑 verify（贵 ~1s，HTTP）；返回最新报告供调用方继续处理。 */
  runVerify: () => Promise<VerifyReport | null>
  /** VerifyMatrix 的「一键自检」复用：手动塞一份 verify 进来。 */
  setVerify: (r: VerifyReport) => void
  /** 手动同步出口 IP（一般不直接用，runVerify 内已自动）。 */
  setExitIp: (ip: string, at: string) => void
}

const NetPolicyProbeContext = createContext<ProbeContextValue | null>(null)

export function NetPolicyProbeProvider({ children }: { children: React.ReactNode }) {
  const [status, setStatus] = useState<Status | null>(null)
  const [conns, setConns] = useState<ConnectionsSnapshot>(EMPTY_CONNS)
  const [verify, setVerifyState] = useState<VerifyReport | null>(null)
  const [exitIp, setExitIpState] = useState<string | null>(null)
  const [exitIpAt, setExitIpAt] = useState<string | null>(null)
  const [verifyUpdatedAt, setVerifyUpdatedAt] = useState<string | null>(null)
  const [probing, setProbing] = useState(false)

  // status 单飞合并：3s 轮询和手动 refresh 撞一起时只发一发请求。
  const statusInFlightRef = useRef<Promise<void> | null>(null)
  // verify 单飞合并：避免 App 挂载时和 NetPolicyPage 挂载时各跑一次。
  const verifyInFlightRef = useRef<Promise<VerifyReport | null> | null>(null)

  const loadStatus = useCallback(() => {
    if (statusInFlightRef.current) return statusInFlightRef.current
    const req = NetPolicyAPI.getStatus()
      .then((s) => setStatus(s))
      .catch(() => {})
      .finally(() => {
        statusInFlightRef.current = null
      })
    statusInFlightRef.current = req
    return req
  }, [])

  const loadConns = useCallback(() => {
    return NetPolicyAPI.getConnections()
      .then(setConns)
      .catch(() => {})
  }, [])

  const refreshFast = useCallback(() => {
    void loadStatus()
    void loadConns()
  }, [loadStatus, loadConns])

  const handleVerifyResult = useCallback((rep: VerifyReport) => {
    setVerifyState(rep)
    setVerifyUpdatedAt(new Date().toLocaleTimeString())
    const ip = rep.cases.find((c) => c.id === 'exit-ip')
    if (ip && ip.status === 'passed') {
      setExitIpState(ip.observed)
      setExitIpAt(new Date().toLocaleTimeString())
    }
  }, [])

  const runVerify = useCallback(async () => {
    if (verifyInFlightRef.current) return verifyInFlightRef.current
    setProbing(true)
    const req = NetPolicyAPI.verify()
      .then((rep) => {
        handleVerifyResult(rep)
        return rep
      })
      .catch((e) => {
        console.warn('[net-policy] verify failed', e)
        return null
      })
      .finally(() => {
        setProbing(false)
        verifyInFlightRef.current = null
      })
    verifyInFlightRef.current = req
    return req
  }, [handleVerifyResult])

  const setVerify = useCallback(
    (rep: VerifyReport) => {
      handleVerifyResult(rep)
    },
    [handleVerifyResult],
  )

  const setExitIp = useCallback((ip: string, at: string) => {
    setExitIpState(ip)
    setExitIpAt(at)
  }, [])

  // App 挂载即跑：status + conns + verify 并行；后续 3s 持续轮询 status/conns。
  useEffect(() => {
    refreshFast()
    void runVerify()
    const id = window.setInterval(refreshFast, 3000)
    return () => window.clearInterval(id)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const value: ProbeContextValue = {
    status,
    conns,
    verify,
    exitIp,
    exitIpAt,
    verifyUpdatedAt,
    probing,
    refreshFast,
    runVerify,
    setVerify,
    setExitIp,
  }

  return <NetPolicyProbeContext.Provider value={value}>{children}</NetPolicyProbeContext.Provider>
}

export function useNetPolicyProbe(): ProbeContextValue {
  const v = useContext(NetPolicyProbeContext)
  if (!v) throw new Error('useNetPolicyProbe 必须在 NetPolicyProbeProvider 下使用')
  return v
}
