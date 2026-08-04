# 软件有效期与授权 —— 实现文档

> 上游方向文档：[docs/license-expiry-design.md](license-expiry-design.md)（信任模型、大原则、拍板项）。
> 本篇只讲**怎么实现**：crate 结构、密钥体系、令牌/指令格式、客户端与服务端集成、邮件、
> 分阶段任务清单。字段/接口给到可直接开工的粒度。
>
> 状态：评审后实施版。按两期交付：**一期为共享离线核心 + zuche 首次接入**；二期为在线续期、
> 措施广播、运营能力和 zero-desktop 接入。实际排期与验收以 §9 的两期清单为准。

---

## 0. 全景与信任边界回顾

一句话映射到实现：**一切"真伪"由 Ed25519 签名决定，通道全都不可信。**

```
      ┌──────────── 你（离线保管的私钥，按角色分开）────────────┐
      │  root(离线)    → 签 License 令牌 TKL1（授权，一对一）      │
      │  directive(离线)→ 签 Directives 措施 TKD1（地址/公告/撤lic）│
      │  recovery(离线冷备)→ 签"撤 kid / clock-reset"高权恢复指令   │
      │  renewal(在 G10) → 签续期 TKL1(受限) + 响应封套 TKR1        │
      └───────────────────┬────────────────────────────────────┘
                                 │ 产物都是"带 kid+角色 的签名字节"
         ┌───────────────────────┼───────────────────────┐
         ▼                       ▼                       ▼
   在线线(要服务在)        措施通道(只要静态托管在)     离线线(什么都不要)
   toolkit-server          Gitee/git/OSS 多镜像          U盘/微信/粘贴
   /api/license/refresh    directives.json(签名)        激活码(签名)
         │                       │                       │
         └───────────────────────┴───────────┬───────────┘
                                             ▼
                             客户端：编译进 pk，只验签
                             LicenseState 状态机 → 正常/临期/宽限/受限
```

三个签名对象，**由不同角色的私钥签、都由客户端编进的公钥集验**（角色权限见 §2.1 / 方向文档 §1.4）：

| 对象 | 前缀 | 签名角色 | 面向 | 载体 |
|---|---|---|---|---|
| **License 令牌** | `TKL1` | `root`（离线主钥）或 `renewal`（G10 续期钥，受限） | 一台/一组机器 | 在线续期返回 / 离线激活码 |
| **Directives 指令** | `TKD1` | `directive`（普通措施）/ `recovery`（**仅撤销 kid**） | 全网 | 多镜像静态文件 |
| **服务端响应封套** | `TKR1` | `renewal` | 单次请求 | 在线线返回体的签名壳（**必签，非可选**，§3.4） |

> **不是"同一把私钥"**：撤销权锁死在离线 `recovery` 角色上，续期钥只能续期，这样任何一把
> 在线/常用私钥泄露都不至于失守（方向文档 §1.4）。

---

## 1. crate 结构：核心进 custom-utils，集成留各项目

**归属决策**：license 核心是**跨项目复用**能力（你有多个项目要同款期限控制），所以它的家在
通用 crate **`custom-utils`**（`D:/git/custom-utils`，`repo: jm-observer/custom-utils`），
和 `updater`/`trace`/`tls` 一样做成 **feature 门控模块**。各业务仓（toolkit 等）只写**集成层**
（DB 表、HTTP 路由、UI、邮件），不重复实现验签/令牌逻辑。

### 1.1 custom-utils 侧：新增 `license` / `license-issuer` feature

```
custom-utils/src/util_license/
  mod.rs        // 公开 re-export + LicenseState/Directive/Payload 类型
  keys.rs       // 公钥加载（多把，轮换用）；调用方注入 hex，不硬编码
  token.rs      // TKL1：Payload 编解码 + Ed25519 验签
  directive.rs  // TKD1：Directives 编解码 + 验签 + seq 单调校验
  machine.rs    // 机器指纹（cfg(windows)/cfg(unix)）
  clock.rs      // 单调水位、多点存放、时钟异常判定
  state.rs      // evaluate()：Payload + machine + clock → LicenseState
  issue.rs      // 仅 feature="license-issuer"：签发逻辑（持私钥）
```

```toml
# custom-utils/Cargo.toml —— 追加
[dependencies]
ed25519-dalek = { version = "2", default-features = false, features = ["std"], optional = true }
base64        = { version = "0.22", optional = true }
sha2          = { version = "0.10", optional = true }  # 机器指纹哈希；chrono/serde 已在别的 feature 里
rpassword     = { version = "7", optional = true }     # 仅签发端读私钥口令

[features]
license         = ["ed25519-dalek", "base64", "sha2", "serde", "chrono"]
license-issuer  = ["license", "rand", "rpassword"]     # 只有你的签发机开；下游产物永不含它
```

**关键约定**：
- **公钥不在 custom-utils 里硬编码**。custom-utils 提供 `key_table_from(&str) -> Vec<KeyEntry>`
  （解析 `kid:role:hex` 列表，见 §2.3），由**下游 crate 的 `build.rs`** 从 `LICENSE_PUBKEY` 注入
  并传入——这样一份 custom-utils 服务多个项目，各项目用各自的一组带角色公钥。
- `tklic` **签发 CLI 放哪**：作为 custom-utils 的 `[[bin]]`（`--features license-issuer`），
  或各项目自带一个薄壳 bin 调 `util_license::issue`。建议前者，签发工具全项目共用一套。

