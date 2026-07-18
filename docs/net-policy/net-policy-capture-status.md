# net-policy 抓包/解密（Phase 1–4）落地状态总表

> **单一权威进度页**：把散落在设计文档头部、两份 ADR、验证报告里的进度/状态/问题收拢到一处。
> 详细设计见 [net-policy-capture-design.md](net-policy-capture-design.md)；真机证物见
> [net-policy-capture-validation-report.md](net-policy-capture-validation-report.md)；引擎/数据面决策见
> [adr-2026-07-phase4-mitm-engine.md](adr-2026-07-phase4-mitm-engine.md) 与
> [adr-2026-07-phase3-npcap-backend.md](adr-2026-07-phase3-npcap-backend.md)。
>
> 更新：2026-07-17。真机：0.228（Windows 11 23H2 `10.0.22631`，SSH 管理员）。

## 1. 总览

| 阶段 | 内容 | 状态 |
|---|---|---|
| **Phase 1** | L2 sniffer（域名嗅探） | ✅ **完整**（core + GUI 开关 + 单测；默认关，真机 DNS/fake-ip 回填口径待补） |
| **Phase 2a** | 全 TUN 抓包（pktmon → pcapng） | ✅ **完整 + 真机 E2E 跑通** |
| **Phase 2b** | 定向抓包（进程/域名/IP）+ GUI | ◑ 解析逻辑 + 过滤器 + GUI 抓包页已落地；定向 E2E + fake-ip golden probe 待补 |
| **Phase 3** | npcap 后端 ADR（可选） | ✅ **已决策**：暂不引入 npcap、维持 pktmon |
| **Phase 4** | L4 应用明文（TLS MITM） | ◑ 功能与安全闭环已落地：自动导流、DPAPI、信任库、GUI、调用者 SID、随机代理认证、事务化启停；待 §18 真机回归矩阵 |

图例：✅ 完整 / ◑ 部分 / 🚫 阻断 / ⏳ 待办。

## 2. 已落地代码（机器无关，全带单测）

