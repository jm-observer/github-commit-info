//! g10-deploy 模块：把「D:\git 下部署到 G10 的服务」集中成一个面板——
//! 列表 + HTTP 连通性 + 本地编译版/远端运行版对比 + 一键交叉编译部署（含重启）。
//!
//! 部署逻辑**复用各仓自己的 PowerShell 部署脚本**（registry 的 `deploy.script`），本模块只
//! 负责编排：以仓库根为工作目录起 `pwsh -File <script>`，把 stdout/stderr 逐行 emit 回前端，
//! 终态再 emit 一条结果。初版仅 `toolkit-server`（本仓 deploy-g10.ps1，已补重启）接入一键部署。
//!
//! 所有 Tauri command 名称以 `g10_` 开头。连通性按用户口径**仅探 HTTP 健康端点**。

mod registry;

use crate::app_state::AppState;
use registry::ServiceDef;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Emitter, State};

/// 一键部署日志事件频道：前端 `listen('g10-deploy://log')` 订阅逐行输出。
pub const DEPLOY_LOG_EVENT: &str = "g10-deploy://log";
/// 一键部署终态事件频道：`listen('g10-deploy://done')`。
pub const DEPLOY_DONE_EVENT: &str = "g10-deploy://done";

/// g10-deploy 模块状态。
pub struct G10DeployState {
    pub workspace: PathBuf,
    /// 正在部署中的服务名集合。**按服务并发**：不同服务可同时部署（各仓 docker 缓存卷/target
    /// 目录互相独立，互不冲突），同一服务不可重入（防重复点）。
    deploying: Mutex<HashSet<String>>,
}

impl G10DeployState {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            deploying: Mutex::new(HashSet::new()),
        }
    }
}

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

fn find_service(state: &G10DeployState, name: &str) -> Result<ServiceDef, String> {
    let (list, _) = registry::load(&state.workspace);
    list.into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("未知服务：{name}"))
}

// ============ 清单 ============

#[derive(Serialize)]
pub struct ServiceList {
    pub services: Vec<ServiceDef>,
    /// 覆盖文件解析失败的提示（成功 / 无文件时为 None）。
    pub warning: Option<String>,
}

/// 返回 G10 服务清单（含是否可一键部署的信息，前端据 `deploy` 是否存在禁用按钮）。
#[tauri::command]
pub fn g10_list_services(state: State<'_, AppState>) -> ServiceList {
    let (services, warning) = registry::load(&state.g10_deploy.workspace);
    ServiceList { services, warning }
}

/// 把编辑后的服务清单（含端口）写回 workspace 的 `g10-services.json` 覆盖文件。
/// 前端编辑端口/字段后调用，下次 `g10_list_services` 即读到新值。
#[tauri::command]
pub fn g10_save_services(
    state: State<'_, AppState>,
    services: Vec<ServiceDef>,
) -> Result<(), String> {
    registry::save(&state.g10_deploy.workspace, &services)
}

// ============ 连通性探测（仅 HTTP 健康端点） ============

#[derive(Serialize)]
pub struct ProbeResult {
    pub name: String,
    /// 健康端点是否可达且返回 2xx。
    pub reachable: bool,
    /// 健康响应里的 `status` 字段（如 "ok"）。
    pub status: Option<String>,
    /// 健康响应里的 `version` 字段 = 远端**正在运行**的版本（语义版本）。
    pub remote_version: Option<String>,
    /// 健康响应里的 `commit` 字段 = 远端**编译版**的 git 短哈希（缺则 None）。
    #[serde(default)]
    pub remote_commit: Option<String>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

/// 探一个服务的健康端点。失败不报错，而是把失败信息塞进结果（前端统一渲染红灯）。
#[tauri::command]
pub async fn g10_probe_service(
    state: State<'_, AppState>,
    name: String,
) -> Result<ProbeResult, String> {
    let svc = find_service(&state.g10_deploy, &name)?;
    if svc.health_url.is_empty() {
        return Ok(ProbeResult {
            name,
            reachable: false,
            status: None,
            remote_version: None,
            remote_commit: None,
            latency_ms: None,
            error: Some("未配置健康端点".into()),
        });
    }

    // 接受自签证书：english 等服务 prod feature 跑 HTTPS（自签），健康端点是 https://…/health。
    // 这是内网 G10 面板的连通性探测，放宽证书校验可让 https 自签服务也能探到绿灯。
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .danger_accept_invalid_certs(true)
        .build()
        .map_err(err)?;

    let started = std::time::Instant::now();
    let resp = client.get(&svc.health_url).send().await;
    let latency_ms = started.elapsed().as_millis() as u64;

    let result = match resp {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(v) => ProbeResult {
                name: name.clone(),
                reachable: true,
                status: v.get("status").and_then(|s| s.as_str()).map(String::from),
                remote_version: v.get("version").and_then(|s| s.as_str()).map(String::from),
                remote_commit: v.get("commit").and_then(|s| s.as_str()).map(String::from),
                latency_ms: Some(latency_ms),
                error: None,
            },
            // 2xx 但响应不是预期 JSON：仍算可达（在线），只是拿不到版本。
            Err(e) => ProbeResult {
                name: name.clone(),
                reachable: true,
                status: None,
                remote_version: None,
                remote_commit: None,
                latency_ms: Some(latency_ms),
                error: Some(format!("健康响应解析失败：{e}")),
            },
        },
        Ok(r) => ProbeResult {
            name: name.clone(),
            reachable: false,
            status: None,
            remote_version: None,
            remote_commit: None,
            latency_ms: Some(latency_ms),
            error: Some(format!("HTTP {}", r.status())),
        },
        Err(e) => ProbeResult {
            name: name.clone(),
            reachable: false,
            status: None,
            remote_version: None,
            remote_commit: None,
            latency_ms: None,
            error: Some(err(e)),
        },
    };
    Ok(result)
}