> 为什么用 Ed25519 而不是 sm2/国密：`ed25519-dalek` 生态成熟、验签快、无外部 C 依赖，
> 客户端体积小。国密不是硬需求（非合规场景）。签名算法在 `token.rs` 收口，将来换实现不影响其余。

### 1.2 toolkit 侧：只依赖，不重造

```toml
# 各消费 crate：开 license feature，不开 issuer
custom-utils = { workspace = true, features = ["updater", "trace", "license"] }
```

- `zero-desktop`、`toolkit-server` 引 `custom_utils::util_license::*`，`build.rs` 注入公钥。
- 集成层（`licenses` 表 / `/api/license` 路由 / Tauri 授权页 / 邮件通知）仍在 toolkit 各仓，
  见 §6/§7。

> **落地顺序**：共享核心直接进入 custom-utils；zuche 只写集成层。不先在 toolkit 里做临时原型，
> 避免一期出现两套实现或后续平移成本。

### 1.3 两种交付形态：桌面（toolkit）与 web 服务（zuche）

license 核心一套，但**集成形态因交付物而异**。目前两个消费方正好覆盖两类，把它们的差异列清，
custom-utils 只需保证核心对两者都够用：

| 维度 | **toolkit / zero-desktop**（桌面 Tauri） | **zuche**（axum web 服务，交付公司客户，[D:/git/zuche](../../zuche)） |
|---|---|---|
| 形态 | 单机桌面 App | 部署在客户 Linux 服务器的后端 + web UI（`zuche-app` / `platform-management`） |
| 已用 custom-utils | 是（updater/trace） | **是**（`logger`，`custom_utils::args::workspace → ~/.config/zuche-rs`） |
| 机器指纹 | Windows `MachineGuid` | **Linux `/etc/machine-id`**（绑客户那台服务器） |
| 授权检查点 | 启动 evaluate + 各 Tauri command 入口 `require_licensed!` | 启动 evaluate + **axum 中间件**（tower layer）拦 web 请求 + **定时采集任务**判活 |
| 到期受限的"只读"是什么 | 看历史 / 导出数据 / 进激活页 | 见下方 allow/deny 矩阵（**按当前真实路由列**，不是想当然） |
| 激活入口 | 桌面授权页 | web 管理页一个「授权」栏 + 命令行 `zuche-rs license import <token>`（二进制名是 `zuche-rs`，package 是 `zuche-app`） |
| 在线续期 / 措施通道 | 内置多镜像 + 你的续期端点 | 同左（zuche 已有 reqwest rustls，直接复用） |

**zuche 受限模式的 allow/deny 矩阵（回应 codex #11——按现状代码，不臆测）**：

| 现状路由 / 能力 | 受限后 | 依据 |
|---|---|---|
| `POST /api/login`、`/api/verify`、`/api/request`（代发上游） | **禁**（生产性：登录/代发） | [gateway.rs](../../zuche/crates/zuche-app/src/gateway.rs) |
| 手动同步 / 配置修改 / `POST /traffic/clear` 等写操作 | **禁** | [web.rs](../../zuche/crates/ui/src/web.rs) |
| 定时采集任务 | **停** | 生产性 |
| `GET /accounts`、`/traffic` 等只读页 | **放行** | 只读，符合红线 |
| 导出已采数据 | **放行** | 红线：永远能导出 |
| 授权页 / `license import` | **放行** | 否则死锁 |

> ⚠️ **现状纠正**：zuche 的"MySQL 跨机直读 + 订单落库"是**目标态、尚未实现**（现状为本地
> SQLite，订单仅在内存 `RunHistory`，进程重启即丢——见 [zuche DESIGN.md](../../zuche/DESIGN.md) §4/§13）。
> 所以**不能**说"到期后公司侧直连读库不受影响"。zuche 一期的正确前置是：**要么先落地 MySQL/订单落库
> 再谈'保留只读'，要么当前阶段的'只读'仅限 web 页面查看 + 导出**。把 MySQL 落库列为**外部前置条件**。

**对 custom-utils 的要求**：`util_license` 必须**不假设 GUI、不假设 Tauri、不假设某个 web 框架**——
它只吐 `LicenseState` 和验签结果，"拿这个状态去拦 Tauri command 还是拦 axum 请求"由各项目自己接。
这也是把它做成纯逻辑模块（无网络/无 UI）的原因。

> zuche 的 `licenses` 台账、续期端点可以**复用 toolkit-server 那一套**，也可以 zuche 自建一份——
> 取决于你想不想把两个产品的授权台账合并管理。见 §10 待确认。

---

## 2. 密钥体系（共享地基）

### 2.1 密钥对

- `tklic keygen --role <root|renewal|directive|recovery>` 生成 Ed25519 密钥对，**每把带 kid + 角色**：
  - 私钥 `<kid>.sk`（32B seed）→ **age/口令加密后落盘**，绝不明文久留。
  - 公钥 → 打印 `<kid>:<role>:<pubkey_hex>`，进下游 `build.rs`。
- 编进客户端的是一组 **`kid → (pubkey, role)`**（`PUBKEYS: &[(Kid, Role, [u8;32])]`）。令牌/指令
  信封头写签它的 `kid`；验签时**三查**：① kid 未被撤销 ② 该 kid 的**角色配得上这种对象**
  （`renewal` 不能签 TKD1、`root` 不能签撤销指令、撤 kid 只认 `recovery`…）③ 签名有效。
