# toolkit

**toolkit 工具中台**（tools-server）—— zero / Agent 生态的统一工具能力底座。

A unified tools-server backing the wechat → Agent(zero) ⇄ llm(GB10) ⇄ **tools-server(toolkit-\*)** ⇄ english
architecture. One Cargo workspace aggregates the base capabilities (Douyin pipeline, RAG, long-running task
engine, HTTP API, desktop cookie harvester) plus a few standalone CLI tools, deployed together on the G10 device.

[English](./README.md) | [中文](./README_zh.md)

> Project guidance for contributors lives in [CLAUDE.md](./CLAUDE.md).

## Workspace members

| Crate | Role |
|---|---|
| `toolkit-core` | Domain types, SQLite schema + migrations, URL classification. |
| `toolkit-tasks` | Generic long-task engine: `TaskKind` registry, submit→spawn→state machine, SQLite persistence. |
| `toolkit-server` | axum daemon assembling core/tasks + business modules; HTTP API + web console; systemd install/self-update. |
| `zero-desktop` | Unified Tauri desktop shell: cookie capture (Douyin/THS), speech, English, music, G10 deploy. |
| `douyin` | Douyin web tools: a-bogus signing, creator/works/tags API, download + ASR pipeline, knowledge md. |
| `rag` | Semantic search over Douyin knowledge md → sqlite-vec (CLI `ingest`/`search`, HTTP `serve`). |
| `github-commit-info` | Standalone CLI tool: fetch GitHub repo commits within a time range (see below). |
| `hf-watcher` | Standalone CLI tool: HuggingFace trending / model-card watcher. |

## Build & run

```bash
cargo check --workspace
cargo test  --workspace        # zero-desktop needs Tauri toolchain; exclude on CI-like envs
cargo run -p toolkit-server -- serve --workspace ./data --bind 127.0.0.1:8788
```

Cross-compile + deploy the CLI tools to the G10 device:

```powershell
pwsh ./deploy-g10.ps1
```

---

## Tool: github-commit-info

Fetch commit information from GitHub repositories within a specified time range.

### Environment

```bash
export GITHUB_TOKEN=ghp_xxxxxxxxxxxx   # required; scope: public_repo or repo
```

### Usage

```bash
github-commit-info --url <URL> [--branch main] [--start-date 2024-01-01] [--days 7] [--output commits.json]
```

| Option | Description |
|--------|-------------|
| `--url` | GitHub repository URL, e.g. `https://github.com/golang/go` |
| `--branch` | Branch (optional; auto-detects default branch) |
| `--start-date` | `yyyy-MM-dd` (optional; defaults to yesterday) |
| `--days` | Number of days from start date (optional; default 1) |
| `--output` | Output file (optional; defaults to stdout) |

Output is a compact JSON array of `{sha, message, author, email, date, html_url}`.
