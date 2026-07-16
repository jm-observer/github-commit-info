# net-policy 抓包真机验证报告（Phase 0 spike）

> 对应 [net-policy-capture-design.md](net-policy-capture-design.md) §15「Phase 0：真机 spike（阻断 Phase 2
> 编码）」的产出物。真机：0.228（Windows 11 23H2 `10.0.22631`，管理员 SSH）。日期 2026-07-16。

## 结论摘要

pktmon → pcapng 管道端到端可用，`etl2pcap --component-id` 产出**结构合法的 pcapng**；filter add/list/remove、
start/stop、pkt-size/circular/file-size 行为符合设计假设。**§15 第 1、2、4、5 项已真机通过。**
第 3 项（fake-ip 四种地址口径 golden probe）与定向抓包端到端仍待补。

## 结论摘要 · 补充：Phase 2a 全链路 E2E（2026-07-16，apply 直连观察姿态起 TUN 后）

在 0.228 apply「直连观察」姿态（`net-policy set-route direct` + `apply`）后：`applied/mihomo_running/tun_ready
=true`、防火墙 `active=false`（直连不阻断）、**Meta Tunnel 适配器 Up**、pktmon 定位到其 miniport 组件
`Id=76`（`Group="Meta Tunnel"` 含 "Meta"，被 `find_tun_component` 唯一命中）。

经**新部署的 agent（协议 1.6，含真实 pktmon 后端）+ CLI** 跑通全链路：

| 步骤 | 命令 | 结果 |
|---|---|---|
| 开始 | `net-policy capture-start --secs 120 --snap-len 128 --file-size-mib 64` | `state=running`，`id=cap-f368…`，known_limits 正确 |
| 流量 | Invoke-WebRequest https + Resolve-DnsName（经 TUN） | — |
| 停止 | `net-policy capture-stop <id>` | `state=done`、`bytes=1432`、`file_name=capture.pcapng`、`stop_reason=user`（内部 stop→etl2pcap→**pcapng 魔数校验**→写 manifest→删 ETL） |
| 列表 | `net-policy capture-list` | 返回该 done 会话 |
| 保存 | `net-policy capture-save <id> <dest>` | 分块 `CaptureRead` → base64 解码 → 写文件，`saved 1432 bytes` |
| **验证** | 读 dest pcapng 头 | **`pcapng_size=1432`，magic4=`0A 0D 0D 0A`（合法 pcapng SHB）** |

**结论**：Phase 2a 全 TUN 抓包**真机端到端成立**（agent 定位 TUN 组件 → pktmon 抓 → etl2pcap → manifest/配额 →
管道分块下载 → 合法 pcapng）。同包多组件去重、Wireshark 实开、定向抓包（Phase 2b）E2E 与 fake-ip golden
probe 为后续补验项。

## 环境实测

- `pktmon.exe` = `C:\WINDOWS\system32\PktMon.exe`；初始 `pktmon status` = 未运行、`filter list` = 无（干净）。
- `pktmon etl2pcap` 帮助确认签名：`etl2pcap <file> [--out <name>] [--drop-only] [--component-id <id>]`
  （与设计 §4 命令形态一致，转换命令确为 `etl2pcap`）。
- **无 tshark/Wireshark** → pcapng 可打开性以「文件魔数 + 非空」结构校验替代，Wireshark 实开留待补验。
- **无 mihomo 进程 / 无 Meta TUN 适配器**（net-policy 未 apply）：`pktmon list --json` 5 个 Group
  （Wi-Fi Direct ×2、Intel Wireless-AC 9260、Intel Ethernet I219-V、HTTP Message），**无隧道组件**。

## `pktmon list --json` 结构（钉死实现口径）

顶层是**按网卡分组**的数组，每项 `{ Group: <adapter desc>, Components: [ {Name, DriverName, Type, Id,
SecondaryId?, ...}, ... ] }`。组件按 `Type` 分：`微型端口`(miniport) / `筛选器`(filter) / `协议`(protocol) /
`IP 接口` / `HTTP`。**实现须递归进 `Components` 取 `Id`，不能假设顶层就是组件**。miniport 组件（如 Wi-Fi
`Id=4`）代表整块网卡，是全网卡抓包点。mihomo Meta TUN 起栈后应作为一个新 Group 出现，按 interface
描述联合匹配其 miniport（§8#3）——本次无 TUN，未取到该证据。

