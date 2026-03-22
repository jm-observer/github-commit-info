# github-commit-info

获取 GitHub 仓库指定时间范围内的 commit 信息。

[English](./README.md) | [中文](./README_zh.md)

## 安装

```bash
cargo install --path .
```

## 环境变量

```bash
# 设置 GitHub Token（必须）
export GITHUB_TOKEN=ghp_xxxxxxxxxxxx
```

获取 Token: https://github.com/settings/tokens  
权限: 勾选 `public_repo`（公开仓库）或 `repo`（私有仓库）

## 使用方法

```bash
github-commit-info --url <URL> [OPTIONS]
```

## 参数

| 参数 | 说明 | 示例 |
|------|------|------|
| `--url` | GitHub 仓库 URL | `https://github.com/golang/go` |
| `--branch` | 分支名称（可选，不指定则自动获取默认分支） | `main` |
| `--start-date` | 起始日期，格式 yyyy-MM-dd（可选，默认昨天） | `2024-01-01` |
| `--days` | 从起始日期开始的天数（可选，默认 1） | `7` |
| `--output` | 输出文件路径（可选，默认为 stdout） | `./commits.json` |

## 示例

```bash
# 获取昨天一天的 commit（默认）
github-commit-info --url https://github.com/golang/go

# 指定日期范围
github-commit-info --url https://github.com/golang/go --start-date 2024-01-01 --days 7

# 指定分支
github-commit-info --url https://github.com/golang/go --branch main --days 3

# 输出到文件
github-commit-info --url https://github.com/golang/go --output commits.json
```

## 输出格式

```json
[
  {
    "sha": "abc123...",
    "message": "commit message",
    "author": "username",
    "email": "user@example.com",
    "date": "2024-01-01T12:00:00Z",
    "html_url": "https://github.com/owner/repo/commit/abc123"
  }
]
```

## 依赖

- [reqwest](https://crates.io/crates/reqwest) - HTTP 客户端
- [tokio](https://crates.io/crates/tokio) - 异步运行时
- [chrono](https://crates.io/crates/chrono) - 日期时间处理
- [clap](https://crates.io/crates/clap) - 命令行参数解析
- [serde](https://crates.io/crates/serde) - JSON 序列化