- **首发编进的 key 表（示例）**：`root-a`/`root-b`(冷备) + `renewal-1`(上 G10) + `directive-1`
  + `recovery-1`(离线锁死，只在撤销时用)。这套角色分权让"泄露任一常用钥都撤不掉 recovery"成立
  （方向文档 §1.4）。
- **撤销**（§3.3）：`recovery` 私钥签一条指令把泄露 `kid` 标 revoked，客户端**并集累加、不可逆**。

### 2.2 私钥保管（这是整套东西的单点，最高优先级）

**按角色分别定保管规则（renewal 必须在线，与"绝不上 G10"不冲突——那条只管离线三角色）**：

| 角色 | 保管 |
|---|---|
| `root` / `directive` / `recovery` | **离线**：加密 U 盘 + 纸质抄写异地各一份；`age -p` 口令加密，`tklic` 用时解密到内存即弃。**不进 git / CI / G10 / 云盘明文** |
| `renewal` | **必须在 G10 在线**（否则续期没法零沟通）。防护降一档但仍要做：落盘 `age`/OS 密钥库加密、文件权限 `600`+专用用户、与业务进程隔离；**它是权限最小的角色**（只能在 `[已激活, business_deadline]` 内延期），泄露不至失守。定期用 recovery 轮换其 kid |
| 轮换 | 每角色首发都多编 1 把冷备公钥（§2.1）；泄露即启用冷备私钥重签 + recovery 撤旧 kid |

### 2.3 公钥进二进制（build.rs 注入）

每把公钥编码为 `kid:role:pubkey_hex`，多把用 `,` 分隔：

```rust
// build.rs —— 注入带 kid+role 的公钥表
fn main() {
    // 形如 "root-a:root:ab12..,renewal-1:renewal:cd34..,recovery-1:recovery:ef56.."
    let pks = std::env::var("LICENSE_PUBKEY").expect("生产/发布构建必须显式注入 LICENSE_PUBKEY");
    println!("cargo:rustc-env=LICENSE_PUBKEY={pks}");
    println!("cargo:rerun-if-env-changed=LICENSE_PUBKEY");
}
```

开发公钥只能由显式 `dev-license` feature 注入；`prod` 与 `dev-license` 必须互斥并在 `build.rs`
直接失败。生产构建绝不能静默回退到仓库内的开发公钥，否则任何持有开发私钥的人都能签生产授权。

```rust
// keys.rs
pub struct KeyEntry { pub kid: String, pub role: Role, pub key: VerifyingKey }
pub enum Role { Root, Renewal, Directive, Recovery }

pub fn key_table() -> Vec<KeyEntry> {           // kid→(role,pubkey)，验签查这张表
    env!("LICENSE_PUBKEY").split(',').filter(|s| !s.is_empty())
        .map(|s| /* "kid:role:hex" → KeyEntry */).collect()
}
/// 验签统一入口：查 kid 未撤销 + 角色允许签 want + 签名有效
pub fn verify_as(magic: &str, kid: &str, want: Role, payload_b64: &str, sig: &[u8],
                 revoked_kids: &RevokedSet) -> Result<()>;
```

**发布构建必须带真公钥表**（`kid:role:hex` 列表，逗号分隔，**不是**裸 hex）：

```bash
LICENSE_PUBKEY="root-a:root:AB..,root-b:root:CD..,renewal-1:renewal:EF..,directive-1:directive:11..,recovery-1:recovery:22.." \
  cargo build --release
```

deploy-g10.ps1 / 桌面打包脚本里把它设成环境变量（生产公钥不是秘密，可以进脚本）。

---

## 3. 令牌与指令格式

### 3.1 通用信封

```
<magic>.<kid>.<payload_b64url>.<sig_b64url>
```

- `kid` = 签名用的 key-id（§2.1），让客户端知道拿哪把公钥验、以及该 kid 是否已被撤销。
- `payload_b64url` = base64url(no-pad) 的 JSON 字节。
- `sig` = `Ed25519(sk, magic_bytes || "." || kid || "." || payload_b64url_bytes)`。
- 验签：切段 → 查 `kid` 未被撤销 → 用其公钥验 `magic + "." + kid + "." + payload_b64url`
  → **通过后才 `serde_json` 解析 payload**。任何字段在验签通过前都不可信。

### 3.2 License 令牌 `TKL1`

```json
{
  "ver": 1,
  "product": "zero-desktop",
  "lic_id": "L-2026-0031",
  "subject": "某某客户",
  "machine": [
    { "id": "a3f1…", "components": { "machineguid": "…", "board": "…", "disk": "…" } }
  ],
  "issued_at":  "2026-08-03T10:00:00Z",
  "not_before": "2026-08-03T00:00:00Z",
  "business_deadline": "2027-02-03T00:00:00Z",
  "expires_at": "2027-02-03T00:00:00Z",
  "lease_until": null,
  "grace_days": 14,
  "features": ["speech", "english", "codeloop"],
  "max_version": "0.9.*",
  "nonce": "7c1f…"
}
```

- **三个日期，`business_deadline` 是客户端能自查的商务上限（回应 codex #5）**：
  - `business_deadline` = 商务硬截止，**首次激活由 `root` 签死、此后视为不可变锚**。客户端把它
    从激活令牌里记下来；续期令牌（renewal 签）的 `business_deadline` 必须与锚**逐字节相等**，且
    `expires_at ≤ business_deadline`——**这条客户端自己就能校验，不依赖服务端台账**。于是 renewal
    钥泄露也**延不过商务截止**（要延过只能 root 重新签一份新锚）。
  - `expires_at` = 当前生效到期（续期时被 renewal 往后推，但封顶在 `business_deadline`）。
  - `lease_until` = 在线租约到期。`null` = **纯离线模式**（撑到 `expires_at`，不保证及时吊销）；
    给值 = **可及时吊销模式**（到 `lease_until` 须续租）。按客户在台账里选，不是全局。
