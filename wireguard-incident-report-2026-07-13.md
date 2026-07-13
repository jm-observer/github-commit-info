# WireGuard 连接故障排查报告

## 1. 报告概述

- 排查日期：2026-07-12 至 2026-07-13
- 客户端：Windows 11，WireGuard for Windows 1.1
- 服务端：Ubuntu Linux，公网地址 `38.209.122.38`
- WireGuard 服务端实际监听端口：`51227/udp`
- 受影响客户端隧道地址：`10.66.66.2/32`、`fd42:42:42::2/128`
- 同机网络组件：Clash Verge / Mihomo TUN

本次排查最终确认存在两个互相独立的问题：

1. 客户端与服务端的预共享密钥曾不一致，导致服务端返回握手响应后客户端判定响应无效。
2. 修复密钥后，WireGuard 可以正常运行，但使用数天后，旧 UDP 端口的回包在公网链路中被选择性丢弃。同时，Windows WireGuardNT 虚拟网卡出现过 Code 31 创建设备失败。

当前已完成驱动重装，并启用新的 `29987/udp` 入口。WireGuard 已恢复连接，且排查阶段仅路由 WireGuard 内网，不影响 Clash Verge。

## 2. 原始网络结构

服务端 WireGuard 并非直接监听 53 或 443，而是：

```text
公网客户端
  ├─ UDP 53 ───┐
  ├─ UDP 443 ──┼─ iptables REDIRECT ──> UDP 51227 ──> WireGuard wg0
  └─ UDP 51227 ┘
```

服务端具备以下配置：

- `wg0` 地址：`10.66.66.1/24`、`fd42:42:42::1/64`
- IPv4/IPv6 转发已开启
- IPv4 出口通过 `eth0` MASQUERADE
- 防火墙允许 WireGuard 入站和隧道转发
- 服务端公网出口正常

客户端同时运行 Clash Verge/Mihomo。Mihomo 创建 TUN 默认路由，因此 WireGuard 服务端地址必须通过 WLAN 物理网关直连，避免 WireGuard 的底层 UDP 连接再次进入代理隧道。

已配置持久直连路由：

```text
38.209.122.38/32 -> WLAN -> 192.168.0.1
```

## 3. 第一次故障：握手响应无效

### 3.1 表现

客户端日志持续出现：

```text
Invalid handshake response from 38.209.122.38:53
```

服务端可以收到客户端的 WireGuard 握手请求，也会向客户端返回标准长度的握手响应，但客户端不建立密钥对。

### 3.2 排查证据

- 客户端发出 WireGuard 握手请求。
- 服务端 peer endpoint 随客户端请求更新，证明服务端收到并识别了请求。
- 服务端抓包显示收到约 148 字节的握手请求，并发出约 92 字节的握手响应。
- 服务端运行状态显示该 peer 没有预共享密钥。
- 客户端配置中存在 `PresharedKey`。

### 3.3 根因

客户端与服务端的 `PresharedKey` 配置不一致。服务端可以处理初始请求，但客户端无法验证后续握手响应，因此将响应判定为无效。

### 3.4 处理

- 将客户端对应的预共享密钥补充到服务端运行配置。
- 同步写入 `/etc/wireguard/wg0.conf` 持久配置。
- 服务端配置修改前创建备份。

修复后结果：

- 客户端日志出现 `Receiving handshake response`。
- WireGuard 创建有效 keypair。
- `10.66.66.1` 可以正常连通。

## 4. Clash 与 WireGuard 路由冲突

### 4.1 原配置

客户端曾配置：

```ini
AllowedIPs = 0.0.0.0/1, 128.0.0.0/1
```

两个 `/1` 合起来覆盖整个 IPv4 地址空间，会让公网流量全部进入 WireGuard。排查期间如果 WireGuard 握手失败，可能影响 Clash Verge 及当前远程排查连接。

### 4.2 排查阶段调整

创建独立配置 `wg0-autostart.conf`，将路由收窄为：

```ini
AllowedIPs = 10.66.66.0/24, fd42:42:42::/64
```

同时移除 WireGuard 接口的 DNS 配置，使公网和 DNS 继续由 Clash Verge 负责。