| 位置 | 内容 |
|---|---|
| `net-policy-core::mihomo` | Phase 1 `sniffer` 块生成 + `NetPolicySettings::sniffer_enabled`（默认关） |
| `net-policy-core::capture` | 抓包 DTO / §9 参数校验 / §5.2 过滤器预算（`plan_filters`）/ 状态机 / session-id 与读窗校验 / manifest schema |
| `net-policy-core::decrypt` | L4 会话/CA/目标 DTO / §17.5 参数与目标校验 / **§17.6 脱敏 + `HttpEvent` http.jsonl 事件构造器（golden 测试）** / 状态机 / artifact |
| `net-policy-agent::decrypt_sink` | **`DecryptSink`**（impl `net_policy_mitm::FlowSink`）：解密 flow → 脱敏 `HttpEvent` → 写 `http.jsonl` + 总字节配额 + 每域名计数（单测）+ **`run_mitm_spike`**（`mitm-spike` 诊断子命令：起 loopback MITM 上游链 mihomo，方案 B E2E 已真机验证）。`net_policy_mitm::install_crypto_provider`（rustls ring） |
| `net-policy-agent::decrypt_manager` | **`DecryptManager`**：CA 生命周期（生成 + **DPAPI machine-scope 私钥密文** `private/ca.key.dpapi` + 旧明文迁移 + `load_authority` 内存装配，**磁盘永不明文私钥**）+ 会话状态机 + store + 分块读 + `active_divert()`（生成自动导流描述）+ `ca_export_public`（导公钥给 GUI 装信任库）。全带单测 |
| `net-policy-agent::dpapi` | **DPAPI 封装**：`protect_machine`（`CRYPTPROTECT_LOCAL_MACHINE`）/`unprotect`；非 Windows 退化仅供 CI round-trip。Windows 真机 round-trip 单测通过 |
| `net-policy-core::routes` / `mihomo` | **L4 自动导流 config-gen**：进程+allowlist 域名+TCP 80/443 置顶规则；`mitm-out` 注入每会话随机 Basic Auth；QUIC 回退规则保持进程+域名作用域。全带单测 |
| `net-policy-mitm::sink` / `proxy` | **QUIC/pinning 降级审计**（§17.7/§17.9）：`FlowSink` 加 `on_passthrough`/`on_client_cert_rejected`（默认空）；proxy 透传分支 + 客户端拒证（TLS alert）回调；`DecryptSink` 累加 per-domain `passthrough`/`pinned`，诚实不宣称解密 |
| `net-policy-gui::ca_trust` / `net_policy` | **CA 信任库安装 + L4 命令**：`install_current_user_root`（certutil 装 `CurrentUser\Root` 弹确认框 + PowerShell 按 DER SHA-256 **实查验证**）+ 精确删 + 取 SID；11 个 `net_policy_decrypt_*` tauri 命令 + `DecryptPage`（CA 卡片 + 会话 + **每域名 decrypted/passthrough/pinned/quic/failed** + Raw 红标 + 常驻「TLS 解密中」+ 二次确认） |
| `net-policy-core::protocol` | 管道协议 **1.6**：`Capture*`（11 错误码）+ `Decrypt*`/`DecryptCa*`（10 错误码）+ `Hello.capabilities` |
| `net-policy-agent::capture` | **真实 pktmon 后端**：`PktmonBackend` shell 封装 + `pktmon list --json` 组件解析 + `resolve_endpoints`（Phase 2b）+ `CaptureStore`（manifest/配额/分块读）+ `CaptureManager`（单会话状态机） |
| `net-policy-mitm`（新 crate） | **自研 Rust MITM 引擎**（fork 自 `D:\git\system-prompt-show`，解耦系统提示词/SQLite 层）：`cert`（CA + 按域名签证书）+ `upstream`（链 mihomo）+ `http`（请求/响应/body 解压/WS 解析）+ `proxy/mitm`（HTTP1/2 + WebSocket MITM）+ **`FlowSink` trait**（消费方插脱敏/落盘）+ `should_intercept` 域名 allowlist。Windows 编译通过，40 单测。**Phase 4 引擎候选，替代 mitmproxy（见 ADR §6.3）** |
| `net-policy-agent::mitm_engine` | mitmproxy 引擎部署（Defender 放行 + 下载/本地 zip + SHA-256 + 解压 + 卸载清理）。**若转用 `net-policy-mitm` 则此模块可退役** |
| `net-policy-agent::server` | 从命名管道客户端 PID/token 提取真实 SID并绑定 CA owner；无环境变量绕过。启动等待 bind+reload，停止先撤导流成功再关代理；失败保持可重试状态 |
| `net-policy-client` / `net-policy-cli` | `capture_*` + `decrypt_*`（含 `decrypt_ca_export_public`）方法；CLI `set-route`/`apply`/`capture-*` 驱动命令 |
| `net-policy-gui`（Tauri + React） | 抓包页 + **应用明文页**（`DecryptPage`）；17 个 `net_policy_capture_*`/`net_policy_decrypt_*` 命令 + `NetPolicyAPI.capture*`/`decrypt*` + `ca_trust`（信任库安装） |
| 协议 | 1.6：`DecryptCaExportPublic` 请求 + `DecryptCaPublic` 响应（导公钥给 GUI 装信任库；追加式，私钥永不出管道） |
| 安装程序 | NSIS `installer-hooks.nsh` POSTINSTALL 调 `install-mitm-engine`；uninstall 清引擎 + 撤 Defender 排除 |

测试：core 57 + agent 26 全过；`clippy --all-targets -D warnings` 零告警（core/mitm/agent/client/gui）；UI `tsc` + `cargo check -p net-policy-gui` 通过。DPAPI round-trip / CA 生命周期 / 迁移 / 导流 config-gen / 审计计数均有单测。

## 3. 真机验证（0.228）

### 3.1 Phase 2a 全 TUN 抓包 E2E ✅

apply 直连观察姿态 → Meta TUN 起（`tun_ready=true`，防火墙不阻断）→ `find_tun_component` 唯一命中
`Group="Meta Tunnel"` miniport `Id=76` → `capture-start` → 真实 HTTPS/DNS 流量 → `capture-stop`
（stop→etl2pcap→**pcapng 魔数 `0A 0D 0D 0A` 校验**→写 manifest→删 ETL）→ `capture-save` 分块下载
**1432B 合法 pcapng**。§15 验收矩阵第 1/2/4/5 项通过。

### 3.2 Phase 4 解密数据面 spike ◑（真实解密成立，上游冲突）

`mitmdump --mode local:curl.exe`（WinDivert）在 fake-ip 直连姿态下：

- ✅ **WinDivert 加载 + 与 mihomo Wintun 共存无冲突**（spike 后 mihomo/TUN 仍健康）。
- ✅ **Local Capture 只截目标进程**（curl.exe）。
- ✅ **真实解密成立**：解出明文 `GET / Host: example.com`。
- 🚫 **上游 502**：见 §4 问题 P-2。

### 3.3 Phase 4 四项后续开发真机验证（2026-07-17，授权后）✅ 核心通过

部署协议 1.6 + 四项功能的新 agent（含 DPAPI/导流/审计）+ CLI decrypt 子命令到 0.228，按命名管道
客户端真实 SID 动态开放能力：

