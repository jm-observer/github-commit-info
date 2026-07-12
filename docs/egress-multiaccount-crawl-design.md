# 多账号出口爬取协调设计（worker 出口 · zuche 登录/验证码 · THS 同步）

> **本文档是交接稿**：给下一个开发 session 用。把「出口 worker(toolkit)+ 登录/验证码(zuche)+
> 站点同步(THS)」三个项目怎么协作、已经做了什么、还要做什么、以及 **cookie↔worker 绑定机制** 一次讲清。
> 以 **同花顺(THS)多账号** 为首个落地场景;**租车 App** 是同形态的下一个消费方(同样"浏览器/App 走 worker 代理")。

相关文档:[distributed-worker-design.md](distributed-worker-design.md) 的「轻模型(借出口)」节(出口系统总设计)。

---

## 0. 三个项目与代码位置

| 项目 | 仓库 | 角色 | 认不认识别人 |
|---|---|---|---|
| **worker / 出口系统** | `D:\git\toolkit`(本仓) | 提供**出口 IP**:代发面 + 代理面 | 不认识 THS/zuche |
| **zuche(验证码/登录)** | `D:\git\zuche`(package `zuche-rs`,用其中 `crates/crawl`) | **THS 登录 + 过验证码**(纯 HTTP + CV,离线) | 不认识 THS/worker |
| **THS(同花顺同步)** | `D:\git\ths`(package `ths`) | **编排者**:登录拿 cookie → 浏览器同步板块成分股(翻页) | 依赖 zuche + worker |

**依赖方向(单向无环)**:
```
THS ──代码依赖──▶ crawl(zuche)     # 登录+验证码:password_login()
THS ──proxy 配置──▶ worker          # 出口:登录 + 同步都走它
crawl(zuche)、worker 互不依赖,都是叶子能力
```

---

## 1. worker 的两张出口面(toolkit,已建)

「借出口」不是一种形态,取决于消费方**怎么发请求**,worker 暴露两张面:

| 出口面 | 适配消费方 | worker 干什么 | 消费方接法 | 状态 |
|---|---|---|---|---|
| **代发执行器** | reqwest 类爬虫(抖音) | 收「method/url」用 reqwest 代发 | `pool.fetch`/`pool.session`(进程内)或 `egress-client`(外部 HTTP) | ✅ 已建 |
| **转发代理** | 浏览器/App(THS、租车) | 跑 HTTP 转发代理,出站绑到自己出口 IP | 给浏览器挂 `--proxy-server` / 给 App 设系统代理 | ✅ 已建(`toolkit-worker proxy`) |

**THS 走的是「代理面」**(headless Chrome 无法逐请求交接,只能整体指向代理)。

代理面实现:`crates/toolkit-worker/src/main.rs` 的 `proxy` 子命令
（`run_proxy`/`handle_conn`/`dial_upstream`）:
- `toolkit-worker proxy --listen <addr> [--interface <name>] [--local-address <ip>]`;
- 处理 HTTP `CONNECT`(HTTPS 隧道,主路径)+ 明文 HTTP(尽力而为);
- 出站用 `tokio::net::TcpSocket` + `socket2`,`--interface`(Linux `SO_BINDTODEVICE`)/`--local-address`
  决定从哪个出口发(cfg-gate:`.interface()` 仅 Linux,Windows 编译退化)。

出口选择的平台差异(重要,已验证):
- **Linux**:`--interface`(`SO_BINDTODEVICE`)逐 socket 选网卡,**同站点多出口可行**(不同 socket 走不同网卡)。
- **Windows**:路由按目的地全局选路 + 强主机模型,**源 IP 绑定选出口不可靠**;全隧道 VPN 下更是全被劫持。
  → **出口机用 Linux;Windows 只适合"一机一默认出口"或跑 controller/桌面端。**
- **多机各一个出口** 在任何平台都简单(一机一 worker,各自默认出口 = 不同公网 IP)。

---

## 2. zuche 的登录/验证码能力(已存在,直接复用)

**关键发现:zuche 已经有现成的、完整的 THS 登录 + 过验证码,不用自己造。**

- 入口:`D:\git\zuche\crates\crawl\src\ths_captcha\login.rs`
  ```rust
  pub async fn password_login(
      client: &reqwest::Client, username: &str, password: &str, long_login: bool,
  ) -> anyhow::Result<LoginOutcome>   // LoginOutcome { errorcode, success, raw }
  ```
  内部全自动:CV 解同花顺滑块验证码(重试 8 次)→ getGS 加盐 → RSA+MD5 密码登录 →
  **传入的 `client` 的 cookie jar 拿到登录态**。