该调整的目的仅是隔离故障和保证排查通道稳定，不代表最终必须采用分流模式。确认 WireGuard 稳定后，可再恢复全局 IPv4 路由。

## 5. 第二次故障：Windows WireGuardNT 虚拟网卡失败

### 5.1 时间线

- 2026-07-13 约 11:12：服务端记录到最后一次有效握手。
- 11:14 后：客户端开始持续重试握手。
- 11:18：隧道被关闭并尝试重新创建。
- 11:18 至 11:20：WireGuard 多次创建虚拟网卡失败。

### 5.2 客户端错误

日志包含：

```text
Timed out waiting for device query
Failed to setup adapter (problem code: 0x1F, ntstatus: 0xC00002F0)
Unable to create network adapter
The system cannot find the file specified
```

系统事件同时记录 WireGuard Tunnel 服务因“系统找不到指定的文件”而停止。

### 5.3 影响

- WireGuard 隧道服务被移除。
- WireGuard 虚拟网卡消失。
- WireGuard 的 `/1` 或内网路由消失。
- Mihomo 重新成为默认网络出口。

### 5.4 处理

执行了完整驱动重装：

1. 静默卸载 WireGuard 1.1。
2. 清理旧 WireGuard 驱动包和设备实例。
3. 从 WireGuard 官方地址重新下载 1.1 MSI。
4. 使用官方 SHA-256 校验值验证安装包。
5. 重新安装 WireGuard。
6. 重新创建 WireGuard 隧道服务。

安装包 SHA-256 验证通过：

```text
6daa5d37a9e2950dfb8c48b95ab8e562cb2bad1c785d020f38f97bea4c6a5566
```

重装后：

- WireGuardNT 1.1 虚拟网卡创建成功。
- Code 31 不再出现。
- WireGuard Tunnel 服务可以保持运行。

## 6. 第三部分：旧 UDP 端口回包丢失

驱动恢复后，分别测试了以下入口：

- `38.209.122.38:53/udp`
- `38.209.122.38:443/udp`
- `38.209.122.38:51227/udp`

三个旧端口的表现完全一致。

### 6.1 服务端证据

服务端抓包可见：

```text
客户端公网地址:临时端口 -> 38.209.122.38:目标端口
UDP payload length 148

38.209.122.38:目标端口 -> 客户端公网地址:临时端口
UDP payload length 92
```

这证明：

- 客户端请求能够到达服务端。
- 服务端 WireGuard 能识别请求并生成响应。
- 服务端已将响应交给 `eth0` 发出。
- 53/443 的 REDIRECT 规则可以正常命中。

服务端抓包中出站 UDP 显示 `bad udp checksum`，曾临时关闭 `eth0` TX checksum offload 进行验证。关闭后故障没有变化，随后已恢复原设置。因此该提示只是常见的网卡校验和卸载抓包现象，不是本次根因。

### 6.2 客户端物理网卡证据

使用 Windows PktMon 在 WLAN 物理网卡层抓取 `38.209.122.38:51227/udp`：

- 可以看到客户端发出的 148 字节 WireGuard 握手请求。
- 完全看不到服务端发出的 92 字节响应。
- Windows 本机也没有记录对应的入站丢弃事件。

因此，返回包在到达 Windows 物理网卡之前已经消失。本机 WireGuard 驱动、Windows 防火墙和 Clash Verge 不是这一阶段的丢包位置。

### 6.3 新端口验证

服务端临时增加全新入口：

```text
29987/udp -> REDIRECT -> 51227/udp
```

客户端切换为：

```ini
Endpoint = 38.209.122.38:29987
```

切换后立即恢复握手：

- `10.66.66.1` 连续可达。
- 延迟约 176 至 285 ms。
- 服务端 latest handshake 正常刷新。
- Clash Verge 公网出口不受影响。

该验证说明服务端 WireGuard、密钥、NAT、客户端驱动均正常，故障与旧 UDP 端口/流量路径相关。

## 7. 根因判断

### 7.1 已确认的直接原因

当前这次无法连接的直接原因，是 53、443、51227 三个旧 UDP 入口的 WireGuard 回包在公网返回路径中被选择性丢弃。