- **`machine` 是对象数组，带分量**：`components` 是若干稳定分量（Windows: machineguid/board/disk；
  Linux 服务器: 只有 `machine_id` 一个）。匹配规则见 §5——**多分量时多数匹配容错；单分量（Linux）
  就是精确匹配，没有"多数"可言**。面向客户签发时 `machine` 不能为空；一期不支持不绑机授权。
- 客户端必须分开保存两份对象：
  - `<config>/license.anchor.tkl`：最近一次 **root 签名**的授权锚；只能由新的 root 授权替换。
  - `<config>/license.lease.tkl`：可选的 **renewal 签名**续期令牌；只能在逐字段对照 root 锚通过后采用。

  在线续期绝不能覆盖或改写 root 锚。每次校验都要重新以 root 锚为基准检查续期令牌，不能拿“上一次
  renewal 令牌”当下一次比较基准，否则泄露的 renewal 私钥可通过多次小改动逐步漂移机器、功能或商务截止。
  一期只有 `license.anchor.tkl`，二期才启用 `license.lease.tkl`。

### 3.3 Directives 指令 `TKD1`（措施下发）

```json
{
  "ver": 1,
  "seq": 42,
  "issued_at": "2026-08-03T10:00:00Z",
  "entries": [
    "https://spark.for-memory.site:38788",
    "https://backup.example.net:38788",
    "https://1.2.3.4:38788"
  ],
  "revoked": ["L-2025-0007", "L-2026-0011"],
  "min_version": "0.7.0",
  "notices": [
    { "id": "n-2026-08", "level": "info",
      "text": "系统将于 8/10 维护", "until": "2026-08-11T00:00:00Z" }
  ],
  "kill": null
}
```

- **`seq` 单调**：客户端持久化 `last_seq`，只接受 `seq > last_seq`。挡的是**网络侧**回放旧文件；
  改本地的 root 能回滚 `last_seq`，所以指令通道是"广播加速"不是"强制"（方向文档 §2.5）。
- **撤销公钥**：`revoked_kids: ["k1"]` 字段，命中即拒收该 kid 签的一切令牌（方向文档 §1.4）。
  `revoked_kids` 与 clock-reset **只允许出现在 `recovery` 角色 + 独立 `rseq` 的指令里**；
  若一份 `directive` 角色指令里出现了 `revoked_kids` 这类越权字段 → **整份拒绝**（不是"忽略该字段"，
  与"绝不部分执行"一致）。反之 `recovery` 指令也只准带获准字段，多余字段一律整份拒绝。
- **两套独立序列 + directive epoch（回应 codex：撤钥后普通通道也要能复活）**：
  - `recovery` 指令走独立 `rseq`（`last_rseq`），普通 seq 再高也压不住它 → 撤钥恒可达。
  - `directive` 指令的序列是**二元组 `(epoch, seq)`**，字典序比较。**`recovery` 有权 bump `epoch`**
    （撤掉泄露 directive 钥的同时，把 `epoch` +1）。于是即便泄露钥先签了 `seq=u64::MAX`，撤钥后
    新 directive 钥用 `epoch+1` 起签，`(epoch+1, 0) > (epoch, MAX)`，**普通措施通道随之复活**——
    密钥恢复才算完整，而不是只恢复了撤钥这一半。
- **撤销集合语义 = 单调并集、永久不可逆、无 unrevoke**：客户端把每份指令的 `revoked` / `revoked_kids`
  **累加进本地持久集合**，绝不"以最新指令为全量快照替换"。撤错了换新 kid/lic 重发，不做解封。
- **`revoked`**：命中即受限（`RestrictReason::Revoked`）。
- **`kill`**：全局降级开关；**永不锁死激活页/导出**（红线）。默认 `null`。
- 客户端拉取顺序 = License 令牌里 `entries` 或内置候选 + 多镜像清单 URL（下 §6.3）。

### 3.4 在线响应封套 `TKR1`（**必须签，不是可选**）

在线线返回的 `{license, server_time}` 必须整体签名 + **回绑本次请求的 nonce**：

```
客户端请求带 client_nonce（随机）
服务端返回 TKR1{ payload: {license, server_time, echo_nonce}, sig }
客户端：验签 + 校验 echo_nonce == 自己发的 client_nonce，否则整份丢弃
```

为什么不能省：

- **不签 → MITM 可伪造** "你已过期" 拒绝服务，或伪造 server_time 制造时钟异常（DoS）。
- **签了但不绑 nonce → 可重放**：合法的旧 "未过期/某时间" 响应能被重放续命或回拨时间。
- **校时口径**：水位**默认只单调增加**（server_time 更晚只会更快判过期，不放宽授权）；
  **下调只在满足 §5.1 误拨恢复判据的签名响应下允许**（renewal 的 TKR1 或 recovery 的 clock-reset）。
  "明显早于"的阈值取**大于最大合理时钟漂移**（如水位 − server_time > 24h 才认作误拨、允许下调），
  且下调后若系统时钟仍停在未来 → 保持 `ClockTampered` 直到系统时钟也回正。前提永远是响应**签名且新鲜**。