- 验证码是 **纯 CV**(`ths_captcha/cv.rs::solve_gap`,Sobel 边缘检测缺口):**零模型、零打码平台、零联网、离线**。
- **纯 HTTP replay,不用浏览器** —— 登录这步很轻,不开 Chrome。
- 耦合度:CV 模块零业务耦合;登录模块只耦合 THS 协议字段(不耦合租车业务),可独立用。

**THS 接法(推荐)**:库依赖 `crawl` 的 `ths-captcha` feature
```toml
# ths/Cargo.toml
crawl = { path = "../zuche/crates/crawl", features = ["ths-captcha"] }
```
（跨仓 path 依赖,与 toolkit 用 `../custom-utils` 同套路;需 `../zuche` 在场才能 build THS。
以后想瘦身可把 `ths_captcha::{cv,captcha,login}` 抽成独立小 crate,zuche/THS 同时依赖——非首版。）

其它平台的 CV 也在 zuche(携程 jigsaw/icon、物体归位),将来别的浏览器类站点可参考。

---

## 3. THS 现状与改动

### 3.1 已改(未提交,在 `D:\git\ths`)
`src/lib.rs::create_browser()`:加了 `THS_PROXY` 环境变量 —— 设了则 Chrome `--proxy-server=<值>`
（目标站也走隧道、去掉 bypass 名单);不设维持原状直连。**编译已过。**

### 3.2 THS 现状要点(来自探查)
- 浏览器是**每进程一个全局单例**,tab 复用/新建;`create_browser()` 在
  `sync_service.rs`/`sync_concept_service.rs`/`main.rs` 各调一次。
  → **"一账号一 IP" = 一账号一进程**(各自 `THS_PROXY` + `THS_COOKIE_FILE`),不必改 THS 多实例逻辑。
- cookie 现状:从 JSON 文件加载(`load_valid_cookies_from_json`,env `THS_COOKIE_FILE`,
  字段 `name/value/domain/path/expires/http_only/secure`),经 CDP `tab.set_cookies` 注入。**无主动登录**。
- 同步:板块成分股**浏览器 JS 翻页**(点 `a.changePage`),https(`q.10jqka.com.cn`),前几页匿名、
  更深需登录态。→ **同步必须走代理面**(不能改成裸 HTTP,会破 JS 翻页 + 反爬指纹)。

### 3.3 待建:THS 的 `login` 步骤(编排核心)
在 THS 里加一个登录步骤(约一个文件):
1. 用**带 worker 代理**的 reqwest client 调 `crawl::ths_captcha::login::password_login`
   （client builder 加 `.proxy(reqwest::Proxy::all("http://<worker>:<port>")?)` + cookie jar）
   → 登录**从 worker 出口 IP 发出** → CV 自动过验证码 → 拿登录 cookie。
2. **导出 cookie 到 THS 的 `THS_COOKIE_FILE` 格式**。
   实现细节:reqwest 默认 cookie store **不可枚举**,要用显式 `Arc<reqwest::cookie::Jar>` 传给 client,
   登录后 `jar.cookies(&url)` 读出 `name=value` 再转 THS 的 `CookieFileEntry` JSON。
3. 进入原有浏览器同步(带 `THS_PROXY` 指向**同一个 worker**)。

---

## 4. ⭐ cookie ↔ worker 绑定机制(本文档重点,已定稿)

**问题**:cookie 在**铸造它的那个出口 IP** 上生成;第二次(及后续)用它同步,**必须还从同一个出口 IP 出**,
否则站点判失效/风控。所以每个账号要**长期钉在同一个 worker(= 同一出口 IP)**。

### 4.1 定稿原则:出口系统账号无关,绑定归「账号归属方」

**account → worker/IP 的绑定,不放在 toolkit 出口系统,而放在「谁拥有账号」的那个 app 里。**

- **出口系统(toolkit)对外只认 `worker_id`(或「随便一台」),永远不认「账号」。** 它只管 worker 清单、
  出口 IP、proxy 端点、在线/占用。代发面、代理面都账号无关。
- **account → worker_id + egress_ip 的映射,归账号归属方 app 自己持久维护**:
  - **zuche**(租车等,**一个 app 管所有账号的登录 + 请求**)→ 它本就是中心 + 有 DB,绑定就是它 DB 里加一张
    表(`account → worker_id/egress_ip`);
  - **THS**(少账号,独立 app)→ 它自己维护(可挨着 cookie 文件存)。

