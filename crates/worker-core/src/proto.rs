//! 远程执行（remote-exec）第一期线格式：controller ⇄ worker 之间的请求/响应类型、
//! 请求头常量与各类硬上限。**controller（toolkit-server）与 worker（toolkit-worker）
//! 共用本模块**，任何一端改字段都必须改这里，不得各自私拼 JSON。
//!
//! 设计出处：`docs/remote-exec-design.md` 第一期 §5（协议）/ §6.3（上限）。

use serde::{Deserialize, Serialize};

// ---------- 请求头 ----------

/// worker 身份（与 egress 面共享的稳定 `worker_id`）。
pub const HDR_WORKER_ID: &str = "x-worker-id";
/// per-worker exec 专用密钥明文（controller 侧比对 `sm3(salt||secret)`）。
pub const HDR_EXEC_SECRET: &str = "x-exec-secret";
/// worker 进程实例 id：每次进程启动随机生成，用于识别「旧实例」。
pub const HDR_INSTANCE_ID: &str = "x-instance-id";

// ---------- 上限 ----------

/// `timeout_secs` 缺省值。
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// `timeout_secs` 硬上限。
pub const MAX_TIMEOUT_SECS: u64 = 3600;
/// 脚本正文字节上限（1 MiB）。
pub const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
/// stdout/stderr 捕获缺省上限（各 1 MiB）。
pub const DEFAULT_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
/// stdout/stderr 捕获硬上限（各 8 MiB）。
pub const MAX_OUTPUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;
/// args 条数上限。
pub const MAX_ARGS: usize = 128;
/// env 条数上限。
pub const MAX_ENV: usize = 128;
/// 单个 arg / env value 的字节上限。
pub const MAX_ARG_BYTES: usize = 8 * 1024;
/// 单个 env key 的字节上限。
pub const MAX_ENV_KEY_BYTES: usize = 256;
/// args 累计字节上限。
pub const MAX_ARGS_TOTAL_BYTES: usize = 64 * 1024;
/// env 累计字节上限。
pub const MAX_ENV_TOTAL_BYTES: usize = 64 * 1024;
/// `cwd` 字节上限。
pub const MAX_CWD_BYTES: usize = 4096;
/// HTTP body 总上限（Axum `DefaultBodyLimit`，超限 413）。留出 JSON 转义余量。
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

// ---------- 注册 / 心跳 ----------

/// `POST /api/internal/exec/register` 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRegisterReq {
    pub worker_id: String,
    pub instance_id: String,
    /// worker 侧探测到的 PowerShell 版本串；探测不到则 `None`。
    #[serde(default)]
    pub powershell: Option<String>,
    /// 便于 operator 辨认的主机名。
    #[serde(default)]
    pub hostname: Option<String>,
}

/// `POST /api/internal/exec/heartbeat` 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecHeartbeatReq {
    pub worker_id: String,
    pub instance_id: String,
}

// ---------- 临时权限申请（worker 自助申请 → 面板批准 N 小时）----------

/// `POST /api/internal/exec/access/request` 请求体。**该端点不要求凭据**——申请的前提
/// 就是还没有凭据；防刷靠 controller 侧的同 id 去重 / pending 上限 / TTL 清理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecAccessReq {
    /// worker 自己算出的稳定 id（MAC + 主机名派生，见 `toolkit-worker`）。
    pub worker_id: String,
    /// 人类可读名，面板上用它辨认是谁的机器；默认取主机名。
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub hostname: String,
    /// `windows` / `linux` / …
    #[serde(default)]
    pub os: String,
}

/// `GET /api/internal/exec/access/poll?worker_id=…` 的应答。
///
/// `state` 取值：`pending` | `approved` | `already_claimed` | `rejected` | `unknown`。
/// **`secret` 只在 `approved` 的那一次出现**（controller 随即清空暂存）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecAccessPollResp {
    pub state: String,
    #[serde(default)]
    pub secret: Option<String>,
    /// 授权到期时间（unix 秒）。
    #[serde(default)]
    pub expires_at: Option<i64>,
}

// ---------- 执行请求 / 响应 ----------

/// controller 派发给 worker 的一次执行请求。`id` 由 controller 生成、永不复用，
/// 是结果归属与临时目录命名的唯一权威键。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub id: String,
    /// 审计归属：由 controller 按命中的 exec token 注入，调用方不能自行指定。
    pub operator: String,
    /// 第一期恒为 `"powershell"`。
    #[serde(default = "default_shell")]
    pub shell: String,
    pub script: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub timeout_secs: u64,
    pub stdout_limit_bytes: usize,
    pub stderr_limit_bytes: usize,
}

fn default_shell() -> String {
    SHELL_POWERSHELL.to_string()
}

/// 第一期唯一支持的 shell。
pub const SHELL_POWERSHELL: &str = "powershell";

/// 一次执行的终态。第一期只有这四种；`unknown` 由 controller 侧投影产生，
/// worker 不会主动回传。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecState {
    /// 进程正常结束（无论退出码是否为 0）。
    Completed,
    /// 超时并已杀死进程树。
    TimedOut,
    /// 进程根本没起来（可执行文件缺失、cwd 不存在、写临时文件失败等）。
    SpawnFailed,
}

impl ExecState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecState::Completed => "completed",
            ExecState::TimedOut => "timed_out",
            ExecState::SpawnFailed => "spawn_failed",
        }
    }
}

