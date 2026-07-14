# net-policy 验证报告（v2.1 真机实测版）

## 0. 元信息

- **状态**：**v2.1 真机实测版**。**唯一权威结论表 = §0.8.2**（含证据强度分级）；正文其余处与之冲突以 §0.8.2 为准。核心机制 + 完整栈 fail-closed + VP-08/09/10 + strict-route 均 **✅ 有原始证物**（§0.9.2 逐行实录 / §0.2-0.8）；仅 VP-11（◑摘要）、DoH（▢研究层）、逐包抓包未达最高取证强度。
- **日期**：2026-06-12 起，§0.9 系列 2026-06-14（含原始证物 §0.9.2）
- **生产化硬约束（必读）**：① DNS 生产模式已拍板"模式 A：允许 bootstrap DNS 物理直连"（§6，有限泄漏面；模式 B respect-rules 零泄漏▢未实测，勿依赖）；② kill-switch 须 detached/service 上下文执行（勿在交互会话内，§0.9/§5）+ 停 mihomo 必须优雅（勿 force-kill，§0.8.2bis）；③ 重启后 WG 须 Automatic 自启/app 拉起（§0.9.1）。
- **实测环境**：0.228 = Windows 11 23H2 / 10.0.22631（注意非设计假设的 26200）
- **关联设计**：docs/unified-desktop-shell-design.md §14
- **底稿来源**：6 个并行子研究（wg-official-client / mihomo-tun-wg / process-rules / dns / ipv6 / firewall-killswitch）汇总；v2 修订基于 4 项关键技术断言的独立验证（见 §0.1）

## 0.1 v2 修订说明

v1 底稿被审查后，对 4 个根本性技术断言做了独立验证（WebSearch + 官方文档/源码核对），结论及对底稿的影响：

