//! G10 部署服务清单（registry）。
//!
//! 把「D:\git 下可部署到 G10 的服务型项目」集中描述为一份清单：每项记下本地仓库路径、
//! HTTP 健康端点、远端 systemd 服务名，以及（可选）一键部署用的 PowerShell 脚本。
//!
//! 清单解析顺序：**workspace 下 `g10-services.json` 覆盖 > 内置默认**（`builtin()`）。
//! 删除该文件即恢复内置默认；新增/改服务时编辑该文件，无需重编译。
//!
//! 初版只有 `toolkit-server`（本仓）接入了一键部署（`deploy` 字段非空）；其余为占位条目，
//! 仅做连通性/版本展示，`deploy` 为空 → 前端禁用部署按钮并提示「脚本待接入」。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 一个可观测 / 可部署的服务定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDef {
    /// 唯一 id（命令参数按此匹配），如 `toolkit-server`。
    pub name: String,
    /// 显示名。
    pub label: String,
    /// 一句话说明。
    #[serde(default)]
    pub note: String,
    /// 本地仓库根目录（取本地 git 版本 / 跑部署脚本的工作目录）。
    pub repo_dir: String,
    /// HTTP 健康端点（GET，期望返回 `{status, version}`）。空串表示「未配置健康端点」。
    #[serde(default)]
    pub health_url: String,
    /// 远端 systemd `--user` 服务名（仅展示用）。
    #[serde(default)]
    pub remote_service: Option<String>,
    /// 服务 web 后台地址（前端「打开后台」按钮跳转）。空串 = 无后台，不显示按钮。
    /// 内置默认仅 toolkit-server 填，其余留空，可在 `g10-services.json` 配置。
    #[serde(default)]
    pub web_url: String,
    /// G10 上该服务所在主机（端口探测的目标）。默认 G10 内网 IP，可在 `g10-services.json` 改。
    #[serde(default = "default_host")]
    pub host: String,
    /// 该服务监听/占用的端口清单（展示 + TCP 连通性探测）。空 = 未登记端口。
    #[serde(default)]
    pub ports: Vec<PortInfo>,
    /// 安装时动态注入 systemd unit 的环境变量（`KEY=VAL`）。部署时拼成 `-Env KEY=VAL,...`
    /// 传给部署脚本，由脚本转发为 `install --env KEY=VAL`（custom-utils 0.16 注入 `Environment=`）。
    /// 仅在该服务 `deploy` 脚本支持 `-Env` 时生效（当前 toolkit-server）。空 = 不注入额外变量。
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// 一键部署定义。`None` → 该服务暂不支持一键部署（仅观测）。
    #[serde(default)]
    pub deploy: Option<DeployDef>,
}

/// 一条注入 systemd unit 的环境变量。`value` 允许含 `=`，但**不应含逗号**
/// （部署链路用逗号分隔多条 `-Env`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    #[serde(default)]
    pub value: String,
}

/// 一个服务监听的端口 + 用途说明。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortInfo {
    pub port: u16,
    /// 用途说明（如 "HTTP API / 控制台"）。
    #[serde(default)]
    pub note: String,
}

/// 默认 G10 主机（与各 health_url 同一台）。
fn default_host() -> String {
    "192.168.0.68".into()
}

/// 一键部署：调该仓自己的 PowerShell 部署脚本（复用 deploy-g10.ps1 范式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployDef {
    /// 相对 `repo_dir` 的脚本路径，如 `deploy-g10.ps1` 或 `scripts/deploy-g10.ps1`。
    pub script: String,
    /// 传给脚本的额外参数（如 `["-Service", "toolkit-server"]`）。
    #[serde(default)]
    pub args: Vec<String>,
}