签这层用**续期私钥**（§6.2），不用主私钥。`TKR1` 与内层 `TKL1` 是两层签名：外层保"这次响应
是 G10 现发的"，内层保"这份授权是你签的"。

---

## 4. 客户端状态机

```rust
pub enum LicenseState {
    NotYetValid { not_before: DateTime<Utc> },   // now < not_before
    Valid    { effective_until: DateTime<Utc>, days_left: i64 }, // = min(expires_at, lease_until?)
    Expiring { days_left: i64, which: Deadline }, // 哪个先到：Grant | Lease
    Grace    { days_left: i64, which: Deadline },  // 到期后 grace_days 内（Grant | Lease）
    Restricted { reason: RestrictReason },
    Missing,                                      // 从未激活
}
pub enum Deadline { Grant, Lease }
pub enum RestrictReason {
    Expired, LeaseExpired, Revoked, MachineMismatch,
    ClockTampered, BadSignature, ProductMismatch, VersionTooOld, BusinessDeadlineViolation,
}

pub fn evaluate(p: &Payload, machine: &MachineId, clock: &Clock, now: DateTime<Utc>,
                revoked: &[String], min_version: Option<&str>) -> LicenseState;
```

判定顺序（任一命中即定）：

1. 验签失败（含 kid 已撤销 / 角色不符）/ 无令牌 → `BadSignature` / `Missing`
2. **日期不变量自检**：`not_before ≤ expires_at ≤ business_deadline`（`lease_until` 若非空须
   `≤ business_deadline`）不成立 → `Restricted{BusinessDeadlineViolation}`。**这一步让该 reason 可达**，
   挡住"续期令牌把 expires_at 写超 business_deadline"或内部日期被构造得矛盾的令牌。
3. `product` 不符 / `max_version` 不满足 / `min_version`（来自指令）不满足 → 受限
4. `lic_id ∈ revoked` → `Revoked`
5. 机器指纹不匹配 → `MachineMismatch`
6. `clock.tampered()` → `ClockTampered`（当已过期处理）
7. `now < not_before` → `NotYetValid`
8. 计算唯一有效截止 `effective_until = min(expires_at, lease_until?)`，并记录来源 `Grant | Lease`。
9. `now > effective_until + grace` → 按来源返回 `Expired` 或 `LeaseExpired`。
10. `effective_until < now ≤ effective_until + grace` → `Grace{which}`。
11. `effective_until - now ≤ 30d` → `Expiring{which}`。
12. 否则 → `Valid{effective_until}`。

必须先取最早截止再判断宽限，不能先判断 `expires_at`、返回后才检查 `lease_until`；否则当商务授权处于
宽限但短租约早已过期时，会错误进入商务宽限并绕过租约限制。`business_deadline` 是不可突破的锚，
不是日常状态机里的当前截止，因此 `Deadline` 使用 `Grant`，避免把 `expires_at` 误称为 Business。

**每次启动同步跑一遍**（不受在线随机化影响，方向文档 §2.3.1）。结果放进
`app_state`，各功能入口用 `require_licensed!(state, "speech")` 宏拦（受限模式下
只放行只读/导出/激活页）。

---

## 5. 机器指纹（machine.rs）

- **分量（带稳定标识名）**：
  - **Windows**：`machineguid`（`HKLM\...\MachineGuid`）+ 可选 `board` / `disk` 序列号 → 多分量。
  - **Linux**（zuche 客户服务器 / G10）：`machine_id`（`/etc/machine-id`）→ **通常只有这一个分量**。
- 每个分量 `sha256("tklic-machine-v1" || product || name || raw)` 取前 16B hex，组成
  `{name: hash}` 映射；`machine.id`（给用户看/报给你的）= 对**排序后全部分量**再 SHA-256 取前 16B hex。
  这里的 domain separator 不是秘密，只用于隔离不同产品/用途；截断到 128 bit，避免原设计 64 bit
  分量摘要在客户量增长后留下不必要的碰撞空间。
- **匹配规则（按分量数自适应，回应 codex #8）**：
  - **多分量（Windows）**：令牌某条目与本机**多数分量一致**即通过（换个硬盘不失效）。
  - **单分量（Linux）**：退化为**该分量精确相等**——没有"多数"可言，换机即失配（服务器场景本就该严）。
- 令牌 `machine[]` 里**任一条目**匹配即整体通过（一份 license 授权多台）。
- **签发要拿到分量、不只是 id（回应 codex #7）**：客户端授权页导出的是一段**机器请求串**
  `MREQ1.<base64(json{id, components:{name:hash}})>`——含 `id` + 已哈希的分量表。用户把这串
  发给你，`tklic issue --machine <MREQ1...>` 解析出 `{id, components}` 原样写进令牌，这样令牌里
  才有 `components` 供客户端做多数分量匹配。**光有 `id` 无法生成支持模糊匹配的令牌**。
- **绝不外传原始值**：`components` 里是**哈希**不是原始序列号；用户发出去的 MREQ1 串不含明文硬件信息。

### 5.1 时钟水位与"误拨未来"的恢复路径（clock.rs，回应 codex #9）

- 水位取"配置文件 + 注册表/隐藏路径 + DB `meta`"三处最大值；`now < 水位 - 容忍(24h)` → `tampered`。
- **问题**：一旦时间被误拨到很远的未来，水位被推高并永久保留；之后就算把系统时间改回正确、
  也拿到合法签名的当前时间，仍会一直卡在 `ClockTampered`（因为在线时间只收紧不回调）。
