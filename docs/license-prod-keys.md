# 生产授权密钥运维清单

> 交付客户用的 zuche（及后续产品）**生产签名密钥**的生成 / 加密 / 备份 / 签发 / 构建全流程。
> 设计背景见 [license-impl-design.md](license-impl-design.md) §2。
>
> 🔑 **信任根**：这些私钥是整套授权系统的信任根。**丢了它 = 无法再签发/续期；泄露它 = 任何人都能
> 伪造 license。** 按本清单严格保管。

工具位置（本机）：
- `tklic` = `D:\git\custom-utils\target\debug\tklic.exe`（`cargo build --features license-issuer --bin tklic` 产出）
- `age` = 已装（winget，v1.3.1），直接 `age`。

⚠️ **两个高频坑**：
1. **不要把 `.age` / `.seed` 放在任何 `target/` 目录**——`cargo clean` 会删掉。放独立目录 / 加密 U 盘。
2. **keygen 打印的 `kid:role:hex` 公钥行要当场存好**（那是 prod 构建要的 `LICENSE_PUBKEY`）。seed 文件本身**不含** kid/role，事后从 `.age` 恢复不出公钥行——见 §5。

---

## 1. 生成密钥（离线机器上做）

当前离线授权需 **root×2（主 + 冷备）+ recovery×1**（renewal/directive 是在线续期/措施 feed 用的，
部署那些时再生成）。每条命令：**stdout 打印 `kid:role:hex` 公钥行**（存好！），seed 写文件。

```powershell
$TK = "D:\git\custom-utils\target\debug\tklic.exe"
& $TK keygen --role root     --kid root-a     --out root-a.seed
& $TK keygen --role root     --kid root-b     --out root-b.seed
& $TK keygen --role recovery --kid recovery-1 --out recovery-1.seed
```

把三行公钥拼成**公钥表**（公钥不是秘密，可进构建脚本 / env），存档：
```
root-a:root:<hex>,root-b:root:<hex>,recovery-1:recovery:<hex>
```

## 2. age 加密（删明文前必做）

```powershell
age -p -o root-a.age     root-a.seed        # 会让你输两遍口令
age -p -o root-b.age     root-b.seed
age -p -o recovery-1.age recovery-1.seed
```
确认三个 `.age` 生成后，**安全删除明文 seed**：
```powershell
Remove-Item root-a.seed, root-b.seed, recovery-1.seed
```

## 3. 备份（三份，缺一不可）

| 备份 | 内容 | 防的是 |
|---|---|---|
| **加密 U 盘 ×2**（不同盘、异地） | 三个 `.age` | U 盘物理损坏 |
| **纸质**（异地存） | 每把 seed 的 hex（§4）+ 对应 kid/role + **age 口令** | U 盘全坏 / 忘口令 |
| **公钥表**（可数字存，非秘密） | §1 的 `kid:role:hex,...` | prod 构建要用 |

⚠️ **age 口令丢了 = `.age` 永久打不开**，所以纸质里要么写 hex（可不靠 age 直接恢复）、要么把口令也记上。

## 4. 纸质备份：导出 seed 的 hex

```powershell
# 32 字节 seed 的 hex（tklic 文件 = 5 字节 magic + 32 字节 seed，跳过前 5）
$b = [IO.File]::ReadAllBytes("root-a.seed"); [Convert]::ToHexString($b[5..36])
```
抄下这 64 个 hex 字符 + 「root-a / role=root」。从纸恢复：把 hex 转回 32 字节、加 `TKSK1` 头写文件
（需要时找 Claude 给还原脚本，或用下方 §5 的待加命令）。

## 5. 公钥行丢了怎么找回（`tklic seed-pubkey`）

§1 的 `kid:role:hex` 公钥行如果没存下来，**不用重新生成密钥**——从 `.age` 反推即可
（公钥可由私钥推出；`kid`/`role` 是标签，seed 里不含，需你重新给出）：

```powershell
$TK = "D:\git\custom-utils\target\debug\tklic.exe"
age -d root-a.age     | & $TK seed-pubkey --sk - --kid root-a     --role root
age -d root-b.age     | & $TK seed-pubkey --sk - --kid root-b     --role root
age -d recovery-1.age | & $TK seed-pubkey --sk - --kid recovery-1 --role recovery
```
三行用 `,` 拼起来即 `LICENSE_PUBKEY`（§7）。也支持 `--sk <seed文件>` 直接读文件。

> `kid`/`role` 给错不会有安全风险（只会让下游按错角色验签而拒绝），但要与当初 keygen 时一致，
> 否则签出的令牌客户端认不了。**仍建议 keygen 时当场把公钥行存档**，这条只是补救。

**尚缺（低优先）**：`seed↔hex` 互转命令（纸质备份/恢复现靠 §4 的 PowerShell 手搓）。见 [license-todo.md](license-todo.md)。

## 6. 签发生产 license

明文不落盘，`age -d` 解密直接管道进 tklic：
```powershell
$TK = "D:\git\custom-utils\target\debug\tklic.exe"
age -d root-a.age | & $TK issue --sk - --kid root-a --product zuche `
  --subject "客户名" --machine "<客户的 MREQ1 整串>" --months 3
```
客户拿到打印的 `TKL1...` 令牌 → 在其机器上 `zuche-rs license import <令牌>`。
（客户机器怎么拿 MREQ1：跑 `zuche-rs license machine`，见 zuche 仓 README「软件授权」节。）

## 7. 生产构建（注入真公钥）

```powershell
$env:LICENSE_PUBKEY = "root-a:root:<hex>,root-b:root:<hex>,recovery-1:recovery:<hex>"
cd D:\git\zuche
cargo make build          # = --features prod；未注入 LICENSE_PUBKEY 会故意失败
```
产出 `target/release/zuche-rs.exe` 才是能交付客户的版本（认真 root 公钥，不认开发公钥）。

## 8. 红线

- 私钥 `*.seed` / `*.age` **绝不进 git / CI / 云盘 / target 目录**。
- `root-b` 是冷备，锁起来不用——root-a 泄露时用它重签 + 换公钥表。
- 生成/签发尽量在**离线机**上做；私钥只在你手里，别让任何自动化/远程持有。