/// 内置默认清单。host 统一为 G10（`192.168.0.68`）；非 toolkit-server 的健康端点为
/// 基于各仓已知端口的最佳猜测，不可达时前端显示红灯（不影响功能）。
pub fn builtin() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            name: "toolkit-server".into(),
            label: "toolkit-server（工具中台）".into(),
            note: "本仓 axum 守护进程 + 抖音/RAG/LLM 工具底座".into(),
            repo_dir: r"D:\git\github-commit-info".into(),
            health_url: "http://192.168.0.68:8788/api/web/health".into(),
            remote_service: Some("toolkit-server".into()),
            web_url: "http://192.168.0.68:8788".into(),
            host: default_host(),
            ports: vec![PortInfo {
                port: 8788,
                note: "HTTP API / Web 控制台".into(),
            }],
            env: vec![],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                // 部署后重启 toolkit-server 用户服务（脚本默认即此 service，显式写出更清晰）。
                args: vec!["-Service".into(), "toolkit-server".into()],
            }),
        },
        ServiceDef {
            name: "zero".into(),
            label: "zero（消息网关）".into(),
            note: "多渠道消息网关 + Nova 编排；scripts/deploy-g10.ps1（远端编译）".into(),
            repo_dir: r"D:\git\zero".into(),
            health_url: String::new(),
            remote_service: Some("zero.service".into()),
            web_url: String::new(),
            host: default_host(),
            ports: vec![], // 网关端口待确认
            env: vec![],
            deploy: None, // 待接入：脚本在 scripts/ 下、远端编译形态，需单独适配
        },
        ServiceDef {
            name: "english".into(),
            label: "english（学习后端）".into(),
            note: "Actix-web 学习平台；deploy-g10.ps1 交叉编译部署，端口经 ENGLISH_BIND 注入".into(),
            repo_dir: r"D:\git\english".into(),
            health_url: "http://192.168.0.68:28080/health".into(),
            remote_service: Some("english.service".into()),
            web_url: String::new(),
            host: default_host(),
            ports: vec![PortInfo {
                port: 28080,
                note: "HTTP API".into(),
            }],
            env: vec![],
            // 面板按 ports[0] 自动追加 `-Bind 0.0.0.0:28080`，脚本据此
            // `english install -e ENGLISH_BIND=<bind>` 注入端口。
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "english".into()],
            }),
        },
        ServiceDef {
            name: "trace-hub".into(),
            label: "trace-hub（全链路追踪）".into(),
            note: "axum 追踪后端，0.0.0.0:9100".into(),
            repo_dir: r"D:\git\trace-hub".into(),
            health_url: "http://192.168.0.68:9100/health".into(),
            remote_service: Some("trace-hub.service".into()),
            web_url: String::new(),
            host: default_host(),
            ports: vec![PortInfo {
                port: 9100,
                note: "HTTP / 追踪后端".into(),
            }],
            env: vec![],
            deploy: None, // 待接入：暂无 deploy-g10.ps1
        },
        ServiceDef {
            name: "system-prompt-show".into(),
            label: "system-prompt-show（LLM 流量观测）".into(),
            note: "axum 代理/路由，:9000 代理 + :8080 路由".into(),
            repo_dir: r"D:\git\system-prompt-show".into(),
            health_url: "http://192.168.0.68:8080/health".into(),
            remote_service: Some("system-prompt-show.service".into()),
            web_url: String::new(),
            host: default_host(),
            ports: vec![
                PortInfo {
                    port: 9000,
                    note: "LLM 代理".into(),
                },
                PortInfo {
                    port: 8080,
                    note: "路由 / HTTP".into(),
                },
            ],
            env: vec![],
            deploy: None, // 待接入：暂无 deploy-g10.ps1
        },
        ServiceDef {
            name: "alarm-server".into(),
            label: "alarm-server（定时器）".into(),
            note: "timer-util 守护进程；有 systemd unit，HTTP 健康端点待确认".into(),
            repo_dir: r"D:\git\timer-util".into(),
            health_url: String::new(),
            remote_service: Some("alarm-server.service".into()),
            web_url: String::new(),
            host: default_host(),
            ports: vec![], // HTTP 端口待确认
            env: vec![],
            deploy: None,
        },
    ]
}

/// workspace 下覆盖文件路径。
pub fn registry_path(workspace: &Path) -> PathBuf {
    workspace.join("g10-services.json")
}

/// 加载清单：存在覆盖文件则用之，否则内置默认。覆盖文件解析失败时**回退内置默认**
/// （不让一个坏 JSON 把整页打挂），并把错误带回供前端提示。
pub fn load(workspace: &Path) -> (Vec<ServiceDef>, Option<String>) {
    let path = registry_path(workspace);
    if !path.exists() {
        return (builtin(), None);
    }
    match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str::<Vec<ServiceDef>>(&s).map_err(|e| e.to_string()))
    {
        Ok(list) => (list, None),
        Err(e) => (
            builtin(),
            Some(format!("解析 {} 失败，已回退内置默认：{e}", path.display())),
        ),
    }
}

/// 把编辑后的服务清单写回 workspace 的 `g10-services.json`（覆盖文件）。
/// 之后 `load` 即读到新值；删除该文件可恢复内置默认。
pub fn save(workspace: &Path, services: &[ServiceDef]) -> Result<(), String> {
    let path = registry_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    }
    let json =
        serde_json::to_string_pretty(services).map_err(|e| format!("序列化清单失败：{e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
    Ok(())
}
