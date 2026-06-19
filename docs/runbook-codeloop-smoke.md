# Runbook: Codeloop Headless Smoke

> 设计:[docs/codeloop-headless-smoke-runner-plan.md](codeloop-headless-smoke-runner-plan.md)
>
> GUI 人工版:[docs/codeloop-mini-e2e-plan.md](codeloop-mini-e2e-plan.md)

---

## 它是什么

`zero-desktop codeloop-smoke` 子命令,**不开 Tauri 窗口**就能跑一次完整的
设计评审 → 代码实现 → 代码评审 三段编排。DB 行与 UI 路径同构,跑完后桌面端能照常
打开看历史。专门给 CI / 自动化 / 头节点 E2E 用。

## ⚠️ 第一条铁律:**别在你正在用的会话上跑**

`smoke runner` 会通过 `claude --resume <id>` / `codex resume <id>` 真去驱动指定的
agent 会话。如果你传的是**当前桌面端或终端里正在跟你对话的 Claude Code session**,
smoke 自己的 prompts 会被追加进同一个 transcript,你的活会话会变得无法预测。

为防止这种"误劫持",runner 默认做两件事:

1. **`--claude-session` 必填**(传具体 id 或 `auto` 让 runner 按 cwd + 最近活跃自动挑);
   不传直接退出 3。
2. **劫持保护**:即使显式指定,如果 transcript 的 `updated_at` 在最近 5 分钟内仍有写入,
   也拒绝(退出 3)。`updated_at` 解析失败同样 fail-closed。

要绕过(开发机上**不要**这么做)就加 `--allow-hijack-current-session`。CI / 自动化里
更推荐另一条路:**在独立 cwd 里跑**(`git clone` 一份;或在 `~/codeloop-smoke-test`
一类目录里建独立测试仓),session 自然不会跟开发会话冲突。

## 两种安全的调用形态

### CI / 无人值守

```powershell
cargo run -p zero-desktop -- codeloop-smoke `
  --repo C:\Users\<me>\codeloop-smoke-test `
  --target design.md `
  --claude-session <id> `
  --max-rounds 2 `
  --new-codex-agent `
  --auto-confirm `
  --json
```

要点:
- `--repo` 指向一个**独立测试仓**,不是当前正在开发的仓。**且必须在 `$HOME`(Windows
  为 `%USERPROFILE%`)下** —— codeloop 的 worktree 创建限制要求 worktree 在用户目录内,
  否则实现阶段虽然能跑完但 `worktree_path` 不会被 DB 写入,smoke 自检不变式触发 exit 2。
- `--claude-session <id>` 必填;`--new-codex-agent` 让 runner 临时新建 Codex 会话,
  避免也得管 Codex 端 id。
- `--auto-confirm` 是无人值守语义,不开会 exit 3。
- `--json` → stdout 是纯 JSONL,适合 CI 解析;人类摘要 / logger 全部走 stderr。

### 本地观察(每阶段卡 gate)

去掉 `--auto-confirm`、`--json`(让人盯):

```powershell
cargo run -p zero-desktop -- codeloop-smoke `
  --repo C:\Users\<me>\codeloop-smoke-test `
  --target design.md `
  --claude-session <id> `
  --new-codex-agent
```

注:目前 runner v1 要求 `--auto-confirm`(见 plan §Command Shape)—— 上面这条命令
当前会直接 exit 3。等"交互式 confirm gate"落地后再放开。

## 准备一个独立测试仓(一次性)

```powershell
# 必须在 $HOME 下
$dir = "$env:USERPROFILE\codeloop-smoke-test"
mkdir $dir
cd $dir
git init -q
git config core.autocrlf false ; git config core.eol lf  # 字节布局类 design 防 CRLF
# 放一份 self-contained 的 design.md,见下方"target 的写法"
git add . ; git commit -qm "init"

