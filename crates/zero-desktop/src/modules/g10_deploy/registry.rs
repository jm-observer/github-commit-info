//! G10 部署服务清单（registry）。
//!
//! 把「D:\git 下可部署到 G10 的服务型项目」集中描述为一份清单：每项记下本地仓库路径、
//! HTTP 健康端点、远端 systemd 服务名，以及（可选）一键部署用的 PowerShell 脚本。
//!
//! 清单解析顺序：**workspace 下 `g10-services.json` 覆盖 > 内置默认**（`builtin()`）。
//! 删除该文件即恢复内置默认；新增/改服务时编辑该文件，无需重编译。
//!
//! 7 个服务（toolkit-server / english / trace-hub / system-prompt-show / alarm-server / zero /
//! orchestrator）均已接入一键部署（`deploy` 字段非空）：各仓根目录有 deploy-g10.ps1（本机 Docker
//! 交叉编译 → scp → install 注入端口 → 重启），健康端点返回 `{status,version,commit}`。
//! orchestrator 2026-06 迁入本仓 crates/orchestrator,与 toolkit-server 共用本仓 deploy-g10.ps1
//! (-Service 区分),故其 repo_dir 指向 D:\git\toolkit。
//!
//! **端口即环境变量**：服务端口由 `<SERVICE>_BIND` 环境变量（值为完整 `host:port`）控制，故面板
//! 不再单列「端口」，而是把它作为 env 清单里的一条（带备注）。部署时整份 env 拼成
//! `-Env KEY=VAL,...` 传给脚本，脚本逐条转发为 `install -e KEY=VAL`（custom-utils 0.16 写进
//! unit 的 `Environment=`）。连通性以 HTTP 健康端点为准（`health_url`，面板可编辑）。

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
    /// HTTP 健康端点（GET，期望返回 `{status, version, commit}`）。空串表示「未配置健康端点」。
    /// 面板可编辑：服务跑 https（自签）就填 `https://`，探测已放宽证书校验。
    #[serde(default)]
    pub health_url: String,
    /// 远端 systemd `--user` 服务名（仅展示用）。
    #[serde(default)]
    pub remote_service: Option<String>,
    /// 服务 web 后台地址（前端「打开后台」按钮跳转）。空串 = 无后台，不显示按钮。
    #[serde(default)]
    pub web_url: String,
    /// 安装时动态注入 systemd unit 的环境变量（`KEY=VAL` + 可选备注）。部署时拼成
    /// `-Env KEY=VAL,...` 传给部署脚本，由脚本转发为 `install -e KEY=VAL`（custom-utils 0.16
    /// 注入 `Environment=`）。**端口即其中的 `<SERVICE>_BIND` 一条**。空 = 不注入额外变量。
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// 一键部署定义。`None` → 该服务暂不支持一键部署（仅观测）。
    #[serde(default)]
    pub deploy: Option<DeployDef>,
}

/// 一条注入 systemd unit 的环境变量。`value` 允许含 `=`，但**不应含逗号**
/// （部署链路用逗号分隔多条 `-Env`）。`note` 为可选备注，仅面板展示用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    #[serde(default)]
    pub value: String,
    /// 备注（可选；说明该变量用途，如「监听地址 host:port」）。
    #[serde(default)]
    pub note: String,
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

/// 端口环境变量的统一备注（值形如 `0.0.0.0:<port>`）。
fn bind_note() -> String {
    "监听地址 host:port（端口经此环境变量注入）".into()
}

