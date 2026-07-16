# ADR：net-policy L4（应用明文 / TLS MITM）引擎选型与依赖

- 状态：**已接受（用户显式批准引擎依赖，2026-07-16）**，进入 Phase 4a 真机 spike。
- 关联设计：[net-policy-capture-design.md](net-policy-capture-design.md) §17–§18（L4 应用明文设计与验收门槛）。
- 决策人：设备所有者（fengqi）。真机：0.228（Windows 11 23H2，`10.0.22631`，SSH 管理员）。

> 本 ADR 满足设计 §17.2 / §18.1 对「Phase 4 开工前必须提交 ADR，列明版本锁定、许可证、资产大小、
> CVE/更新策略、签名/哈希校验和卸载方案，并取得用户批准」的要求。

## 1. 背景

L4 需要主动终止客户端 TLS、由代理向服务端另建 TLS，并为目标域名动态签发叶子证书。设计 §17.2 给出
四个候选（mitmproxy sidecar / mihomo 定向到本地 MITM / 自研 Rust MITM / 商业 SDK），并要求先 spike。
自研 TLS/HTTP2/3+证书+WebSocket 的成本极高（§17.2 明确不采用）；商业 SDK 有许可/闭源/分发约束。

## 2. 决策

> **⚠ 决策已被 §6.4 的真机结果推翻**：mitmproxy 方案 A 在 mihomo fake-ip 下上游 502（§6.2）；改用自研
> Rust MITM（`crates/net-policy-mitm`）+ 方案 B（显式代理 + 上游按域名链 mihomo）**已真机 E2E 解密成功**
> （§6.4）。**现行引擎 = `net-policy-mitm`**；下方 mitmproxy 表保留作历史与对比。

**（历史）引擎 = mitmproxy（`mitmdump.exe` sidecar），锁定版本 v12.2.3。**

| 项 | 值 |
|---|---|
| 名称 | mitmproxy（`mitmdump` 无头模式） |
| 版本 | **12.2.3**（锁定；升级须新 ADR 修订） |
| 许可证 | **MIT**（宽松，允许再分发；无 copyleft 传染） |
| 分发形态 | 官方自包含 Windows zip（内置 CPython + OpenSSL，**无需在目标机装 Python**） |
| 资产 | `mitmproxy-12.2.3-windows-x86_64.zip`，**81.7 MB** |
| 来源 | `https://downloads.mitmproxy.org/12.2.3/mitmproxy-12.2.3-windows-x86_64.zip`（HTTP 200 实测；GitHub release 自 12.x 起 0 资产，二进制迁至官方下载站） |
| SHA-256 | 见 §6（真机下载后实测记录，写死进安装校验） |

**数据面：先验证方案 A（mitmproxy Local Capture，`mode local:<pid>`），A 不成立再退方案 B（mihomo 定向路由到本地 MITM）。** 两者判据见设计 §17.3。若 A/B 均不满足「最少改动数据面 / 精确目标 / 无递归 / 出口语义不变 / 可彻底回滚」，则 L4 保持不实现，不以全局系统代理替代。

## 3. 理由

- mitmproxy 具备成熟 HTTP/1.1、HTTP/2、WebSocket 与动态证书能力，且提供 addon 机制写结构化明文索引
  （满足 §17.6「固定随安装包发布、启动前校验哈希的最小 addon」）。
- 自包含 zip 让供应链面收敛为「一个可校验哈希的 zip」，不在目标机引入 pip/系统 Python 依赖树。
- MIT 许可无再分发障碍。

## 4. 供应链与安全约束（写死进实现）