- **恢复路径（必须有，否则是不可恢复的误锁）**，任一即可：
  1. **在线**：收到 `renewal` 角色签名 + nonce 新鲜的 `TKR1`（§3.4），其 `server_time` 明显早于
     当前水位且签名有效 → **允许把水位下调到 server_time**（这是唯一可信来源，签名+新鲜=不可伪造）。
  2. **离线人工**：`recovery` 角色签一张一次性 `clock-reset` 令牌（带目标时间 + 本机 machine.id
     + `nonce`），用户导入后重置水位。用于你的服务/域名都不在时的兜底。
     **一次性靠"已消费 nonce 持久化"落地**：客户端把用过的 clock-reset `nonce` 记进一张持久集合
     （与水位同处多点存放），**重复导入同一张即拒绝**——防止把一张旧 reset 令牌反复用来回拨时间。
- **只有这两条**能下调水位；本地任何操作、未签名时间都不行。`recovery` 角色因此不止"撤 kid"，
  还含 clock-reset（与方向文档 §1.4 的角色定义一致）。

---

## 6. 网络集成

### 6.1 客户端在线线（zero-desktop）

- 时机：启动后随机延迟 + 之后每次落在 `[18h,30h]` 随机点（§2.3.1）；相位 = `机器hash % 周期`。
- 动作（各自失败静默），**每次带一个随机 `client_nonce`**：
  1. `POST {entry}/api/license/refresh`（body 带 `lic_id/machine/product/ver/client_nonce`）→ 返回 `TKR1`。
     使用 POST 是为了避免标识落入 URL、代理和访问日志；它本身不构成身份认证。客户端先验 TKR1
     签名（renewal 角色）并校验 `echo_nonce == client_nonce`，不过则整份丢弃；通过后取内层 TKL1，
     对照 `license.anchor.tkl` 执行 §6.2 的全部约束，通过后只覆盖 `license.lease.tkl`。
  2. 校时：**只认 `TKR1` 里签名且回绑 nonce 的 `server_time`**；**绝不用未签名的 HTTP `Date` 头**
     （那个能被 MITM 随意改，推高水位制造 DoS）。**默认只收紧（水位只增）；下调水位仅在 §5.1 的两条
     签名路径下允许**（在线 TKR1 或离线 recovery clock-reset）——这是"误拨未来后能恢复"的唯一出口。
- 实现放 `crates/zero-desktop/src/modules/license/`（Tauri command + 后台 tokio 任务）。

### 6.2 服务端在线线（toolkit-server）

- 表 `licenses`（`toolkit-core/schema.rs` `DDL_V1` 追加，bump `SCHEMA_VERSION`）——**三日期分开存**：
  `lic_id / product / subject / contact_email / machine_ids / not_before /
   business_deadline（商务上限，root 签死、续期不得越过）/
   grant_window_days（每次续期把 expires_at 推到 now+此值）/
   lease_days（在线租约天数，NULL=纯离线模式）/
   grace_days / features / max_version / revoked_at / note / created_at`。
  续期签发：`TKL1.expires_at = min(now + grant_window_days, business_deadline)`、
  `lease_until = min(now + lease_days, business_deadline)`（若非 NULL）、`business_deadline` 原样带
  （客户端锚比对）。
  **"后台改到期日自动恢复" = 调 `grant_window_days`（在商务上限内，renewal 现签）；延长商务总期 =
  改 `business_deadline` 并用离线 `root` 重签新锚**（renewal 改不了它）。
- 路由 `/api/license`：
  - `POST /api/license/refresh`（可免 Bearer）→
    **服务端现签**一份新令牌（内层 TKL1）+ `TKR1` 封套返回。两种选择：
    (a) 服务端持一把**"续期专用"私钥**（独立 kid，也编进客户端），主私钥仍离线。**推荐**。
    (b) 服务端不签，只返回台账状态，真正的新令牌仍你离线签——续期就不"零沟通"了。
    → 取 (a)。`lic_id + machine + nonce` **不是自证或秘密**，这里只能用于定位、回绑响应和限流；
    真正的安全边界是签名、不可变 root 锚和客户端机器匹配。接口必须做每 IP/每 lic_id 限流，且日志
    不记录完整机器请求串。若二期需要防授权记录被枚举，再增加激活时生成的设备密钥挑战，不在一期预埋
    一个名义上的“共享秘密”。

    **"续期 kid 只能续期"必须在客户端强制**（不能只靠服务端良好行为）。客户端拿续期 kid 签的
    新令牌与**`license.anchor.tkl` 的 root 授权锚**逐字段比对，按下表准入：

    | 字段 | 续期时 | 说明 |
    |---|---|---|
    | `lic_id` / `product` / `subject` / `business_deadline` | **必须与本地锚逐字节相等** | `business_deadline` 是 root 签死的锚，renewal 改了就拒绝——**客户端自查，不靠台账** |
    | `machine` / `features` / `max_version` / `not_before` / `grace_days` | **必须不变** | 任何"扩权"（加机器/加功能/放宽版本）一律拒绝 |
    | `expires_at` | **不得早于当前已采用值，且 `≤ business_deadline`** | 上限就在 root 锚里，客户端直接比；服务端也二次卡台账 |
    | `lease_until` | 不得早于当前已采用值，且 `≤ business_deadline` | 纯离线模式此字段恒 null |
    | `issued_at` / `nonce` | **必然变**（新签名） | 不参与"是否变化"判断 |

    关键：**商务上限 `business_deadline` 是 root 签在令牌里、客户端锚定的不可变字段**，不是"服务端台账里
    才有的值"。所以 renewal 钥泄露也只能在 `[已激活, business_deadline]` 内延期，越不过截止、提不了权。
    要延过商务截止或签全新授权，只能用离线 `root` 私钥。比较基准始终是独立保存的 root 锚；
    “不得倒退”的动态字段才额外与当前已采用的 lease 比较。
  - `GET/POST/PUT/DELETE /api/web/license`（走 Bearer，控制台管理：签发/续期/吊销/改联系人）。
