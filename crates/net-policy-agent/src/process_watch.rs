//! 进程 / 连接观察：列出近期有公网连接的进程，供 UI 选作直连程序组。

use crate::win::run_ps;
use anyhow::{Context, Result};
use net_policy_core::types::ProcessCandidate;

/// 列出近期有已建立公网连接的进程候选（按 pid 去重）。
pub fn list_candidates() -> Result<Vec<ProcessCandidate>> {
    let script = r#"
$rows = Get-NetTCPConnection -State Established -ErrorAction SilentlyContinue |
  Where-Object { $_.RemoteAddress -notmatch '^(127\.|::1|0\.0\.0\.0)' } |
  Group-Object OwningProcess | ForEach-Object {
    $procId = [int]$_.Name
    $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
    [pscustomobject]@{
      pid     = $procId
      name    = if($p){ $p.ProcessName + '.exe' } else { '' }
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
    let val: serde_json::Value =
        serde_json::from_str(trimmed).context("parse process candidates json")?;
    let candidates: Vec<ProcessCandidate> = match val {
        serde_json::Value::Array(_) => serde_json::from_value(val)?,
        other => vec![serde_json::from_value(other)?],
    };
    Ok(candidates)
}