1. **哈希校验**：安装器只接受 §6 记录的 SHA-256；不匹配即拒绝，不静默继续。
2. **离线安装**：zip 可预置随产品分发；agent 不在运行期联网拉引擎。
3. **资产隔离**：引擎（可执行体）解压到 **`%ProgramFiles%\net-policy\mitm\engine\<version>\`**——与 mihomo
   同放受保护 ProgramFiles（普通用户只读，符合 D6「绝不执行用户可写目录里的东西」）；CA 私钥 / 明文产物等
   **数据**才落 `%ProgramData%\net-policy\`（ACL 仅 SYSTEM + Administrators）。由 agent 安装子命令
   `install-mitm-engine` / `install --mitm-zip` 落地（`crates/net-policy-agent/src/mitm_engine.rs`）。
4. **addon 固定**：结构化明文索引 addon 随产品发布并启动前校验哈希；**禁止**从 workspace / 请求参数 /
   环境变量加载任意 Python 脚本（否则普通用户可借 LocalSystem sidecar 执行代码，§17.6）。
5. **进程排除**：mitmdump 自身 PID 必须排除、其出站仍交现有 TUN/路由，避免捕获递归（§17.3）。
6. **CVE/更新策略**：订阅 mitmproxy GitHub security advisories；升级 = 新版本号 + 新 SHA-256 + 重跑
   §18 全验收矩阵 + 修订本 ADR。不做自动升级。

## 5. 卸载方案（§18.2 / §18.5）

- 停所有 L4 会话 → 删临时转发/阻断规则 → 从 `CurrentUser\Root` 按 `thumbprint` 精确删本产品 CA →
  DPAPI 安全删私钥 → 删明文产物 → 删引擎目录。卸载后验证浏览器证书库无本产品 CA、无残留临时规则。

## 6. 真机记录（Phase 4a spike，0.228）

> 本节随 spike 进展追加实测证物；未完成项标 ⏳。

- 环境：admin=True、pktmon 在、**无系统 Python**（`WindowsApps\python.exe` 为 Store stub）、
  net-policy-agent 服务 Running（PID 4240）、**当前无 mihomo 进程 / 无 TUN 适配器**（直连观察姿态）。
- 下载：`downloads.mitmproxy.org/12.2.3/mitmproxy-12.2.3-windows-x86_64.zip`，实测 **81.67 MB**，HTTP 200。
- **引擎 SHA-256（实测）= `04A01EA95AE96DF75058A893E774957D294E69012DAB1F4E256CE2B0C6725483`**。
- 解压：`mitmdump.exe`(27.5MB) / `mitmproxy.exe`(28.8MB) / `mitmweb.exe`(29.3MB)，均为 PyInstaller onefile。
- **Defender 查杀（初次冷启动阻断）**：首次解压后 **Windows Defender 主动查杀 `mitmdump.exe`**
  （`Get-MpThreatDetection` 实录 2026-07-16 12:14:30 命中，文件被隔离）。PyInstaller 打包体 + 动态签发叶子
  证书的 MITM 行为命中启发式。**已解决（见 §6.1）**：部署时对引擎目录加 Defender 排除后，二进制存活。
- **✅ `mitmdump --version`（部署后实测）**：`Mitmproxy: 12.2.3 binary / Python: 3.14.4 /
  OpenSSL 3.5.5 / Platform: Windows-11-10.0.22631`——引擎冷启动成功。

### 6.1 Defender 处置（用户已决策）

设备所有者确认这是**个人使用项目**，接受由安装程序自动加 Defender 排除项作为部署机制（`Add-MpPreference
-ExclusionPath <ProgramFiles>\net-policy\mitm\engine`）。已落地到 agent 部署逻辑
（`mitm_engine.rs::deploy`：Defender 放行 → 下载/本地 zip → SHA-256 校验 → 解压 → 存活校验），并**真机
端到端验证通过**（`DEFENDER=excluded` → `SHA256` 匹配 → `STATUS=deployed` → 6s 后 `POST_EXISTS=True` →
`--version` 跑通）。

> 产品化（非个人）分发若不想每台机排除 MITM 引擎目录，仍应走 §6 其余候选（自签名提信誉 / 换非 PyInstaller
> 分发 / 换引擎）；本 ADR 的排除方案限个人项目范围。

### 6.2 数据面 spike 实测（2026-07-16，用户授权装 CA + WinDivert 后）

在 0.228 直连观察姿态（Meta TUN 起、fake-ip 生效）下用 `mitmdump --mode local:curl.exe` 做方案 A spike：

- ✅ **WinDivert 加载成功、与 mihomo Wintun 共存无驱动冲突**（doc §18.1 头号阻断项清除）：mitmdump 冷启动
  正常、无 stderr 错误、CA 自动生成，网络栈未崩。
- ✅ **Local Capture 拦截目标进程成立**：mitmdump 捕获到 curl.exe 的连接（`client connect` → `server connect`），
  非目标进程不受影响（仅 `curl.exe` 被 WinDivert 重定向）。
- ✅ **真实解密成立**：mitmdump 解出 curl 的**明文 HTTP** —— `GET / Host: example.com`（TLS 内层被解密）。
- 🚫 **方案 A 与 fake-ip 架构冲突（关键阻断）**：curl 把域名解析成 mihomo fake-ip（`198.18.0.16`），WinDivert
  在**DNS 解析之后**抓包，mitmdump 拿到的原始目的地是 fake-ip；直连该 fake-ip 做上游 TLS 失败（fake-ip 只有
  mihomo TUN 懂），即使 `connection_strategy=lazy` 仍按原始 IP 连上游 → **上游 502**。这正是 §17.3/§3.1
  预警的 fake-ip × Local Capture 冲突：Local Capture 看到的是包面 fake-ip，无法可靠还原真实上游。

**结论**：方案 A（mitmproxy Local Capture）在 mihomo **fake-ip** 模式下**上游路由不成立**，不满足「出口语义
不变」判据。不把半通方案强推进生产（§17.3：两者均不满足则 L4 不实现）。可行方向（择一，需后续 spike）：

1. **方案 B**（doc §17.3）：由 mihomo 生成最高优先级临时规则，把目标进程的 TCP HTTP(S) **按域名**送到 loopback
   mitmdump（regular/reverse 模式），上游出口仍由 mihomo 按原策略选——保真域名、绕开 fake-ip 包面歧义。
2. 或让 mitmdump 上游走 mihomo 的 mixed-port HTTP 代理（按 Host/SNI 连接，避免 fake-ip）。
3. 或对被解密目标临时关 fake-ip（redir-host DNS），改动面大、影响全局，不优先。

CA 信任库安装（`CurrentUser\Root`）本轮**未做**：上游数据面未通前装 CA 不产生可用解密，且会在信任库留一张
MITM 根证书；待方案 B/上游修复后再纳入 Phase 4b 一并验证与清理。本轮用 `curl --cacert` 证明解密机制，
**未修改系统信任库**。

- CA 生成 / DPAPI 私钥 / CurrentUser\Root 信任安装与精确删除：⏳（Phase 4b，待数据面方案 B 定案）。
- 协议/CA/会话/脱敏的机器无关逻辑层已落地并单测（`net_policy_core::decrypt` + 协议 1.6）。

### 6.3 更优路径（提案）：复用自研 Rust MITM 替代 mitmproxy

发现 `D:\git\system-prompt-show`（用户自研）已是一个**完整的 Rust MITM 显式正向代理**（rustls + rcgen + h2 +
tokio-socks，~4800 行），且其架构**天然同时解掉 P-1 与 P-2**：

- 流程（`src/proxy/mod.rs::handle_connection`）：解析 CONNECT 拿**真实域名** → **上游按域名链到 mihomo**
  （`src/upstream`，`socks5://127.0.0.1:7890` / http CONNECT，注释明确「never resolve DNS internally」）→
  非 allowlist 域名走纯 TCP 隧道透传、allowlist 域名才 MITM（`should_parse_http`）。**这正是 §17.3 方案 B，已跑通。**