- **license_gate 中间件**（方向文档 §2.4，二期）：在 `auth.rs` 验完 token 后，
  查该 token 绑定的 `lic_id` 是否有效 → 过期/吊销返回 `403 {"error":"license_expired|revoked"}`。

### 6.3 措施通道（多镜像 pull）

- **两个长期独立 feed，不能互相覆盖（回应 codex #3）**：
  - `directives.json`（`directive` 角色，`(epoch, seq)`）—— 地址/公告/kill/撤 lic_id/min_version。
  - `recovery.json`（`recovery` 角色，`rseq`）—— 撤 kid / bump directive epoch。**它是独立文件、
    独立单调 `rseq`，永不被普通措施覆盖**；否则"发了 recovery 文件又被下一份普通措施盖掉，
    期间离线的客户端就永远收不到撤钥"。客户端每轮**两个 feed 各拉一次**、各按自己的序列前进。
- 内置**镜像 URL 清单**（面向大陆排序）：Gitee raw → OSS(阿里/腾讯公开只读 bucket) →
  GitHub raw/jsDelivr → 自有 VPS。逐个拉，**第一个验签通过且序列更大**的采用（两 feed 各自判）。
- 拉取时机同 §6.1（随机）。命中 `revoked`/`revoked_kids`/`min_version`/`kill` 即时反映到状态机。
- **发布 = 本地 `tklic sign-directives`（directive）/ `revoke-kid`（recovery）生成对应文件 →
  push 到 Gitee/git + 上传 OSS**。`publish-directives.ps1` 把两个文件三处同步。

---

## 7. 邮件通知（toolkit-server 驱动）

- 依赖 `lettre`（异步 SMTP）；配置全走环境变量，**未配置就不发**（同 TTS/LLM 可选）：
  `SMTP_HOST/PORT/USER/PASS/FROM`、`LICENSE_ALERT_TO`（你的收件箱）。
- 一个 tokio 定时任务（每天一次）扫 `licenses`：
  - 命中 T-30/14/7/3/1 阈值 → 发临期提醒（每 `lic_id`×阈值去重，落一张 `license_alerts` 表记已发）。
  - 事件触发（吊销下发 / refresh 反复失败 / 客户端上报时钟异常）→ 即时发。
- 收件人：`LICENSE_ALERT_TO`（你）+ 可选 `licenses.contact_email`（客户）。
- **定位是带外提醒**：邮件不参与授权判定，可丢可伪造，不驱动客户端任何行为（方向文档 §4.3）。

---

## 8. 签发 CLI（`tklic`，仅 `--features issuer`）

```bash
tklic keygen --role root|renewal|directive|recovery \
             --kid root-a --out root-a.sk               # 每把带 kid+role（私钥可加口令）
tklic issue  --sk root-a.sk --product zuche --subject "某某" \
             --machine <MREQ1...> [--machine <MREQ1...>] \   # 客户端导出的机器请求串(含 id+components)
             --months 6                                 # → business_deadline/expires（纯离线模式）
             [--lease-days 14]                          # 给了 = 可及时吊销模式（带 lease_until）
             [--features speech,english] [--grace 14] [--emergency]   # 签 TKL1（root 角色）
tklic clock-reset --sk recovery-1.sk --machine <MREQ1...> --to <时间> --rseq 12   # 离线校时(仅 recovery)
tklic sign-directives --sk directive-1.sk --in d.json --seq 43        # 签 TKD1（directive 角色）
tklic revoke-kid --sk recovery-1.sk --kid <泄露kid> --rseq 7 \
             [--bump-directive-epoch]                     # 撤销公钥(仅 recovery，独立 rseq，可顺带升 epoch)
tklic inspect <token>                                    # 解码查看（不验也能看结构）
tklic verify  <token>                                    # 本地验签自检（查 kid+role+签名）
```

- `issue` 从 `--machine` / 期限算 payload，用私钥签。`--emergency` 只缩短期限和功能范围，仍必须绑定
  MREQ1；不生成可复制到任意机器的万能码。
- 私钥加载：优先 `--sk` 文件，加密则提示口令（`rpassword`），解密只在内存。

---

## 9. 两期任务与完成判据

### 一期：共享离线核心 + zuche 首次接入