/// worker 回传给 controller 的执行结果（`POST /api/internal/exec/result`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResponse {
    pub id: String,
    pub state: ExecState,
    /// 仅 `completed` 时有值；`timed_out` / `spawn_failed` 恒为 `None`。
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub duration_ms: u64,
    /// `spawn_failed` 时的原因描述；其余状态为 `None`。
    #[serde(default)]
    pub error: Option<String>,
}

impl ExecResponse {
    /// 构造一个 `spawn_failed` 结果。
    pub fn spawn_failed(id: impl Into<String>, error: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            id: id.into(),
            state: ExecState::SpawnFailed,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            duration_ms,
            error: Some(error.into()),
        }
    }
}

// ---------- 校验 ----------

/// 字段级校验失败（controller 侧映射 422，worker 侧直接拒执行）。
#[derive(Debug, Clone)]
pub struct LimitError(pub String);

impl std::fmt::Display for LimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for LimitError {}

/// 双端共用的字段上限校验（controller 收到 operator 请求时校验一次，
/// worker 收到派发时再校验一次）。
pub fn validate(req: &ExecRequest) -> Result<(), LimitError> {
    let bad = |m: String| Err(LimitError(m));
    if req.shell != SHELL_POWERSHELL {
        return bad(format!("unsupported shell: {}", req.shell));
    }
    if req.script.is_empty() {
        return bad("script is empty".into());
    }
    if req.script.len() > MAX_SCRIPT_BYTES {
        return bad(format!("script exceeds {MAX_SCRIPT_BYTES} bytes"));
    }
    if req.timeout_secs == 0 || req.timeout_secs > MAX_TIMEOUT_SECS {
        return bad(format!("timeout_secs must be 1..={MAX_TIMEOUT_SECS}"));
    }
    if req.stdout_limit_bytes == 0 || req.stdout_limit_bytes > MAX_OUTPUT_LIMIT_BYTES {
        return bad(format!(
            "stdout_limit_bytes must be 1..={MAX_OUTPUT_LIMIT_BYTES}"
        ));
    }
    if req.stderr_limit_bytes == 0 || req.stderr_limit_bytes > MAX_OUTPUT_LIMIT_BYTES {
        return bad(format!(
            "stderr_limit_bytes must be 1..={MAX_OUTPUT_LIMIT_BYTES}"
        ));
    }
    if req.args.len() > MAX_ARGS {
        return bad(format!("args exceeds {MAX_ARGS} items"));
    }
    let mut args_total = 0usize;
    for a in &req.args {
        if a.len() > MAX_ARG_BYTES {
            return bad(format!("single arg exceeds {MAX_ARG_BYTES} bytes"));
        }
        args_total += a.len();
    }
    if args_total > MAX_ARGS_TOTAL_BYTES {
        return bad(format!("args total exceeds {MAX_ARGS_TOTAL_BYTES} bytes"));
    }
    if req.env.len() > MAX_ENV {
        return bad(format!("env exceeds {MAX_ENV} items"));
    }
    let mut env_total = 0usize;
    for (k, v) in &req.env {
        if k.is_empty() || k.len() > MAX_ENV_KEY_BYTES {
            return bad(format!("env key must be 1..={MAX_ENV_KEY_BYTES} bytes"));
        }
        if v.len() > MAX_ARG_BYTES {
            return bad(format!("env value exceeds {MAX_ARG_BYTES} bytes"));
        }
        env_total += k.len() + v.len();
    }
    if env_total > MAX_ENV_TOTAL_BYTES {
        return bad(format!("env total exceeds {MAX_ENV_TOTAL_BYTES} bytes"));
    }
    if let Some(cwd) = &req.cwd {
        if cwd.len() > MAX_CWD_BYTES {
            return bad(format!("cwd exceeds {MAX_CWD_BYTES} bytes"));
        }
    }
    Ok(())
}

/// 脚本正文的 SM3 短哈希（前 8 字节 → 16 hex），审计里代替正文记录。
pub fn script_hash(script: &str) -> String {
    use sm3::{Digest, Sm3};
    let mut h = Sm3::new();
    h.update(script.as_bytes());
    let out = h.finalize();
    out.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ExecRequest {
        ExecRequest {
            id: "id-1".into(),
            operator: "alice".into(),
            shell: SHELL_POWERSHELL.into(),
            script: "Get-ChildItem".into(),
            args: vec![],
            cwd: None,
            env: vec![],
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            stdout_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
            stderr_limit_bytes: DEFAULT_OUTPUT_LIMIT_BYTES,
        }
    }

    #[test]
    fn accepts_normal_request() {
        assert!(validate(&req()).is_ok());
    }

    #[test]
    fn rejects_over_limits() {
        let mut r = req();
        r.timeout_secs = MAX_TIMEOUT_SECS + 1;
        assert!(validate(&r).is_err());

        let mut r = req();
        r.args = vec!["a".into(); MAX_ARGS + 1];
        assert!(validate(&r).is_err());

        let mut r = req();
        r.shell = "bash".into();
        assert!(validate(&r).is_err());

        let mut r = req();
        r.script = String::new();
        assert!(validate(&r).is_err());
    }

    #[test]
    fn hash_is_stable_short_hex() {
        let h = script_hash("Get-ChildItem");
        assert_eq!(h.len(), 16);
        assert_eq!(h, script_hash("Get-ChildItem"));
        assert_ne!(h, script_hash("Get-ChildItem2"));
    }
}
