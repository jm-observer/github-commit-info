# 软件授权（license）待办

> 已落地部分见 [docs/license-impl-design.md](license-impl-design.md) §9（一/二期 ✅、三期在线续期 ✅）。
> 本文件记**已决定但暂缓 / 未做**的部分，避免遗失。

---

## 1. 措施 feed 客户端拉取 —— 暂缓（decision: 保持委托，先不做）

**是什么**：一个你签名的「公告文件」挂在 Gitee/OSS 多镜像上，客户端定期拉取、验签、照办
（一对多广播，不依赖服务器在线）。设计见 [license-impl-design.md](license-impl-design.md) §2.5 / §3.3 / §6.3。

**为什么暂缓**：当前模式（短 license ~3 个月 + 在线续期）已覆盖多数撤销场景——想掐一个走续期的
客户，**台账里不给他续、lease 到期即受限**（≈"停止续期即撤销"）；短 license 最坏自然到期。
feed 真正独一份的价值只有三样，当前不急：① **撤销泄露的签名钥**（recovery feed，安全兜底）
② **纯离线客户**提前撤销 ③ **换服务器地址广播** / 公告。

**触发条件（出现任一再做）**：担心签名钥泄露要能补救 / 发长期或纯离线 license 需中途掐 / 换外网入口要老客户端自动改道。

### 已完成（签发侧，custom-utils，**未 commit**）
- `tklic sign-directives`（directive 角色签 `directives.json`：entries/revoked lic_id/min_version/notices/kill）。
- `tklic revoke-kid`（recovery 角色签 `recovery.json`：revoked_kids + 可选 bump directive epoch）。
- `scripts/publish-directives.ps1` 骨架（多镜像发布，Gitee/OSS 目标为 TODO 占位）。

### 待做
1. **custom-utils 一个 foundation 缺口**：`decode_and_verify_directive_delegated`
   —— directive 是 **root 委托的子密钥**（§2.4），但现有 `decode_and_verify_tkd1` 只认 **baked kid**
   （走 `verify_as`），验不了委托签的 directive TKD1。需镜像 `decode_and_verify_license_delegated`
   加一个「委托链验 TKD1」的函数（directive feed 随文件带证书）。
   **recovery feed 不受影响**——recovery 是直接 bake 的锚，现有 `decode_and_verify_tkd1` 就能验。
2. **zuche 客户端拉取**（`crates/app-core/src/license.rs`）：
   - 多镜像拉 `directives.json` + `recovery.json`（随随机 ticker，同在线续期节奏）。
   - 验签：directive 走委托链（上面第 1 项）、recovery 走 baked。
   - **两套独立单调状态**持久化：directive 的 `(epoch, seq)`、recovery 的 `rseq`；`revoked` /
     `revoked_kids` **并集累加、不可逆**（设计 §2.5）。
   - 接进 `compute_state`：目前 `revoked_lic_ids` 传的是 `&[]`、`RevokedSet::new()` 恒空——把拉到的
     撤销名单喂进去，命中即受限。
3. **发布脚本**：`publish-directives.ps1` 填真实 Gitee 仓 + OSS bucket（凭据你的）。

---

## 2. 其它待办

- **custom-utils 发版 crates.io（部署前提，绕不过）**：`license` / `license-sign` feature 目前只在
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
