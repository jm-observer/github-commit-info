# 授权管理平台（license 控制台）设计

> 给 toolkit-server 控制台加一个 **license 页**：台账看板 + 生命周期操作 + **签发助手**。
> 上游：[license-impl-design.md](license-impl-design.md)（信任模型/端点）、[license-prod-keys.md](license-prod-keys.md)（离线密钥流程）。
>
> 状态：设计草案，待拍板后实现。

---

## 0. 一句话

**平台管「台账和生命周期」，root 签发始终离线。** 平台不持私钥、不签令牌，只做数据管理 +
把离线命令「拼好给你复制」。

---

## 1. 安全边界（先划死，这是本设计的地基）

| 能做（平台内） | 不能做（永远离线） |
|---|---|
| 台账 CRUD：客户/到期日/绑定机器/续期参数 | ❌ 持有 root / recovery 私钥 |
| 吊销、改续期窗口、改联系人 | ❌ 签发初始 license（TKL1 anchor） |
| 看板：临期高亮、状态统计、续期记录 | ❌ 签委托证书、签措施 feed |
| **签发助手**：拼出 `tklic issue` 命令给你复制 | ❌ 把 `.age`/seed 上传到服务器 |
| 登记：把离线签好的令牌粘回来存档 | ❌ 任何形式的"在线签发按钮" |

**为什么**：root 是整套系统的信任根，一旦放到 web 可达的服务器上，攻破服务器 = 攻破全部授权
（能给任意机器签任意期限）。而 **renewal 私钥**在 G10 上是可接受的——它权限最小（只能在
`[已激活, business_deadline]` 内延期，改不了机器/功能位，且受委托证书有效期封顶）。

**签发助手的工作流**（关键设计，兼顾便利与安全）：
```
客户机: zuche-rs license machine → MREQ1 串 → 发给你
  ↓
平台: 新建台账（填客户名/期限/粘 MREQ1）→ 生成一条 tklic 命令（含 MREQ1、期限、product）
  ↓ 你复制
离线机: age -d root-a.age | tklic issue --sk - ... → 得到 TKL1 令牌
  ↓ 你粘回
平台: 「登记令牌」→ 校验（解码+比对台账字段）+ 存档，看板显示"已签发"
  ↓
客户机: zuche-rs license import <令牌>
```

---

## 2. 现状：后端其实已经有了

做在线续期时已建好 `/api/web/license`（Bearer 鉴权，见 `crates/toolkit-server/src/license/routes.rs`）：

| 端点 | 现状 | 平台用途 |
|---|---|---|
| `GET /api/web/license` | ✅ 已有 | 看板列表 |
| `POST /api/web/license` | ✅ 已有（收 MREQ1 数组，解码落库） | 新建台账 |
| `PUT /api/web/license/{lic_id}` | ✅ 已有（改 grant_window/lease/联系人） | 改续期参数 |
| `POST /api/web/license/{lic_id}/revoke` | ✅ 已有 | 吊销 |
| `DELETE /api/web/license/{lic_id}` | ✅ 已有 | 删除 |

**所以平台的主体工作量在前端**，后端只需补几个小端点（§4）。

---

## 3. 前端形态（贴合现有控制台，零新依赖）

照抄 `hub` 的既有模式——**原生 HTML/JS/CSS 三件套 + `include_str!` 进二进制**，
不引入任何前端框架/构建链：

```
crates/toolkit-server/web/license.html   # 页面骨架
crates/toolkit-server/web/license.js     # fetch /api/web/license + 渲染 + 交互
crates/toolkit-server/web/license.css    # 样式（可复用 style.css，仅补差异）
```
`static_assets.rs` 加三个 `include_str!` + 三个 handler；`lib.rs::build_router` 加
`/license`、`/license.js`、`/license.css` 三条路由（与 `/hub` 并列）。

**鉴权**：页面本身走静态豁免（同 hub），**数据端点 `/api/web/license` 仍要 Bearer**。
控制台目前没有填 token 的入口 —— 沿用现有做法（页面上一个 token 输入框存 `localStorage`，
fetch 时带 `Authorization: Bearer`），与 exec 页面同风格。

---

## 4. 页面设计

### 4.1 看板（主视图）

一张表 + 顶部统计条：

| 列 | 说明 |
|---|---|
| 客户 (`subject`) | 主标识 |
| `lic_id` | 台账主键，可复制 |
| **状态** | 徽标：`有效` / `临期`(≤30d 黄) / `已过期`(红) / `已吊销`(灰) |
| **商务截止** | `business_deadline` + 剩余天数 |
| 续期窗口 | `grant_window_days`；`lease_days`（空=纯离线） |
| 机器 | 绑定台数，hover 看 machine.id |
| 联系人 | `contact_email` |
| 操作 | 改参数 / 吊销 / 删除 / 查看详情 |