- ✅ **身份 gate**：只对能从命名管道客户端 token 取得真实 SID 的连接声明/开放 `decrypt_v1`；无环境变量绕过。
- ✅ **② DPAPI 私钥静态保护**：以 LocalSystem 服务创建 CA → `mitm/private/ca.key.dpapi` 为 **DPAPI 密文**
  （470B，`contains PRIVATE KEY = False`），**无明文 `ca.key`**，`private/` ACL 仅 `SYSTEM`+`Administrators`。
- ✅ **① 自动导流 config-gen 真机注入**：`decrypt-start`（目标 curl.exe / allowlist）→ 生成配置出现
  `- name: mitm-out`（http 127.0.0.1:18081）+ `AND,((PROCESS-PATH,...curl.exe),(NETWORK,tcp),(DST-PORT,80|443)),mitm-out`。
- ✅ **MITM 拦截真机 engage**：curl 经导流/直连代理 → 日志 `MITM TLS established (ALPN: http/1.1)` +
  `Starting HTTP/1.1 interception`（对 www.microsoft.com）。
- ✅ **CA 协议生命周期**：create/status/export（公钥 PEM）/confirm（指纹+SID）/remove 全通。
- 🐛 **修复两个真机 bug**（见 §4 P-8/P-9）：`harden_machine_data_dir` 空 DACL 损坏、① 导流 reload 被自身
  会话 op 互斥挡死。修后 workspace 自愈（`record_store_degraded=false`）、导流规则正确注入。
- ⏳ **未闭环（环境阻）**：① 全绿路 plaintext http.jsonl E2E——(a) example.com 中国直连不可达（mihomo 7890
  实测 000，microsoft 200）；(b) **Windows curl 用 Schannel，`--cacert` 不生效**，需 CA 真装系统证书库才信任
  伪造叶子——而信任库安装**需交互桌面**（headless certutil/X509Store 均 `ERROR_NOT_SUPPORTED`）。解密机制
  本身已在 §3.2/ADR §6.4 spike 用 example.com 证过（http.jsonl 明文）。GUI（桌面）装信任库 + reachable 域名
  的全绿路 E2E 待桌面环境跑。

**未碰系统信任库**（全程 `--cacert`；实测 CurrentUser\Root 与 LocalMachine\Root 各 0 张 mitmproxy 证书）。

## 4. 未决问题 / 阻断项

| 编号 | 问题 | 状态 / 处置 |
|---|---|---|
| P-1 | mitmproxy PyInstaller 二进制被 **Windows Defender 查杀** | ✅ **已解决**：安装程序对引擎目录 `Add-MpPreference -ExclusionPath`（个人项目范围；产品化见 ADR §6 其余候选） |
| P-2 | ~~方案 A（Local Capture）× fake-ip 上游冲突~~ | ✅ **已解决**：改用自研 `net-policy-mitm`（显式代理 + 上游按域名链 mihomo），**方案 B 真机 E2E 解密成功**（`example.com` 拦截=200、透传=200、明文脱敏落 http.jsonl，ADR §6.4）。mitmproxy 方案 A 弃用 |
| P-3 | CA 生命周期（生成/DPAPI 私钥/CurrentUser\Root 安装与精确删除） | ◑ 代码闭环：DPAPI + ACL + 信任库实查 + 管道调用者 SID/owner 绑定；剩 Windows 真机装删与多用户隔离验收 |
| P-4 | Phase 2b 定向抓包 E2E + fake-ip 四种地址口径 golden probe | ⏳ 需 TUN + 目标有活跃连接；解析逻辑与过滤器已就绪 |
| P-5 | agent 崩溃恢复（抓包租约 `active-capture.json` + orphaned 归属） | ⏳ 后续增量 |
| P-6 | sniffer 真机回归（DNS/fake-ip 回填、纯 IP、TLS、QUIC、ECH/无 SNI 降级） | ⏳ 未验证前不改新安装默认 |
| P-7 | 抓包同包多组件去重、Wireshark/tshark 实开 pcapng | ⏳ 观察项（测试机无 tshark） |
| P-8 | **`harden_machine_data_dir` 把 workspace 文件锁成空 DACL**（`record_store_degraded` 之源） | ✅ **已修**（真机 2026-07-17）：旧 `icacls /grant:r "SID:(OI)(CI)F" /T` 中 `(OI)(CI)` 对**文件**是「仅继承」标志，用 `/T` 打到文件上时文件本身无有效授权 → 空 DACL 拒绝所有人（连 SYSTEM 都读不了 settings.json/net-policy.db）。改为 `takeown /R`（自愈旧损坏文件）→ `icacls /reset /T`（子节点继承）→ 只在**根**设可继承授权（不带 `/T`）。真机验证：服务启动即自愈 workspace，`record_store_degraded=false` |
| P-9 | **① 导流 reload 被自身活跃会话的 op 互斥挡死** | ✅ **已修**（真机 2026-07-17）：`try_begin_op` 有活跃 decrypt/capture 会话即 Conflict（§7，挡用户 apply/reload/stop）；但 `spawn_divert_reload` 在会话活跃时跑 reload → 永远 Conflict → 导流永不生效。加 `try_begin_op_internal`（会话生命周期内部用，只与并发 op 互斥、不因自身会话 Conflict）+ 内部 reload 短重试。真机验证：导流规则正确注入实时配置 |