# 在这个 cwd 下用 `claude -p` 种一条会话(必需 —— v1 还不会自动新建 Claude session)
claude -p "你好。仅回复'收到'。" --permission-mode plan
# 找最新 jsonl 拿到 session id
ls $env:USERPROFILE\.claude\projects\C--Users-<me>-codeloop-smoke-test\
```

把那个 `<uuid>.jsonl` 的 uuid 当成 `--claude-session` 的值传给 smoke。

## target 的写法

写一份**自包含**的小 design,描述足够清晰、不引用本仓外的代码。最好:

- 字节布局或文件计数类的可机器验收的产物(`hello.txt` 22 字节、最后一字节 `0a` 之类),
  避免 Codex / Claude 反复在"风格 / 措辞"上拉锯;
- 明确写出"评审范围**只**包含新增 X,主工作树里其它未跟踪文件(`.smoke-ws/`、`*.log`)
  不在本设计范围内" —— 防 Codex 复核抓你 smoke 自己的临时产物当 NeedsWork 理由;
- 字节布局类一定固定 LF / 不允许 CRLF 之类的换行符约定,否则平台差异会触发模型 ASK_USER。

参考实例见 git 历史里 `C:\Users\36225\codeloop-smoke-test\design.md`(写法范本)。

## 常见退出码与含义

| 码 | 含义 | 典型情形 | 重试? |
|---:|---|---|---|
| 0 | smoke 全程通过。 | design / impl / review 三段都 done/pass。 | — |
| 1 | LLM 判定失败或被中止。 | Codex `NEEDS_WORK` 跑满 max_rounds;`ASK_USER` 没答案被中止;全局 `--timeout` 到期(此时 runner 会在 DB 里把活跃 loop 自动 finalize 成 `aborted_timeout`,不会留 running 行)。 | 可以,可能换轮就过。 |
| 2 | runner 自检不变式违反。 | 实现 PASS 但 DB 没 `worktree_path`(产品 bug,见 [task_859ecd39](../crates/zero-desktop/src/modules/codeloop/mod.rs));review 记录 round 0 出现 `claude_implement`;DB 一致性查询失败。 | **不要**重试,先排查。 |
| 3 | preflight / 配置错。 | 未传 `--claude-session`、未传 `--auto-confirm`、`--target` 不存在、`--repo` 不是 git 顶层、**触发劫持保护**、`--recover-after-stop`(v1 占位)。 | 修参数或 cwd 后再来。 |
| 4 | `--verify` 的 cargo 命令失败。 | 流程通过但产物不编译 / 不测。 | 看日志 tail。 |

## 常见失败模式排查

### `[error/preflight] 缺少 --claude-session`(exit 3)
你忘了传。要么显式传 `<uuid>`,要么传 `auto`(仍受劫持保护)。

### `Claude 会话 <id> 的 transcript 在最近 Ns 内仍有写入`(exit 3)
劫持保护命中。**这不是 bug,是护栏**。两种解法:
- 推荐:用一个静默已久的 session,或换一个**独立测试仓**(那里没有你正在用的活会话);
- 不推荐:加 `--allow-hijack-current-session` 强行放行。开发机上千万别这么干。

### `实现记录已 PASS 但 worktree_path 为空`(exit 2)
你的 `--repo` 不在用户家目录下。codeloop 的 worktree 校验拒绝 `$HOME` 之外的路径
([mod.rs:617](../crates/zero-desktop/src/modules/codeloop/mod.rs:617))。把测试仓搬到
`$HOME\codeloop-smoke-test` 之类。

### `final_verdict=aborted_timeout`(exit 1)
全局 `--timeout` 到期了。runner 已经把这条 loop 在 DB 里写成 `aborted_timeout`(不会
留 `running` 行),JSON `smoke_done` 会带 `aborted_loop_id`。两条路:
- 加长 `--timeout`(默认 `15m`);
- 检查 driver 是否卡在 Codex / Claude 真后端慢响应。

### Claude 触发 `ASK_USER`,smoke 中止(exit 1)
你的 design.md 在某个细节上模糊。两种解法:
- 改 design,把那个细节写死(`'\n' 而非 '\r\n'`、`UTC` 而非 local、字节数精确到 21 而
  非"大约 20")。改完重新 commit。
- 提供 `--ask-user-answers map.json`,用子串匹配预备答案。**不允许**静默自动放行。

### Codex 抓主工作树脏文件当 NeedsWork
你把 smoke 的 `--workspace` 放进 `--repo` 里了,DB / logs 都成了 `--repo` 的未跟踪文件。
**workspace 必须放仓库外**(默认 `%LOCALAPPDATA%\zero-desktop`,或显式
`--workspace D:\tmp\smoke-ws`)。

## 看历史

smoke 跑出来的 DB 行就是普通 codeloop 行,桌面端打开 Codeloop 模块就能看。如果只想
命令行查:

```powershell
sqlite3 "$env:LOCALAPPDATA\zero-desktop\codeloop\state.db" `
  "SELECT id, mode, status, final_verdict, worktree_path FROM loops ORDER BY id DESC LIMIT 10;"
```

## 还没实现 / 已知坑

- `--recover-after-stop`:v1 占位,会直接 exit 3。设计见 plan §5,helper 落地后放开。
- v1 不能新建 Claude session(`agent-session` 只暴露了 `create_codex_session`),所以必须
  你手动 `claude -p ...` 预热一条;之后才有 id 给 `--claude-session`。
- 劫持保护**只**查 `SessionSummary.updated_at`,**不**查 `CLAUDE_PROJECT_DIR` 环境变量
  也**不**扫描运行中的 `claude` 子进程。
- worktree 路径必须在 `$HOME` 下,见上面那条 exit 2 排查。
