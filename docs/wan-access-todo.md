# 外网接入（caddy TLS 入口）— 待办 TODO

> 状态:部分完成  ·  记录日期:2026-08-04  ·  范围:`zero-desktop`(WS 客户端) · G10 caddy 入口
>
> 关联:[settings.rs](../crates/zero-desktop/src/shared/settings.rs)(局域网/外网两 host,协议端口
> 按部署事实派生) · [speech/commands/remote.rs](../crates/zero-desktop/src/modules/speech/commands/remote.rs)
>
> 背景:外网入口 `spark.for-memory.site:38788` 经 G10 上的 caddy 做 TLS 终止 → 明文转 `127.0.0.1:8788`,
> 故外网档协议是 **https / wss**(局域网档是 http / ws)。

---

## TODO-1 外网档语音识别连不上:`TLS support not compiled in`

- [x] **现象**(2026-08-04,桌面端切外网):

  ```text
  无法连接识别服务 wss://spark.for-memory.site:38788/api/asr/stream:
  URL error: TLS support not compiled in
  ```

- **判断**:**这是 zero-desktop 的依赖特性漏配,与 caddy / 证书 / 网络都无关**。
  [`Cargo.toml`](../crates/zero-desktop/Cargo.toml) 里 `tokio-tungstenite = "0.24"`
  **没开任何 TLS feature**;该 crate 默认特性不含 TLS 连接器,于是
  [`remote.rs:575`](../crates/zero-desktop/src/modules/speech/commands/remote.rs:575) 的
  `connect_async` 一遇到 `wss://` 就直接在 URL 阶段报 `TLS support not compiled in`。
  - 局域网档是 `ws://192.168.x.x:8788/api/asr/stream`(明文),所以**一直没暴露**——问题只在
    外网档出现。
  - 对照:同 workspace 里 `orchestrator` / `toolkit-server` 也用 tokio-tungstenite,但它们连的是
    **本机回环的 ws://**(FunASR :9100、pronunciation-assess :8098),不需要 TLS,所以那两处不用改。

- **待办**:
  1. [x] 给 zero-desktop 的 `tokio-tungstenite` 开 TLS feature —— 选 **`rustls-tls-webpki-roots`**
     (与 workspace `reqwest` 的 rustls 取向一致,不引 native-tls);
     0.24 的 `connect_async` 在开启对应 feature 后**可直接支持 wss**,无需改调用形式。
     已改:[`Cargo.toml`](../crates/zero-desktop/Cargo.toml),`cargo check -p zero-desktop` 通过。
  2. [x] **跟读流式评测**:`shadow_stream_endpoint` 外网档同样派生 `wss://`,但由
     webview 侧 `new WebSocket(url)` 发起
     ([`ShadowService.ts`](../crates/zero-desktop/ui/src/modules/english/shadow/ShadowService.ts)),
     **不走 tokio-tungstenite**,不受本 bug 影响,无需额外改动。
  3. [ ] 回归:外网档跑一次实时语音识别 + 一次跟读流式评测,确认握手成功且能出结果。

- **验收**:桌面端切「外网」档,语音识别能连上 `wss://spark.for-memory.site:38788/api/asr/stream`
  并正常出字;局域网档行为不变。

---

## TODO-2 开了 TLS feature 之后仍崩:rustls 进程级 CryptoProvider 未安装

- [x] **现象**(2026-08-09,桌面端外网档一按录音就崩):

  ```text
  [speech] remote mode -> wss://spark.for-memory.site:38788/api/asr/stream (picked=Wan)
  thread 'tokio-rt-worker' panicked at rustls-0.23.40/src/crypto/mod.rs:249:14:
  Could not automatically determine the process-level CryptoProvider from Rustls crate features.
  ```

- **判断**:TODO-1 只解决了「有没有 TLS 连接器」,这条是「TLS 连接器用哪个加密后端」。
  rustls 0.23 要求进程级 `CryptoProvider`;当依赖树里 **aws-lc-rs 与 ring 两个 provider 同时
  被启用**时它拒绝自动选择,首次握手即 panic。`cargo tree -e features -i rustls` 可见两条链:
  - `reqwest 0.12`(经 tauri-plugin-http)→ `hyper-rustls` feature `aws-lc-rs`
  - `tokio-rustls` / `tokio-tungstenite` 一路 → feature `ring`

  注意**这不是 zero-desktop 自己的 feature 写错**——两个 provider 分别由不同上游拉进来,
  单靠调 feature 很难消掉,显式安装才是正解。

- **改法**:[`main.rs`](../crates/zero-desktop/src/main.rs) 最开头(任何 TLS 使用之前)装 ring:

  ```rust
  let _ = rustls::crypto::ring::default_provider().install_default();
  ```

  并把 `rustls = { version = "0.23", default-features = false, features = ["ring"] }`
  加为直接依赖。重复安装返回 `Err`,忽略即可。

- [ ] **回归**:与 TODO-1 第 3 条合并——外网档跑一次实时语音识别 + 一次跟读流式评测。

---

## 备忘:外网档的既有约定(别改错了)

- **38788 → caddy(TLS 终止) → G10:8788 = toolkit-server**;**28080 是 english 自己的入口**
  (自持同一份证书),两者是不同端口的不同服务,别混。
- 桌面端只配「局域网 IP / 外网域名」两个 host,协议 / 端口 / ASR 路径全部按档位派生
  (见 `NetScheme`),`auto` 模式经 health 探测自动选路。
