# github-commit-info

Fetch commit information from GitHub repositories within a specified time range.

[English](./README.md) | [中文](./README_zh.md)

## Installation

```bash
cargo install --path .
```

## Environment Variables

```bash
# Set GitHub Token (required)
export GITHUB_TOKEN=ghp_xxxxxxxxxxxx
```

Get Token: https://github.com/settings/tokens  
Scope: Select `public_repo` (public repos) or `repo` (private repos)

## Usage

```bash
github-commit-info --url <URL> [OPTIONS]
```

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--url` | GitHub repository URL | `https://github.com/golang/go` |
| `--branch` | Branch name (optional, auto-detects default branch if not specified) | `main` |
| `--start-date` | Start date in yyyy-MM-dd format (optional, defaults to yesterday) | `2024-01-01` |
| `--days` | Number of days from start date (optional, defaults to 1) | `7` |
| `--output` | Output file path (optional, defaults to stdout) | `./commits.json` |

## Examples

```bash
# Get commits from yesterday (default)
github-commit-info --url https://github.com/golang/go

# Specify date range
github-commit-info --url https://github.com/golang/go --start-date 2024-01-01 --days 7

# Specify branch
github-commit-info --url https://github.com/golang/go --branch main --days 3

# Output to file
github-commit-info --url https://github.com/golang/go --output commits.json
```

## Output Format

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

## Dependencies

- [reqwest](https://crates.io/crates/reqwest) - HTTP client
- [tokio](https://crates.io/crates/tokio) - Async runtime
- [chrono](https://crates.io/crates/chrono) - Date/time handling
- [clap](https://crates.io/crates/clap) - CLI argument parsing
- [serde](https://crates.io/crates/serde) - JSON serialization
