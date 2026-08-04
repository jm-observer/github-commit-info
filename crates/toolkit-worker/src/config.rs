//! worker 本机配置：**零参数 `run`** 的落点。
//!
//! 以前所有东西都靠命令行传（controller / token / id-file / egress-ip / exec-secret-file /
//! exec-root …），对方机器上跑一行命令要拼七八个参数，还容易把密钥写进命令行。现在统一收
//! 进一个 workspace 目录：
//!
//! ```text
//! ~/.config/toolkit-worker/          (Windows 走 %USERPROFILE%，与 toolkit-server 同约定)
//!   config.json      # controller / worker_id / label / 开关；**不含密钥**
//!   exec-secret      # exec 凭据明文 + 到期时间，单独一份、权限收紧
//!   remote-exec/     # 临时脚本 + 本地审计（原 --exec-root）
//! ```
//!
//! 密钥**刻意不进 config.json**：配置文件是会被截图、贴进聊天、上传排查的东西，
//! 分开放能让「贴配置」这个动作本身不泄密。
//!
//! `TOOLKIT_WORKER_WORKSPACE` 可覆盖工作目录（测试与多实例并存时用）。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// controller 默认地址——**部署事实，不是用户配置**：外网入口 38788 经 G10 上的 caddy
/// TLS 终止转到 toolkit-server:8788。写死成默认值，是为了让对方机器上那行命令不带参数。
/// （28080 是 english 自己的入口，指过去只会拿到英语系统的页面。）
pub const DEFAULT_CONTROLLER: &str = "https://spark.for-memory.site:38788";

/// 覆盖 workspace 根目录的环境变量。
pub const WORKSPACE_ENV: &str = "TOOLKIT_WORKER_WORKSPACE";

/// workspace 根：`$TOOLKIT_WORKER_WORKSPACE` → `$HOME/.config/toolkit-worker`
/// （Windows 走 `%USERPROFILE%`），与 `toolkit-server::workspace_dir` 同形状。
pub fn workspace_dir() -> Result<PathBuf> {
    if let Some(ws) = std::env::var_os(WORKSPACE_ENV) {
        return Ok(PathBuf::from(ws));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("HOME / USERPROFILE 均未设置，无法定位 worker workspace")?;
    Ok(PathBuf::from(home).join(".config").join("toolkit-worker"))
}

/// 本机配置（`config.json`）。所有字段都有默认值：文件不存在时等价于「全默认」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// controller 基址；缺省 [`DEFAULT_CONTROLLER`]。
    #[serde(default = "default_controller")]
    pub controller: String,
    /// 稳定 worker id（首次 `run` 时按 MAC + 主机名派生后写入，之后以本字段为准）。
    #[serde(default)]
    pub worker_id: String,
    /// 人类可读名，面板上用它认人；默认取主机名。
    #[serde(default)]
    pub label: String,
    /// 出口代理（egress）面的共享 token；controller 没配 `EGRESS_WORKER_TOKEN` 时留空即可。
    #[serde(default)]
    pub egress_token: String,
    /// 上报的出口 IP；留空 = 启动时探测（探不到记 `unknown`）。
    #[serde(default)]
    pub egress_ip: Option<String>,
    /// 代发流量绑定的本地源 IP（高级选项，**Linux 有效**；Windows 上绑非默认路由网卡会直接发不出包）。
    #[serde(default)]
    pub local_address: Option<String>,
    /// 代发流量绑定的网卡名（`SO_BINDTODEVICE`，**仅 Linux**）。
    #[serde(default)]
    pub interface: Option<String>,
    /// 是否启用远程命令执行面。默认 `true`——这个二进制现在的主要用途就是远程排查，
    /// 而且没有凭据时它只会停在「等待批准」，不构成额外风险。
    #[serde(default = "default_true")]
    pub allow_exec: bool,
}

fn default_controller() -> String {
    DEFAULT_CONTROLLER.to_string()
}
fn default_true() -> bool {
    true
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            controller: default_controller(),
            worker_id: String::new(),
            label: String::new(),
            egress_token: String::new(),
            egress_ip: None,
            local_address: None,
            interface: None,
            allow_exec: true,
        }
    }
}

/// workspace 内的路径集合。
pub struct Paths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub secret: PathBuf,
    /// 原 `--exec-root`：临时脚本 + 本地审计的落点。
    pub exec_root: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let root = workspace_dir()?;
        Ok(Self {
            config: root.join("config.json"),
            secret: root.join("exec-secret"),
            exec_root: root.clone(),
            root,
        })
    }

    /// 确保 workspace 目录存在（权限尽量收紧）。
    pub fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("创建 worker workspace 失败: {}", self.root.display()))?;
        set_private_dir(&self.root);
        Ok(())
    }
}

/// 读配置；文件不存在返回全默认（首次运行的正常路径，不是错误）。
pub fn load(path: &Path) -> Result<WorkerConfig> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("解析配置失败: {}（删掉它即可回到默认）", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(WorkerConfig::default()),
        Err(e) => Err(e).with_context(|| format!("读配置失败: {}", path.display())),
    }
}