**为什么这样分**(对应讨论过的「B1 客户端本地 / B2 传 worker_id / 服务端建账号表」几种取舍的最终定论):
- 「多账号需要集中 + 持久」这个诉求,**已经被账号归属方满足了**——zuche 是单一中心 app、有持久存储,
  所以不必把账号搬进出口系统。B1「散在一堆进程本地文件、多机不同步」的毛病在这里不存在(归属方是正经中心 app)。
- 出口系统保持账号无关 → 领域解耦(账号是爬取域概念),多个消费方(THS / zuche / 租车…)共用同一套出口系统。

### 4.2 职责划分

| | 归谁 | 存/做什么 |
|---|---|---|
| **account → worker/IP 绑定** | **账号归属方 app**(zuche / THS) | `account → worker_id + egress_ip`,持久;分配策略(账号 > IP 时打包、`max_accounts_per_worker`);IP 变则重绑 + 重登 |
| **worker 清单 + 占用** | **toolkit controller** | 有哪些 worker、各自 egress_ip / proxy 端点 / 在线;(仅多 app 共池时)加「认领/占用」防跨 app 重复分配 |

绑定解析流程(账号归属方每次跑):
1. 按 `account` 查自己的表:
   - **命中 & 该 worker 在线 & egress_ip 未变** → 复用其 `proxy_url`(登录 reqwest `.proxy` + 同步 `THS_PROXY` 都用它)。
   - **首次 / 未绑** → 从 toolkit 的 worker 清单挑一台(按分配策略)→ 落绑 + 记 egress_ip。
   - **worker 掉线 / egress_ip 变了** → 老 cookie 作废 → 换台重绑 + **重新 `password_login`**(CV 免费,重登不心疼)。
2. 全程(登录 + 同步)都指同一 `proxy_url` → 同一出口 IP,cookie 一致。

### 4.3 toolkit 侧要不要「认领/占用」,取决于一个条件
**多个账号归属方 app 会不会共用同一个 worker 池?**
- **各 app 独占自己那批 worker(或 zuche 是唯一分配者)** → toolkit 只给**清单**,归属方自己分配保证不重复。最省。
- **多个 app 共用一个池** → toolkit 加**认领/占用**接口(controller 仲裁),防跨 app 把同一 worker 分给两个账号。
  这是唯一需要 toolkit 掺和账号分配的情形。

### 4.4 硬约束(软件解决不了)
- **cookie 铸造 IP == 使用 IP**;「同一个 worker」本质是「同一个出口 IP」。worker 换机/IP 变 = cookie 失效 = 重登。
  所以绑定必存 `egress_ip` 并在复用前校验。
- **多账号防关联 = 需要多 IP**。互不关联的出口 IP 有多少,就决定能同时跑多少个互不关联的账号;
  **账号数 > IP 数** → 超出的只能共享 IP(被部分关联)或排队轮流,靠 `max_accounts_per_worker` 打包策略控制。
  绑定表只管映射,**变不出 IP** —— 出口 IP(多机 / 多网卡 Linux / 住宅代理)要备够,这是账号规模的真正天花板。

> 收敛注:现在代发面 `pool.session(typ, account)` 仍按账号 key(便利、in-memory)。按本定稿方向应收敛成
> `pool.session_on(worker_id)`——把账号 key 从出口系统拿掉、交回归属方。**首版可不动,方向记此。**

---

## 5. 每账号完整流程(端到端)

```
账号 A(worker-1,出口 IP-1)
  1. worker-1 起代理:  toolkit-worker proxy --listen 0.0.0.0:8899 [--interface eth1]   (Linux)
  2. 绑定解析:         账号归属方(zuche/THS)查自己的表 A → proxy_url = http://worker-1:8899
                        (首次则从 toolkit 清单挑一台 + 落绑;egress_ip 变了则换台 + 重登)
  3. 登录(zuche,HTTP):reqwest client(.proxy(proxy_url) + Arc<Jar>)
                        → crawl::ths_captcha::password_login(&client, userA, passA, true)
                        → CV 过验证码 → cookie(从 IP-1 铸造)
  4. 导出 cookie:      jar.cookies(url) → THS_COOKIE_FILE(accA.json)
  5. 同步(THS,浏览器):THS_PROXY=proxy_url  THS_COOKIE_FILE=accA.json  ./ths_sync_service
                        → Chrome 从 IP-1 出,cookie 有效,板块成分股翻页
```