| 断言 | 结论 | 对底稿的影响 |
|---|---|---|
| Windows Firewall 中 Block 永远击败 Allow | **确认成立**（[MS 文档 2025-06](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/rules) 三条优先级规则；OverrideBlockRules 仅在 IPsec 场景可绕过） | v1 §5 的「Allow 白名单 + Block 兜底」结构在 PowerShell 层根本不工作。重写为「**Profile 默认 Block + 纯 Allow 白名单**」模式 |
| NRPT 不支持自定义端口（Add-DnsClientNrptRule -NameServers 只能裸 IP） | **确认成立**（[NRPT WMI MOF schema](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/ramgmtpsprov/dnsclientnrptrule) 19 个属性中无端口字段） | v1 §6 listen :1053 + §7.1 NRPT 127.0.0.1 必然全部超时。**改为放弃 NRPT、依赖 mihomo TUN dns-hijack + strict-route** 拦截所有 53 端口 DNS |
| Wintun InterfaceType 不算 Wired | **部分成立**（Wintun INF 写 IfType=53 IF_TYPE_PROP_VIRTUAL、MediaType=19 NdisMediumIP；但 [MS Win32 API 文档](https://learn.microsoft.com/en-us/windows/win32/api/netfw/nf-netfw-inetfwrule-get_interfacetypes) 未公开 IF_TYPE → FW_INTERFACE_TYPE 映射表） | v1 §5 规则 6 依赖未验证假设。**改用 InterfaceAlias 白名单**（按适配器名精确放行 TUN）代替 InterfaceType 黑名单 |
| Set-NetFirewallRule -Group 是 ByGroup 参数集 selector | **确认成立**（[MS PS 文档 2025-05](https://learn.microsoft.com/en-us/powershell/module/netsecurity/set-netfirewallrule)；与 -Name 互斥） | v1 §5 Set-FwRule 函数 update 分支需补 `$updateParams.Remove('Group')` |

v2 改动范围：§1（关键判断重写两条 + 软化"不需要自研 WFP"）、§2（推荐栈重写防火墙/DNS 行）、§3.4 / §3.6（v1 NRPT / InterfaceType 结论加 v2 校正注解，保留作背景）、§5（脚本整段重写 + 状态快照/恢复 + mihomo 上游 DNS 风险标注）、§6（注释端口选择 + 删除 NRPT 提示）、§7.1（重排步骤 + 加 InterfaceType 探针 + 浏览器 DoH 可执行命令）、§4 VP-01 修 Wireshark filter、§9 移除已验证项 + 新增高优问题 6（mihomo 上游 DNS 路径）。原 v1 内容保留在 git 历史。

### v2 的诚实声明（重要）

**v2 修订完全基于 web 研究（官方文档 / 源码 / GitHub issue）+ 子 agent 验证，未在任何 Windows 主机上实际部署或执行。** 修订消除了 v1 的 4 个文档可证伪的错误（防火墙仲裁、NRPT 端口、Set-NetFirewallRule 参数集、Wintun IfType 数值），但 v2 自身仍有未实测的核心空白，至少包括：
- mihomo 进程上游 DNS 路径（§9 #6）—— 决定 §5 脚本是否需要再加 `Program=mihomo.exe` 白名单
- VP-08（mihomo 崩溃）/ VP-10（路由被改）下默认 Block 是否独立维持 —— 决定"首版不需自研 WFP"是否成立
- Wintun InterfaceType 实测值 —— 即使 v2 用 InterfaceAlias 绕开，未来回到 InterfaceType 方案仍要这数据

请把 v2 视为"实测前底稿 v2"，而不是"已就绪可生产的脚本"。Step 0 InterfaceType 探针 + Step 5 部署后的连通性测试必须在主路径前跑通。

## 0.2 真机实测结果（v2.1，2026-06-12）

> v2 的"诚实声明"说"未在任何 Windows 主机上执行"——这一节起部分作废：以下 4 项已在**真机**上实测。
> **环境**：主机 `192.168.0.228`（feng，管理员），**Windows 11 专业版 23H2 / 10.0.22631**（注意：**不是**设计文档假设的 26200），经 SSH 远程执行。该机已装官方 WireGuard 客户端（WireGuard-NT 驱动）并建立全量隧道（出口 38.209.122.38 / US-LA）。
> **测试方法**：全程只读或用 `RemoteAddress=9.9.9.9`（该机未使用的无害目标）精确隔离 + try/finally 保证清理；未改 Profile 默认动作、未建 InterfaceType=Wired Block（避免切断经以太网的 SSH）。

| # | 断言 | 真机结果 | 判定 |
|---|---|---|---|
| ① | WireGuard/Wintun 适配器 InterfaceType 非 Wired/Wireless | 以太网=**6**(802.3)、WLAN=**71**(802.11)、**wg-0228 隧道=53**(IF_TYPE_PROP_VIRTUAL, Media=IP) | ✅ 源数据假设成立（IfType=53），与 Wintun INF 一致 |
| ② | Windows Firewall 中 Block 击败更具体的 Allow | 基线 ping 9.9.9.9 通；加【具体 Allow(ICMPv4) + 宽 Block(any)】后 **ping 失败** | ✅ **真机确认**——驱动整个 §5 重写的核心断言成立 |
| ③ | Set-NetFirewallRule -Name X -Group Y 报参数集错误 | 实测报错"无法使用指定的命名参数来解析参数集" | ✅ 真机确认，v1 Set-FwRule bug 属实 |
| ④ | Profile 原始 DefaultOutboundAction（决定 Remove 恢复目标值） | Domain/Private/Public 全 = **NotConfigured**（非 Allow） | ✅ 验证 §5 Remove「快照+按原值恢复」是必要的；盲恢复成 Allow 会篡改基线，fallback 应恢复成 **NotConfigured** |

**仍未实测（不能因本节而认为已闭环）**：
- ~~①的**行为层**：`-InterfaceType Wired` Block 是否误命中 53 号适配器未测~~ → **已补测，见 §0.3**：用 RemoteAddress=9.9.9.9 限定的安全探针证实 Wired 规则**不命中**隧道流量（对照组有效）。v2 用 InterfaceAlias 的决策保留，但理由改为工程稳健性而非"假设未验证"。
- ②的**默认 Block 形态**：本次测的是「显式 Block 规则胜过 Allow」；v2 实际用的是「Profile DefaultOutboundAction=Block + 无显式 Block，仅 Allow 白名单」，这条路径（尤其 VP-08 崩溃 / VP-10 改路由下是否独立维持）**未实测**。
- §9 #6 mihomo 进程上游 DNS 路径、VP-01~12 全套——**未实测**。
- 本机用的是**官方 WireGuard 客户端 + WireGuard-NT**，不是 v2 推荐的 mihomo Wintun；IfType=53 两者按 INF 一致，但 mihomo 全栈（TUN dns-hijack / strict-route / 进程规则）在本机**未部署验证**。

## 0.2.1 实测接入方式（复现用）

实测主机 0.228 的远程操作方式，便于后续会话/他人复现（**仅记方法与指针，密钥材料不入库**）：

- **主机 / 账号**：`192.168.0.228`，用户 `36225`（whoami 显示 `feng\36225`），SSH 会话即**管理员**（`IsInRole(Administrator)=True`，改防火墙/Profile 无需再提权）。
- **SSH 密钥**：操作机 `~/.ssh/id_ed25519_228`（专用 key，非默认 id_rsa）。`ssh -i ~/.ssh/id_ed25519_228 36225@192.168.0.228`。
- **回程路径**：SSH 从操作机 `192.168.0.45` 经**以太网**（ifIndex 8）进入，与 0.228 同在 `192.168.0.0/24`。**这是所有"远程安全"判断的前提**——LAN /24 路由比隧道/默认路由更具体，始终走以太网；故只要不显式 Block 这条 LAN 路径，远程通道不断。
- **默认 shell 是 cmd.exe**：直接 `powershell -Command "..."` 经 SSH→cmd→PS 多层转义会把中文/引号搞坏（实测踩过"字符串缺少终止符"）。**统一改用 `-EncodedCommand`**：本地写 `.ps1` → `iconv -f UTF-8 -t UTF-16LE x.ps1 | base64 -w0` → `ssh ... "powershell -NoProfile -EncodedCommand <b64>"`。彻底免转义，中文正常。
- **输出过滤**：PS 经 SSH 回传会夹带 `#< CLIXML` 进度流噪声，用 `grep -avE 'CLIXML|<Objs|progress|RefId'` 滤掉。
- **编码工具坑**：操作机（Git Bash）无 `python3`，用 `iconv`+`base64`；PowerShell 工具跑在沙箱用户下、SSH 会因 key 权限报错拒绝，**SSH 只能走 bash 工具**。
- **WireGuard 现状**：0.228 已装官方客户端（WireGuard-NT），隧道服务名 `WireGuardTunnel$wg-0228`，全量隧道出口 `38.209.122.38`（US-LA）。

## 0.3 InterfaceType 行为探针 — 方案与结果（v2.1，2026-06-12）

补 §0.2 测试①唯一缺的"行为层"：Windows Firewall 是否把 IfType=53 隧道适配器归类为 `Wired`（即 `-InterfaceType Wired` 的 Block 会不会误伤隧道流量）。

**安全设计**：所有 Block 限定 `RemoteAddress=9.9.9.9`（该机未用、非 DNS `1.1.1.1`、非 SSH 路径 `192.168.0.45`），永不触碰远程管理通道——故可远程执行，无需本地控制台。全程 try/finally + 唯一规则组 `NetPolicyIfProbe` 保证清理。

**步骤**：① baseline ping 9.9.9.9 → ② 加 `-InterfaceType Wired -RemoteAddress 9.9.9.9 -Block` 后 ping → ③ 对照 `-InterfaceType Any -RemoteAddress 9.9.9.9 -Block` 后 ping（必须拦住，验证测法）→ ④ 清理。

**结果（0.228 / Win11 23H2 / 官方 WireGuard-NT 全量隧道）**：

| 步骤 | 实测 | 含义 |
|---|---|---|
| baseline | ping = **True** | 9.9.9.9 经隧道可达 |
| `-InterfaceType Wired` Block | ping = **True** | Wired 规则**未命中**隧道流量 |
| `-InterfaceType Any` Block（对照）| ping = **False** | 规则机制 + ping 探测有效（排除假阴性）|

**判定：隧道适配器（IfType 53）不被归类为 Wired** ✅。`-InterfaceType Wired,Wireless` 的 Block 不会误伤隧道——WFP 按流量实际出口适配器（隧道）分类，53 号不在 Wired 桶内。对照组拦截成功，结论可信。

**对 v2 决策的影响**：v1 的 InterfaceType 假设在**行为层也成立**（至少本机/本驱动）。但 v2 改用 InterfaceAlias 的决策**不需要回退**——理由从"假设未验证"变为"工程取舍"：InterfaceAlias 不依赖任何（即便已实测的）分类行为，跨 OS 版本/驱动更稳。本结论的价值是：**InterfaceType 方案现已被证明可作为等价备选**，二者皆可。

**边界**：测的是 WireGuard-NT 适配器，非 mihomo Wintun（两者 IfType 均 53，结论大概率可迁移但未直接验证）；单机 Win11 23H2 单次；未测多网卡/RemoteAccess 桶。

## 0.4 默认 Block + Allow 白名单 kill-switch — 方案与结果（v2.1，2026-06-12）

验证 v2 防火墙模型的**完整闭环**：`DefaultOutboundAction=Block` + 纯 Allow 白名单，且引擎（隧道）死亡时 fail-closed 不泄漏。这是 §5 整套脚本设计的真机验证。

**说明**：本机"引擎"用的是官方 WireGuard-NT 隧道（非 mihomo），但被验证的是 **Windows Firewall 机制本身**——默认 Block + 白名单 + 引擎死亡后是否泄漏，与隧道软件是 WireGuard 还是 mihomo 无关。结论可迁移到 mihomo。

**三重安全**：① 死人开关（8 分钟后自动 `DefaultOutboundAction=NotConfigured` + 删 KS 规则的 SYSTEM 计划任务，锁出也自愈）② 预放行 `192.168.0.0/24` 保 SSH ③ 机器在身边。

**Phase 1（建白名单 + 设默认 Block + 验证存活）**：

| 项 | 实测 | 判定 |
|---|---|---|
| 原 DefaultOutboundAction 快照 | `NotConfigured` | 与测试④一致 |
| 设后 DefaultOutboundAction | `Block` | kill-switch 激活 |
| ping 9.9.9.9（经隧道，白名单内）| **True** | 隧道+白名单在默认 Block 下仍通 |
| public IP | `38.209.122.38` | 全量隧道出口 + 经隧道 DNS 均正常 |
| PHASE1-DONE 经 SSH 返回 | ✅ | **SSH 在默认 Block 下存活** |

**Phase 2（fail-closed：停隧道引擎）**：

| 项 | 实测 | 判定 |
|---|---|---|
| 实存白名单 | LAN / WGep / TUN（3 条）| LAN 规则在 → SSH 靠**显式 LAN Allow** 存活，非侥幸的有状态放行 |
| 停隧道 + 默认 Block，ping 9.9.9.9 | **False** | ✅✅ **fail-closed 确认**：引擎死，未白名单流量**不泄漏物理网卡** |
| 停隧道时 SSH | 仍活 | LAN 路径与隧道独立 |
| 重启隧道，ping 9.9.9.9 | **True** | 恢复正常 |

**Phase 3（恢复）**：DefaultOutboundAction 还原 `NotConfigured`、KS 规则清 0、死人开关删除、出口 IP 复原 `38.209.122.38`。现场干净。

**核心结论**：v2 的「默认 Block + Allow 白名单」kill-switch 模型**真机端到端成立**——
1. 默认 Block 下，显式 LAN Allow + 隧道适配器 Allow + WG-endpoint Allow 三条即可让 SSH 与隧道 internet 全程存活；
2. **fail-closed 真机证实**（VP-08/VP-09 等价）：引擎一停，非白名单目标立即不可达，不泄漏；
3. 远程可安全执行（预放行 LAN + 死人开关）；
4. §5 的「快照原值 + 按原值恢复」必要性再次坐实（原值 NotConfigured，盲恢复 Allow 会篡改）。

**实测发现的脚本 bug（已据此修 §5，且追加根因确证）**：建白名单时 loopback 那条 `-RemoteAddress 127.0.0.0/8,::1` **创建失败**（4 条只成 3 条）。
- **初判错误**：先以为是"混合 v4/v6 地址族"不被接受。
- **追加确证**（专门跑了对照实验）：真因是 **`::1`（IPv6 环回）被 `New-NetFirewallRule` 拒绝**，报"未指定/多播/广播或环回 IPv6 地址"。对照实验显示：混合 v4 CIDR + `fc00::/7` + `fe80::/10`（即 §5 R4 那种）**创建成功**——所以 R4 无需改，**问题仅限 `::1` 这一个特殊地址**。
- **修法**：§5.2 R2 改为只放行 `127.0.0.0/8`，丢弃 `::1`（Windows loopback 本就 WFP 豁免，无需显式 v6 规则）。**R4 维持不变**。
- 教训：基于实测改脚本前先确证根因——若按初判去拆 R4，是多余且把对的规则改坏。

**边界**：引擎用 `Stop-Service`（干净停）非硬杀（`taskkill /f`）；但默认 Block 是 Profile 设置，独立于隧道生死，硬杀结论应一致。单机 Win11 23H2 单次；未引入 mihomo 全栈（dns-hijack/strict-route/进程规则仍未实测，见 §9）。

## 0.5 mihomo 进程分流 + DNS 机制 — 实测（v2.1，2026-06-12）

验证 mihomo 在 Windows 上最不确定的两点：**PROCESS 规则按进程分流是否生效**、DNS 解析链路。

**取材**：mihomo `v1.19.27`（go1.26.4, with_gvisor），从 github.com 官方 release 下载（**经 0.228 自己的 WG 隧道 6 秒下完 17MB**——比操作机转发快，是利用现网隧道下载的实用技巧）。

**测法（非破坏性，关键决策）**：核心机制用 **mihomo 代理模式**测，**不开 TUN、不停 WG、不碰路由/防火墙**——`PROCESS-NAME` 匹配在代理模式与 TUN 模式走同一个 Windows 进程查找 API（`GetExtendedTcpTable`+`QueryFullProcessImageName`），故代理模式是忠实代理。config：`mixed-port:7890` + `find-process-mode:always` + 规则 `PROCESS-NAME,curl.exe,REJECT` / `MATCH,DIRECT`。

**结果（debug 日志 = ground truth）**：

```
[TCP] 127.0.0.1:54768(curl.exe)       --> example.com:80 match ProcessName(curl.exe) using REJECT
[TCP] 127.0.0.1:54769(powershell.exe) --> example.com:80 match Match using DIRECT
[DNS] example.com --> [104.20.23.154 172.66.147.243] A from udp://223.5.5.5
```

| 验证项 | 结果 | 判定 |
|---|---|---|
| **PROCESS-NAME 按进程分流** | curl.exe → `ProcessName(curl.exe)` → REJECT；powershell.exe(同目标) → DIRECT | ✅ **真机确认**——mihomo 正确识别连接源进程并分流，解决 §3.3 核心不确定性 |
| `find-process-mode: always` | 每连接查进程，日志逐条标注进程名 | ✅ 有效 |
| DNS 上游解析 | example.com → 真实 IP，from 223.5.5.5 | ✅ 上游链路通 |
| curl 的"失败"(exit7/code000) | = REJECT 生效的预期表现，**非 bug** | ✅ 复核纠偏 |

**未测/待续（仍是 v2 主路径的空白）**：
- **TUN 系统级集成**：`dns-hijack any:53` 是否真拦系统 DNS、`auto-route`/`strict-route` 行为、TUN 下 fail-closed——需开 TUN，会与现网官方 WG 抢路由，须停 WG + 死人开关，本轮未做。
- **§9 #6 mihomo 上游 DNS 路径**：需配 WG outbound（服务端加 peer），未做。
- **fake-ip 取值**：API 未捕获（见下方进程生命周期坑），但 fake-ip 是平台无关成熟行为，非 Windows 特异风险。

**实测发现的工程坑（记下供后续）**：经 SSH 用 `Start-Process` 后台起的 mihomo，**在该 SSH 会话结束后被回收**（下一次 SSH 连进去发现进程没了）。后续 TUN 常驻测试必须把 mihomo 注册成**服务**（`sc create` / NSSM）或 schtasks，不能靠 Start-Process。

**残留**：mihomo 二进制/config/log 留在 `C:\Users\36225\mihomo\`（进程已停、非服务、无害），供后续 TUN/WG-outbound 测试复用。

## 0.6 mihomo TUN 系统级集成 — 实测（v2.1，2026-06-12）

验证 TUN 模式的系统级行为：`dns-hijack` 是否真拦系统 DNS、`auto-route` 接管、TUN 下进程匹配、出站 egress。

**测法（有损，三重安全）**：① 死人开关（8 分钟 `taskkill mihomo + 重启 WG`）② 停官方 WG 让出路由 ③ mihomo 以 **schtask（SYSTEM）常驻**（避开 Start-Process 被回收）④ **strict-route=false**（不动防火墙，先降风险）。config：TUN + gvisor + `dns-hijack: any:53` + `auto-route` + `auto-detect-interface` + DIRECT outbound。

**结果**：

| 验证项 | 实测 | 判定 |
|---|---|---|
| TUN 起栈 | Meta 适配器 Up，IfType 53，**wintun 内嵌自动释放**（zip 不含 dll 也能起）| ✅ |
| auto-route 接管 | 默认路由出现 `Meta(ifIndex40) → 198.18.0.2 metric0`，物理以太网路由保留 | ✅ |
| SSH 存活 | TUN 接管默认路由后 SSH 仍通（LAN /24 更具体）| ✅ 远程安全前提再次成立 |
| **dns-hijack 拦系统 DNS** | `Resolve-DnsName example.com -Server 8.8.8.8` → **198.18.0.14**（fake-ip）；系统默认解析同样 fake-ip | ✅✅ **即便显式查公网 8.8.8.8 也被 TUN 拦截**，解决 §3.4/§9#5 hijack 层关切 |
| 进程匹配（TUN 模式）| connections 里 svchost/OneDrive/chrome/msedge 各连接进程识别准确 | ✅ 与 §0.5 代理模式一致 |
| **DIRECT 出站 egress** | 初测 api.ipify(境外) 超时 → **复测国内 baidu.com=200**（见下纠偏）| ✅ **egress 正常** |
| schtask SYSTEM 常驻 | mihomo 跨 SSH 会话存活 | ✅ 解决 §0.5 的生命周期坑 |
| 拆栈恢复 | 杀 mihomo → Meta 适配器+路由自动消失；重启 WG → 出口复原 38.209.122.38 | ✅ 干净可逆 |

**⚠️ 纠偏（v2.1 复测推翻初判）**：初测时 `api.ipify.org`（境外 HTTPS）超时、connections 的 `destinationIP` 空，我**错误地**归因为"路由环/egress 失败 = 头号风险坐实"。复测发现这是误判：
- 用**国内目标** baidu.com 复测 → **HTTP 200，egress 正常**；ping 223.5.5.5/1.1.1.1 均通。
- 对照实验：`auto-detect-interface: true`（无 interface-name）与显式 `interface-name: 以太网` **两种配置 baidu 都 200**——**interface-name 并非 egress 必需**。
- 真因：初测目标 api.ipify.org 是境外站，WG 停掉后走国内直连本就不可达（与本机 winget/ip.sb 失败同因），**与 mihomo egress / 路由环无关**。
- 教训：又一次下结论太快（同 §0.4 loopback 误判）。测 egress 必须用**确定可达**的目标（国内站），不能用境外站，否则把网络可达性误读成组件故障。

**对 v2 的意义（修正后）**：mihomo TUN 的两个系统级核心机制——**dns-hijack 拦系统 DNS** 与 **DIRECT/auto-route egress**——真机均正常。头号风险"路由环"**未被坐实**（本轮 DIRECT 形态没出现）；WG outbound 形态下的握手包绕 TUN 仍需单测（§9#1）。`interface-name` 降级为"可选加固"，非必需。

## 0.7 完整 v2 栈（mihomo TUN + WG outbound）— 实测（v2.1，2026-06-12）

搭 v2 实际目标形态：mihomo TUN + **WireGuard userspace outbound**（复用 peer .5 密钥，官方 WG 停时身份空出，无需服务端加 peer），跑 VP 用例。

| 用例 | 实测 | 判定 |
|---|---|---|
| **VP-01 未知流量→海外** | mihomo `MATCH,wg-out`，public IP = **38.209.122.38（US）**；wg-out proxy alive=True | ✅ |
| **§9#1 路由环** | WG 握手 UDP 经 auto-detect 绑定物理网卡到达服务端，**未被 TUN 回捕**，无需手动 route-exclude | ✅ **头号风险解除**（DIRECT + WG outbound 两形态均未现环）|
| **VP-04 进程分流** | `PROCESS-NAME,curl.exe,DIRECT` + `MATCH,wg-out`：curl.exe 出口 **58.23.139.139（国内 DIRECT）**、powershell 出口 **38.209.122.38（美国 wg-out）** | ✅ 同刻双出口，铁证 |
| **VP-02/03 IP/域名直连** | 用同一规则引擎（VP-04 已证），机制相同 | ◑ 推断成立，未单独取证 |

**核心结论**：v2 设计的端到端数据通路（TUN 劫持 → fake-ip → 规则分流 → WG userspace outbound → 海外出口）**真机完整跑通**，且最担心的路由环不存在。

### 0.7.1 ⚠️ 重大发现：kill-switch 默认 Block + mihomo TUN 同时激活 → 切断 LAN SSH

在 mihomo TUN 栈之上叠加 §0.4 验证过的 kill-switch（`DefaultOutboundAction=Block` + LAN/WGep/Meta/loopback 白名单）测 §9#6 时，**SSH 立即断连**——与 §0.4「纯 kill-switch（无 mihomo）下 SSH 靠显式 LAN Allow 存活」的结果**相反**。

- §0.4：官方 WG 隧道 + kill-switch，LAN Allow 规则保住 SSH ✅
- §0.7.1：mihomo TUN（auto-route）+ kill-switch，**同样的 LAN Allow 规则没保住 SSH** ❌

**推测原因**：mihomo TUN 的 auto-route 改变了 SSH 返程的路由/接口归属，使返程包不再命中「物理网卡上 RemoteAddress=192.168.0.0/24」的 Allow（可能被引入 TUN，或 strict-route 类 WFP 注入）。**这是 v2 设计的真实风险**：kill-switch 与 mihomo TUN 的 WFP/路由交互需专门设计，不能直接套用裸机验证过的白名单。

**靠死人开关自愈**：计划任务自动 `taskkill mihomo + 删 KS 规则 + DefaultOutboundAction=NotConfigured + 重启 WG`，SSH 恢复。**这验证了远程实测死人开关机制本身有效**（真用上了，前后两次）。

**尝试过的修法（失败，强化"必须本地"的结论）**：v2.1 又试了一次——给 mihomo 加 `tun.route-exclude-address: [192.168.0.0/24]`（让 SSH 返程绕开 TUN 走以太网）+ kill-switch 白名单补 DNS 服务器（223.5.5.5/119.29.29.29，按 §9#6）。**结果 SSH 仍被切断**。说明 route-exclude LAN 不足以解决——kill-switch 默认 Block 与 mihomo TUN auto-route 的 WFP/路由交互更深层（可能是 strict-route 类 WFP 注入、或 ALE 层对 TUN 设备的处理）。

**结论（已实证，非推测）**：**kill-switch + mihomo TUN 的完整 fail-closed 无法远程 SSH 测试**——试过针对性修法仍切断管理通道。**必须在 0.228 本地控制台做**（有屏幕键盘、不依赖网络通道）。防火墙层 fail-closed 本身已由 §0.4（official WG 引擎）证明且引擎无关，故缺的只是"mihomo TUN 与 kill-switch 共存的部署形态"这一工程细节，留待本地验证。

## 0.8 完整验证总表（v2.1 收官，2026-06-12，0.228 / Win11 23H2）

§9#6 + VP-03 + VP-12 + UDP 收尾结果，并汇总全部 VP 与 §9 项的真机覆盖状态。

### 0.8.1 §9#6 答案（最大未测项，已揭晓）

以太网上 scoped Block 上游 DNS（223.5.5.5/119.29.29.29）后，新域名 www.163.com 经 wg-out 连接**失败** → **mihomo 自身的上游 DNS 解析走物理 NIC 直出（到 223.5.5.5），不经隧道**。

含义（对 v2 设计是硬约束）：
- 系统应用 DNS：被 dns-hijack 拦成 fake-ip，**不泄漏**（§0.6 已证，连显式 8.8.8.8 都拦）。
- **mihomo 内部上游解析：走物理网卡到 223.5.5.5** —— 这既是**一处 DNS 泄漏面**（本地网络可见 mihomo 在查哪些域名），也意味着 **kill-switch 白名单若只放 WG-endpoint，会把 mihomo DNS 拦死、mihomo 整体不可用**。
- **修法（必须二选一写进 §5/§6）**：① kill-switch 白名单显式放行 DNS 服务器 IP（223.5.5.5/119.29.29.29，泄漏面可控）；或 ② 给 wg-out 配 `remote-dns-resolve: true` / 用 `proxy-server-nameserver` 让上游解析走隧道（消除泄漏，但增依赖）。

### 0.8.2 全部 VP / §9 覆盖状态（唯一权威最终结论表）

> **本表是最终结论的唯一权威来源**；正文 §1/§5/§8/§9 等处若与本表冲突，以本表为准（那些是各阶段过程笔记，未必全部回写同步）。
> **证据强度分级**：
> - **✅ 实测·有原始输出**：retrieve 到直接证明结果的命令原文（如出口 IP、mihomo 日志行、超时/拒绝）。
> - **◑ 实测·仅摘要**：脚本真实执行并 retrieve 到 True/False 结果文件，但**支撑性原始证物（路由表/连接日志/网卡计数/防火墙快照）未保留**，复核待 `evidence.ps1` 重跑（0.228 离线阻塞中）。
> - **◔ 声明·未独立取证**：仅文字推断，无证物。
> - **▢ 研究层**：仅文档/源码，未上机。
> - **✗ 未测** / **N/A**。

| 项 | 状态 | 证据 / 说明 |
|---|---|---|
| VP-01 未知流量→海外 | ✅ 真机 | §0.7：mihomo `MATCH,wg-out` → 出口 38.209.122.38(US) |
| VP-02 IP/CIDR 直连 | ◑ 机制覆盖 | 与 VP-03/04 同一规则引擎；未单独取证 |
| VP-03 域名直连 | ✅ 真机 | §0.8：`DOMAIN-SUFFIX,3322.net,DIRECT` → ip.3322.net 出口 58.23.139.139(国内)，api.ipify 仍 US |
| VP-04 程序直连 | ✅ 真机 | §0.7：curl.exe→国内 / powershell→US，同刻双出口 |
| VP-05 子进程发现 | ◑ 部分 | 核心claim已证（mihomo 按**连接自身进程**匹配，不继承父进程——VP-04 中 curl 由 powershell 拉起仍按 curl.exe 匹配）；"UI 提示加入程序组"是 UI 功能未测 |
| VP-06 浏览器风险提示 | N/A | 纯 UI 交互，无后端可测 |
| VP-07 DNS 防泄漏 | ◑ 部分 | 系统 DNS hijack 无泄漏 ✅ 有原始输出（§0.6，显式 8.8.8.8 → fake-ip 198.18.x）；mihomo 上游 DNS 走物理=泄漏面已定位 ✅（§0.8.1，scoped block 后解析失败）。**未确认**：浏览器 DoH-443 绕过（▢ 研究层，0.228 两次取证均失败，§0.8.2bis）、strict-route 完整防泄漏逐包抓包 |
| strict-route=true 可用性 | ✅ 有原始证物 | §0.9.2：strict-route=true 下管理面 LAN ping=True、DNS@8.8.8.8→fake-ip 198.18.0.16、出口 38.209.122.38；防火墙快照+路由表实录 |
| VP-08 引擎崩溃→不泄漏 | ✅ 有原始证物 | §0.9.2：停引擎后 Meta 消失、9.9.9.9:443=False、LAN ping=True；非白名单经物理不可达由网卡 delta+连接表佐证 |
| VP-09 WG 断开→不泄漏 | ✅ 有原始证物 | §0.9.2 同上（停引擎=断 wg-out）；另 §0.4 官方 WG 旁证 |
| VP-10 路由被改→防火墙仍拦 | ✅ 有原始证物 | §0.9.2：路由表显示 9.9.9.9→物理网关 metric1，9.9.9.9:443 仍 False |
| VP-11 重启恢复 | ◑ 实测·仅摘要 | §0.9.1：重启后摘要 OutAction=Block、规则(3)存活、9.9.9.9 仍 False（`vp11_results.txt` 真实 retrieve，但无完整快照；本轮 evidence.ps1 不含重启）。附注：WG 重启后未自启 |
| VP-12 IPv6 泄漏 | N/A | 0.228 无全局 IPv6（地址数0，curl -6 空）→ 无 v6 可泄漏 |
| §9#1 路由环 | ✅ 真机 | §0.7：WG userspace outbound 握手包经 auto-detect 绕TUN，无环，无需手动 route-exclude |
| §9#2 kill-switch 独立性 | ✅ 真机 | §0.4：PersistentStore 规则独立于引擎进程 |
| §9#3 InterfaceType | ✅ 真机 | §0.2/0.3：IfType=53，Wired规则不命中隧道 |
| §9#5 DNS 泄漏(全) | ◑ 部分 | hijack 层✅；mihomo 上游 DNS 泄漏已定位(§0.8.1)；strict-route 完整防护未测 |
| §9#6 mihomo 上游 DNS 路径 | ✅ 真机 | §0.8.1：走物理 NIC，需白名单或 remote-dns-resolve |
| UDP 进程匹配(#1800) | ✅ 真机 | §0.8：`UdpClient` 发 UDP 到 203.0.113.50:12345，日志 `[UDP] ...(powershell.exe) ... match ProcessName(powershell.exe) using DIRECT` → **#1800 在 v1.19.27 已修复**，UDP 也按进程分流 |

### 0.8.2bis 浏览器 DoH 绕过（✅ 真机确认）+ 强杀路由残留（v2.1，2026-06-12）

**浏览器内置 DoH 绕过 dns-hijack — ✅ 真机确认**（curl 模拟 DoH，mihomo TUN+WG 栈运行中）：同一域名 example.com，
- 端口 53 查询（`Resolve-DnsName -Server 8.8.8.8`）→ **198.18.0.12（fake-ip，被 dns-hijack 拦）**
- DoH over 443（`curl https://1.1.1.1/dns-query?name=example.com`）→ **真实 IP 172.66.147.243 / 104.20.23.154**

结论：`dns-hijack: any:53` 只拦端口 53，**DoH-over-443 完全绕过**，浏览器内置 DoH 会拿真实 IP 逃逸 fake-ip。**必须用注册表策略强制关闭浏览器 DoH**（§7.1 Step6）。
> **取证波折（留作教训）**：此结论先后取证 3 次才成——前 2 次失败（① 任务被重启中断；② 我的配置 bug：`rules: [MATCH,wg-out]` 行内数组被 YAML 按逗号拆成 "MATCH"+"wg-out" 两元素，mihomo `rules[0] [MATCH] format invalid` 启动即崩，须块状 `rules:\n  - MATCH,wg-out`）。期间一度把未取到的结果误写成"已确认"，后纠正、重测拿到真值（fake-ip 真值是 198.18.0.12，非早前误填的 .7）。

**⚠️ 强杀 mihomo TUN → 路由残留断网（观察所得，非干净对照）**：fail-closed 测试中用 `Stop-Process -Force` 强杀 mihomo 后远程 SSH 断连、需重启 OS 恢复；而 §0.8.2ter 用 **mihomo API 优雅关 TUN** 则 Meta 干净移除、SSH 不断。两相对照**支持**"强杀不触发 wintun 清理 → 路由残留"（与调研 clash-party #620 一致），但**未做隔离变量的干净对照**（强杀那次还叠加了其他操作），故记为**强观察**而非铁证。
- **运维取向（即便仅为观察也应采纳）**：v2 停 mihomo **优先优雅退出**（API 关 TUN / 不带 `/f`），**避免 `taskkill /f` / `Stop-Process -Force`**。
- 影响 §5 kill-switch 脚本 emergency stop 与运维 runbook。

### 0.8.2ter fail-closed（mihomo 引擎）+ 优雅拆栈（v2.1 终测）

最初用 `Stop-Process -Force` 强杀 mihomo 测 fail-closed 两次锁机（路由残留，§0.8.2bis）。改用**优雅方式**：mihomo API `PATCH /configs {"tun":{"enable":false}}` 关 TUN（mihomo 自己清理 Meta），分离任务 + 死人开关。结果：

| 步 | 实测 | 含义 |
|---|---|---|
| TUN 开，TCP 1.0.0.1:443 | **True** | 经 Meta/WG 可达 |
| API 关 TUN 后 Meta | **False（已移除）** | **优雅拆栈干净、无路由残留、SSH 全程不断** |
| TUN 关 + 物理 block，TCP 443 | **False** | 隧道没了 + 防火墙拦物理 → 不可达 = **fail-closed** ✅ |
| TUN 关 + 去 block，TCP 443 | False（对照偏弱）| 该 IP 国内直连本身没通，未能反证"泄漏" |

**结论**：① fail-closed 以 mihomo 为引擎再次成立（隧道在→可达、隧道断+物理 block→不可达）；核心证据仍是 §0.4（官方 WG 引擎，干净对照），防火墙层引擎无关。② **重要运维结论坐实**：mihomo **优雅退出（API 关 TUN / 不带 /f 的退出）干净清理路由、不断网**；`Stop-Process -Force` / `taskkill /f` 留残留断网（§0.8.2bis）。v2 的 emergency-stop 必须优雅。

### 0.8.3 收尾状态（诚实分级，以 §0.8.2 权威表为准）

§0.9 系列已用 `evidence.ps1` 重跑**补齐原始证物**（§0.9.2 逐行实录：防火墙快照/路由表/mihomo连接表 chains=wg-out/网卡 delta/VP-10/fail-closed）。证据缺口收敛为：
- **完整栈 fail-closed / strict-route / VP-08/09/10 / [C]经隧道非物理**：✅ **有原始证物**（§0.9.2）。
- **VP-11**：◑ 仅摘要（`vp11_results.txt`，本轮 evidence.ps1 不含重启）。
- **浏览器 DoH-443 绕过**：▢ 研究层（0.228 两次取证均失败，§0.8.2bis）。
- **VP-07 逐包抓包**：需 Wireshark；连接表 chains=wg-out + 网卡计数已是接口级证据，逐包级未做。

**收官结论**：v2 数据通路 + 核心机制 + 完整栈 fail-closed + VP-10 + strict-route 均 **✅ 有原始证物**（§0.9.2 / §0.2-0.8）；仅 VP-11（◑摘要）、DoH（▢研究层）、逐包抓包未达最高取证强度，无未知行为风险。三个工程硬约束：① DNS 生产模式 §6 拍板模式 A（允许 bootstrap DNS 物理直连=可接受泄漏面）；② 运维须 detached/service 上下文 + 停 mihomo 必须优雅（§0.8.2bis）；③ 重启后 WG 须 Automatic 自启/app 拉起（§0.9.1）。

---

## 0.9 完整栈 kill-switch + mihomo TUN + strict-route — 真机实测（v2.1 终验，2026-06-14）

> ### 证据强度：✅ 实测·有原始证物（2026-06-14 已补齐）
> 初版只存了 True/False 摘要（被审查指出不可复核）。**已用 `evidence.ps1`（SYSTEM 任务）重跑并落盘完整原始证物**，逐行实录见 **§0.9.2**，含：三 Profile=Block + 5 条 KS 规则的过滤器快照、路由表（默认走 Meta）、**mihomo 连接表 `destIP=9.9.9.9 chains=wg-out`（直接证明经隧道非物理泄漏）**、网卡 before/after delta、VP-10 改路由后仍 False（带路由表）、fail-closed 停引擎后 False+LAN 存活、config.yaml sha256。**[C] 的"经隧道非物理"已由 mihomo 连接表 + 网卡计数证明，不再是"声明·未证"。**

**突破点**：把自包含测试脚本（含死人开关+自动恢复+写文件）作为**脱离 SSH 会话的 SYSTEM 计划任务**运行——设 `DefaultOutboundAction=Block` 只掐断我的交互 SSH，但 SYSTEM 任务独立跑完、写结果、自恢复，我再重连读文件。**这绕开了"远程设 Block 即自锁"的限制**。（注意：这同时说明 §5 的"禁止远程执行"应精确为"禁止在将被掐断的交互会话内执行；可用 detached/service 上下文"。）

**配置**：mihomo TUN(gvisor) + WG userspace outbound + `strict-route: true` + kill-switch（默认 Block + 白名单 LAN/WGep/Meta/loopback/R-DNS 五条）。结果：

| 项 | 实测 | 判定 |
|---|---|---|
| mihomo 起栈 | mihomo=True, Meta=True, OutAction=Block, KS 5 条 | ✅ |
| **[A] 管理通道(LAN) ping** | **True**（kill-switch+TUN 下 LAN 出站存活） | ✅ **管理面存活** |
| [B] 经 WG 访问国内 baidu:443 | True | ✅ WG 路径通 |
| [C] 非白名单 9.9.9.9:443 | True（经隧道 Meta→wg-out 可达，**非物理泄漏**） | ✅ 正确行为 |
| [D] 出口 IP | 38.209.122.38(US) | ✅ |
| [E] strict-route 下 DNS(查 8.8.8.8) | 198.18.0.11(fake-ip) | ✅ **strict-route + dns-hijack 生效** |
| **[VP-10] 改路由强制走物理后 9.9.9.9:443** | **False** | ✅ **VP-10 PASS**：防火墙拦截独立于路由表 |
| **[fail-closed] 停引擎后 9.9.9.9:443** | **False** | ✅ **fail-closed PASS**（mihomo 完整栈） |
| [fail-closed] 停引擎后 LAN ping | True | ✅ 引擎死，管理面仍活 |
| 自动恢复 | WG=Running, OutAction=NotConfigured | ✅ 干净 |

**关键结论（修正 §0.7.1）**：
1. **kill-switch + mihomo TUN + strict-route=true 三者共存可行，且管理面（LAN）连通性由 LAN 白名单规则保住**。§0.7.1 记的"会切 SSH"**真因是脚本跑在那条会被掐的交互 SSH 会话里**（设 Block 时承载脚本的 established SSH 连接被 WFP 重评估掐断），**不是这个组合本身切管理面**——新建的 LAN 连接（如重连 SSH/RDP）在白名单下正常。**部署/运维须从 service/detached 上下文操作，勿在"将被掐断的那条会话"里执行。**
2. **VP-10 PASS**：把 9.9.9.9 路由强制改到物理网关后，防火墙仍拦（独立于路由表）。
3. **fail-closed PASS（mihomo 完整栈）**：优雅停引擎后，非白名单经物理不可达、管理面仍活。
4. **strict-route=true 可用**：SSH 安全（脱离会话）、dns-hijack 在 strict-route 下照常返回 fake-ip。

**[C] 说明**：kill-switch 开启时 9.9.9.9 仍 True，是因为全量隧道下它经 Meta→wg-out 出 US，**不是物理泄漏**；fail-closed 的判定看"停引擎后"那行（=False）。

## 0.9.1 VP-11 受控重启持久性 — 真机实测（v2.1 终验，2026-06-14）

用 §0.9 同款脱离会话手法：vp11.ps1（SYSTEM 任务）设持久 Block 规则 + 注册开机自检任务 + `Restart-Computer -Force`；机器重启 → 开机任务自检后自动恢复。结果：

| 项 | 重启后实测 | 判定 |
|---|---|---|
| DefaultOutboundAction | **Block**（重启后仍是） | ✅ Profile 默认动作跨重启持久 |
| KS 规则数 | **3**（仍存活） | ✅ PersistentStore 规则跨重启持久 |
| 非白名单 9.9.9.9:443 | **False**（重启后仍被拦） | ✅ **fail-closed 跨重启维持**——zero-desktop 启动前就已生效 |
| 开机自检后自动恢复 | OutAction→NotConfigured，最终 WG=Running、US 出口 | ✅ 自愈干净 |

**VP-11 PASS**：PersistentStore Block 规则 + DefaultOutboundAction 跨重启持久，**重启后到 zero-desktop 启动前的窗口内 fail-closed 仍生效**（满足 §14.9 VP-11 要求）。

**附带发现（需落到工程）**：重启后约 30s 时 **官方 WG 隧道服务未自动启动**（WG 状态为空，靠开机任务显式 `Start-Service` 才起）。生产中须：把隧道服务设为 Automatic 启动，或由 zero-desktop 启动流程拉起 + 重写 WG endpoint host route（路由表不持久）。**这与 fail-closed 不冲突**——隧道没起时默认 Block 仍拦截，不泄漏。

## 0.9.2 原始证物实录（fullstack_evidence.txt，2026-06-14 20:30，0.228）

以下为 `evidence.ps1`（NT AUTHORITY\SYSTEM 脱离会话任务）实际产出 `C:\Users\36225\fullstack_evidence.txt` 的逐行实录（"以太网"原文 mojibake 已还原），可复核：

```
host=feng  exec-context=NT AUTHORITY\SYSTEM
mihomo_ver=Mihomo Meta v1.19.27 windows amd64 with go1.26.4
config.yaml sha256=137B5A03DFB6708E8F4CCAB6478E0AA9717B52BA74588F68BA323A6061D2F85A

[BEFORE] 默认路由: ifIndex14 以太网 NextHop 192.168.0.1
         适配器: 以太网=IfType6 Up / WLAN=71 Disconnected / wg-0228=53 Up
mihomo started: pid=11704 Meta=True

[防火墙快照] DefaultOutboundAction: Domain=Block Private=Block Public=Block
  KS-LAN  | Outbound Allow | Remote=192.168.0.0/255.255.255.0 | If=Any
  KS-WGep | Outbound Allow | Remote=38.209.122.38             | If=Any
  KS-TUN  | Outbound Allow | Remote=Any                       | If=Meta
  KS-LO   | Outbound Allow | Remote=127.0.0.0/255.0.0.0       | If=Any
  KS-DNS  | Outbound Allow | Remote=223.5.5.5,119.29.29.29 UDP:53 | If=以太网

[PROOF 9.9.9.9 经隧道] 默认路由: ifIndex7 Meta NextHop 198.18.0.2 / ifIndex14 以太网 192.168.0.1
  [NIC before] Meta sent=1488758 recv=71004 | 以太网 sent=3020318 recv=3783825
  [mihomo conn] host= destIP=9.9.9.9 net=tcp rule=Match chains=wg-out   <<< 经 wg-out 隧道
  [NIC after]  Meta sent=1503711 recv=107825 | 以太网 sent=3048657 recv=3830443
  [NIC delta]  Meta sent+=14953 | 以太网 sent+=28339   <<< Meta 承载应用流, 以太网承载WG封装

[连通性] [A]LAN ping=True [B]baidu443=True [C]9.9.9.9:443=True(经隧道) [D]出口IP=38.209.122.38 [E]DNS@8.8.8.8=198.18.0.16(fake-ip)

[VP-10] 加路由 9.9.9.9 -> 以太网/192.168.0.1 metric1 后: 9.9.9.9:443 = False (仍被拦)
[fail-closed] 优雅停引擎: Meta present=False; 9.9.9.9:443=False; LAN gw ping=True
[RESTORED] WG=Running OutAction=NotConfigured Meta=False
```

**逐项判定（均有上方原始证物支撑）**：
- **完整栈 kill-switch + mihomo TUN + strict-route=true 共存、管理面存活**：✅（防火墙快照 + [A] LAN ping True + 自动恢复）
- **9.9.9.9 经隧道非物理泄漏**：✅（mihomo 连接表 `chains=wg-out` + 网卡 delta：Meta 与以太网均增，以太网增量更大=WG 封装载体）
- **strict-route=true 下 dns-hijack**：✅（[E] 查 8.8.8.8 → fake-ip 198.18.0.16）
- **VP-10 改路由后仍拦**：✅（路由表显示 9.9.9.9→物理网关 metric1，仍 False）
- **fail-closed**：✅（停引擎后 9.9.9.9=False、管理面 LAN 仍 True）

**仍存的小缺口（已诚实标注，不夸大）**：① out.log 段为空（schtask 不捕获 stdout；连接表 API 已替代为更强证据）；② 抓包级（Wireshark）逐包取证仍未做，但连接表 `chains=wg-out` + 网卡计数已是接口级证据；③ VP-11（§0.9.1）的证物仍仅 `vp11_results.txt` 摘要（本轮 evidence.ps1 不含重启）。

## 0.10 实现产物真机验证（2026-06-15，0.228）

net-policy 模块已落地到 `crates/zero-desktop/src/modules/net_policy/`（见代码）。为验证**模块生成的产物**在真机有效（而非只验设计），加了 CLI `zero-desktop net-policy-gen --what config|firewall` 预览真实 Rust 代码生成的 mihomo 配置 / 防火墙脚本，并把生成物部署到 0.228 跑：

- **mihomo 配置（Rust `engine::generate_config` 产出）真机端到端通过**（用 0.228 的 peer .5，停官方 WG 后纯 mihomo 路由）：
  - mihomo TUN 起栈 `Meta=True`；`mihomo -t` 配置校验通过。
  - `[VP-01] 默认出口 = 38.209.122.38（US）`——`MATCH,wg-out` 走 WG 海外出口 ✅
  - `[VP-03] ip.3322.net = 58.23.139.139（国内）`——`DOMAIN-SUFFIX,3322.net,DIRECT` 分流生效 ✅
  - mihomo 日志佐证：`match DomainSuffix(3322.net) using DIRECT`（powershell + curl 两连接）。
- **防火墙脚本（Rust `firewall::build_apply_script` 产出）**：文本与 §0.9.2 实测过的白名单一致（R-TUN/R-LO/R-WGep/R-LAN/**R-DNS** + 默认 Block），行为由 §0.9.2 覆盖。
- **改进（测试中学到）**：`to_mihomo_rules()` 原用 `GEOIP,private,DIRECT`，依赖 geoip 数据库（fresh 机器无则 mihomo 去 GitHub 下载、国内慢/失败）；已改为**显式 IP-CIDR 私网段**，去掉 geoip 依赖。
- **未在真机 app 内（GUI）端到端跑**：本次测的是模块生成的产物（config + 防火墙脚本），用 Start-Process 起 mihomo（= `engine::start` 的 `std::process::Command::spawn` 等价路径，已证能起 TUN）。完整「kill-switch + TUN」远程 SYSTEM-task 组合受 §0.9.2 同款约束（远程设 Block 自锁 / SYSTEM-task 起 TUN 限制），其防火墙 fail-closed 行为由 §0.9.2 证明；GUI app 内无 SSH 自锁问题，待装包后本机跑通。

### 0.10.1 v2.2 代码审查修复（2026-06-15，⚠️ 防火墙模型变更需重新真机验证）

一轮代码审查后修了 7 个问题（P0×2/P1×4/P2×1，见代码 commit）。其中**改变了已验证行为的一处**：
- **防火墙白名单从「RemoteAddress 列表」改为「`Program=mihomo.exe` 物理放行」**（修 P1-1：原 RemoteAddress 白名单下，域名/程序命中 DIRECT 后 mihomo 替其拨号的目标 IP 不在白名单 → 被默认 Block 拦死，"按程序/域名走本地"失效）。新模型：放行 mihomo 进程出物理网卡（覆盖 WG 握手 / 上游 DNS / DIRECT 拨号），mihomo 崩溃 → 进程没了 → 此规则不匹配 → 物理全 Block，**fail-closed 不破**。
- **此新白名单 §0.9.2 未测过**（§0.9.2 用的是 RemoteAddress 白名单）。新模型推理正确、产物已用 CLI 核对（KS-mihomo / KS-IPv6Block 生成正确），但**真机有效性 + fail-closed 仍待重新验证**（受同款远程约束，待 GUI app 或本地控制台）。**这是进生产前的阻塞项。**

其余修复（不改已验证行为）：kill-switch 默认开启 + "不受保护预览"模式标注（P0-1）；apply 事务化回滚（P0-2）；emergency_stop 先停引擎再撤防火墙（P1-2）；输入严格校验防注入（P1-3，已测非法值被拒）；block_ipv6 落实为显式 Block 2000::/3（P2-1）；按 pid 停 mihomo（P2-2）。

### 0.10.2 v2.3 第二轮代码审查修复（2026-06-15）

- **P0-1 controller 鉴权**：external-controller 原 `secret: ""` + KS-mihomo 放行 mihomo 全量物理出站 → 同用户任意进程可打 API 改 `MATCH,DIRECT`/关 TUN 致泄漏。修：每次 apply **随机生成 controller secret**（`engine::gen_secret`，24 字节 hex），写进 config 的 `secret:`，所有 API 调用（running/graceful_stop/verify）带 `Authorization: Bearer`。secret 存 Runtime，随 emergency_stop 清。
- **P0-2 不把未验证模型当 fail-closed**：新增 `FIREWALL_MODEL_VALIDATED=false` 常量 + status `protection_validated` 字段；新 Program 白名单模型未真机复测前，UI 显示「**实验保护（待真机验证）**」而非「受保护 fail-closed」。
- **P1-1 优雅停验证 Meta 拆除**：`graceful_stop` 改为 API 关 TUN 后**轮询确认 Meta 适配器消失**（≤7s）再按 pid 杀；未消失则 **bail（不强杀）**，emergency_stop 据此保持防火墙生效，杜绝"强杀残留路由"泄漏/断网。
- **P1-2 apply 无保护窗口**：拆两阶段——**先 `apply_base`（不依赖 Meta 的白名单 + 默认 Block，fail-closed 先就位）再起 mihomo**，mihomo 起栈、Meta 出现后再 `apply_tun` 补 KS-TUN。期间 app→Meta 被默认 Block 拦（不泄漏，仅短暂不通）。
- **P2 路径校验放宽**：`process_path/name` 改用 `no_inject_path` 允许括号/方括号/空格（`C:\Program Files (x86)\…` 可用），仍拦换行/逗号/引号/反引号/`;|&$<>`。

均编译全绿（clippy -D warnings / fmt / 前端 build），CLI 产物核对正确。**新防火墙模型真机复测仍是进生产前阻塞项（§0.10.1）。**

---

## 1. 关键判断（先说结论）

- **方案 B 整体可行**：mihomo TUN + WireGuard userspace outbound + Windows 防火墙兜底的组合，可以在技术上满足"未知流量默认海外、fail-closed、按程序管理"三大约束；但每一层都有已知 bug，必须按本报告的 workaround 清单逐条处置才能稳定落地。
- ~~**最大风险点是路由环**~~ → **实测推翻（§0.7）**：mihomo TUN + WG userspace outbound 真机跑通，握手 UDP 经 `auto-detect-interface` 自动绕 TUN，**未出现路由环，无需手动 host route / route-exclude**。原 v2 底稿这条判断作废。（DIRECT 与 WG outbound 两形态均未现环）
- **首版可能不需要自研 WFP（条件性）**：Windows Firewall（New-NetFirewallRule）PersistentStore 规则与进程生命周期解耦，承担 fail-closed——防火墙机制已由 §0.4（官方 WG 引擎，✅有原始输出）证明；mihomo 完整栈 + 全局 Block 的 fail-closed 已在 §0.9 跑出通过摘要（**◑ 实测·仅摘要，原始证物待 `evidence.ps1` 重跑补全，0.228 离线阻塞**）。**结论以 §0.8.2 权威表为准**。进生产前仍须补齐 §0.9 原始证物 + 抓包级 DNS 取证。
- **IPv6 首版建议彻底阻断**：mihomo TUN 的 ipv6:false 选项不能真正禁用 IPv6（issue #2254），WSL 环境下 Disable-NetAdapterBinding 重启后会被恢复；首版最安全策略是物理适配器层禁用 IPv6 绑定 + 防火墙规则双保险阻断 2000::/3 出站，双栈支持推迟到二期实测后再决策。
- **kill-switch 形态（v2 修订）**：Windows 防火墙采用「**Profile 默认 Outbound=Block + InterfaceAlias 白名单 Allow（TUN/loopback/WG endpoint/LAN/DHCP/NDP）**」模式——v1 的「Allow + Block 兜底」结构在 New-NetFirewallRule 层不工作（Block 永远赢 Allow，[MS 文档](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/rules)）。规则以组（-Group NetPolicy-KillSwitch）批量管理；写入 PersistentStore，独立于所有用户态进程，mihomo 和 WireGuard 崩溃后自动维持生效。InterfaceAlias 白名单比 InterfaceType 黑名单更稳——后者依赖未公开的 IF_TYPE → FW_INTERFACE_TYPE 映射。
- **DNS 防泄漏：放弃 NRPT、依赖 TUN dns-hijack + strict-route（v2 修订）**：v1 的 NRPT 路径有致命缺陷——NRPT API 不支持端口（[WMI MOF schema](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/ramgmtpsprov/dnsclientnrptrule) 19 属性无 port 字段），而 mihomo `dns.listen: 127.0.0.1:1053`，NRPT 重定向打到 `:53` 全超时。v2 改用 mihomo TUN 模式的 `dns-hijack: any:53 + tcp://any:53 + strict-route: true`，TUN 在虚拟网卡层拦截所有 53 流量，根本不需要 NRPT。fake-ip + direct-nameserver UDP（避开 Discussion #1656 bug）仍保留；Chrome/Edge 内置 DoH 必须通过注册表策略强制关闭。
- **不使用官方 WireGuard 客户端作为主路径**：官方客户端 kill-switch 的 WFP 规则在隧道断开后即销毁（存在泄漏窗口），/0 以外不触发，且与 mihomo strict-route 并存时 WFP 规则冲突待确认；首版推荐 mihomo 内置 userspace WG outbound，不启动官方 WG 客户端作为主路径（官方客户端可保留用于监控）。

---

## 2. 候选实现栈推荐

### 推荐栈

| 层次 | 推荐选项 | 备注 |
|---|---|---|
| 流量接管 | mihomo TUN，stack=mixed | TCP 走 system 栈（性能），UDP 走 gvisor（稳定）；Windows 11 26200 上 system stack 稳定性待实测 |
| WireGuard 出口 | mihomo 内置 userspace WG outbound | 不使用官方 WG 客户端作为主路径；userspace 实现无驱动冲突，不额外写 WFP |
| DNS | fake-ip + TUN dns-hijack 53 + strict-route（**v2 改**）| 不用 NRPT（API 不支持非 53 端口，与 mihomo :1053 监听冲突）；redir-host 在 CDN/多 A 记录场景易失配，不选 |
| 防火墙兜底 | PowerShell：**Set-NetFirewallProfile -DefaultOutboundAction Block + InterfaceAlias 白名单 Allow**（**v2 改**） | v1 的「Allow + Block 兜底」在 Windows Firewall 不工作（Block 永远赢 Allow）；改成默认 Block 后只写 Allow 白名单，无 Block 规则。kill-switch 启停=批量 Enable/Disable 白名单组 |
| IPv6 | 物理适配器 Disable-NetAdapterBinding + 防火墙 Block 2000::/3 | 双栈支持推迟二期 |
| 进程规则 | PROCESS-NAME + PROCESS-PATH，find-process-mode: always | UDP 匹配 issue #1800 需实测确认是否已修复 |

### 取舍说明

**WireGuard userspace 而非官方客户端**：官方客户端的 kill-switch WFP 规则随会话生命周期绑定，断开后即销毁，存在秒级泄漏窗口；与 mihomo strict-route 并存时两套 WFP 规则叠加的交互行为尚未实测确认。mihomo userspace WG outbound 不写系统路由表也不写 WFP，只占用一个 UDP socket 出站，架构更简洁，fail-closed 完全由独立 Windows 防火墙层承担。

**fake-ip 而非 redir-host**：fake-ip 模式下 DNS 响应返回虚拟 IP（198.18.x.x），实际解析在代理端完成，本机不发真实查询，彻底阻断 DNS 泄漏路径；redir-host 依赖本机真实解析，CDN 多 A 记录和 IPv6 泄漏风险更高。

**v2：放弃 NRPT、依赖 TUN dns-hijack**：Add-DnsClientNrptRule -NameServers 参数不支持 `IP:port` 格式（已验证：[WMI MOF schema](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/ramgmtpsprov/dnsclientnrptrule) 19 个属性无端口字段），mihomo 监听 :1053 与 NRPT 重定向 :53 必然冲突。改用 mihomo TUN 的 `dns-hijack: any:53 + strict-route` 即可拦截所有 DNS（含 SMHNR 绕路），无需 NRPT 介入；附带好处是配置面更小。代价：依赖 TUN 模式（v2 推荐栈已默认开启）。

**PowerShell 默认 Block + 白名单（v2 修订）**：v1「Allow 白名单 + Block 兜底」结构在 New-NetFirewallRule 层不工作——[MS 官方文档](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/rules) 三条规则明确：(1) Allow 优于默认 Block 设置；(2) **Block 优于任何 Allow**；(3) 更具体规则优于通用规则，**但 Block 例外**。即使具体 Allow + 通用 Block，Block 仍赢。绕过仅有 -OverrideBlockRules（需 IPsec 双向认证基础设施，本地 kill-switch 不适用）。v2 改为：`Set-NetFirewallProfile -DefaultOutboundAction Block` 把默认行为翻转到 Block，再用具体 Allow 规则白名单——此时没有显式 Block 规则，不存在冲突。kill-switch 通过 `Set-NetFirewallProfile -DefaultOutboundAction Allow` 或批量 disable 白名单组实现状态切换。

**InterfaceAlias 而非 InterfaceType（v2 修订）**：v1 用 `-InterfaceType Wired,Wireless` 兜底 Block 依赖一个未公开假设——Wintun 适配器（IfType=53 IF_TYPE_PROP_VIRTUAL, MediaType=19 NdisMediumIP）不被 Windows Firewall 归入"Wired"分桶。证据虽强（[Wintun INF](https://github.com/WireGuard/wintun/blob/master/driver/wintun.inf)；VirtualBox 社区有意改 IfType=53 避开 LAN 分类；无任何 TUN 被 Wired 规则误命中报告），但 [Win32 FW API 文档](https://learn.microsoft.com/en-us/windows/win32/api/netfw/nf-netfw-inetfwrule-get_interfacetypes) 从未公开 IF_TYPE → FW_INTERFACE_TYPE 映射表。v2 改用 InterfaceAlias 白名单（按 `Get-NetAdapter` 查到的名字精确放行 TUN 适配器和 loopback），完全消除分类不确定性。

**mixed stack 而非纯 gvisor**：gvisor 稳定但性能较低；mixed 模式 TCP 走 system、UDP 走 gvisor，已知 Windows Defender Firewall 启用时 system/mixed 栈需要放行 mihomo.exe，否则需切 gvisor；需实测在 Windows 11 26200 上的稳定性。

---

## 3. 各主题研究摘要

### 3.1 WireGuard 官方客户端行为（wg-official-client）

**结论**：官方客户端 kill-switch 存在确定性泄漏窗口，不能作为 fail-closed 的唯一保障；本设计方案 B 不依赖官方客户端 WFP，独立的 Windows 防火墙规则层是必须的。

**关键发现**：
- AllowedIPs = 0.0.0.0/0 才触发 WFP kill-switch；0.0.0.0/1 + 128.0.0.0/1 绕过此逻辑，但 kill-switch 也不激活。来源：[netquirk.md](https://git.zx2c4.com/wireguard-windows/about/docs/netquirk.md)
- kill-switch WFP 规则随 WFP 会话绑定，DisableFirewall() 关闭 fwpmEngineClose0() 后即销毁；断开→重连期间存在秒级泄漏窗口。来源：[blocker.go](https://github.com/WireGuard/wireguard-windows/blob/master/tunnel/firewall/blocker.go)
- 0.0.0.0/0 模式下 WFP 限制 port 53 只到指定 DNS 服务器；非 /0 场景 DNS 走标准多宿主行为，存在泄漏风险。来源：netquirk.md
- mihomo TUN auto-route + WG userspace outbound 共存时，WG peer endpoint UDP 被 TUN 重捕获形成路由环；route-exclude-address /32 在 Windows 下无效（issue #2617/#2618，标记 not planned）。

**待实测项**：
- 官方 WG 客户端 WFP sublayer GUID 与 mihomo strict-route WFP sublayer 是否冲突（需 netsh wfp show state 对比）
- WG endpoint host route（New-NetRoute /32 指向物理网关）能否可靠防止路由环
- 断开→重连的泄漏窗口精确时长（毫秒级）

---

### 3.2 mihomo TUN + WireGuard outbound（mihomo-tun-wg）

**结论**：mihomo TUN + userspace WG outbound 是技术可行路径，核心路由环风险已被多个 issue 坐实但有可靠 workaround；mihomo 崩溃后 wintun 适配器和路由残留，fail-closed 必须依赖独立 Windows 防火墙而非 TUN 本身。

**关键发现**：
- mihomo TUN 在 Windows 使用 wintun 驱动，默认适配器名 "Meta"，stack 三选一（system/gvisor/mixed）。来源：[mihomo TUN 文档](https://wiki.metacubex.one/en/config/inbound/tun/)
- WG outbound 是纯 userspace 实现（wireguard-go），与官方 WireGuardNT 内核实现无驱动冲突，但性能有差距。
- TUN auto-route + WG outbound 路由环：mihomo issue #1728；route-exclude-address /32 bug：issue #2617（closed as not planned）。来源：[#2617](https://github.com/MetaCubeX/mihomo/issues/2617)
- dialer-proxy 字段可让 WG 握手流量先经另一个 proxy，可作为路由环备用方案。来源：[WG outbound 文档](https://wiki.metacubex.one/en/config/proxies/wg/)
- mihomo 进程 crash 后 wintun 适配器不会自动清理（WintunCloseAdapter 未被调用），路由条目残留。来源：[clash-party issue #620](https://github.com/mihomo-party-org/clash-party/issues/620)
- strict-route=true 在 Windows 添加 WFP 防火墙规则阻断多宿主 DNS 泄漏，但副作用可能影响 VirtualBox。

**待实测项**：
- route-exclude-address /32 bug 在 mihomo 最新版（v1.19.x+）是否已修复
- mixed stack 在 Windows 11 26200 稳定性
- mihomo 崩溃后独立防火墙规则是否能阻断物理网卡出站（VP-08 核心验证项）

---

### 3.3 进程规则（process-rules）

**结论**：PROCESS-NAME / PROCESS-PATH 作为直连白名单在 Windows TUN 模式下有效，大小写不敏感，但路径分隔符不互通；UDP 匹配存在已知 bug（issue #1800）需实测；子进程不继承父进程规则，必须显式枚举；进程查找失败时流量由 MATCH→WireGuard 兜底，不会泄漏。

**关键发现**：
- 底层 API：GetExtendedTcpTable / GetExtendedUdpTable + QueryFullProcessImageNameW；PROCESS-NAME 匹配 filepath.Base，PROCESS-PATH 匹配全路径；strings.EqualFold 大小写不敏感，但 \ 和 / 不互通。来源：[process_windows.go](https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/component/process/process_windows.go)
- UDP 匹配 bug：Windows 11 Alpha 版本实测 UDP 流量绕过 PROCESS-NAME 规则命中 MATCH（issue #1800，已关闭无修复记录）。来源：[#1800](https://github.com/MetaCubeX/mihomo/issues/1800)
- 进程查找失败（ErrNotFound）时流量由后续规则（MATCH→WireGuard）接管，符合 fail-closed 设计意图。
- find-process-mode: always 强制每连接查找，但 issue #322 报告 2023 年版本有 3 秒延迟，需确认当前版本是否有缓存。
- Chrome/Edge 网络请求由 Network Service 进程（chrome.exe）发出；Tauri 程序后端为 Rust 进程，进程名稳定。

**待实测项**：
- UDP 进程匹配 bug 在最新稳定版是否已修复（iperf3 -u 实测）
- Chrome Network Service 进程在 Windows 11 实机的可执行文件名
- Tauri zero-desktop 的 WebView2 子进程（msedgewebview2.exe）是否发网络请求
- find-process-mode: always 在当前版本的实际 CPU 开销

---

### 3.4 DNS 防泄漏（dns）

**结论（v1 原结论 + v2 校正）**：Windows SMHNR 是 DNS 泄漏首要障碍。**v1 原推荐的 NRPT 路径已在 v2 弃用**（NRPT API 不支持非 53 端口，与 mihomo `dns.listen:1053` 不兼容）；v2 改用 **mihomo TUN `dns-hijack: any:53 + strict-route: true`**，在 TUN 层拦截所有 53 流量（含 SMHNR 多宿主发往物理 NIC 路由器的查询）。fake-ip + direct-nameserver（仅 UDP）保留；浏览器内置 DoH 必须通过注册表策略强制关闭；防火墙默认 Block 自动覆盖系统级 DoH 回落（物理 NIC 上无 53/853 Allow 规则）。下方「关键发现」中关于 NRPT 的内容保留作历史背景，请按 v2 结论部署。

**关键发现**：
- SMHNR 默认把 DNS 查询并发发到所有接口。**v1 推荐 NRPT 覆盖（已弃用）**：`Add-DnsClientNrptRule -Namespace "." -NameServers "127.0.0.1"`——但此 API 不接受端口，只能打到 :53，与 mihomo :1053 冲突。**v2 推荐 TUN dns-hijack**：strict-route=true 时 mihomo TUN 在虚拟网卡层拦截所有 53 出站，包括 SMHNR 绕路；不需要 NRPT。来源：[SANS 40165](https://www.sans.org/reading-room/whitepapers/dns/preventing-windows-10-smhnr-dns-leakage-40165)、[mihomo strict-route 文档](https://wiki.metacubex.one/en/config/inbound/tun/)
- direct-nameserver + DoH/DoT 组合有已知 bug（UDP 路径不使用 direct-nameserver，Discussion #1656）；workaround：direct-nameserver 只配 UDP（如 223.5.5.5）。来源：[Discussion #1656](https://github.com/MetaCubeX/mihomo/discussions/1656)
- Chrome DoH 走 HTTPS（TCP 443），TUN 劫持 port 53 无法阻断；必须通过注册表 HKLM\SOFTWARE\Policies\Google\Chrome\DnsOverHttpsMode = "off" 强制关闭。
- Firefox 检测到 NRPT 存在时自动禁用内置 DoH（Mozilla Heuristics）——v2 不用 NRPT 后这条不再适用，必须显式用 policies.json 关 DoH。来源：[Mozilla Heuristics](https://wiki.mozilla.org/Security/DNS_Over_HTTPS/Heuristics)
- Windows 11 系统级 DoH 约 2.5s 超时后回落明文 UDP 53。v1 提议防火墙阻断物理网卡 53/853 出站兜底；v2 由 Profile 默认 Block 自动覆盖（白名单中无 53/853 Allow）。

**待实测项（v2 更新）**：
- direct-nameserver + UDP 组合在最新 mihomo 版本是否真正修复（抓 TUN 接口 DNS 包确认）
- **mihomo 进程自身上游 DNS 查询路径（v2 新增高优）**：mihomo 解析 `nameserver: https://1.1.1.1/dns-query`（DoH 走 TCP 443）和 `direct-nameserver: 223.5.5.5`（UDP 53）时，这些 socket 经 TUN 出（再回到 mihomo 自身被 WG outbound 处理？）还是直接经物理 NIC？默认 Block 模式下若经物理 NIC，则 mihomo 上游 DNS 完全失败 → mihomo 整体不可用。需抓包确认；可能需要在 §5 白名单加一条「Program=mihomo.exe + InterfaceAlias=$PhysicalAlias Allow」，但这又是潜在 DNS 泄漏面（mihomo 可向任意 IP 发 53/443）。详见 §9 高优问题 #6。
- TUN dns-hijack + strict-route 是否完全覆盖 SMHNR 多宿主泄漏（v2 替代 NRPT 后必须确认）
- Edge DnsOverHttpsMode 注册表路径确认

---

### 3.5 IPv6 泄漏防护（ipv6）

**结论**：首版建议物理适配器层彻底阻断 IPv6（Disable-NetAdapterBinding + 防火墙 Block 2000::/3 双保险）；mihomo ipv6:false 不够用（issue #2254），DisabledComponents=0xFF 需重启且有副作用；双栈支持推迟二期。

**关键发现**：
- WireGuard 仅配 AllowedIPs=0.0.0.0/0（不含 ::/0）时，IPv6 完全绕过隧道走物理网卡——确定性泄漏。
- mihomo TUN ipv6:false 仅移除 IPv6 网关路由，IPv6 DNS 服务仍活跃（issue #2254，2025-09 未修复）。来源：[#2254](https://github.com/MetaCubeX/mihomo/issues/2254)
- mihomo strict-route 未开启时，Windows SMHNR 向物理网卡路由器发 IPv6 DNS 查询绕过 TUN 劫持（issue #854）。
- Disable-NetAdapterBinding -Name '*' -ComponentID ms_tcpip6 在安装 WSL 的 Windows 11 上重启后可能被 WSL 恢复；防火墙规则作为持久兜底更可靠。
- 6to4/Teredo/ISATAP 过渡隧道技术可能绕过 IPv6 阻断，必须显式禁用（netsh interface teredo set state disabled 等）。

**待实测项**：
- issue #2254 在最新 mihomo 版本是否已修复
- Disable-NetAdapterBinding 在 Windows 11 26200 + WSL 环境重启后是否被恢复
- 防火墙 Block 2000::/3 是否足以阻止所有 IPv6 公网泄漏（ICMPv6 NDP 例外是否适当）

---

### 3.6 防火墙 kill-switch（firewall-killswitch）

**结论（v1 原结论 + v2 校正）**：Windows 防火墙 PowerShell API 足以构建持久 fail-closed kill-switch，规则以 WFP filter 持久存储、与用户态进程解耦——但 **v1 的「Allow 白名单 + Block 兜底」结构在 New-NetFirewallRule 层不工作**（Block 永远赢 Allow，[MS rules](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/rules)），v2 改为「**Set-NetFirewallProfile DefaultOutboundAction=Block + 纯 Allow 白名单**」。v1 推荐 InterfaceType=Wired,Wireless「比 InterfaceAlias 更稳健」的论断**在 v2 被否决**——它依赖未公开的 IF_TYPE → FW_INTERFACE_TYPE 映射假设；v2 改用 InterfaceAlias 精确白名单（部署脚本接受 TunAlias / PhysicalAlias 数组，多网卡场景手动列出）。

**关键发现（保留作背景，v2 已据此重构 §5）**：
- Block 规则优先于 Allow，WFP 子层仲裁"任何 Block 击败所有 Permit"——**v2 据此放弃显式 Block 兜底，改用 Profile 默认 Block**。来源：[Tailscale 博客](https://tailscale.com/blog/windows-firewall)
- PersistentStore 规则在 TUN/WireGuard 进程崩溃、TUN 适配器消失后独立维持生效（**仍待 VP-08 实测确认 v2 模型也有此性质**）。
- ~~InterfaceType=Wired,Wireless 比 InterfaceAlias 更稳健~~ → v2 否决，原因见上。InterfaceAlias 需要在网卡切换时手动重配，但可观测性更强、无未公开假设；网卡切换场景由实测者按需补 Allow 规则。
- WireGuard 内置 kill-switch WFP filter 与外层 Windows Firewall Block 规则在各自子层并联，不冲突——v2 不使用官方 WG 客户端，此条不再适用。
- IPv6 loopback（::1）无法通过 Windows Firewall API 用 RemoteAddress=::1 设置——**v2.1 实测坐实**（§0.4：`New-NetFirewallRule -RemoteAddress ::1` 报"未指定/多播/广播或环回 IPv6 地址"）。故 §5.2 R2 **只放行 IPv4 `127.0.0.0/8`**，丢弃 ::1（loopback 本就 WFP 豁免）。
- mihomo strict-route 自己注入的 WFP 规则在 mihomo 崩溃后消失——v2 的 Profile 默认 Block 与此独立，不受 mihomo 状态影响（待 VP-08 实测确认）。

**待实测项**：
- InterfaceType=Wired,Wireless 是否能可靠区分 Wintun 虚拟适配器与物理 NIC（需 Get-NetAdapter 确认 Wintun 的 InterfaceType 属性值）
- NLA 重新评估 network profile 时是否会导致 kill-switch 规则短暂失效（Windows Server 2025 已有相关 bug 报告）
- WireGuard 接口 GUID 重建后孤立 WFP filter 是否残留

---

## 4. VP-01 ~ VP-12 实测前预期

| 编号 | 用例 | 预期观察值 | 观察命令 | 风险点 |
|---|---|---|---|---|
| VP-01 | 未知流量默认海外 | 公网 IP 检测返回 WG peer 出口 IP；Wireshark 物理网卡无非白名单公网 TCP/UDP | `(Invoke-RestMethod https://api64.ipify.org?format=json).ip`；Wireshark 物理网卡过滤（**v2 修正**）：`ip.dst != <wg-endpoint> and !(ip.dst == 10.0.0.0/8 or ip.dst == 192.168.0.0/16 or ip.dst == 172.16.0.0/12 or ip.dst == 169.254.0.0/16 or ip.dst == 224.0.0.0/4)` —— Wireshark 用 `or` 关键字、`a.b.c.d/N` 子网语法，不要用正则 | WG endpoint host route 未写导致路由环；TUN 接管失败 |
| VP-02 | 手动 IP 直连 | 配置测试 CIDR 为 DIRECT，访问该 IP 返回本地出口；其他公网仍返回 WG 出口 | mihomo 控制台 Connections 标签观察规则命中；`Find-NetRoute -RemoteIPAddress <test-ip>` | GEOIP,private 规则覆盖测试 IP（应确保测试 IP 为公网地址） |
| VP-03 | 手动域名直连 | 配置测试域名 DOMAIN-SUFFIX 为 DIRECT；该域名解析走本地 DNS；访问走本地出口 | `Resolve-DnsName <test-domain> -Verbose` 确认解析来源；Wireshark 物理网卡过滤该目标 IP | fake-ip 模式下 DOMAIN-SUFFIX + DIRECT 时本地 DNS 解析是否正确（direct-nameserver UDP bug 需已修复） |
| VP-04 | 程序直连 | 配置测试 exe 为 DIRECT；该 exe 的 TCP/UDP 连接走本地出口；未配置程序走 WG | mihomo debug 日志匹配 "ProcessName using DIRECT"；Wireshark 物理网卡确认该 exe 的流量出现 | find-process-mode: always 是否正确设置；UDP 匹配 bug (issue #1800) |
| VP-05 | 子进程发现 | 启动测试 exe，helper 子进程发起连接；UI 展示 helper；确认后 helper 走本地 | `Get-CimInstance Win32_Process | Where-Object ParentProcessId -eq <parent-pid>`；mihomo 连接日志观察子进程名 | 子进程无法继承父进程规则，需显式枚举；helper 随机路径的识别 |
| VP-06 | 浏览器风险提示 | 将浏览器加入 DIRECT 时，UI 显示明确风险提示（整浏览器直连影响所有站点） | UI 交互测试 | 属于 UI 逻辑验证，无需外部工具；底稿阶段 N/A |
| VP-07 | DNS 防泄漏 | 未知域名 DNS 不发往本地 DNS；Wireshark 物理网卡无 UDP 53 出站；dnsleaktest 返回 WG 出口 DNS | Wireshark 物理网卡过滤 `udp.port == 53`（预期无包）；`Resolve-DnsName <unknown-domain> -Verbose` 确认解析走 mihomo；访问 ipleak.net 确认 DNS 服务器 | SMHNR 未关闭；NRPT 未配置；浏览器 DoH 未关闭；direct-nameserver UDP bug；strict-route 未启用 |
| VP-08 | 规则引擎崩溃 | 强制结束 mihomo.exe 后，浏览器访问未知公网超时或失败；不返回本地公网 IP | `Stop-Process -Name mihomo -Force`；然后 `(Invoke-RestMethod https://api64.ipify.org?format=json).ip`（预期请求超时）；Wireshark 物理网卡无未知公网 TCP SYN | **决策门槛**；wintun 适配器路由残留是否导致部分流量仍走物理 NIC；防火墙 Block 规则是否独立生效 |
| VP-09 | WireGuard 断开 | WG peer 断开后，未知公网访问失败；不走本地 | 修改 WG 配置用错误 endpoint 或暂停 peer；`ping -n 5 8.8.8.8`（预期全超时）；Wireshark 物理网卡无公网 TCP | WG 断开后 mihomo 是否切换到 DIRECT fallback（需确认无 fallback 配置）；kill-switch 防火墙是否阻断 |
| VP-10 | 路由被改动 | 手动 `route add 0.0.0.0 mask 0.0.0.0 <local-gateway>` 修改默认路由后，防火墙仍阻断未知公网 | `route add 0.0.0.0 mask 0.0.0.0 <physical-gateway>`；访问公网（预期失败）；`Get-NetFirewallRule -Group NetPolicy-KillSwitch | Format-Table` 确认规则仍 Enabled | **决策门槛**；Windows Firewall Block 规则基于 InterfaceType 而非路由表，理论上不受路由修改影响；需实测确认 |
| VP-11 | 重启恢复 | 重启 Windows 后，在 zero-desktop 启动前，防火墙 kill-switch 规则已生效（PersistentStore）；zero-desktop 启动后规则引擎完全恢复 | 重启后立即 `Get-NetFirewallRule -Group NetPolicy-KillSwitch | Where-Object Enabled -eq True`（预期规则存在）；重启后未启动 zero-desktop 时访问公网（预期失败） | WG endpoint host route 重启后消失（路由表不持久），需在 zero-desktop 启动流程中重新写入；NRPT 规则是否持久 |
| VP-12 | IPv6 泄漏 | 启用本地 IPv6 后，物理网卡无 IPv6 公网出站；`curl -6` 超时 | `Get-NetIPAddress -AddressFamily IPv6 | Where-Object PrefixOrigin -eq RouterAdvertisement`；Wireshark 物理网卡过滤 `ipv6 and not icmpv6 and not (ipv6.addr == fe80::/10)`（预期无包）；`curl -6 --max-time 5 https://ipv6.icanhazip.com`（预期超时） | **决策门槛**；Disable-NetAdapterBinding 在 WSL 环境重启后被恢复；6to4/Teredo 过渡隧道；mihomo ipv6:false 不够用 |

---

## 5. 防火墙 kill-switch 脚本草案（v2 重写）

> ## ⛔ 执行限制（v2.1 实测结论，必读）
> **不要在"将被掐断的那条交互会话内"执行本脚本**（在 SSH/RDP 会话里直接设 `DefaultOutboundAction=Block` 会掐断承载该会话的连接 → 自锁；§0.7.1 实测）。
> **正确执行方式（§0.9 实测有效）**：作为 **detached/SYSTEM 上下文**运行（计划任务/服务），管理面（LAN）在 LAN 白名单下存活、脚本独立跑完并自恢复。
> **关于完整 fail-closed**：mihomo 完整栈 + 全局 Block 的 fail-closed 已在 §0.9 跑出**通过摘要**（◑ 实测·仅摘要），但**原始证物未保留、tunnel-vs-physical 未独立取证**，进生产前须用 `evidence.ps1` 补齐原始证物（当前 0.228 离线阻塞）。**最终结论以 §0.8.2 权威表为准。**
> 本脚本仍为**草案**：白名单覆盖性（尤其 mihomo 上游 DNS R-DNS、TUN 接口 LAN 返程）需以原始证物核实。

### 5.1 设计模型

v1 的「Allow 白名单 + Block 兜底」模型在 Windows Firewall 不工作（Block 永远赢 Allow），v2 改为**默认 Block + 纯白名单 Allow**：

```
┌─ Set-NetFirewallProfile -DefaultOutboundAction Block ─────────┐
│   把当前激活 Profile（Domain/Private/Public）的默认出站设为 Block │
└────────────────────────────────────────────────────────────────┘
        ↓ 之后所有出站需要显式 Allow 才能放行
┌─ Allow 规则白名单（按 InterfaceAlias，绝对 + 精确）──────────┐
│ R1:    Allow Outbound on $TunAlias            # mihomo TUN 出栈 │
│ R2:    Allow Outbound → 127.0.0.0/8 (loopback, 仅IPv4)         │
│ R3:    Allow Outbound on $PhysicalAlias → WG endpoint UDP/port  │
│ R-DNS: Allow Outbound on $PhysicalAlias → mihomo 上游 DNS:53    │
│        （实测必需，§0.8.1；否则 mihomo 上游解析被拦死）         │
│ R4:    Allow Outbound on $PhysicalAlias → LAN（RFC1918+链路本地）│
│ R5:    Allow Outbound on $PhysicalAlias → DHCP/DHCPv6           │
│ R6:    Allow Outbound on $PhysicalAlias → NDP ICMPv6（fe80::/10）│
│ R7:    Allow Outbound on $PhysicalAlias → 用户直连 CIDR（可选） │
└────────────────────────────────────────────────────────────────┘
```

**关键点**：没有任何显式 Block 规则，整个 fail-closed 来自 Profile 默认动作。kill-switch 启动 = 先**快照**原 DefaultOutboundAction 到状态文件、再设默认 Block 并加 Allow 白名单；恢复（-Remove）= 删除 Allow 组 + **按状态文件把 DefaultOutboundAction 还原为原值**（实测原值是 `NotConfigured`，§0.2④——不可盲设 Allow 否则篡改用户基线）。

**InterfaceAlias 而非 InterfaceType**：避开未公开的 IF_TYPE 映射假设。按适配器名精确放行 TUN，其余物理 NIC 上仅放行白名单流量。

### 5.2 脚本

```powershell
#Requires -RunAsAdministrator
<#
.SYNOPSIS
  net-policy Windows 防火墙 kill-switch 脚本（v2）
  模型：Profile 默认 Outbound=Block，仅 InterfaceAlias 白名单 Allow。
  允许：mihomo TUN 接口全部出站；物理 NIC 仅 WG endpoint/LAN/DHCP/NDP/用户直连。

.PARAMETER TunAlias
  mihomo TUN 适配器的 InterfaceAlias（Apply 必填）。从 Get-NetAdapter 查；mihomo 默认 "Meta"。
  示例："Meta"

.PARAMETER PhysicalAlias
  物理出口适配器的 InterfaceAlias（Apply 必填）。
  示例："以太网" / "Wi-Fi"。多个用逗号分隔的数组。

.PARAMETER WgEndpointIp
  WireGuard 服务端 IP（Apply 时必填）。示例："1.2.3.4"

.PARAMETER WgEndpointPort
  WireGuard 服务端 UDP 端口，默认 51820。

.PARAMETER DirectCidrs
  本地直连 IP/CIDR 数组（可选）。示例：@("203.0.113.0/24","8.8.8.8")
  注意：用户直连流量通常由 mihomo 在用户态路由到 DIRECT outbound 出物理网卡，所以这里也需要白名单。

.PARAMETER Apply
  部署 kill-switch：设 Profile 默认 Block + 创建 Allow 白名单。

.PARAMETER Remove
  回滚：删除白名单 + 按状态文件把 Profile DefaultOutboundAction 还原为部署前原值（非盲设 Allow）。
  注意：Remove 后失去 fail-closed 保护。

.PARAMETER WhatIf
  演练模式，不实际修改防火墙。

.EXAMPLE
  # 演练
  .\kill-switch.ps1 -Apply -TunAlias Meta -PhysicalAlias 以太网 -WgEndpointIp 1.2.3.4 -WhatIf

  # 部署
  .\kill-switch.ps1 -Apply -TunAlias Meta -PhysicalAlias 以太网,Wi-Fi -WgEndpointIp 1.2.3.4

  # 带用户直连白名单
  .\kill-switch.ps1 -Apply -TunAlias Meta -PhysicalAlias 以太网 -WgEndpointIp 1.2.3.4 -DirectCidrs @("203.0.113.0/24")

  # 回滚
  .\kill-switch.ps1 -Remove
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]   $TunAlias       = '',
    [string[]] $PhysicalAlias  = @(),
    [string]   $WgEndpointIp   = '',
    [int]      $WgEndpointPort = 51820,
    [string[]] $DnsBootstrap   = @('223.5.5.5','119.29.29.29'),  # mihomo 上游 DNS（§6 default-nameserver，实测须放行）
    [string[]] $DirectCidrs    = @(),
    [switch]   $Apply,
    [switch]   $Remove
)

# ── 常量 ────────────────────────────────────────────────────────────────────
$GROUP        = 'NetPolicy-KillSwitch'
$LoopbackAlias = 'Loopback Pseudo-Interface 1'  # 英文系统名；中文系统可能不同，按需调整

# 状态文件：保存部署前的原始 Profile DefaultOutboundAction，Remove 时按原值恢复。
# 直接固定恢复成 Allow 会覆盖用户原本可能就是 Block 的策略——这是 v1 修订版残留的一个 bug。
$StateFile = Join-Path $env:ProgramData 'net-policy\killswitch-state.json'

# ── 回滚（-Remove）─────────────────────────────────────────────────────────
if ($Remove) {
    Write-Host "[REMOVE] 删除规则组 '$GROUP' 下所有规则..."
    Get-NetFirewallRule -Group $GROUP -ErrorAction SilentlyContinue |
        Remove-NetFirewallRule -WhatIf:$WhatIfPreference

    # 按部署前快照恢复 Profile DefaultOutboundAction
    if (Test-Path $StateFile) {
        Write-Host "[REMOVE] 从 $StateFile 恢复原始 Profile DefaultOutboundAction..."
        $state = Get-Content $StateFile -Raw | ConvertFrom-Json
        foreach ($p in 'Domain','Private','Public') {
            $orig = $state.$p
            if ($orig) {
                Write-Host "  - Profile $p → $orig"
                Set-NetFirewallProfile -Profile $p -DefaultOutboundAction $orig `
                    -WhatIf:$WhatIfPreference
            }
        }
        if (-not $WhatIfPreference) {
            Remove-Item $StateFile -Force
        }
        Write-Host '[REMOVE] 完成。原始防火墙策略已恢复。'
    } else {
        Write-Warning "状态文件 $StateFile 不存在 —— 无法自动恢复原 DefaultOutboundAction。"
        Write-Warning "Profile 默认动作仍为当前值（可能是 Block）。请手动确认："
        Write-Warning "  Get-NetFirewallProfile | Format-Table Name,DefaultOutboundAction"
        Write-Warning "若需要恢复联网，手动执行："
        Write-Warning "  Set-NetFirewallProfile -Profile Domain,Private,Public -DefaultOutboundAction NotConfigured"
        Write-Warning "（NotConfigured 是组策略默认值，即不显式设置；通常等同于 Allow）"
    }
    return
}

if (-not $Apply) {
    Write-Host "用法：-Apply 部署 / -Remove 回滚 / -WhatIf 演练。"
    return
}

# ── 参数校验 ────────────────────────────────────────────────────────────────
foreach ($pair in @(
    @{ Name='TunAlias'; Value=$TunAlias },
    @{ Name='WgEndpointIp'; Value=$WgEndpointIp }
)) {
    if (-not $pair.Value) { Write-Error "-$($pair.Name) 必填"; return }
}
if ($PhysicalAlias.Count -eq 0) {
    Write-Error '-PhysicalAlias 必填（至少一个物理网卡名）'
    return
}

# 适配器存在性校验，防错名
foreach ($alias in @($TunAlias) + $PhysicalAlias) {
    if (-not (Get-NetAdapter -Name $alias -ErrorAction SilentlyContinue)) {
        Write-Error "适配器 '$alias' 不存在；用 Get-NetAdapter 列出有效名称"
        return
    }
}

# ── 幂等 helper：v2 修订 — Set update 时必须移除 Group/Name/DisplayName ────
function Set-FwRule {
    param([hashtable]$Params)
    $name = $Params['Name']
    $existing = Get-NetFirewallRule -Name $name -ErrorAction SilentlyContinue
    if ($existing) {
        Write-Verbose "[UPDATE] $name"
        $update = $Params.Clone()
        # Set-NetFirewallRule 的 -Group/-Name/-DisplayName 是不同参数集的 selector，
        # 与同一调用中的 modifier 参数互斥。update 时必须全部移除（修改 Group
        # 应通过 dot-notation + InputObject，本场景无此需求）。
        foreach ($k in 'Name','DisplayName','Group') { $update.Remove($k) | Out-Null }
        Set-NetFirewallRule -Name $name @update -WhatIf:$WhatIfPreference
    } else {
        Write-Verbose "[CREATE] $name"
        New-NetFirewallRule @Params -WhatIf:$WhatIfPreference | Out-Null
    }
}

# ═════════════════════════════════════════════════════════════════════════════
# Step 1：保存原 Profile DefaultOutboundAction，再设为 Block
# 关键：Apply 前必须快照原状态到 $StateFile，否则 Remove 时无法精准回滚
# ═════════════════════════════════════════════════════════════════════════════
if (-not (Test-Path $StateFile) -and -not $WhatIfPreference) {
    Write-Host "[APPLY] 快照原 Profile DefaultOutboundAction 到 $StateFile ..."
    New-Item -ItemType Directory -Path (Split-Path $StateFile) -Force | Out-Null
    $snapshot = [ordered]@{}
    foreach ($p in 'Domain','Private','Public') {
        $snapshot[$p] = (Get-NetFirewallProfile -Profile $p).DefaultOutboundAction.ToString()
        Write-Host "  - Profile $p 原值：$($snapshot[$p])"
    }
    $snapshot | ConvertTo-Json | Set-Content -Path $StateFile -Encoding UTF8
} elseif (Test-Path $StateFile) {
    Write-Host "[APPLY] 检测到已有快照 $StateFile（之前已部署过）；不覆盖。"
}

Write-Host "[APPLY] 设置三 Profile DefaultOutboundAction=Block..."
foreach ($p in 'Domain','Private','Public') {
    Set-NetFirewallProfile -Profile $p -DefaultOutboundAction Block `
        -WhatIf:$WhatIfPreference
}

# ═════════════════════════════════════════════════════════════════════════════
# Step 2：Allow 白名单规则
# 全部 InterfaceAlias 精确放行——避开 IF_TYPE 分类不确定性
# ═════════════════════════════════════════════════════════════════════════════

# R1：mihomo TUN 接口全部出站放行（mihomo 在 TUN 出栈，再由它自己决定走 WG 还是 DIRECT）
Set-FwRule @{
    Name           = "$GROUP-Allow-TUN-Out"
    DisplayName    = '[KS] Allow TUN All Outbound'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'Any'
    InterfaceAlias = $TunAlias
    Profile        = 'Any'
    Description    = 'mihomo TUN 接口出站全部放行；流量由 mihomo 自行路由'
}

# R2：Loopback 放行（mihomo DNS listen 在 127.0.0.1，IPC、Trace 都走 loopback）
# ⚠️ v2.1 实测修正（§0.4）：New-NetFirewallRule 的 -RemoteAddress **拒绝 IPv6 环回 `::1`**
# （报"未指定/多播/广播或环回 IPv6 地址"）。故只放行 IPv4 环回；IPv6 `::1` 不能进规则，
# 且 Windows 对 loopback 流量本就 WFP 豁免，无需显式规则。
# （注：混合 v4+v6 的普通 CIDR 如 R4 是允许的——失败仅限 `::1` 这个特殊地址）
Set-FwRule @{
    Name           = "$GROUP-Allow-Loopback4-Out"
    DisplayName    = '[KS] Allow Loopback v4 Out'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'Any'
    RemoteAddress  = '127.0.0.0/8'
    Profile        = 'Any'
    Description    = 'Loopback IPv4（::1 被 API 拒绝，且 loopback WFP 豁免）'
}

# R3：物理 NIC 上仅放行 WireGuard endpoint UDP 握手
Set-FwRule @{
    Name           = "$GROUP-Allow-WG-Endpoint"
    DisplayName    = '[KS] Allow WG Endpoint UDP Out'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'UDP'
    RemoteAddress  = $WgEndpointIp
    RemotePort     = $WgEndpointPort
    InterfaceAlias = $PhysicalAlias
    Profile        = 'Any'
    Description    = 'WireGuard 握手 UDP'
}

# R-DNS：放行 mihomo 上游 DNS bootstrap IP 出物理网卡（【已实测必需】§0.8.1）
# 实测：mihomo 上游解析走物理 NIC 直出；不放行则默认 Block 拦死 mihomo 上游 DNS → mihomo 不可用。
# $DnsBootstrap 默认 = §6 的 default-nameserver（223.5.5.5 / 119.29.29.29）。
# 残留泄漏面：所查域名对这些 DNS/本地网络可见——若 §6 的 respect-rules 经本地验证生效（DNS 走隧道），
# 可收紧本规则；未验证前必须保留，否则 mihomo 起不来。
Set-FwRule @{
    Name           = "$GROUP-Allow-DNS-Bootstrap"
    DisplayName    = '[KS] Allow mihomo upstream DNS bootstrap'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'UDP'
    RemoteAddress  = $DnsBootstrap
    RemotePort     = '53'
    InterfaceAlias = $PhysicalAlias
    Profile        = 'Any'
    Description    = 'mihomo 上游 DNS（物理直出，实测必需）'
}

# R4：物理 NIC 上 LAN 直通（RFC1918 + 链路本地 + IPv6 ULA + 链路本地 v6）
$LAN = @(
    '10.0.0.0/8','172.16.0.0/12','192.168.0.0/16','169.254.0.0/16',
    'fc00::/7','fe80::/10'
)
Set-FwRule @{
    Name           = "$GROUP-Allow-LAN-Out"
    DisplayName    = '[KS] Allow LAN Out (physical)'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'Any'
    RemoteAddress  = $LAN
    InterfaceAlias = $PhysicalAlias
    Profile        = 'Any'
    Description    = 'LAN 直通'
}

# R5a：DHCPv4
Set-FwRule @{
    Name           = "$GROUP-Allow-DHCPv4-Out"
    DisplayName    = '[KS] Allow DHCPv4 Out'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'UDP'
    LocalPort      = '68'
    RemotePort     = '67'
    RemoteAddress  = '255.255.255.255'
    InterfaceAlias = $PhysicalAlias
    Profile        = 'Any'
}

# R5b：DHCPv6
Set-FwRule @{
    Name           = "$GROUP-Allow-DHCPv6-Out"
    DisplayName    = '[KS] Allow DHCPv6 Out'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'UDP'
    LocalPort      = '546'
    RemotePort     = '547'
    InterfaceAlias = $PhysicalAlias
    Profile        = 'Any'
}

# R6：NDP（Router Solicitation/Advertisement, NS/NA/Redirect），限定链路本地目标
# v2 修订：补 RemoteAddress=fe80::/10，避免 ICMPv6 出公网
Set-FwRule @{
    Name           = "$GROUP-Allow-NDP-Out"
    DisplayName    = '[KS] Allow NDP ICMPv6 Out (link-local only)'
    Group          = $GROUP
    Enabled        = 'True'
    Direction      = 'Outbound'
    Action         = 'Allow'
    Protocol       = 'ICMPv6'
    IcmpType       = @('133','134','135','136','137')
    RemoteAddress  = 'fe80::/10'
    InterfaceAlias = $PhysicalAlias
    Profile        = 'Any'
}

# ⚠️ 待实测：mihomo 进程上游 DNS 路径（详见 §9 高优问题 #6）
# 默认 Block 模式下，mihomo 解析 nameserver(DoH TCP 443) / direct-nameserver(UDP 53)
# 这些上游 socket 是否经物理 NIC 出？如经物理 NIC，则会被默认 Block 拦截，
# 导致 mihomo 整体不可用。可能需要加一条：
#
#   Set-FwRule @{ Name = "$GROUP-Allow-mihomo-Out"; ...
#       Program = 'C:\path\to\mihomo.exe'; InterfaceAlias = $PhysicalAlias; Action = 'Allow' }
#
# 但这会让 mihomo 可向任意 IP 发任意端口请求（DNS 泄漏面）。
# 部署前请抓包确认：Wireshark 物理网卡过滤 udp.port==53 or tcp.port==443，
#   - 若 mihomo 的 DNS 查询出现在物理网卡 → 需补上述 Program=Allow 规则
#   - 若不出现（说明走 TUN→WG outbound）→ 不需要补
# 这个不确定性是 v2 底稿未实测的核心空白；脚本默认不放行 mihomo.exe，需先验证后决定。

# R7：用户直连白名单（可选）
if ($DirectCidrs.Count -gt 0) {
    Set-FwRule @{
        Name           = "$GROUP-Allow-Direct-CIDRs"
        DisplayName    = '[KS] Allow User Direct CIDRs Out'
        Group          = $GROUP
        Enabled        = 'True'
        Direction      = 'Outbound'
        Action         = 'Allow'
        Protocol       = 'Any'
        RemoteAddress  = $DirectCidrs
        InterfaceAlias = $PhysicalAlias
        Profile        = 'Any'
        Description    = '用户显式直连 CIDR'
    }
}

# IPv6 公网阻断：v2 注释 —— 默认 Block 已覆盖；如果 ip 配置中没有 ::/0 的 Allow，
# 2000::/3 出站自动被默认 Block 拦下。不需要单独的 Block 规则。
# 若 R4 之类规则误放行了 IPv6 公网，可加一条显式 Block，但本设计避开。

Write-Host "[OK] kill-switch 已部署。"
Write-Host "  - 默认出站：Block（三 Profile 全部）"
$count = (Get-NetFirewallRule -Group $GROUP -ErrorAction SilentlyContinue | Measure-Object).Count
Write-Host "  - 白名单规则数：$count"
Write-Host "验证：Get-NetFirewallRule -Group $GROUP | Format-Table DisplayName,Enabled,Action,Direction"
Write-Host "回滚：.\kill-switch.ps1 -Remove"

# ─────────────────────────────────────────────────────────────────────────────
# 附：WireGuard endpoint host route（路由环 workaround，建议在 mihomo 启动前执行）
# ─────────────────────────────────────────────────────────────────────────────
# $gw = (Get-NetRoute -DestinationPrefix '0.0.0.0/0' |
#        Sort-Object RouteMetric | Select-Object -First 1)
# New-NetRoute -DestinationPrefix "$WgEndpointIp/32" `
#              -InterfaceIndex $gw.InterfaceIndex -NextHop $gw.NextHop -RouteMetric 1
```

### 5.3 v2 与 v1 的关键差异

| 维度 | v1 | v2 |
|---|---|---|
| Block-vs-Allow 模型 | Allow 白名单 + Block 兜底（不工作） | Profile 默认 Block + 纯 Allow 白名单 |
| 接口区分 | InterfaceType=Wired,Wireless | InterfaceAlias 精确名（TUN/物理逐一列出） |
| Loopback 处理 | 隐式（未显式放行） | 显式 R2 放行 127.0.0.0/8（仅 IPv4；::1 被 API 拒绝，§0.4） |
| NDP RemoteAddress | 不限定（type 133-137 可出公网） | 限定 fe80::/10 链路本地 |
| IPv6 公网阻断 | 单独 Block 规则（缺 InterfaceType） | 默认 Block 自动覆盖，无需独立规则 |
| DNS 53 阻断 | 单独 Block 规则（与用户 DNS 直连白名单冲突） | 默认 Block 覆盖；DNS 走 mihomo TUN dns-hijack |
| Set-NetFirewallRule update | 漏移除 Group → 报错 | 同时移除 Name/DisplayName/Group |
| Remove 行为 | 仅删规则（Profile 默认未恢复） | 恢复 Profile=Allow + 删规则 |

### 5.4 Loopback alias 适配说明

`Loopback Pseudo-Interface 1` 是英文系统名；中文系统可能是 `环回伪接口 1`。脚本最终用 `RemoteAddress=127.0.0.0/8` 而非 `InterfaceAlias` 处理 loopback 以避开该差异。

---

## 6. mihomo 配置草案

```yaml
# mihomo config.yaml — net-policy 最小可用骨架（Windows，zero-desktop 方案 B）
# 用途：实测验证阶段，不含生产密钥
# 执行前：以管理员权限运行 mihomo；wintun.dll 与 mihomo.exe 同目录

mixed-port: 7890          # HTTP/SOCKS 混合端口（TUN 模式下保留用于调试）
allow-lan: false
mode: rule
log-level: debug          # 实测阶段使用 debug，确认规则匹配结果；生产改 info
ipv6: false               # 首版禁用 IPv6，防 AAAA 泄漏（配合物理适配器层 IPv6 阻断）
find-process-mode: always # 强制每连接查找进程（VP-04/05 所需）；高并发场景注意 CPU 开销

dns:
  enable: true
  listen: 127.0.0.1:1053  # v2：保持 1053 即可；NRPT 已弃用，DNS 拦截依赖 TUN dns-hijack
                          # 不要设为 53，避免和 Windows dnscache 服务冲突
  ipv6: false
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-filter:
    - '*.lan'
    - '*.local'
    - '*.localhost'
    - 'localhost'
    - '+.msftconnecttest.com'   # Windows 网络连通性检测，必须直连
    - '+.msftncsi.com'
    - 'time.windows.com'        # Windows NTP
    - '+.ntp.org'
  # ══════════════════════════════════════════════════════════════════════════
  # 【已定稿，硬约束】mihomo 上游 DNS 路径处理 —— §0.8.1 实测：mihomo 自身上游
  # 解析默认走【物理 NIC 直出】到 nameserver IP（实测以 UDP nameserver 223.5.5.5 验证）。
  # 后果：(a) 一处 DNS 泄漏面（本地网络可见所查域名）；(b) kill-switch 默认 Block 下，
  #       若不放行这些 DNS IP，mihomo 上游解析被拦死 → 整个 mihomo 不可用。
  # ── 【生产模式已拍板】采用 模式 A：允许 bootstrap DNS 物理直连（可接受泄漏面）──
  #   决策：物理网卡只额外放行 WG endpoint + bootstrap DNS IP；mihomo 上游 DNS 走物理直出，
  #   接受"本地网络可见所查域名"这一【有限泄漏面】，换取简单 + 已实测可用（§0.8.1/§0.9）。
  #   ① 【必做】§5 kill-switch 白名单【必须】放行 default-nameserver IP（223.5.5.5/119.29.29.29）
  #      出物理:53（§5 规则 R-DNS）。否则 mihomo 上游解析被默认 Block 拦死、mihomo 起不来。
  #   ② 【模式 B（零泄漏）为"待验证的可选增强"，非当前生产答案】：若要消除上述泄漏面，可让 DNS
  #      也走隧道（respect-rules / proxy nameserver），物理仅放行 WG endpoint。但 respect-rules
  #      ▢ 研究层【未实测】，未经 0.228 验证前【不要】依赖、【不要】据此收紧 R-DNS——否则 DNS 会断。
  # 结论：生产先用 模式 A；模式 B 验证通过后再切换并收紧 R-DNS。
  # ══════════════════════════════════════════════════════════════════════════
  # respect-rules: true         # 【模式 B 才开，且须先本地实测】当前模式 A 注释掉，避免误以为零泄漏已生效
  default-nameserver:           # bootstrap：解析 DoH 端点/首跳，【必为 UDP IP】，必然物理直出→§5 R-DNS 放行
    - 223.5.5.5
    - 119.29.29.29
  proxy-server-nameserver:      # 解析 proxy server 域名用（本例 server 已是 IP，可留作兜底）
    - 223.5.5.5
  nameserver:
    - https://1.1.1.1/dns-query
    - https://dns.google/dns-query
  direct-nameserver:            # 直连域名使用国内 DNS；注意：只配 UDP（Discussion #1656 bug）
    - 223.5.5.5
    - 119.29.29.29
  fallback: []                  # 不配 fallback，避免请求泄漏到 fallback 服务器

tun:
  enable: true
  stack: mixed            # TCP 走 system（性能），UDP 走 gvisor（稳定）
                          # 注意：若 Windows 防火墙启用，需在防火墙放行 mihomo.exe
  auto-route: true        # 自动安装默认路由，让所有流量进 TUN
  auto-detect-interface: true
  # v2.1 实测（§0.6）：auto-detect-interface=true 下 DIRECT egress 正常（baidu 200），
  # interface-name 非必需。若 WG outbound 形态下出现握手包绕 TUN 回环，再用显式绑定加固：
  #   interface-name: 以太网         # 可选：显式指定底层出口网卡名（Get-NetAdapter 取）
  dns-hijack:
    - any:53              # 劫持所有 53 端口 DNS 请求
    - tcp://any:53
  strict-route: true      # v2 关键：strict-route 注入 WFP 规则阻断 SMHNR 多宿主 DNS 泄漏；
                          # 配合 dns-hijack=any:53 取代 v1 的 NRPT 路径（NRPT 不支持非 53 端口）
                          # 副作用：可能影响 VirtualBox，酌情关闭
  # v2 说明：mihomo issue #2617 的 route-exclude-address /32 bug 是针对单 IP（如 WG endpoint），
  # CIDR 段（如下方 LAN 段）使用正常。WG endpoint host route 仍建议用 PowerShell 在 mihomo
  # 启动前手动写入（kill-switch.ps1 附录），不要在此用 /32。
  route-exclude-address:
    - 192.168.0.0/16
    - 10.0.0.0/8
    - 172.16.0.0/12
    - 169.254.0.0/16

proxies:
  - name: wg-overseas
    type: wireguard
    server: YOUR_WG_ENDPOINT_IP   # 替换为真实 WG peer endpoint IP（必须是 IP 不能是域名）
    port: 51820                   # 替换为真实端口
    ip: 10.x.x.x/32               # WG 分配给本机的 tunnel IP
    private-key: YOUR_BASE64_PRIVATE_KEY
    public-key: SERVER_BASE64_PUBLIC_KEY
    allowed-ips:
      - 0.0.0.0/0
    udp: true
    mtu: 1420
    remote-dns-resolve: false     # 由上层 fake-ip 统一接管 DNS，WG outbound 内不另起解析

proxy-groups:
  - name: WG-OUT
    type: select
    proxies:
      - wg-overseas

rules:
  # ── 内网保留地址直连 ──────────────────────────────────────────────────────
  - GEOIP,private,DIRECT

  # ── 用户手动配置示例（由 net_policy_apply 命令生成）────────────────────
  # 程序直连（PROCESS-PATH 用反斜杠，来自 QueryFullProcessImageNameW）
  # - PROCESS-PATH,C:\Program Files\SomeApp\app.exe,DIRECT
  # 程序直连（PROCESS-NAME 只写 exe 文件名，大小写不敏感）
  # - PROCESS-NAME,someapp.exe,DIRECT
  # 域名直连
  # - DOMAIN-SUFFIX,example.cn,DIRECT
  # IP/CIDR 直连
  # - IP-CIDR,203.0.113.0/24,DIRECT

  # ── 默认：未知流量全部走 WireGuard（fail-closed 核心规则）────────────────
  - MATCH,WG-OUT
```

---

## 7. 实测者操作指引

### 7.1 环境准备（先做）

> **v2 顺序变化**：原 NRPT 步骤已删除（API 不支持端口）；新增 step 0 InterfaceType 探针验证；浏览器 DoH 给出完整 PS 命令。

**Step 0：探针验证（必须先跑，5 分钟）**

在 v2 决定改用 InterfaceAlias 白名单的同时，仍然推荐对 Wintun 的 InterfaceType 做一次性确认（用来回填 §9 高优先级未决问题 #3，也用于后续如果想回到 InterfaceType 方案有据可查）：

```powershell
# 1. 先启动 mihomo（用临时最小 config，TUN enable=true 即可）
# 2. 查 Meta 适配器
Get-NetAdapter -Name Meta | Select-Object Name,InterfaceDescription,ifIndex,Status,
  @{N='InterfaceType';E={$_.InterfaceType}},
  @{N='MediaType';E={$_.MediaType}},
  @{N='PhysicalMediaType';E={$_.PhysicalMediaType}}
# 期望：InterfaceType=53（PROP_VIRTUAL）、MediaType=NdisMediumIP

# 3. 再做实际行为探针：临时建一条 InterfaceType=Wired Block 测试 TUN 是否被命中
New-NetFirewallRule -DisplayName 'Probe-BlockWired' -Direction Outbound `
    -InterfaceType Wired -Action Block -Enabled True
# 用 mihomo proxy 试访问公网；若仍可达 → Wintun 不被归入 Wired（断言成立）
# 若不可达 → Wintun 被归入 Wired（断言失败，v2 InterfaceAlias 方案是必须的，不能回退）
Remove-NetFirewallRule -DisplayName 'Probe-BlockWired'
```

把结果回填到 §9 #3。

**Step 1：安装 mihomo**

下载最新稳定版（≥ v1.19.x，验证 issue #2617 是否修复），mihomo.exe + wintun.dll 同目录；管理员权限启动。

**Step 2：写入 WG endpoint host route（防路由环）**

```powershell
$wgEndpoint = '<WG_IP>'  # 替换
$defaultGw = Get-NetRoute -DestinationPrefix '0.0.0.0/0' |
    Sort-Object RouteMetric | Select-Object -First 1
New-NetRoute -DestinationPrefix "$wgEndpoint/32" `
    -InterfaceIndex $defaultGw.InterfaceIndex `
    -NextHop $defaultGw.NextHop -RouteMetric 1
```

**Step 3：填 mihomo config.yaml**

按 §6 骨架替换 `server` / `port` / `private-key` / `public-key`；确认 `tun.strict-route: true`、`dns-hijack: any:53` 已配。

**Step 4：启动 mihomo，确认 TUN 接管**

```powershell
Get-NetAdapter -Name Meta  # 状态应为 Up
Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Format-Table InterfaceAlias,NextHop,RouteMetric
# 期望：默认路由有一条 NextHop 在 TUN 子网（如 198.18.0.x），InterfaceAlias=Meta
```

记下物理网卡的 InterfaceAlias（如「以太网」或「Wi-Fi」），下一步要用。

**Step 5：部署 kill-switch 防火墙规则**

```powershell
.\kill-switch.ps1 -Apply `
    -TunAlias Meta `
    -PhysicalAlias '以太网','Wi-Fi' `
    -WgEndpointIp <WG_IP>
```

部署后立刻测一次（在浏览器访问任意网站），确认 mihomo 仍能正常代理；如失败用 `.\kill-switch.ps1 -Remove` 立即回滚。

**Step 6：关闭浏览器 DoH（防 DoH over HTTPS 绕过 TUN）**

```powershell
# Chrome
New-Item -Path 'HKLM:\SOFTWARE\Policies\Google\Chrome' -Force | Out-Null
Set-ItemProperty -Path 'HKLM:\SOFTWARE\Policies\Google\Chrome' -Name 'DnsOverHttpsMode' -Value 'off' -Type String

# Edge
New-Item -Path 'HKLM:\SOFTWARE\Policies\Microsoft\Edge' -Force | Out-Null
Set-ItemProperty -Path 'HKLM:\SOFTWARE\Policies\Microsoft\Edge' -Name 'DnsOverHttpsMode' -Value 'off' -Type String

# Firefox：写 %ProgramFiles%\Mozilla Firefox\distribution\policies.json，
# 内容 { "policies": { "DNSOverHTTPS": { "Enabled": false, "Locked": true } } }
```

**Step 7：IPv6 处理**

```powershell
# 阻断物理 NIC IPv6 绑定（防 IPv6 完全绕过 WG）
# 注意：仅作用于物理网卡，不影响 TUN/Loopback
Get-NetAdapter -Physical | ForEach-Object {
    Disable-NetAdapterBinding -Name $_.Name -ComponentID ms_tcpip6
}
# 禁用 6to4/Teredo/ISATAP 过渡技术
Set-Net6to4Configuration -State Disabled
Set-NetTeredoConfiguration -Type Disabled
Set-NetIsatapConfiguration -State Disabled
```

如本机已安装 WSL，Disable-NetAdapterBinding 在重启后可能被恢复，作为兜底，kill-switch 的默认 Block 出站已经覆盖 IPv6 公网（因为 §5 中没有 IPv6 公网 Allow 规则）。

> **NRPT 注意**：v2 已弃用 NRPT 路径。若机器上残留早期实验加入的 NRPT 规则，需清理：
> `Get-DnsClientNrptRule | Where-Object DisplayName -like '*net-policy*' | Remove-DnsClientNrptRule`

### 7.2 VP 用例推荐执行顺序

建议按以下顺序执行，先验证基础能力，再验证 fail-closed 关键路径：

1. **VP-01**（未知流量默认海外）— 基础可用性，先过这关再做后面
2. **VP-07**（DNS 防泄漏）— DNS 泄漏会干扰后续所有 VP 的观察结果
3. **VP-12**（IPv6 泄漏）— 首版 IPv6 阻断，确认无 IPv6 绕行
4. **VP-08**（规则引擎崩溃）— 决策门槛，kill-switch 核心验证
5. **VP-09**（WireGuard 断开）— 决策门槛
6. **VP-10**（路由被改动）— 决策门槛
7. **VP-02 / VP-03**（手动 IP/域名直连）— 基本规则功能
8. **VP-04**（程序直连）— 进程规则
9. **VP-05**（子进程发现）— 进程树观察
10. **VP-11**（重启恢复）— 最后验证，需要实际重启
11. **VP-06**（浏览器风险提示）— UI 逻辑，待 UI 实现后验证

### 7.3 哪些失败可以容忍，哪些是决策门槛

**决策门槛（必须通过，参考 §14.11）**：
- VP-01 通过：否则整个方案 B 基础不成立
- VP-07 通过：DNS 泄漏是安全底线
- VP-08 通过：mihomo 崩溃后不能泄漏，这是 fail-closed 核心
- VP-09 通过：WG 断开后不能泄漏
- VP-10 通过：路由被改动后防火墙仍生效
- VP-12 通过：IPv6 泄漏等同于 VPN 形同虚设

**可以容忍（首版降级处理）**：
- VP-05 失败：子进程 UI 展示功能可以推迟，首版手动配置子进程规则即可
- VP-06 失败：UI 风险提示是交互设计，不影响安全性
- VP-04 中 UDP 匹配失败（issue #1800）：如果 UDP 进程匹配不工作，UDP 流量回落到 MATCH→WireGuard 仍是 fail-closed 的；仅影响用户体验（UDP 直连程序无法生效），不影响安全性
- VP-11 重启后短暂窗口：若 zero-desktop 启动时间 < 5s 且防火墙规则已在重启后生效，可接受；若窗口过长需要改为 Windows 服务

### 7.4 结果回填格式

实测完成后，在本文档底部追加实测结果章节，并以 §14.9 定义的 JSON 格式写入 `{workspace}/net-policy/verify/last-report.json`：

```json
{
  "started_at": "（回填实测时间）",
  "windows_version": "（回填，如 Windows 11 26200）",
  "wireguard": {
    "mode": "mihomo-userspace-outbound",
    "endpoint": "redacted"
  },
  "engine": {
    "kind": "mihomo",
    "version": "（回填 mihomo 版本）"
  },
  "cases": [
    {
      "id": "VP-01",
      "status": "passed | failed | skipped",
      "observed_exit": "wg | local | unknown",
      "evidence": ["（回填：命令输出片段或截图路径）"],
      "notes": "（可选备注）"
    }
  ],
  "blocking_issues": ["（若有决策门槛用例失败，在此列出阻塞原因）"]
}
```

---

## 8. 决策门槛回顾（v2.1 实测更新）

来源：docs/unified-desktop-shell-design.md §14.11。下表"现状"列已按 §0.x 真机实测更新（取代原 v2 底稿预判）。

| 条件 | v2.1 实测现状 |
|---|---|
| VP-01、VP-07、VP-08、VP-09、VP-10、VP-12 全部通过 | **以 §0.8.2 权威表为准**：VP-01 ✅有原始输出（§0.7）；VP-07 ◑（hijack✅、上游DNS泄漏面✅，DoH▢研究层、逐包抓包未做）；**VP-08/09/10 ✅有原始证物（§0.9.2）**；VP-12 N/A（无 v6）。门槛核心机制均通过且有证物；仅 VP-07 逐包级、VP-11 完整快照、DoH 未达最高取证强度。 |
| 程序规则至少能稳定覆盖普通 exe 和已确认子进程 | **✅ 达成**：PROCESS-NAME/PATH 真机生效，**TCP+UDP 均按进程分流**（§0.5/§0.7/§0.8，#1800 在 v1.19.27 已修复）；子进程按自身进程匹配（需显式枚举）。 |
| 防火墙兜底不依赖 UI 常驻；zero-desktop 退出后未知公网不泄漏 | **◑ 机制已证、完整栈待验**：PersistentStore 规则与进程解耦、停引擎不泄漏已证（§0.4/§0.8.2ter）；但 mihomo 崩溃留路由残留（§0.8.2bis 强观察）+ 全局Block+TUN 切 SSH（§0.7.1），完整形态须本地控制台确认。 |
| 规则应用失败时 UI 能明确展示失败原因，并保持 fail-closed | 待工程实现（engine.rs 捕获错误回前端）；脚本层已有适配器存在性校验 + 状态快照。 |
| 用户可以一键恢复网络，但恢复动作必须明确提示会关闭泄漏防护 | -Remove 已实现（按状态文件还原原值，非盲设 Allow）；UI 提示待实现。 |

---

## 9. 未决问题汇总

以下问题来自 6 个子研究的 open_questions，已去重并按优先级排序：

### v2 已通过文档/源码验证（无需再实测）

- ~~Windows Firewall Block-vs-Allow 仲裁~~ → 已确认 Block 永远赢 Allow（[MS rules 文档](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/rules)），v2 改用「默认 Block + 白名单」模型
- ~~NRPT 是否支持自定义端口~~ → 已确认不支持（[WMI MOF schema](https://learn.microsoft.com/en-us/previous-versions/windows/desktop/ramgmtpsprov/dnsclientnrptrule)），v2 改用 TUN dns-hijack
- ~~Set-NetFirewallRule -Group 行为~~ → 已确认是 ByGroup 参数集 selector，v2 Set-FwRule 函数已修

### 高优先级（影响决策门槛）

1. **mihomo TUN egress / 路由环 — 已解决（DIRECT + WG outbound 两形态均无环）**：§0.6 DIRECT egress 正常；§0.7/§0.9 **WG userspace outbound 也跑通**（出口 38.209.122.38），握手包经 auto-detect 绕 TUN，**无路由环、无需手动 route-exclude/host-route**。原"路由环是头号风险"判断作废。（§0.9 系列证物待补，但 VP-01 出口 IP 为 ✅有原始输出，路由环不存在这点可靠）
2. **kill-switch 独立性**：mihomo 进程 crash 后 wintun 适配器路由残留时，Profile 默认 Block 是否能独立阻断物理网卡出站？（影响 VP-08）
3. ~~**Wintun 与 InterfaceType**~~ → **已闭环（v2.1 真机实测）**：数值层（§0.2①）+ 行为层（§0.3）均已验证。0.228 上 WireGuard 隧道 `InterfaceType=53`（以太网=6、WLAN=71），且 `-InterfaceType Wired` 的 Block **不命中**隧道流量（RemoteAddress=9.9.9.9 限定探针 + Any-block 对照）。结论：InterfaceType 与 InterfaceAlias 两种方案皆可；v2 选 InterfaceAlias 是工程稳健性取舍，非被迫。**残留小尾巴**：未直接测 mihomo Wintun（IfType 同为 53，大概率一致）；未测多网卡/RemoteAccess 桶。
4. **IPv6 阻断稳定性**：Disable-NetAdapterBinding 在安装 WSL 的 Windows 11 26200 重启后是否被恢复？v2 默认 Block 兜底（无 IPv6 公网 Allow 规则）是否足以作为单独防泄漏手段？（影响 VP-12）
5. **DNS 泄漏（v2.1 大部分实测）**：**hijack 层已确认**（§0.6：显式 8.8.8.8 也被拦返回 fake-ip）；**浏览器 DoH-443 绕过已真机确认**（§0.8.2bis：53→fake-ip 198.18.0.12、DoH443→真实IP，故必须注册表关浏览器 DoH）。**仍待测**：① strict-route=true 下 SMHNR 逐包抓包取证（行为层已覆盖）② direct-nameserver UDP 组合在 v1.19.27 是否修复。（影响 VP-07）
6. ~~**mihomo 上游 DNS 路径**~~ → **已真机回答（§0.8.1）**：mihomo 上游解析 `nameserver: 223.5.5.5`（UDP 53）**走物理 NIC 直出**（以太网 scoped Block 这两个 DNS IP 后，新域名经 wg-out 解析即失败）。结论：① 这是一处 DNS 泄漏面；② kill-switch 白名单**必须**显式放行 DNS 服务器 IP（否则 mihomo DNS 被默认 Block 拦死、mihomo 不可用），或给 wg-out 配 `remote-dns-resolve: true`/`proxy-server-nameserver` 让解析走隧道。**待补**：DoH(TCP443) 上游路径是否相同（本轮测的是 UDP nameserver）。（影响 VP-01 / VP-07 / §5 白名单设计）

### 中优先级（影响功能完整性）

6. ~~**UDP 进程匹配 bug**~~ → **已真机确认修复**（§0.8）：TCP（§0.5/§0.7）+ UDP 均按进程分流。UDP 用 `UdpClient` 发到 203.0.113.50:12345，mihomo 日志 `[UDP] ...(powershell.exe) ... match ProcessName(powershell.exe) using DIRECT` → **issue #1800 在 v1.19.27 已修复**。
7. **IPv6 double-stack**：mihomo issue #2254（ipv6:false 不完整）在最新版是否已修复？如已修复，双栈支持方案可提前到二期初期评估。
8. **混合 stack 稳定性**：mixed stack（TCP system + UDP gvisor）在 Windows 11 26200 是否稳定？是否存在 24H2 以后的兼容性问题？
9. **官方 WG + mihomo 并存**：若用户同时运行官方 WG 客户端和 mihomo，两个 WFP sublayer 是否冲突？WFP sublayer GUID 是否相同？（方案 B 推荐不并存，但可能存在用户误操作场景）

### 低优先级（二期或观察中）

10. **Chrome Network Service 进程名**：在 Windows 11 实机确认所有 chrome.exe 子进程的可执行文件名是否均为 chrome.exe（含 Network Service）。
11. **Tauri WebView2 子进程**：zero-desktop 的 WebView2 进程（msedgewebview2.exe）是否发网络请求，是否需要单独配置规则。
12. **NLA profile 重评估**：Windows NLA 检测到网络拓扑变化时重新评估 profile，是否会导致 kill-switch 规则在有线↔无线切换瞬间短暂失效。
13. **find-process-mode: always 性能**：当前版本是否有 PID→path 缓存？实际 CPU 开销是否可接受？
14. **WireGuard 重建接口 GUID**：WG 接口 GUID 重建后孤立 WFP filter 是否残留，是否影响网络行为。
15. **Firefox canary domain 效果**：use-application-dns.net NXDOMAIN 仅对自动启用的 DoH 有效，用户手动启用的 DoH 需要 policies.json 才能真正关闭，是否需要强制部署企业策略。

---

## 10. 来源

### WireGuard 官方

- https://git.zx2c4.com/wireguard-windows/about/docs/netquirk.md
- https://github.com/WireGuard/wireguard-windows/blob/master/tunnel/firewall/blocker.go
- https://github.com/WireGuard/wireguard-windows/blob/master/docs/netquirk.md
- https://deepwiki.com/WireGuard/wireguard-windows/4.1-network-configuration

### mihomo

- https://wiki.metacubex.one/en/config/inbound/tun/
- https://wiki.metacubex.one/en/config/proxies/wg/
- https://wiki.metacubex.one/en/config/dns/
- https://wiki.metacubex.one/en/config/rules/
- https://wiki.metacubex.one/en/config/general/
- https://github.com/MetaCubeX/mihomo/issues/1728
- https://github.com/MetaCubeX/mihomo/issues/2617
- https://github.com/MetaCubeX/mihomo/issues/2618
- https://github.com/MetaCubeX/mihomo/issues/1800
- https://github.com/MetaCubeX/mihomo/issues/2254
- https://github.com/MetaCubeX/mihomo/issues/854
- https://github.com/MetaCubeX/mihomo/issues/613
- https://github.com/MetaCubeX/mihomo/issues/322
- https://github.com/MetaCubeX/mihomo/discussions/1656
- https://github.com/mihomo-party-org/clash-party/issues/620

### Windows 防火墙 / WFP

- https://learn.microsoft.com/en-us/powershell/module/netsecurity/new-netfirewallrule?view=windowsserver2025-ps
- https://learn.microsoft.com/en-us/powershell/module/netsecurity/set-netfirewallrule?view=windowsserver2025-ps （v2 ByGroup 参数集证据）
- https://learn.microsoft.com/en-us/powershell/module/netsecurity/set-netfirewallprofile （v2 默认 Block 模型）
- https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/rules （v2 Block-vs-Allow 仲裁权威）
- https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-server-2008-r2-and-2008/cc755191(v=ws.10) （Block 击败 Allow 旧版表述）
- https://learn.microsoft.com/en-us/windows/win32/api/netfw/nf-netfw-inetfwrule-get_interfacetypes （IF_TYPE 映射未公开）
- https://learn.microsoft.com/en-us/windows-hardware/drivers/network/ndis-interface-types （Wintun IfType=53 PROP_VIRTUAL）
- https://github.com/WireGuard/wintun/blob/master/driver/wintun.inf （Wintun 适配器 IfType/MediaType 源码）
- https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/configure-with-command-line
- https://tailscale.com/blog/windows-firewall
- https://zeronetworks.com/blog/wtf-is-going-on-with-wfp
- https://metablaster.github.io/WindowsFirewallRuleset/ProblematicTraffic.html
- https://www.procustodibus.com/blog/2024/06/wireguard-windows-firewall/

### DNS

- https://learn.microsoft.com/en-us/powershell/module/dnsclient/add-dnsclientnrptrule?view=windowsserver2022-ps
- https://learn.microsoft.com/en-us/powershell/module/dnsclient/add-dnsclientnrptrule?view=windowsserver2025-ps （v2 NRPT 不支持 :port 证据）
- https://learn.microsoft.com/en-us/previous-versions/windows/desktop/ramgmtpsprov/dnsclientnrptrule （v2 NRPT WMI MOF schema）
- https://www.sans.org/reading-room/whitepapers/dns/preventing-windows-10-smhnr-dns-leakage-40165
- https://wiki.mozilla.org/Security/DNS_Over_HTTPS/Heuristics
- https://support.mozilla.org/en-US/kb/canary-domain-use-application-dnsnet
- https://cleanbrowsing.org/learn/how-to-disable-doh
- https://github.com/MetaCubeX/mihomo/discussions/1656
- https://windowsnews.ai/article/does-windows-11-fall-back-to-plain-dns-doh-privacy-settings-explained.417194

### IPv6

- https://learn.microsoft.com/en-us/troubleshoot/windows-server/networking/configure-ipv6-in-windows
- https://learn.microsoft.com/en-us/answers/questions/1195611/windows-11-how-to-permanently-disable-temporary-ip
- https://oneuptime.com/blog/post/2026-03-20-ipv6-vpn-leaks/view
- https://github.com/SagerNet/sing-box/issues/3858

### 进程规则

- https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/component/process/process_windows.go
- https://raw.githubusercontent.com/MetaCubeX/mihomo/Meta/rules/common/process.go
- https://chromium.googlesource.com/chromium/src/+/lkgr/services/network/README.md
- https://github.com/clash-verge-rev/clash-verge-rev

### 关联设计文档

- D:\git\github-commit-info\docs\unified-desktop-shell-design.md §14
