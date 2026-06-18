//! 进程 / 连接观察：列出近期有公网连接的进程，供 UI 选作直连程序组（§14.4）。
//!
//! 首版用 `Get-NetTCPConnection` + `Get-Process` 取「已建立连接」的进程快照；
//! 子进程树自动补全留待迭代（当前由用户在 UI 手动确认加入程序组）。

use super::win::run_ps;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessCandidate {
    pub pid: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    /// 该进程近期连接的远端地址样本。
    #[serde(default)]
    pub remotes: Vec<String>,
}

/// 列出近期有已建立公网连接的进程候选（按 pid 去重）。
pub fn list_candidates() -> Result<Vec<ProcessCandidate>> {
    // 输出 JSON 数组，Rust 侧 serde 解析（避免逐行文本解析的编码坑）。
    let script = r#"
$rows = Get-NetTCPConnection -State Established -ErrorAction SilentlyContinue |
  Where-Object { $_.RemoteAddress -notmatch '^(127\.|::1|0\.0\.0\.0)' } |
  Group-Object OwningProcess | ForEach-Object {
    $procId = [int]$_.Name
    $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
    [pscustomobject]@{
      pid     = $procId
      name    = if($p){ $p.ProcessName + '.exe' } else { '' }
      # 受保护进程取不到 Path 时为 $null；强制成 '' 避免 JSON 出现 null（Rust 侧 path 是非 Option String，
      # null 会反序列化失败 → 整批 scan 报错）。
      path    = if($p){ try { [string]$p.Path } catch { '' } } else { '' }
      remotes = @($_.Group | ForEach-Object { $_.RemoteAddress } | Select-Object -Unique -First 5)
    }
  }
$rows = @($rows)
if($rows.Count -eq 0){ '[]' } else { $rows | ConvertTo-Json -Depth 4 -Compress }
"#;
    let out = run_ps(script)?;
    let trimmed = out.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return Ok(Vec::new());
    }
    // ConvertTo-Json 单元素时不会输出数组，做容错。
    let val: serde_json::Value =
        serde_json::from_str(trimmed).context("parse process candidates json")?;
    let candidates: Vec<ProcessCandidate> = match val {
        serde_json::Value::Array(_) => serde_json::from_value(val)?,
        other => vec![serde_json::from_value(other)?],
    };
    Ok(candidates)
}
