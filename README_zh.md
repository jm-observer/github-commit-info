# toolkit

**toolkit 工具中台**（tools-server）—— zero / Agent 生态的统一工具能力底座。

承载 wechat → Agent(zero) ⇄ llm(GB10) ⇄ **tools-server(toolkit-\*)** ⇄ english 架构里的
「工具侧」：一个 Cargo workspace 汇聚基础能力（抖音管线、RAG、长任务引擎、HTTP API、桌面 cookie 采集）
以及若干独立 CLI 工具，整体部署在 G10 设备上。

[English](./README.md) | [中文](./README_zh.md)

> 面向贡献者的工程导航见 [CLAUDE.md](./CLAUDE.md)。

## workspace 成员

| crate | 职责 |
|---|---|
| `toolkit-core` | 领域类型、SQLite schema + 迁移、URL 模式识别。 |
| `toolkit-tasks` | 通用长任务引擎：`TaskKind` 注册、submit→spawn→状态机、SQLite 持久化。 |
| `toolkit-server` | axum daemon，装配 core/tasks + 业务模块；HTTP API + web 控制台；systemd 安装/自更新。 |
| `zero-desktop` | 统一 Tauri 桌面壳：cookie 采集（抖音/同花顺）、语音、英语、codeloop、G10 部署。 |
| `douyin` | 抖音 web 工具：a-bogus 签名、作者 / 作品 / 标签 API、下载 + ASR 管线、knowledge md。 |
| `rag` | 抖音 knowledge md 的语义检索 → sqlite-vec（CLI `ingest`/`search`，HTTP `serve`）。 |
| `github-commit-info` | 独立 CLI 工具：获取 GitHub 仓库指定时间范围的 commit 信息（见下）。 |
| `hf-watcher` | 独立 CLI 工具：HuggingFace trending / model-card 监听。 |

## 构建与运行

```bash
cargo check --workspace
cargo test  --workspace        # zero-desktop 需 Tauri 工具链，CI 式环境可排除
cargo run -p toolkit-server -- serve --workspace ./data --bind 127.0.0.1:8788
```

交叉编译并部署 CLI 工具到 G10：

```powershell
pwsh ./deploy-g10.ps1
```

---

## 工具：github-commit-info

获取 GitHub 仓库指定时间范围内的 commit 信息。

### 环境变量

```bash
export GITHUB_TOKEN=ghp_xxxxxxxxxxxx   # 必须；权限 public_repo 或 repo
```

### 使用方法

```bash
github-commit-info --url <URL> [--branch main] [--start-date 2024-01-01] [--days 7] [--output commits.json]
```

| 参数 | 说明 |
|------|------|
| `--url` | GitHub 仓库 URL，如 `https://github.com/golang/go` |
| `--branch` | 分支（可选，不指定自动取默认分支） |
| `--start-date` | `yyyy-MM-dd`（可选，默认昨天） |
| `--days` | 从起始日期起的天数（可选，默认 1） |
| `--output` | 输出文件（可选，默认 stdout） |

输出为紧凑 JSON 数组：`{sha, message, author, email, date, html_url}`。