**多账号 = 多组 (worker, 账号) 配对,各跑一套上面的流程**(各自进程、各自 proxy、各自 cookie 文件)。
账号归属方(zuche/THS)自己的绑定表保证每个账号**每次都解析到它固定的那台 worker**;
账号 > IP 时按 `max_accounts_per_worker` 打包(§4.4)。

---

## 6. 部署形态

- **本地 / 同内网**(起步,简单):浏览器/App 和 worker 在同一台或同内网,`THS_PROXY=http://<worker-lan>:port` 直连。
- **远端 NAT 后的 worker 当代理**(后续):浏览器连不进 NAT 后的 worker,需**反向隧道 / 公网可达**(frp/Cloudflare Tunnel/mesh),
  或把 worker 放公网可达处。**首版只做本地/同内网,别碰远端 NAT。**
- 出口机优先 **Linux**(`--interface` 选出口);Windows 只做单默认出口。

---

## 7. 已完成 vs 待开发

### 已完成(本轮,大部分已提交)
- **出口系统(toolkit)**:egress-pool(Registry/Pool/Session、代发面 in-memory 绑定)、
  toolkit-server `/api/internal` + `/api/web/egress`(消费+观测面、SessionStore+reaper)、
  egress-client、egress-tester、toolkit-worker(`run`/`install`/`update`/`list`/`scan`/**`proxy`**、
  `--interface`/`--local-address`、MAC 派生 id、上报接口/出口)。
  - 提交:后端在分支 `feat/egress-pool`(commit `19f46bd`);**worker `proxy` 子命令 + 桌面面板两列 + THS 改动尚未提交**。
- **THS**:`THS_PROXY` → Chrome `--proxy-server`(未提交,在 `D:\git\ths`)。
- **zuche**:`password_login` + CV 验证码(**本来就有,不用改**)。

### 待开发(新 session)
1. **THS `login` 步骤**:接 `crawl`(`ths-captcha` feature)→ 带 worker 代理的 reqwest 调 `password_login`
   → 导出 cookie 到 `THS_COOKIE_FILE`(用 `Arc<Jar>`)→ 衔接原有同步。
2. **账号→worker 绑定,建在账号归属方 app 里**(§4.1/4.2,已定稿):zuche 的 DB 加一张
   `account → worker_id/egress_ip` 表(THS 少账号自维护);解析:命中且 egress_ip 未变则复用,首次/失配则
   从 toolkit 清单挑一台 + 落绑(+ 重登)。**出口系统不建账号表。**
3. **toolkit 侧:worker 清单/观测**:让 `proxy` 子命令的 worker 也 register 上来,`/api/web/egress/workers`
   暴露 `proxy 端点 + egress_ip + 在线`,供归属方 app 查询挑选。(多 app 共池时再加「认领/占用」接口,§4.3。)
4. **编排入口**:决定"一账号一进程"的启动方式(zuche 内置 / THS 内置 / 薄 orchestrator);
   把 §5 流程串起来(查绑定 → 起/复用代理 → 登录 → 导 cookie → 同步)。
5.（后续)远端 NAT worker 的代理可达(反向隧道 / 公网可达);`pool.session(typ,account)` 收敛成
   `pool.session_on(worker_id)`(§4.4 收敛注)。

### 提交状态清理(交接注意)
- `feat/egress-pool` 已提交后端;工作区还有:worker `proxy`、桌面 egress 面板 + 两列、THS(另一仓)的 `THS_PROXY`,
  均**未提交**,且桌面/文档改动与用户其它进行中改动缠绕 —— 新 session 提交前需按仓分别 scope。

---

## 8. 决策记录 & 仍开放的点

**已定(§4 定稿)**:
- **绑定归属**:account→worker 绑定放在**账号归属方 app**(zuche/THS 各自持久),**出口系统账号无关**(只认 worker_id)。
- **分配权**:默认由账号归属方自己分配(toolkit 只给清单);**仅多 app 共池时** toolkit 才加「认领/占用」仲裁。

**仍开放(留给新 session)**:
- **编排入口形态**:zuche 内置 / THS 内置 `login`+`sync` 一条龙 vs 独立薄 orchestrator?倾向账号归属方内置(它就是编排者)。
- **cookie 有效期/续期策略**:多久重登一次、失效检测信号(同步撞登录页 → 触发重登)。
- **租车 App 接入**:App 走系统/SOCKS 代理(worker 可能要加 **SOCKS5 面**,现只有 HTTP CONNECT);账号↔worker 绑定同一套。
- **分配策略细节**:`max_accounts_per_worker`、挑 worker 的算法(轮询 / 最少账号 / 就近)。