## 5. 0.228 机器当前状态

- 已部署**新 agent（协议 1.6，含真实 pktmon 后端）+ net-policy CLI** 到 `C:\Program Files\net-policy\`。
- mitmproxy 引擎在 `C:\Program Files\net-policy\mitm\engine\12.2.3\`（Defender 已排除）。
- **直连观察姿态 apply 中**（mihomo + Meta TUN 运行；`enabled=false` 不跨重启持久）。撤销：`net-policy stop`。
- spike 产物在 `C:\ProgramData\net-policy\{captures,decrypt-spike,mitm-spike,deploy}\`（decrypt-spike 的 CA
  仅在隔离 confdir，**未安装到信任库**）。
- 信任库干净（0 张 mitmproxy 证书）；无残留 mitmdump 进程。

## 6. 下一步（按优先级）

1. ✅ **自研 Rust MITM 已 fork 进 `crates/net-policy-mitm`**（Windows 编译 + clippy + 40 单测，见 ADR §6.3）。
2. ✅ **脱敏 `FlowSink` 已落地**：`net_policy_core::decrypt::HttpEvent`（脱敏 http.jsonl 事件构造器）+
   `net_policy_agent::decrypt_sink::DecryptSink`（写 http.jsonl + 配额 + 每域名计数），纯代码 + 单测，不碰网络。
3. ✅ **方案 B 真机 E2E 解密成功**（ADR §6.4）：agent `mitm-spike` 子命令用 `net_policy_mitm::proxy::run_proxy`
   起 loopback MITM（上游 `http://127.0.0.1:7890`）+ `DecryptSink`；curl 经代理拦截 `example.com`=200（上游链
   mihomo 无 fake-ip 502）、透传 microsoft=200、明文脱敏落 http.jsonl。修了 rustls CryptoProvider（`install_crypto_provider`）。
4. ✅ **`DecryptManager` + 协议 handler + client 方法**：CA 生命周期 + 会话状态机 + 分块读，全带单测。
5. ✅ **① mihomo 自动导流 config-gen**（§17.3 方案 B）：`DecryptDivert` + `divert_lines`（`AND(PROCESS,
   NETWORK tcp,DST-PORT 80/443)→mitm-out` 置顶 + `mitm-out` http outbound；防环=只匹配目标进程，上游 agent
   连接不复命中）；`DecryptManager::active_divert()` 注入 apply/reload；server start/stop 后 best-effort
   reload。**代码 + 单测全绿；真机「不成环/出口语义不变」E2E 仍需授权跑**（改实时 mihomo 配置）。
6. ✅ **② CA DPAPI 私钥保护 + 真装信任库**：agent DPAPI machine-scope 密文 `private/ca.key.dpapi`（磁盘永不
   明文 + 旧明文迁移 + SYSTEM-only ACL）；GUI `ca_trust` 用 certutil 装 `CurrentUser\Root`（弹确认框）+
   PowerShell 按 DER SHA-256 **实查验证** + 取 SID → agent 复核（指纹须一致）。**Windows 真机信任库装/删验收仍需跑**。
7. ✅ **③ GUI L4 页**（`DecryptPage`）：CA 卡片（创建/安装/移除）+ 会话（进程选择 + 域名 allowlist + opts）+
   **每域名 decrypted/passthrough/pinned/quic/failed** + Raw 红标 + 常驻「TLS 解密中」+ 二次确认。
8. ✅ **④ QUIC/pinning 降级审计**（§17.7/§17.9）：透传 → per-domain `passthrough`；客户端拒证（TLS alert）
   → `pinned`（诚实不宣称解密）；`force_tcp_for_quic` 生成进程+域名+UDP/443 REJECT 逼回退。
9. ⏳ **剩余验收**：§18 真机矩阵（信任库多用户隔离 / pinning-mTLS-QUIC 样本 / 卸载无残留 /
   脱敏 golden）；Raw 15 分钟删除。调用者 SID、代理认证和总字节配额已闭环。
10. ⏳ Phase 2b 定向抓包 E2E + fake-ip golden probe；sniffer 真机回归（P-6）、抓包崩溃恢复（P-5）。