// ============ 本地编译版（git 短哈希 + 是否有未提交改动） ============

#[derive(Serialize)]
pub struct LocalVersion {
    pub name: String,
    /// 本地仓库当前 commit 短哈希。
    pub git_hash: Option<String>,
    /// 工作区是否有未提交改动（脏 = 本地相对远端运行版可能已漂移）。
    pub dirty: bool,
    pub error: Option<String>,
}

fn run_git(repo: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    crate::shared::proc::hide_console(&mut cmd); // 不弹控制台窗口
    let out = cmd.output().map_err(|e| format!("git 调用失败：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} 失败：{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 读取某服务本地仓库的「将部署版本」标识：当前 commit 短哈希 + dirty 标记。
#[tauri::command]
pub async fn g10_local_version(
    state: State<'_, AppState>,
    name: String,
) -> Result<LocalVersion, String> {
    let svc = find_service(&state.g10_deploy, &name)?;
    let repo = PathBuf::from(&svc.repo_dir);

    tokio::task::spawn_blocking(move || {
        if !repo.exists() {
            return LocalVersion {
                name,
                git_hash: None,
                dirty: false,
                error: Some(format!("本地仓库不存在：{}", repo.display())),
            };
        }
        let hash = run_git(&repo, &["rev-parse", "--short", "HEAD"]);
        let dirty = run_git(&repo, &["status", "--porcelain"])
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        match hash {
            Ok(h) => LocalVersion {
                name,
                git_hash: Some(h),
                dirty,
                error: None,
            },
            Err(e) => LocalVersion {
                name,
                git_hash: None,
                dirty,
                error: Some(e),
            },
        }
    })
    .await
    .map_err(err)
}

// ============ 一键部署（流式日志） ============

#[derive(Serialize, Clone)]
struct DeployLog {
    name: String,
    /// stdout / stderr。
    stream: String,
    line: String,
}

#[derive(Serialize, Clone)]
struct DeployDone {
    name: String,
    success: bool,
    /// 进程退出码（拿不到时为 None）。
    code: Option<i32>,
    error: Option<String>,
}

/// 当前正在部署中的服务名列表（前端进页时据此恢复"哪些服务部署中"的状态）。
#[tauri::command]
pub fn g10_deploying_services(state: State<'_, AppState>) -> Vec<String> {
    let set = state.g10_deploy.deploying.lock().unwrap();
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort();
    v
}

/// 触发一键部署：以仓库根为 cwd 起 `pwsh -File <script> <args...>`，stdout/stderr 逐行
/// emit `g10-deploy://log`，结束 emit `g10-deploy://done`。命令本身**立即返回**（后台跑）。
/// **按服务并发**：不同服务可同时部署；同一服务部署中则拒绝重入。
#[tauri::command]
pub async fn g10_deploy(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<(), String> {
    let gs = state.g10_deploy.clone();
    let svc = find_service(&gs, &name)?;
    let deploy = svc
        .deploy
        .ok_or_else(|| format!("{} 暂未接入一键部署（脚本待接入）", svc.label))?;

    let repo = PathBuf::from(&svc.repo_dir);
    if !repo.exists() {
        return Err(format!("本地仓库不存在：{}", repo.display()));
    }
    let script_path = repo.join(&deploy.script);
    if !script_path.exists() {
        return Err(format!("部署脚本不存在：{}", script_path.display()));
    }

    // 动态环境变量注入：把 registry 的 env 拼成 `-Env KEY=VAL,KEY2=VAL2` 单参追加，脚本据此
    // 逐条转发为 `install -e KEY=VAL`（custom-utils 0.16 注入 unit 的 `Environment=`）。
    // **端口即其中的 `<SERVICE>_BIND` 一条**——不再单独走 `-Bind`，统一从 env 注入（脚本默认
    // `-Bind` 作兜底，env 里的同名 `<SERVICE>_BIND` 后置覆盖之）。
    // 用逗号分隔（PowerShell `[string[]]` 数组的 `-File` 传参约定），故 value 不应含逗号。
    // 脚本已显式带 `-Env` 时不重复追加（registry args 优先）。
    let mut deploy_args = deploy.args.clone();
    if !svc.env.is_empty() {
        let has_env = deploy_args.iter().any(|a| a.eq_ignore_ascii_case("-Env"));
        if !has_env {
            let joined = svc
                .env
                .iter()
                .map(|e| format!("{}={}", e.key, e.value))
                .collect::<Vec<_>>()
                .join(",");
            deploy_args.push("-Env".into());
            deploy_args.push(joined);
        }
    }

    // 抢占该服务的部署位：同一服务部署中则拒绝重入；不同服务可并发。
    {
        let mut set = gs.deploying.lock().unwrap();
        if set.contains(&name) {
            return Err(format!("{} 已在部署中，请等待其完成", svc.label));
        }
        set.insert(name.clone());
    }

    let name_for_task = name.clone();
    let app_bg = app.clone();
    tokio::spawn(async move {
        let result = run_deploy(&app_bg, &name_for_task, &repo, &deploy.script, &deploy_args).await;
        // 无论成败，释放该服务的部署位。
        gs.deploying.lock().unwrap().remove(&name_for_task);
        let done = match result {
            Ok(code) => {
                // 部署成功（退出码 0）→ 盖上「上次部署时间」并落盘。失败不更新。
                if code == Some(0) {
                    let when = chrono::Utc::now().to_rfc3339();
                    if let Err(e) = registry::mark_deployed(&gs.workspace, &name_for_task, when) {
                        let _ = app_bg.emit(
                            DEPLOY_LOG_EVENT,
                            DeployLog {
                                name: name_for_task.clone(),
                                stream: "stderr".into(),
                                line: format!("（部署成功，但记录部署时间失败：{e}）"),
                            },
                        );
                    }
                }
                DeployDone {
                name: name_for_task.clone(),
                success: code == Some(0),
                code,
                error: if code == Some(0) {
                    None
                } else {
                    Some(format!("部署进程以退出码 {code:?} 结束"))
                },
                }
            }
            Err(e) => DeployDone {
                name: name_for_task.clone(),
                success: false,
                code: None,
                error: Some(e),
            },
        };
        let _ = app_bg.emit(DEPLOY_DONE_EVENT, done);
    });

    Ok(())
}

/// 实际跑 pwsh 脚本并流式转发输出。返回退出码（None = 拿不到）。
async fn run_deploy(
    app: &tauri::AppHandle,
    name: &str,
    repo: &std::path::Path,
    script: &str,
    args: &[String],
) -> Result<Option<i32>, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let emit_line = |stream: &str, line: String| {
        let _ = app.emit(
            DEPLOY_LOG_EVENT,
            DeployLog {
                name: name.to_string(),
                stream: stream.to_string(),
                line,
            },
        );
    };

    emit_line(
        "stdout",
        format!("$ pwsh -File {script} {}", args.join(" ")),
    );

    // 脚本带 `#requires -Version 7` 且用 PS7 语法，必须用 pwsh（非 Windows PowerShell 5）。
    let mut cmd = Command::new("pwsh");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        script,
    ])
    .args(args)
    .current_dir(repo)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
    crate::shared::proc::hide_console_tokio(&mut cmd); // 不弹控制台窗口
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动 pwsh 失败（未安装 PowerShell 7？）：{e}"))?;

    let stdout = child.stdout.take().ok_or("无法取得 stdout")?;
    let stderr = child.stderr.take().ok_or("无法取得 stderr")?;

    let app_o = app.clone();
    let name_o = name.to_string();
    let out_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = app_o.emit(
                DEPLOY_LOG_EVENT,
                DeployLog {
                    name: name_o.clone(),
                    stream: "stdout".into(),
                    line,
                },
            );
        }
    });
    let app_e = app.clone();
    let name_e = name.to_string();
    let err_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = app_e.emit(
                DEPLOY_LOG_EVENT,
                DeployLog {
                    name: name_e.clone(),
                    stream: "stderr".into(),
                    line,
                },
            );
        }
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("等待 pwsh 结束失败：{e}"))?;
    let _ = out_task.await;
    let _ = err_task.await;

    Ok(status.code())
}
