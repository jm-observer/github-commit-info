# 软件授权（license）待办

> 已落地部分见 [docs/license-impl-design.md](license-impl-design.md) §9（一/二期 ✅、三期在线续期 ✅、三期措施 feed ✅ 见下 §1）。
> 本文件记**已决定但暂缓 / 未做**的部分，避免遗失。

---

## 1. 措施 feed —— ✅ 代码已落地（2026-08-10），**待你签生产 directive 子钥才能真正上线**

**是什么**：一个你签名的「公告文件」挂在 COS/Gitee/GitHub 多镜像上，客户端定期拉取、验签、照办
（一对多广播，不依赖服务器在线）。设计见 [license-impl-design.md](license-impl-design.md) §2.5 / §3.3 / §6.3。

**为什么做了**：原「暂缓」的触发条件是「担心签名钥泄露要能补救 / 纯离线客户需中途掐 / 换外网入口
要老客户端改道」。实做后发现代价比预估小得多——**委托机制让它不需要改 `LICENSE_PUBKEY`、不需要
重新构建分发已装机的客户端**（directive 是 root 委托的子钥，证书随 feed 文件走），所以提前做掉了。

### 已完成

**custom-utils**
- `decode_and_verify_tkd1_delegated`（`directive.rs`）：委托链验 directive feed。返回 `DirectiveBody`
  而非 `DirectiveEnvelope`——类型上杜绝「委托钥签 recovery 动作」。
- `feed.rs`：feed 文件格式 `{feed_version, token, cert?}` + 多镜像拉取 + 两个 feed 各自的验签口径。
  拉取端**自动识别 forge contents-API 的 base64 包装**，所以镜像清单可混装 raw URL 和 API URL。
- `feed_runtime.rs`：`FeedRuntime` —— 消费方只管「何时拉」「拿结果干什么」，其余全在库里：
  镜像顺序 / 双序列 / 撤销并集累加 / 原子持久化 / **recovery 先于 directive 拉**（否则被撤的
  签名链能在同一轮里再生效一次）/ **拉不到不改变任何状态**（绝不因拉不到而受限）。
- `publish.rs` + `tklic publish`：COS（原生 HMAC-SHA1 签名）/ GitHub / Gitee / 本地目录四种目标，
  发布前强制本地验签，逐镜像报结果（镜像是冗余不是事务，一处失败不回滚其它）。
- `tklic fetch-feed`：以客户端视角匿名读回自检。
- `tklic sign-directives --cert --feed-out` / `revoke-kid --feed-out`：产出可直接发布的 feed 文件。
- `scripts/publish-directives.ps1`：重写为 `tklic publish` 的薄封装（固化镜像清单 + 读回自检）。

**zuche**（只做接线，规则一律不重造）
- `LicenseRuntime` 持 `FeedRuntime`；`compute_state` 的撤销 kid / 撤销 lic_id / min_version 三处
  全部来自 `feed.measures()`（原先是 `RevokedSet::new()` 和 `&[]` 恒空）。
- `poll_feed()` + `main.rs` 后台循环（复用在线续期的 jitter 节奏）。**不依赖任何配置即启用**——
  feed 是离线线自己的能力，与 `LICENSE_REFRESH_URL` 无关。
- `LICENSE_FEED_MIRRORS` 可覆盖内置镜像清单；设为空串 = 完全关闭 feed（纯离线交付）。

**测试**：custom-utils 7 个端到端（wiremock + 真签名）覆盖 replay 拒绝 / 撤 kid 同轮生效 /
epoch bump 解卡 MAX seq / 镜像回退 / 全挂不报错；zuche 4 个覆盖接线（撤销→受限且重启后仍受限、
min_version→受限、拉不到→不受影响、陌生密钥签的 feed→整份丢弃）。

**真机验证**：三个镜像写入与匿名读回全部通过（用一次性演练密钥，`--prefix smoke`）。

### 待做（**只剩需要你的私钥的部分**）
1. **生成生产 `directive-1` 子钥 + 用 root-a 签委托证书**（要 age 口令，只有你能做）：
   ```
   tklic keygen --role directive --kid directive-1 --out directive-1.seed
   age -d root-a.age | tklic delegate --sk - --kid root-a --sub-kid directive-1 \
       --role directive --sub-pub <上一步的 hex> --months 12
   ```
   证书串存好，每次 `sign-directives --cert` 都要用。
2. **发首份正式 feed**：`seq` 从 **1** 起（`0` 是「尚未接受任何」的哨兵，签 0 会被静默拒绝）。
3. **清理 smoke 演练文件**：三个镜像的 `smoke/` 路径下还留着演练密钥签的 feed（内容无秘密）。
   `tklic` 目前没有 unpublish 子命令，需手工删或留作通道健康探针。

---

## 2. 其它待办

- **custom-utils 发版 crates.io（部署前提，绕不过）**：`license` / `license-sign` /
  **`license-feed` / `license-publish`**（措施 feed 新增）feature 目前只在
  `main` 未发版；toolkit + zuche 现用 `[patch.crates-io] custom-utils = path` 引本地。G10 交叉编译
  （Docker）**编不了 path patch** → 部署 renewal 端点前必须发一个含 license 的新版本（bump 0.16→0.17
  + `cargo publish`），然后删两处 patch、改回版本号依赖。
- **build.rs 公钥改 committed 文件**（§2.3 计划）：二期 zuche `build.rs` 现用 `LICENSE_PUBKEY` 环境变量 +
  内置 dev 常量；三期改成读 `license-pubkeys/{prod,dev}.txt`（公钥非秘密可进仓库，免每次 prod 构建记得设 env）。
- **真机 e2e 联跑**：在线续期这条链（zuche 客户端 → toolkit-server `/api/license/refresh`）尚未 client↔server
  真机跑通，各层单测/集成测试齐全但没端到端联跑过。
- **zero-desktop 接入**（三期未做）：Windows 指纹 + Tauri guard；仅对跑在我方服务器的价值能力加
  per-device token→lic_id 硬闸门。
- **生产真密钥离线生成**：`root×2 + recovery×1`（+ 三期 `renewal×1 + directive×1` 委托子钥）须在离线机
  `tklic keygen` 生成、`age` 加密、双备份；renewal 私钥 + 委托证书部署到 G10（`LICENSE_RENEWAL_*`）。
