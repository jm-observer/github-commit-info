//! 长操作（operation）状态机与进度模型（设计文档 §4/§8）。
//!
//! apply/stop/reload/enabled 在 agent 内作为**独立作业**执行（持操作锁、断线不亡）；这里定义
//! 与传输无关的进度/终态类型，供：① agent 作业上报；② protocol `Event`；③ `GetCurrentOperation`
//! 返回（含守护重启后的 `Interrupted`/`Unknown`，由真实机器状态重新对齐）。

use serde::{Deserialize, Serialize};

/// apply 的 6 个阶段（与观察者文档 §3.3 stepper 对齐，索引从 0 起）。
pub const APPLY_STEPS: [&str; 6] = [
    "校验配置",
    "装防火墙基线",
    "启动引擎",
    "等待 TUN 起栈",
    "补 TUN 白名单",
    "验证连通",
];

/// 单步进度。`status` ∈ {running, ok, fail}；`detail` 为可选补充。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyProgress {
    /// 步索引（0..6），对应 `APPLY_STEPS`。
    pub step: usize,
    /// 步名（冗余给前端）。
    pub name: String,
    /// running / ok / fail。
    pub status: String,
    /// 可选补充信息（进度、错误原文等）。
    pub detail: Option<String>,
}

impl ApplyProgress {
    pub fn new(step: usize, status: &str, detail: Option<String>) -> Self {
        Self {
            step,
            name: APPLY_STEPS.get(step).copied().unwrap_or("").to_string(),
            status: status.to_string(),
            detail,
        }
    }
}

/// 长操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Apply,
    Stop,
    Reload,
    SetEnabled,
}

/// 长操作状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// 正在执行。
    Running,
    /// 成功终态。
    Succeeded,
    /// 失败终态（带 error）。
    Failed,
    /// 守护在操作进行中崩溃/重启，内存态丢失 → 未见终态（由真实机器状态重新对齐，不谎报）。
    Interrupted,
    /// 无法判定（既非明确成功也非明确失败）。
    Unknown,
}

impl OperationStatus {
    /// 是否终态（非 Running）。
    pub fn is_terminal(self) -> bool {
        !matches!(self, OperationStatus::Running)
    }
}

/// 一次长操作的当前信息（`GetCurrentOperation` 返回；`None` = 从未跑过长操作）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationInfo {
    /// operation ID（同一 agent 生命周期内单调递增）。
    pub id: u64,
    pub kind: OperationKind,
    pub status: OperationStatus,
    /// 当前/最后一步（apply 的 0..6）。
    pub step: Option<usize>,
    /// 当前/最后一步名。
    pub name: Option<String>,
    /// 失败/中断时的错误。
    pub error: Option<String>,
}

/// 长操作终态事件（protocol `Event::OperationFinished`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub id: u64,
    pub kind: OperationKind,
    /// 终态：Succeeded / Failed / Interrupted / Unknown。
    pub status: OperationStatus,
    pub error: Option<String>,
}