证据链如下：

```text
客户端 WireGuard 发出请求
  -> Windows WLAN 抓包可见
  -> 服务端 eth0 抓包可见
  -> 服务端 WireGuard 识别请求并生成响应
  -> 服务端 eth0 抓到出站响应
  -> Windows WLAN 完全看不到响应
  -> 更换全新端口 29987 后立即恢复
```

### 7.2 最可能的上游原因

仅凭两端抓包无法确定具体是哪一家中间网络设备丢包，但结合以下规律：

- 端口最初可用，持续一段时间后失效。
- 51227、443、53 依次出现相同问题。
- 新端口可以立即恢复。
- WireGuard 原生流量没有协议伪装。

最可能是运营商、跨境链路或上游风控/DPI对特定 UDP 五元组或 WireGuard 流量进行识别后丢弃。家庭路由器/NAT异常也不能被绝对排除，但“换服务端目标端口立即恢复”的规律更符合链路侧按端口或流量特征限制。

这不是 DNS 污染。客户端直接连接公网 IP，域名解析不参与 WireGuard 握手。

## 8. 当前配置与状态

### 8.1 客户端

当前活动配置：`D:\git\toolkit\wg0-autostart.conf`

关键参数：

```ini
Address = 10.66.66.2/32, fd42:42:42::2/128
Endpoint = 38.209.122.38:29987
PersistentKeepalive = 25
AllowedIPs = 10.66.66.0/24, fd42:42:42::/64
```

服务状态：

```text
服务名：WireGuardTunnel$wg0-autostart
启动类型：AUTO_START
当前状态：RUNNING
```

使用独立名称 `wg0-autostart`，是为了避免 WireGuard GUI 中已有的同名禁用隧道删除外部安装的自动服务。

### 8.2 服务端

新增持久规则：

```text
INPUT allow 29987/udp
PREROUTING 29987/udp -> REDIRECT 51227/udp
```

规则已写入 `/etc/wireguard/wg0.conf` 的 `PostUp`/`PostDown`，并通过 `wg-quick strip wg0` 配置校验。

服务端修改前备份：

```text
/etc/wireguard/wg0.conf.bak-20260713-121557
```

## 9. 风险与后续建议

### 9.1 短期

- 保持当前 29987/udp 入口。
- 保持 `PersistentKeepalive = 25`。
- 保留服务端 IP 的 WLAN `/32` 直连路由。
- 观察 29987 是否再次在数天后失效。
- 排查阶段继续使用小范围 `AllowedIPs`，避免影响 Clash Verge。

### 9.2 中期

如果 29987 也在类似时间后失效，继续换端口只能作为临时绕过，不能视为稳定方案。建议增加端口健康检查和备用入口，但不要无限累积旧转发规则。

### 9.3 长期

原生 WireGuard 不提供流量伪装。如果确认存在持续的 WireGuard 流量识别，应考虑：

- 给 WireGuard 增加可靠的流量封装或混淆层。
- 使用适合受限 UDP 网络的替代传输方案。
- 更换服务端公网 IP或网络提供商，验证是否为当前服务端线路的专项限制。
- 使用手机热点或另一家宽带对比，进一步区分本地运营商与服务端上游。

在当前 29987 端口连续稳定运行一段时间前，不建议立即恢复全局 WireGuard 路由。恢复全局路由时，应继续保留 `38.209.122.38/32` 的 WLAN 直连规则，并先验证 DNS 和海外 HTTPS 访问。

## 10. 最终结论

本次故障并非单一原因：

1. 早期存在服务端缺少客户端 `PresharedKey` 的配置问题，已修复。
2. Windows WireGuardNT 曾出现 Code 31 虚拟网卡故障，已通过完整重装 WireGuard 和驱动修复。
3. 当前主要网络故障是旧 UDP 端口的返回流量在公网路径中被丢弃。两端抓包与新端口对比测试已形成完整证据链。
4. 新端口 `29987/udp` 当前正常，客户端采用小范围路由，Clash Verge 不受影响。

如果新端口再次在数天后失效，应停止把“更换端口”当作最终修复，转向流量封装、替代协议或更换线路/IP。
