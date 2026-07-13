//! 进程树：`Get-CimInstance Win32_Process` → 父子关系 → 嵌套 [`ProcessNode`]。

use crate::win::run_ps;
use anyhow::{Context, Result};
use net_policy_core::types::ProcessNode;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
struct RawProc {
    #[serde(rename = "ProcessId", default)]
    pid: u32,
    #[serde(rename = "ParentProcessId", default)]
    ppid: u32,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "ExecutablePath", default)]
    path: Option<String>,
}

/// 取当前进程树（roots + 递归 children）。
pub fn process_tree() -> Result<Vec<ProcessNode>> {
    let script = r#"Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath | ConvertTo-Json -Compress"#;
    let out = run_ps(script)?;
    let trimmed = out.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(Vec::new());
    }
    let val: serde_json::Value =
        serde_json::from_str(trimmed).context("parse process list json")?;
    let raws: Vec<RawProc> = match val {
        serde_json::Value::Array(_) => serde_json::from_value(val)?,
        other => vec![serde_json::from_value(other)?],
    };

    // 建 info + 父→子 邻接表。
    let mut info: HashMap<u32, RawProc> = HashMap::new();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in raws {
        children.entry(p.ppid).or_default().push(p.pid);
        info.insert(p.pid, p);
    }

    // 根 = ppid 不在 info 里（或 ppid==pid 自环）的进程。
    let mut roots: Vec<u32> = info
        .values()
        .filter(|p| p.ppid == p.pid || !info.contains_key(&p.ppid))
        .map(|p| p.pid)
        .collect();
    roots.sort_unstable();

    let mut visited = HashSet::new();
    let mut out: Vec<ProcessNode> = roots
        .into_iter()
        .filter_map(|pid| build_node(pid, &info, &children, &mut visited))
        .collect();
    out.sort_by_key(|n| n.pid);
    Ok(out)
}

fn build_node(
    pid: u32,
    info: &HashMap<u32, RawProc>,
    children: &HashMap<u32, Vec<u32>>,
    visited: &mut HashSet<u32>,
) -> Option<ProcessNode> {
    if !visited.insert(pid) {
        return None; // 防环/pid 复用
    }
    let p = info.get(&pid)?;
    let mut kids: Vec<ProcessNode> = children
        .get(&pid)
        .map(|cs| {
            cs.iter()
                .filter(|&&c| c != pid)
                .filter_map(|&c| build_node(c, info, children, visited))
                .collect()
        })
        .unwrap_or_default();
    kids.sort_by_key(|n| n.pid);
    Some(ProcessNode {
        pid,
        ppid: p.ppid,
        name: p.name.clone(),
        path: p.path.clone().unwrap_or_default(),
        children: kids,
    })
}