| 工作包 | 内容 | 完成判据 |
|---|---|---|
| **共享核心** | `custom-utils` 增加 `license` / `license-issuer` feature；完成 root/recovery 密钥、TKL1、MREQ1、Linux machine-id、时钟水位、状态机和 `tklic keygen/issue/clock-reset/inspect/verify` | 坏签名、改字段、错 product、错机器、日期不变量、未生效、临期、宽限、过期、回拨和 clock-reset 重放测试全过；prod 未注入公钥时构建失败 |
| **zuche 命令行** | `zuche-rs license machine`、`license import <token>`、`license status`；授权文件原子写入 `~/.config/zuche-rs/license.anchor.tkl` | 全离线完成“导出机器串→签发→导入→生效”；导入失败不破坏旧授权；新 root 授权可恢复过期状态 |
| **zuche web** | 共享 `LicenseRuntime`；始终可访问的授权页；能力级 guard 接到合并后的总 Router 和后台生产任务 | `/api/login|verify|request`、同步、重放、配置修改、验证码 drive/auto/abandon 等生产/写能力受限；授权页、明确列入 allowlist 的只读页和导出可用；未知新增路由默认拒绝生产能力 |
| **zuche 数据红线** | 不删、不改、不加密客户已有数据；补齐真实可用的导出入口，或明确把导出列为一期的独立前置工作包 | 过期前后数据文件一致；受限状态下能导出当前实际持久化的数据。内存 `RunHistory` 不得被文档冒充为已持久化数据 |
| **回归与发布** | custom-utils、zuche 各自完整运行仓库规定的 clippy/fmt/test；aarch64 prod 构建注入真实公钥 | 两仓完整质量循环全绿；测试私钥不进入发布产物和仓库；在一台 Linux 测试机完成到期/恢复演练 |

一期路由策略不能简单写成“GET 放行、POST 禁止”：zuche 的 GET/SSE 中可能驱动后台状态，POST 也包含
未来可能新增的导出操作。应定义 `LicenseCapability::{Activate, Read, Export, Produce, Mutate}`，handler 或
子 Router 显式标注能力；`Restricted` 只允许前三类。最外层 guard 负责兜底，长驻后台循环在每次实际
生产动作前再次读取共享状态，避免仅在启动时检查一次。

`zuche-rs` 当前只手工解析 `--port`，没有命令框架。一期先用一个小型、可测试的参数解析模块在启动
数据库和浏览器前分流 `license` 子命令，不为三个子命令额外引入 clap；后续 CLI 增长到确实需要命令
框架时再单独评估依赖。

### 二期：在线续期、措施运营与其它产品

| 工作包 | 内容 | 完成判据 |
|---|---|---|
| **在线续期** | toolkit-server 台账、受限 renewal 私钥、POST refresh、TKR1 + nonce；客户端保留 root anchor，续期只写 lease | 旧 TKR1 重放被拒；renewal 修改机器/功能/商务截止被拒；续期钥泄露也不能越过 root 商务截止 |
| **措施与恢复** | directive/recovery 双 feed、双序列、epoch、镜像发布脚本、撤 lic/kid、入口轮换 | 高 seq 普通指令压不住 recovery；撤销集合单调；任一镜像故障不影响其它镜像 |
| **运营能力** | 管理页、审计、限流、邮件提醒；在线 renewal 密钥以专用账户和 `0600` 文件/系统凭据保护 | 提醒去重；所有签发/续期/吊销留痕；密钥轮换演练通过 |
| **扩展接入** | zero-desktop 接入；仅对运行在我方服务器的价值能力增加 per-device token→lic_id 硬闸门 | 桌面离线能力遵循同一状态机；远端过期 token 返回 403；服务端不可用不破坏仍有效的离线授权 |

在线私钥无法靠“age 加密文件”单独解决无人值守解密：解密秘密若与文件同机，安全性只等同于该机器账户。
因此二期以最小权限、专用用户、文件权限、进程隔离、审计和可撤销轮换为控制手段；若环境已有 KMS/系统
密钥库再接入，不在设计里虚构一个不存在的安全边界。

---

## 10. 已采用的实现取舍

| 点 | 决策 | 分期说明 |
|---|---|---|
| 首次消费方 | **zuche** | 一期完成；zero-desktop 放二期 |
| 签名算法 | **Ed25519** | 无 C 依赖、体积小；没有国密合规要求时不引入 SM2 复杂度 |
| 一期公钥 | **root×2 + recovery×1** | 主 root、冷备 root、离线时钟恢复；生产构建禁止开发公钥回退 |
| 二期新增公钥 | **renewal×1 + directive×1** | 开在线续期/措施通道时再加入，不让一期承担未使用角色 |
| 离线私钥 | **age 口令加密 + 离线双备份** | root/recovery/directive 不进入服务端和 CI |
| 在线私钥 | **专用 renewal 私钥 + OS 权限/隔离/审计** | 二期放 G10；客户端权限约束 + recovery 可撤销控制泄露半径 |
| 措施镜像 | **Gitee + OSS + GitHub** | 二期实现，面向大陆时 Gitee/OSS 排前 |
| 授权台账 | **一期无台账；二期集中到 toolkit-server** | zuche 不复制一套签发后台，只持本机授权文件；统一管理不同 product |
| 服务端硬闸门 | **仅保护我方远端能力** | 不把客户服务器上的 zuche 本地中间件描述成不可绕过的硬保护 |

---

## 11. 相关

- 方向与信任模型：[docs/license-expiry-design.md](license-expiry-design.md)
- license 核心归属：custom-utils（[D:/git/custom-utils](../../custom-utils)，新增 `license` feature）
- zuche（第一个消费方，web 服务型交付）：[D:/git/zuche](../../zuche) · [DESIGN.md](../../zuche/DESIGN.md)
- 全局 Bearer 鉴权（license_gate 挂载点）：[crates/toolkit-server/src/auth.rs](../crates/toolkit-server/src/auth.rs)
- 外网入口 / host 派生：[crates/zero-desktop/src/shared/settings.rs](../crates/zero-desktop/src/shared/settings.rs)
- SQLite schema（licenses 表落点）：[crates/toolkit-core/src/schema.rs](../crates/toolkit-core/src/schema.rs)
