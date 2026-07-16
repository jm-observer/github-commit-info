# net-policy 抓包/解密（Phase 1–4）落地状态总表

> **单一权威进度页**：把散落在设计文档头部、两份 ADR、验证报告里的进度/状态/问题收拢到一处。
> 详细设计见 [net-policy-capture-design.md](net-policy-capture-design.md)；真机证物见
> [net-policy-capture-validation-report.md](net-policy-capture-validation-report.md)；引擎/数据面决策见
> [adr-2026-07-phase4-mitm-engine.md](adr-2026-07-phase4-mitm-engine.md) 与
> [adr-2026-07-phase3-npcap-backend.md](adr-2026-07-phase3-npcap-backend.md)。
>
> 更新：2026-07-16。真机：0.228（Windows 11 23H2 `10.0.22631`，SSH 管理员）。

## 1. 总览

| 阶段 | 内容 | 状态 |
|---|---|---|
| **Phase 1** | L2 sniffer（域名嗅探） | ✅ **完整**（core + GUI 开关 + 单测；默认关，真机 DNS/fake-ip 回填口径待补） |
| **Phase 2a** | 全 TUN 抓包（pktmon → pcapng） | ✅ **完整 + 真机 E2E 跑通** |
| **Phase 2b** | 定向抓包（进程/域名/IP）+ GUI | ◑ 解析逻辑 + 过滤器 + GUI 抓包页已落地；定向 E2E + fake-ip golden probe 待补 |
| **Phase 3** | npcap 后端 ADR（可选） | ✅ **已决策**：暂不引入 npcap、维持 pktmon |
| **Phase 4** | L4 应用明文（TLS MITM） | ◑ 协议/核心逻辑/自研引擎(`net-policy-mitm`)/脱敏 sink 完整；**方案 B 真机 E2E 解密成功**（curl→MITM→上游链 mihomo→200，明文脱敏落 http.jsonl）；剩工程化（CA 生命周期/DecryptManager/mihomo 自动导流/协议 handler） |

图例：✅ 完整 / ◑ 部分 / 🚫 阻断 / ⏳ 待办。

## 2. 已落地代码（机器无关，全带单测）

| 位置 | 内容 |
|---|---|
| `net-policy-core::mihomo` | Phase 1 `sniffer` 块生成 + `NetPolicySettings::sniffer_enabled`（默认关） |
| `net-policy-core::capture` | 抓包 DTO / §9 参数校验 / §5.2 过滤器预算（`plan_filters`）/ 状态机 / session-id 与读窗校验 / manifest schema |
| `net-policy-core::decrypt` | L4 会话/CA/目标 DTO / §17.5 参数与目标校验 / **§17.6 脱敏 + `HttpEvent` http.jsonl 事件构造器（golden 测试）** / 状态机 / artifact |
| `net-policy-agent::decrypt_sink` | **`DecryptSink`**（impl `net_policy_mitm::FlowSink`）：解密 flow → 脱敏 `HttpEvent` → 写 `http.jsonl` + 总字节配额 + 每域名计数（单测）+ **`run_mitm_spike`**（agent `mitm-spike` 子命令：起 loopback MITM 上游链 mihomo，方案 B E2E 已真机验证）。`net_policy_mitm::install_crypto_provider`（rustls ring） |
| `net-policy-core::protocol` | 管道协议 **1.6**：`Capture*`（11 错误码）+ `Decrypt*`/`DecryptCa*`（10 错误码）+ `Hello.capabilities` |
| `net-policy-agent::capture` | **真实 pktmon 后端**：`PktmonBackend` shell 封装 + `pktmon list --json` 组件解析 + `resolve_endpoints`（Phase 2b）+ `CaptureStore`（manifest/配额/分块读）+ `CaptureManager`（单会话状态机） |
| `net-policy-mitm`（新 crate） | **自研 Rust MITM 引擎**（fork 自 `D:\git\system-prompt-show`，解耦系统提示词/SQLite 层）：`cert`（CA + 按域名签证书）+ `upstream`（链 mihomo）+ `http`（请求/响应/body 解压/WS 解析）+ `proxy/mitm`（HTTP1/2 + WebSocket MITM）+ **`FlowSink` trait**（消费方插脱敏/落盘）+ `should_intercept` 域名 allowlist。Windows 编译通过，40 单测。**Phase 4 引擎候选，替代 mitmproxy（见 ADR §6.3）** |
| `net-policy-agent::mitm_engine` | mitmproxy 引擎部署（Defender 放行 + 下载/本地 zip + SHA-256 + 解压 + 卸载清理）。**若转用 `net-policy-mitm` 则此模块可退役** |
| `net-policy-agent::server` | 抓包 handler 接线 + `capture_v1` 能力声明；Decrypt* 返回 `decrypt_unsupported`（数据面未定案，不声明 `decrypt_v1`） |
| `net-policy-client` / `net-policy-cli` | `capture_*` 方法；CLI `set-route`/`apply`/`capture-start/-list/-stop/-save` 驱动命令 |
| `net-policy-gui`（Tauri + React） | 6 个 `net_policy_capture_*` 命令 + `NetPolicyAPI.capture*` + `CapturePage`（抓包页：确认框/完整包红标/分块下载 pcapng） |
| 安装程序 | NSIS `installer-hooks.nsh` POSTINSTALL 调 `install-mitm-engine`；uninstall 清引擎 + 撤 Defender 排除 |