- **解 P-1（Defender）**：纯 Rust、编译成自有可签名 exe，无 PyInstaller 启发式查杀、无 Python/OpenSSL 供应链。
- **解 P-2（fake-ip）**：显式代理拿真实域名 + 上游按域名链 mihomo → 全链路无 fake-ip；mihomo 按 CONNECT 域名
  选出口，天然满足「服务端连接保留原本 direct/WG 出口」。
- **不需依赖 ADR**：自有代码，fork/vendor 进 toolkit 即可，绕开新增第三方大依赖评审。§17.2 当初把「自研 Rust
  MITM」判为「不采用（成本极高）」——**该成本已由 system-prompt-show 付清**，前提改变，故重开此选项。

**可直接复用**：`cert/ca.rs`（CA 生成/加载）、`cert/site.rs`（按域名签叶子证书 + 缓存）、`proxy/connect.rs` +
`proxy/mod.rs`（CONNECT + 代理入口）、`proxy/mitm/*`（HTTP/1.1 + HTTP/2 + WebSocket MITM 核心）、`upstream/*`
（上游链 mihomo）、`http/body.rs`（gzip/brotli 解压）。**需新做**：把目标进程导入代理——mihomo 加规则
`PROCESS-NAME,<目标>` → 出口 = `http://127.0.0.1:<mitmport>`（http 型 outbound 指 loopback MITM）；loopback
127/8 直连不被 TUN 再抓 → 无递归。**Windows 移植成本**：栈全跨平台，主要是文件权限 → ACL/DPAPI（§17.4 本就要求）
+ 真机编译跑通（system-prompt-show 标 "Linux only" 仅为测试范围，非硬锁）。