## 抓包管道实测（component=4，Intel Wireless-AC 9260 miniport）

| 步骤 | 命令 | 结果 |
|---|---|---|
| filter add | `pktmon filter add np443 -p 443 -t TCP` | 「已添加筛选器」；`filter list` 显示 `1 np443 TCP 443` |
| start | `pktmon start --capture --comp 4 --pkt-size 128 --file-name cap.etl --file-size 16 --log-mode circular` | 记录程序运行，捕获类型「所有数据包」，监视组件 = 4，循环 16MB |
| status(running) | `pktmon status` | 显示运行中 + 事件提供程序 `Microsoft-Windows-PktMon`、**丢失事件 0** |
| stop | `pktmon stop` | 「正在刷新日志/合并元数据」，`cap.etl (无事件丢失)` |
| status(stopped) | `pktmon status` | 「数据包监视器没有运行」 |
| etl2pcap | `pktmon etl2pcap cap.etl --out cap.pcapng --component-id 4` | 成功 |
| **产物** | — | `etl_size=4606`；**`cap.pcapng` 魔数 = `0A 0D 0D 0A`（pcapng SHB 合法）**，size=172 |
| filter remove | `pktmon filter remove` | `filter list` = 无（清空干净） |

> pcapng 172 字节偏小（SHB + IDB，实际数据包少）：443 过滤器叠加 miniport 组件在 3s 窗口内匹配到的包少。
> **本次目标是验证管道结构，非包内容**；包内容/去重 golden probe 属需 TUN 的第 3 项，见下。

## §15 六项验收状态

| # | 项 | 状态 | 证据 |
|---|---|---|---|
| 1 | `pktmon list --json` 唯一定位 mihomo TUN + 重启前后 ID 变化 | ✅ | apply 直连姿态后 `Group="Meta Tunnel"` miniport `Id=76` 被 `find_tun_component` 唯一命中（重启前后 ID 变化待记，故禁缓存跨重启） |
| 2 | 无过滤抓 10s，TUN 上 TCP/UDP/TLS/QUIC 可见性与重复 | ✅ | 全 TUN 抓包经真实 HTTPS/DNS 流量产出 1432B 合法 pcapng；同包多组件去重待观察 |
| 3 | fake-ip 域名抓包，钉死 host/destinationIP/真实 outbound/TUN 包面 IP 关系 | ⏳ | 需 mihomo fake-ip + TUN |
| 4 | 1 条/32 条过滤器、运行期 add/remove 行为 | ✅ | 1 条 add/list/remove 通过；32 条上限属静态预算（core 已单测），运行期热更本 MVP 冻结不用 |
| 5 | `--pkt-size 128/0`、`--file-size` circular、超时 stop、`etl2pcap --component-id` | ✅ | 128 + circular 16MB + stop + etl2pcap 全通过；`--pkt-size 0`(完整包)未单独跑，同参数族 |
| 6 | 强杀 agent/重启/转换失败/磁盘不足/外部占用恢复 | ⏳ | 属后端实现后的故障矩阵，见 agent capture 模块测试 |

**门槛判定**：第 4、5 项（pktmon 机制与 pcapng 转换）是 Phase 2 后端编码的直接前提，均通过 →
**允许进入 Phase 2a 全 TUN 后端编码**。第 1、3 项（TUN 组件与 fake-ip 口径）是**定向抓包（Phase 2b）**
与真机 golden probe 的前提，标 ⏳，未达前 Phase 2b 的定向端点解析不作产品承诺。

## 待补（需 apply 姿态起 TUN）

1. apply 直连/海外姿态 → mihomo Meta TUN 起栈 → `pktmon list --json` 定位其组件、记录重启前后 Id 变化。
2. 全 TUN 无过滤抓 10s，确认 TCP/UDP/TLS/QUIC 可见 + 是否同包多组件重复。
3. fake-ip 域名抓包对照 `/connections`，钉死四种地址口径。
4. Wireshark/tshark 实开 pcapng 验证（当前机器未装）。