/// 内置默认清单。每个服务的端口以 `<SERVICE>_BIND` 环境变量呈现（值含端口）；非 toolkit-server
/// 的健康端点为基于各仓已知端口的最佳猜测，不可达时前端显示红灯（不影响功能）。
pub fn builtin() -> Vec<ServiceDef> {
    vec![
        ServiceDef {
            name: "toolkit-server".into(),
            label: "toolkit-server（工具中台）".into(),
            note: "本仓 axum 守护进程 + 抖音/RAG/LLM 工具底座".into(),
            repo_dir: r"D:\git\toolkit".into(),
            health_url: "http://192.168.0.68:8788/api/web/health".into(),
            remote_service: Some("toolkit-server".into()),
            web_url: "http://192.168.0.68:8788".into(),
            env: vec![EnvVar {
                key: "TOOLKIT_BIND".into(),
                value: "0.0.0.0:8788".into(),
                note: bind_note(),
            }],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "toolkit-server".into()],
            }),
        },
        ServiceDef {
            name: "english".into(),
            label: "english（学习后端）".into(),
            note: "Actix-web 学习平台；deploy-g10.ps1 交叉编译，端口经 ENGLISH_BIND 注入".into(),
            repo_dir: r"D:\git\english".into(),
            // prod feature 跑 HTTPS（自签）→ 健康端点走 https；面板探测已开 danger_accept_invalid_certs。
            health_url: "https://192.168.0.68:28080/health".into(),
            remote_service: Some("english.service".into()),
            // prod feature = https（自签证书）；前端 SPA 挂根路径。
            web_url: "https://192.168.0.68:28080".into(),
            env: vec![EnvVar {
                key: "ENGLISH_BIND".into(),
                value: "0.0.0.0:28080".into(),
                note: bind_note(),
            }],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "english".into()],
            }),
        },
        ServiceDef {
            name: "trace-hub".into(),
            label: "trace-hub（全链路追踪）".into(),
            note: "axum 追踪后端；deploy-g10.ps1 交叉编译，端口经 TRACE_HUB_BIND 注入".into(),
            repo_dir: r"D:\git\trace-hub".into(),
            health_url: "http://192.168.0.68:9100/health".into(),
            remote_service: Some("trace-hub.service".into()),
            // 追踪 Web UI 挂主端口根路径（随 TRACE_HUB_BIND）。
            web_url: "http://192.168.0.68:9100".into(),
            env: vec![EnvVar {
                key: "TRACE_HUB_BIND".into(),
                value: "0.0.0.0:9100".into(),
                note: bind_note(),
            }],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "trace-hub".into()],
            }),
        },
        ServiceDef {
            name: "system-prompt-show".into(),
            label: "system-prompt-show（LLM 流量观测）".into(),
            note: "axum 路由/代理；deploy-g10.ps1 交叉编译，路由口经 SPS_BIND 注入".into(),
            repo_dir: r"D:\git\system-prompt-show".into(),
            // /health 挂在路由 server（主端口 9000），与 SPS_BIND 同口。
            health_url: "http://192.168.0.68:9000/health".into(),
            remote_service: Some("system-prompt-show.service".into()),
            // Web 控制台在 8081，默认绑 127.0.0.1 → 需把 config.toml 的 web host 改 0.0.0.0 才能外部访问。
            web_url: "http://192.168.0.68:8081".into(),
            // 主端口（路由 9000）经 SPS_BIND 注入；代理口 8080 / Web UI 8081 仍由 config 控制。
            env: vec![EnvVar {
                key: "SPS_BIND".into(),
                value: "0.0.0.0:9000".into(),
                note: "路由 / API 监听地址（主端口）".into(),
            }],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "system-prompt-show".into()],
            }),
        },
        ServiceDef {
            name: "alarm-server".into(),
            label: "alarm-server（定时器）".into(),
            note: "timer-util 守护进程（Actix-web）；deploy-g10.ps1 交叉编译，端口经 ALARM_SERVER_BIND 注入".into(),
            repo_dir: r"D:\git\timer-util".into(),
            health_url: "http://192.168.0.68:8080/api/health".into(),
            remote_service: Some("alarm-server.service".into()),
            // dashboard 挂主端口根路径（随 ALARM_SERVER_BIND）。
            web_url: "http://192.168.0.68:8080".into(),
            env: vec![EnvVar {
                key: "ALARM_SERVER_BIND".into(),
                value: "0.0.0.0:8080".into(),
                note: bind_note(),
            }],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "alarm-server".into()],
            }),
        },
        ServiceDef {
            name: "zero".into(),
            label: "zero（消息网关）".into(),
            note: "多渠道消息网关 + Nova 编排；deploy-g10.ps1 交叉编译部署，端口经 ZERO_BIND 注入".into(),
            repo_dir: r"D:\git\zero".into(),
            health_url: "http://192.168.0.68:9001/health".into(),
            remote_service: Some("zero.service".into()),
            // 控制台挂在 gateway server（随 ZERO_BIND 绑 0.0.0.0:9001）的 /console 前缀。
            web_url: "http://192.168.0.68:9001/console".into(),
            env: vec![
                EnvVar {
                    key: "ZERO_BIND".into(),
                    value: "0.0.0.0:9001".into(),
                    note: bind_note(),
                },
                EnvVar {
                    key: "ALARM_SERVER_URL".into(),
                    value: "http://127.0.0.1:8080".into(),
                    note: "alarm-server 基址（agent 调 alarm-cli 设闹钟用；未设则回退 alarm-cli 默认）".into(),
                },
                EnvVar {
                    key: "TRACE_HUB_ENDPOINT".into(),
                    value: "http://127.0.0.1:9100/v1/spans".into(),
                    note: "trace-hub 宿主端点（未设则零追踪）".into(),
                },
            ],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "zero.service".into()],
            }),
        },
        ServiceDef {
            name: "orchestrator".into(),
            label: "orchestrator（语音编排）".into(),
            note: "streaming-speech 语音编排（WebSocket + ASR + vLLM 串联）；2026-06 迁入本仓 \
                   crates/orchestrator，宿主 systemd 服务，deploy-g10.ps1 交叉编译一键部署".into(),
            // 已迁入 toolkit 本仓：复用本仓 deploy-g10.ps1（与 toolkit-server 同脚本，-Service 区分）。
            repo_dir: r"D:\git\toolkit".into(),
            health_url: "http://192.168.0.68:8090/health".into(),
            remote_service: Some("orchestrator".into()),
            // 控制台挂根路径（实时流 + 历史分段 + 说话人管理）。
            web_url: "http://192.168.0.68:8090".into(),
            // 端口经 ORCH_BIND 注入；另注入 asr/trace 连接地址（orchestrator 是宿主进程，走回环：
            // 宿主 9100 被 trace-hub 占用 → asr 容器发布到 9110；trace-hub 在宿主 9100）。
            // vLLM 地址不在此（权威值在 orchestrator SQLite 的 vllm.base = 127.0.0.1:12340）。
            env: vec![
                EnvVar {
                    key: "ORCH_BIND".into(),
                    value: "0.0.0.0:8090".into(),
                    note: bind_note(),
                },
                EnvVar {
                    key: "ASR_WS".into(),
                    value: "ws://127.0.0.1:9110".into(),
                    note: "asr 内部 WS（容器发布到宿主 9110；宿主 9100 被 trace-hub 占用）".into(),
                },
                EnvVar {
                    key: "TRACE_HUB_ENDPOINT".into(),
                    value: "http://127.0.0.1:9100/v1/spans".into(),
                    note: "trace-hub 宿主端点（未设则零追踪）".into(),
                },
            ],
            deploy: Some(DeployDef {
                script: "deploy-g10.ps1".into(),
                args: vec!["-Service".into(), "orchestrator".into()],
            }),
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