顶部统计：`总数 / 有效 / 30 天内到期 / 已过期 / 已吊销`，临期数字点击可筛选。
默认按「剩余天数升序」排 —— **最该处理的排最前**。

### 4.2 新建（签发助手）

表单：客户名、`lic_id`（可自动生成 `L-YYYY-NNNN`）、product（下拉：zuche/zero-desktop）、
`business_deadline`（日期选择）、`grant_window_days`（默认 30）、`lease_days`（可空）、
`grace_days`（默认 14）、features（可选）、联系人邮箱、备注、**MREQ1 串（可多行，一行一台）**。

提交 → `POST /api/web/license` 落台账 → 页面**立即显示一条可复制的离线签发命令**：
```
age -d root-a.age | tklic issue --sk - --kid root-a --product zuche \
  --subject "客户名" --machine "MREQ1.xxx" --months 3
```
（`--months` 由 `business_deadline` 反算；多台机器多个 `--machine`。）

### 4.3 详情 / 登记令牌

详情抽屉显示台账全字段 + 绑定机器列表 + **续期记录**（§4.4）+ 一个「登记已签发令牌」框：
粘贴离线签好的 `TKL1...` → 后端**解码并比对**（product/lic_id/machine/business_deadline 是否与台账一致）
→ 一致则存档（`issued_token` 列）+ 标记「已签发」；不一致**明确报错**（防止把张冠李戴的令牌发给客户）。

> 存令牌只为**留档和排查**（令牌不是秘密，客户手里也有一份）。

---

## 5. 需要补的后端（小）

1. **`POST /api/web/license/{lic_id}/register-token`**：登记离线签发的令牌。
   解码验签（用**已 bake 的公钥表**——服务端也需要一份 `LICENSE_PUBKEY` 才能验，
   或仅做**结构解析 + 字段比对**不验签）+ 比对台账 → 存 `issued_token` / `issued_at`。
   **取舍**：服务端要不要验签?验签需要它持有公钥表（公钥非秘密，可配 env）。**建议验**——
   能挡住"粘错令牌"这类操作事故。
2. **`licenses` 表加两列**（纯加列，走 `migrations.rs` 幂等 ALTER，因 `CREATE TABLE IF NOT EXISTS`
   不会给已有表补列）：`issued_token TEXT NULL`、`issued_at TEXT NULL`。
3. **`GET /api/web/license/{lic_id}/refresh-log`**（可选，见 §4.4）。

### 4.4 续期记录（可选，二期加）
现在 `/api/license/refresh` 只打日志、不落库。想在看板看到「谁最后续期于何时」，需加一张
`license_refresh_log(lic_id, at, expires_at_granted, peer_ip)`。**价值**：一眼看出哪个客户端
长期没来续期（可能已下线/被弃用/网络不通）。**建议**：一期先不做，看板够用了再说。

---

## 6. 分期

| 阶段 | 内容 | 判据 |
|---|---|---|
| **P1（核心）** | license.html/js/css 三件套 + 看板（列表/状态徽标/统计/排序筛选）+ 新建表单 + 签发助手命令生成 + 改参数/吊销/删除 | 能完全替代手敲 curl 管台账；临期一眼可见 |
| **P2** | 登记令牌（含 `issued_token` 两列 + register-token 端点 + 字段比对）+ 详情抽屉 | 签发闭环留档；粘错令牌被拒 |
| **P3（可选）** | 续期记录表 + 看板「最后续期」列；与邮件提醒联动展示「已发提醒」 | 能看出僵尸客户端 |

---

## 7. 取舍与非目标

- **不做多用户/权限系统**：这是你自用的运营台，沿用单一 `TOOLKIT_API_TOKEN`。真要多人用再说。
- **不做在线签发**：见 §1，红线。
- **不做前端框架**：与现有控制台一致，原生三件套；表格 + 表单不值得引入构建链。
- **不与措施 feed 耦合**：feed 已搁置（见 [license-todo.md](license-todo.md)）；将来做了可在本页加
  「发布措施」入口（同样只生成离线 `tklic sign-directives` 命令，不在线签）。

---

## 8. 相关

- 端点实现：[crates/toolkit-server/src/license/routes.rs](../crates/toolkit-server/src/license/routes.rs)
- 台账表：[crates/toolkit-core/src/schema.rs](../crates/toolkit-core/src/schema.rs)（`licenses`）
- 现有控制台形态参考：`crates/toolkit-server/web/hub.{html,js,css}` + `static_assets.rs`
- 离线密钥/签发流程：[license-prod-keys.md](license-prod-keys.md)