测试：core 48 + agent 16 全过；`clippy --all-targets -D warnings` 零告警（core/agent/client/cli）；UI `tsc` + `cargo check -p net-policy-gui` 通过。

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

**未碰系统信任库**（全程 `--cacert`；实测 CurrentUser\Root 与 LocalMachine\Root 各 0 张 mitmproxy 证书）。

## 4. 未决问题 / 阻断项

| 编号 | 问题 | 状态 / 处置 |
|---|---|---|
| P-1 | mitmproxy PyInstaller 二进制被 **Windows Defender 查杀** | ✅ **已解决**：安装程序对引擎目录 `Add-MpPreference -ExclusionPath`（个人项目范围；产品化见 ADR §6 其余候选） |
| P-2 | ~~方案 A（Local Capture）× fake-ip 上游冲突~~ | ✅ **已解决**：改用自研 `net-policy-mitm`（显式代理 + 上游按域名链 mihomo），**方案 B 真机 E2E 解密成功**（`example.com` 拦截=200、透传=200、明文脱敏落 http.jsonl，ADR §6.4）。mitmproxy 方案 A 弃用 |
| P-3 | CA 生命周期（生成/DPAPI 私钥/CurrentUser\Root 安装与精确删除） | ⏳ Phase 4b，待 P-2 数据面定案后一并做（数据面未通前装 CA 无用且留 MITM 根证书） |
| P-4 | Phase 2b 定向抓包 E2E + fake-ip 四种地址口径 golden probe | ⏳ 需 TUN + 目标有活跃连接；解析逻辑与过滤器已就绪 |
| P-5 | agent 崩溃恢复（抓包租约 `active-capture.json` + orphaned 归属） | ⏳ 后续增量 |
| P-6 | sniffer 真机回归（DNS/fake-ip 回填、纯 IP、TLS、QUIC、ECH/无 SNI 降级） | ⏳ 未验证前不改新安装默认 |
| P-7 | 抓包同包多组件去重、Wireshark/tshark 实开 pcapng | ⏳ 观察项（测试机无 tshark） |

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
4. ⏳ **剩工程化**（不再有架构未知数）：① `DecryptManager`（会话状态机 + 起停 loopback 监听 + 每会话 CA/confdir）；
   ② mihomo **自动导流规则** `PROCESS-NAME,<目标>→http://127.0.0.1:<port>`（替代手动 `--proxy`；需真机验证不成环/
   出口语义不变）；③ CA 生命周期（DPAPI 私钥 + `CurrentUser\Root` 装/删）；④ 协议 handler 接线 + 声明 `decrypt_v1`；
   ⑤ force-TCP-for-QUIC / pinning 降级审计。②需改实时 mihomo 配置，需用户授权。
2. 方案 B 成立后：落 CA 生命周期（Phase 4b，DPAPI 私钥 + CurrentUser\Root 装/删）+ `DecryptManager` + 脱敏输出
   （复用 `net_policy_core::decrypt` 的脱敏逻辑）+ 协议 handler，声明 `decrypt_v1`。
3. Phase 2b 定向抓包 E2E + fake-ip golden probe（TUN 已起时可做）。
4. sniffer 真机回归（P-6）、抓包崩溃恢复（P-5）。
