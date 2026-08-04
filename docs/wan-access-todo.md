# 外网接入（caddy TLS 入口）— 待办 TODO

> 状态:待排期  ·  记录日期:2026-08-04  ·  范围:`zero-desktop`(WS 客户端) · G10 caddy 入口
>
> 关联:[settings.rs](../crates/zero-desktop/src/shared/settings.rs)(局域网/外网两 host,协议端口
> 按部署事实派生) · [speech/commands/remote.rs](../crates/zero-desktop/src/modules/speech/commands/remote.rs)
>
> 背景:外网入口 `spark.for-memory.site:38788` 经 G10 上的 caddy 做 TLS 终止 → 明文转 `127.0.0.1:8788`,
> 故外网档协议是 **https / wss**(局域网档是 http / ws)。

---

## TODO-1 外网档语音识别连不上:`TLS support not compiled in`

- [ ] **现象**(2026-08-04,桌面端切外网):

  ```text
  无法连接识别服务 wss://spark.for-memory.site:38788/api/asr/stream:
  URL error: TLS support not compiled in
  ```

- **判断**:**这是 zero-desktop 的依赖特性漏配,与 caddy / 证书 / 网络都无关**。
  [`Cargo.toml:52`](../crates/zero-desktop/Cargo.toml:52) 里 `tokio-tungstenite = "0.24"`
  **没开任何 TLS feature**;该 crate 默认特性不含 TLS 连接器,于是
  [`remote.rs:575`](../crates/zero-desktop/src/modules/speech/commands/remote.rs:575) 的
  `connect_async` 一遇到 `wss://` 就直接在 URL 阶段报 `TLS support not compiled in`。
  - 局域网档是 `ws://192.168.x.x:8788/api/asr/stream`(明文),所以**一直没暴露**——问题只在
    外网档出现。
  - 对照:同 workspace 里 `orchestrator` / `toolkit-server` 也用 tokio-tungstenite,但它们连的是
    **本机回环的 ws://**(FunASR :9100、pronunciation-assess :8098),不需要 TLS,所以那两处不用改。

- **待办**:
  1. 给 zero-desktop 的 `tokio-tungstenite` 开 TLS feature —— 与仓库既有取向一致选 **rustls**
     (workspace 的 `reqwest` 已是 `rustls`,不引 native-tls 免得多拖一套 OpenSSL);
     同时确认 `connect_async` 是否需要换成带 connector 的调用形式(0.24 的 `connect_async`
     在开启对应 feature 后可直接支持 wss)。
  2. **顺带检查跟读流式评测**:`shadow_stream_endpoint`
     ([settings.rs:247](../crates/zero-desktop/src/shared/settings.rs:247))外网档同样派生 `wss://`。
     当前该端点由 webview 侧发起还是 Rust 侧发起要确认——**若也走 tokio-tungstenite,则同一处
     修复顺带覆盖;若走 webview 原生 WebSocket 则不受影响**。
  3. 回归:外网档跑一次实时语音识别 + 一次跟读流式评测,确认握手成功且能出结果。

- **验收**:桌面端切「外网」档,语音识别能连上 `wss://spark.for-memory.site:38788/api/asr/stream`
  并正常出字;局域网档行为不变。

---

## 备忘:外网档的既有约定(别改错了)

- **38788 → caddy(TLS 终止) → G10:8788 = toolkit-server**;**28080 是 english 自己的入口**
  (自持同一份证书),两者是不同端口的不同服务,别混。
- 桌面端只配「局域网 IP / 外网域名」两个 host,协议 / 端口 / ASR 路径全部按档位派生
  (见 `NetScheme`),`auto` 模式经 health 探测自动选路。