/// 写配置（美化 JSON，便于人工查看/编辑）。
pub fn save(path: &Path, cfg: &WorkerConfig) -> Result<()> {
    let s = serde_json::to_string_pretty(cfg).context("序列化配置失败")?;
    std::fs::write(path, s).with_context(|| format!("写配置失败: {}", path.display()))?;
    Ok(())
}

/// 已落盘的 exec 凭据（`exec-secret` 文件内容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSecret {
    pub secret: String,
    /// 授权到期时间（unix 秒）；`None` = 手工签发的永久凭据。
    #[serde(default)]
    pub expires_at: Option<i64>,
}

impl StoredSecret {
    /// 是否已过期（提前 `slack_secs` 秒判定，留出续期余量）。
    pub fn is_expired(&self, now: i64, slack_secs: i64) -> bool {
        self.expires_at.is_some_and(|exp| now + slack_secs >= exp)
    }
}

/// 读凭据文件。不存在 → `None`（首次运行的正常路径）。
///
/// 兼容两种内容：JSON（本版写出的形态）与**裸 secret 文本**（早期 `--exec-secret-file`
/// 手工投放的形态，视为永不过期）。
pub fn load_secret(path: &Path) -> Result<Option<StoredSecret>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("读凭据失败: {}", path.display())),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.starts_with('{') {
        let s: StoredSecret = serde_json::from_str(trimmed)
            .with_context(|| format!("解析凭据失败: {}", path.display()))?;
        return Ok(Some(s));
    }
    Ok(Some(StoredSecret {
        secret: trimmed.to_string(),
        expires_at: None,
    }))
}

/// 写凭据文件并收紧权限（Unix `0600`；Windows 用 `icacls` 只留当前用户）。
pub fn save_secret(path: &Path, s: &StoredSecret) -> Result<()> {
    let body = serde_json::to_string(s).context("序列化凭据失败")?;
    std::fs::write(path, body).with_context(|| format!("写凭据失败: {}", path.display()))?;
    set_private_file(path);
    Ok(())
}

#[cfg(unix)]
fn set_private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_private_dir(_path: &Path) {}

#[cfg(unix)]
fn set_private_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// Windows 没有 POSIX mode，改用 `icacls`：去掉继承、只留当前用户读写。
/// 尽力而为——失败只警告，不阻塞启动（对方可能在受限环境里跑）。
#[cfg(windows)]
fn set_private_file(path: &Path) {
    let user = match std::env::var("USERNAME") {
        Ok(u) if !u.is_empty() => u,
        _ => return,
    };
    let out = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:(R,W)"))
        .output();
    match out {
        Ok(o) if o.status.success() => {}
        _ => log::warn!("收紧凭据文件权限失败（icacls），请自行确认仅当前用户可读: {path:?}"),
    }
}

#[cfg(not(any(unix, windows)))]
fn set_private_file(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_is_all_defaults() {
        let cfg = load(Path::new("/__nonexistent__/config.json")).unwrap();
        assert_eq!(cfg.controller, DEFAULT_CONTROLLER);
        assert!(cfg.allow_exec);
        assert!(cfg.worker_id.is_empty());
    }

    #[test]
    fn config_roundtrip_keeps_fields() {
        let dir = std::env::temp_dir().join(format!("tw-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.json");
        let mut cfg = WorkerConfig::default();
        cfg.worker_id = "w-abc".into();
        cfg.label = "老王的机器".into();
        save(&p, &cfg).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back.worker_id, "w-abc");
        assert_eq!(back.label, "老王的机器");
        assert_eq!(back.controller, DEFAULT_CONTROLLER);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secret_file_accepts_json_and_bare_text() {
        let dir = std::env::temp_dir().join(format!("tw-sec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // JSON 形态（本版写出的）。
        let p1 = dir.join("s1");
        save_secret(
            &p1,
            &StoredSecret {
                secret: "abc".into(),
                expires_at: Some(123),
            },
        )
        .unwrap();
        let s1 = load_secret(&p1).unwrap().unwrap();
        assert_eq!(s1.secret, "abc");
        assert_eq!(s1.expires_at, Some(123));

        // 裸文本形态（老的 --exec-secret-file 手工投放），视为永久。
        let p2 = dir.join("s2");
        std::fs::write(&p2, "deadbeef\n").unwrap();
        let s2 = load_secret(&p2).unwrap().unwrap();
        assert_eq!(s2.secret, "deadbeef");
        assert!(s2.expires_at.is_none());

        // 不存在 / 空文件 → None。
        assert!(load_secret(&dir.join("nope")).unwrap().is_none());
        std::fs::write(dir.join("empty"), "  \n").unwrap();
        assert!(load_secret(&dir.join("empty")).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expiry_uses_slack() {
        let s = StoredSecret {
            secret: "x".into(),
            expires_at: Some(1000),
        };
        assert!(!s.is_expired(900, 60)); // 还差 100s 到期，slack 60 → 未过期
        assert!(s.is_expired(950, 60)); // 差 50s，落进 slack → 视为过期，提前续期
        assert!(s.is_expired(1000, 0));
        // 永久凭据永不过期。
        let forever = StoredSecret {
            secret: "x".into(),
            expires_at: None,
        };
        assert!(!forever.is_expired(i64::MAX / 2, 60));
    }
}