**建议**：Phase 4b 起以此替代 mitmproxy 引擎；mitmproxy spike 的结论（WinDivert 可用但 fake-ip 上游不通）
作为「为何转显式代理 + 上游链 mihomo」的依据保留。待用户确认后修订本 ADR 决策（§2）。

**进展（2026-07-16）**：已 fork 进 `crates/net-policy-mitm`——`cert`（CA + 按域名签证书）+ `upstream`（链 mihomo）
+ `http`（解析/解压）+ `proxy/mitm`（HTTP1/2 + WebSocket MITM），解耦 system-prompt-show 的提取/SQLite 层为通用
`FlowSink` trait + `should_intercept` allowlist。Windows `cargo build/clippy -D warnings/test`（40 单测）全绿，无
sqlite/sps 依赖，遗留耦合扫描为空。

### 6.4 方案 B（自研 `net-policy-mitm`）真机 E2E —— ✅ 成功

在 0.228 直连姿态（mihomo + Meta TUN 起、fake-ip 生效）下，用 agent 诊断子命令
`net-policy-agent mitm-spike --listen 127.0.0.1:18080 --upstream http://127.0.0.1:7890 --domains example.com`
起前台 loopback MITM（上游按域名链 mihomo 混合端口 7890），`DecryptSink` 写 http.jsonl：

- ✅ **拦截解密 `example.com` = HTTP 200**：`curl --proxy 127.0.0.1:18080 --cacert <ca> https://example.com` 成功
  —— **上游经 mihomo 按域名解析路由，彻底无 fake-ip 502**（方案 A 的死结解决）。
- ✅ **非白名单域名透传**：`www.microsoft.com` = 200（纯 TCP 隧道，不解密）。
- ✅ **真实解密明文落盘**：http.jsonl 两行——`{kind:request, GET / Host:example.com}` +
  `{kind:response, status:200, body:"<!doctype html>...Example Domain..."}`，经 `net_policy_core::decrypt` 脱敏。
- 修复：rustls 0.23 需进程启动装 `CryptoProvider`——加 `net_policy_mitm::install_crypto_provider()`（ring），
  消费方启动调一次。全程 `--cacert`，**未装 CA 到信任库**（实测两 Root 存均 0 张）。

**结论**：L4 真实解密数据面（方案 B）**架构成立、真机验证通过**，不再有未知数。**剩余是工程化**：
CA 生命周期（DPAPI 私钥 + `CurrentUser\Root` 装/删）、`DecryptManager`（会话状态机 + 起停 loopback 监听）、
mihomo 自动导流规则（`PROCESS-NAME,<目标>→http://127.0.0.1:<port>`，替代手动 `--proxy`）、协议 handler +
声明 `decrypt_v1`、force-TCP-for-QUIC / pinning 降级审计。

## 7. 后果

- 正面：复用成熟引擎，避开自研 TLS 栈；供应链可哈希校验；MIT 无许可风险。
- 负面：+81.7 MB 资产；引入 mitmproxy/CPython/OpenSSL 的 CVE 跟踪义务；Local Capture 依赖 WinDivert
  内核驱动，须真机证明与 Wintun 不冲突（§18.1 阻断项）。
- 回滚：若 4a 判据不通过，删引擎目录即回到无 L4 状态，不影响 L1–L3 与三姿态。
