//! toolkit-worker:出口代理节点(轻模型「借出口」的执行端)。
//!
//! pull 模型(NAT 友好):worker 主动连 controller —— register → 心跳(10s)→ 长轮询
//! `egress/next` 取待发请求 → 用本机(本节点出口 IP)reqwest 代发 → `egress/result` 回传。
//!
//! per-session cookie:请求带 `session_id` 时用一个带 cookie jar 的独立 client(按 session_id
//! 惰性建、复用),使同一 session 的登录态在多次请求间连续;匿名请求走无 cookie 的共享 client。
//!
//! **零参数运行**:身份(MAC 派生的稳定 id)、controller、凭据全部落在 workspace
//! (`~/.config/toolkit-worker/`,见 [`config`])。首次 `run` 会自行派生 id、提交远程执行
//! 权限申请,并停在「等待批准」——你在 zero-desktop 面板批准 N 小时后它自动继续
//! (见 [`identity`])。凭据到期时回到同一条路续期,进程不退出。
//!
//! 两个面共用 `worker_id`,其余完全独立:
//!
//! - **egress 面**(借出口):`/api/internal/egress/*`,共享 token,可并发代发。
//! - **exec 面**(远程排查):`/api/internal/exec/*`,per-worker 临时凭据,单任务。
//!
//! 出口选择(历史包袱,**实测只有 Linux 有效**):`--local-address` 绑本地源 IP、
//! `--interface` 绑网卡名(`SO_BINDTODEVICE`)。后者只有 Linux/Android 有,故整个参数
//! 只在 Linux 编译进 CLI;前者在 Windows 上绑非默认路由网卡会直接发不出包(实测每个
//! 非默认网卡的探测全部失败),所以**别指望在 Windows 上靠它换出口**——换出口走
//! net-policy(已拆独立仓)。
//!
//! 正经 Linux 守护进程:复用 `custom-utils` 的日志 + systemd 安装(rootless `systemctl --user`)+
//! 自更新 + watchdog + trace 能力(见 `linux_service()`),形态照抄 `toolkit-server`。

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use custom_utils::updater::{CliAction, DeployCommand, LinuxService};
use egress_pool::{EgressError, EgressRequest, EgressResponse};
use log::LevelFilter::Info;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

mod config;
mod exec;
mod identity;

/// 长轮询请求的客户端超时(> controller 端 25s 挂起上限)。
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(40);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

const APP: &str = "toolkit-worker";
const REPO_OWNER: &str = "jm-observer";
const REPO_NAME: &str = "toolkit";

/// 安装/自更新统一描述。ExecStart 固定跑 `toolkit-worker run`,具体连接参数
/// (controller/token/local_address 等)全靠 install 时注入的 `EGRESS_*` env 决定
/// (`run` 子命令的 clap `env = "EGRESS_*"` 会读到)。
fn linux_service() -> LinuxService {
    LinuxService::new(APP, REPO_OWNER, REPO_NAME, env!("CARGO_PKG_VERSION"))
        .bin_name(APP)
        .description("toolkit-worker: 出口代理执行端(借出口)")
        .exec_args("run")
        .watchdog_sec(60)
        .restart_sec(5)
}

#[derive(Parser)]
#[command(
    name = "toolkit-worker",
    version,
    about = "出口代理节点(借出口执行端)",
    // 裸 `toolkit-worker`(无子命令)直接打印帮助;`help` / `help <子命令>` 由 clap 内置提供。
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 启动 worker。**零参数即可**:身份、controller、凭据全部读 workspace 配置
    /// (`~/.config/toolkit-worker/`);首次启动会自动派生 id 并提交权限申请,等你在
    /// zero-desktop 面板批准后自动继续。下面这些参数只是临时覆盖,平时不用传。
    Run {
        /// controller 基址覆盖(默认读配置,配置也没有则用内置外网入口)。
        #[arg(long, env = "EGRESS_CONTROLLER")]
        controller: Option<String>,
        /// 人类可读名,面板上用它认机器(默认取主机名);传了会写进配置。
        #[arg(long)]
        label: Option<String>,
        /// 出口代理(egress)面的共享 token 覆盖。
        #[arg(long, env = "EGRESS_WORKER_TOKEN")]
        token: Option<String>,
        /// 关闭远程命令执行面,只跑出口代理。
        #[arg(long, default_value_t = false)]
        no_exec: bool,
        /// 出口 IP 覆盖(默认启动时探测 https://api.ipify.org)。
        #[arg(long)]
        egress_ip: Option<String>,
        /// 代发流量绑定的本地源 IP。**高级选项,实际只在 Linux 有意义**:Windows 上绑非
        /// 默认路由网卡的源 IP 会直接发不出包(实测),换出口请改用 net-policy。
        #[arg(long, env = "EGRESS_LOCAL_ADDRESS")]
        local_address: Option<IpAddr>,
        /// 代发流量绑定的网卡名(`SO_BINDTODEVICE`)。仅 Linux 提供该参数。
        #[cfg(target_os = "linux")]
        #[arg(long, env = "EGRESS_INTERFACE")]
        interface: Option<String>,
    },
    /// 打印本机状态:workspace 路径、id/label、凭据剩余有效期、controller 连通性。
    Status,
    /// 安装为 systemd 用户级服务(rootless,`~/.local/bin` + `~/.config/toolkit-worker`)。
    Install {
        #[arg(long, short = 'n', help = "只打印渲染后的 unit 不真正安装")]
        dry_run: bool,
        /// 显式 workspace 路径,覆盖 `~/.config/toolkit-worker` 默认。
        #[arg(long, short = 'w')]
        workspace: Option<String>,
        /// controller 基址,写进 unit 的 `Environment=EGRESS_CONTROLLER=<..>`。
        #[arg(long)]
        controller: Option<String>,
        /// 共享鉴权 token,写进 unit 的 `Environment=EGRESS_WORKER_TOKEN=<..>`。
        #[arg(long)]
        token: Option<String>,
        /// 代发流量绑定的本地源 IP,写进 unit 的 `Environment=EGRESS_LOCAL_ADDRESS=<..>`。
        #[arg(long)]
        local_address: Option<IpAddr>,
        /// 代发流量绑定的网卡名,写进 unit 的 `Environment=EGRESS_INTERFACE=<..>`(仅 Linux 生效)。
        #[arg(long)]
        interface: Option<String>,
        /// 额外注入 unit 的环境变量,`KEY=VAL`,可重复。追加在内置 `.env()` 之后,
        /// 键冲突时此处的值生效。
        #[arg(long, short = 'e', value_name = "KEY=VAL")]
        env: Vec<String>,
    },
    /// 从 GitHub Release 自更新当前可执行文件。
    Update {
        #[arg(short, long, help = "即使版本未升级也强制更新")]
        force: bool,
    },
    // `List`(列本机网卡 IP,辅助挑 `run --local-address`)已随 net-policy 拆仓退役:
    // 实现函数当时一并删掉,只剩这条 Linux-only 的变体与调用,Windows 上 cfg 门控使其
    // 永不编译,直到交叉编译 aarch64-linux 才暴露为「找不到 list_egress」。换出口现在
    // 一律走独立仓 net-policy,故直接删掉空壳而非补实现。
    /// 启动 HTTP 转发代理:浏览器/App 把代理指向它,流量就从本 worker 的出口发出
    /// (整体换 IP 场景,如 THS headless Chrome、手机 App)。支持 `CONNECT`(HTTPS 隧道)
    /// 与明文 HTTP(absolute-form)两种请求。
    Proxy {
        /// 监听地址
        #[arg(long, env = "EGRESS_PROXY_LISTEN", default_value = "127.0.0.1:8899")]
        listen: String,
        /// 出站连接绑定的网卡名(`SO_BINDTODEVICE`,仅 Linux)。非 Linux 传了此项会打印警告并忽略。
        #[arg(long, env = "EGRESS_INTERFACE")]
        interface: Option<String>,
        /// 出站连接绑定的本地源 IP(跨平台;与 --interface 可并存,--interface 优先级更高)。
        #[arg(long, env = "EGRESS_LOCAL_ADDRESS")]
        local_address: Option<IpAddr>,
    },
}

/// 启用 trace-hub 全链路追踪——仅当设置了环境变量 `TRACE_HUB_ENDPOINT` 时生效;
/// 未设则完全无副作用(record_* 全 no-op,不起后台任务)。
fn init_trace() {
    if let Ok(endpoint) = std::env::var("TRACE_HUB_ENDPOINT") {
        custom_utils::trace::init(custom_utils::trace::TraceConfig::new(
            endpoint,
            "toolkit-worker",
        ));
        log::info!("trace enabled → trace-hub");
    }
}

/// 复用的 HTTP 客户端集合 + controller 连接信息。
struct Worker {
    controller: String,
    token: String,
    worker_id: String,
    /// 连 controller 的控制面 client。
    ctrl: reqwest::Client,
    /// 匿名代发(无 cookie)。
    anon: reqwest::Client,
    /// session_id -> 带 cookie jar 的代发 client(惰性建)。
    sessions: Mutex<HashMap<String, reqwest::Client>>,
    /// 代发流量绑定的本地源 IP(见文件顶部文档);`session_client` 惰性建 client 时需要。
    local_address: Option<IpAddr>,
    /// 代发流量绑定的网卡名(仅 Linux 生效,见文件顶部文档);`session_client` 惰性建 client 时需要。
    interface: Option<String>,
}

/// 在 `builder` 上按平台应用 `.interface(name)`(仅 Linux)。非 Linux 分支静默跳过——
/// 调用前应已在别处打印过一次性警告(见 `run_worker`),这里不重复刷屏。
#[cfg(target_os = "linux")]
fn apply_interface(
    builder: reqwest::ClientBuilder,
    interface: Option<&str>,
) -> reqwest::ClientBuilder {
    match interface {
        Some(name) => builder.interface(name),
        None => builder,
    }
}

#[cfg(not(target_os = "linux"))]
fn apply_interface(
    builder: reqwest::ClientBuilder,
    _interface: Option<&str>,
) -> reqwest::ClientBuilder {
    builder
}

impl Worker {
    fn ctrl_post(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/api/internal{}", self.controller, path);
        let rb = self.ctrl.post(url);
        if self.token.is_empty() {
            rb
        } else {
            rb.header("x-egress-token", &self.token)
        }
    }

    fn ctrl_get(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/api/internal{}", self.controller, path);
        let rb = self.ctrl.get(url);
        if self.token.is_empty() {
            rb
        } else {
            rb.header("x-egress-token", &self.token)
        }
    }

    async fn register(&self, egress_ip: &str) -> Result<()> {
        let body = serde_json::json!({
            "worker_id": self.worker_id,
            "egress_ip": egress_ip,
            "interface": self.interface,
            "local_address": self.local_address.map(|ip| ip.to_string()),
        });
        self.ctrl_post("/workers/register")
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// 取该 session 的带 cookie client(惰性建、复用)。
    async fn session_client(&self, session_id: &str) -> reqwest::Client {
        let mut m = self.sessions.lock().await;
        if let Some(c) = m.get(session_id) {
            return c.clone();
        }
        let mut builder = reqwest::Client::builder().cookie_store(true);
        if let Some(ip) = self.local_address {
            builder = builder.local_address(ip);
        }
        builder = apply_interface(builder, self.interface.as_deref());
        let c = builder.build().expect("build cookie client");
        m.insert(session_id.to_string(), c.clone());
        c
    }

    /// 代发一个请求,产出可回传的响应(错误也归一为 EgressResponse,不抛)。
    async fn execute(&self, req: EgressRequest) -> EgressResponse {
        let client = match &req.session_id {
            Some(sid) => self.session_client(sid).await,
            None => self.anon.clone(),
        };
        let method =
            reqwest::Method::from_bytes(req.method.as_bytes()).unwrap_or(reqwest::Method::GET);
        let mut rb = client.request(method, &req.url);
        for (k, v) in &req.headers {
            rb = rb.header(k, v);
        }
        if let Some(b) = &req.body {
            rb = rb.body(b.clone());
        }
        match rb.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let ok = resp.status().is_success();
                let headers: Vec<(String, String)> = resp
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                let error = match status {
                    429 => Some(EgressError {
                        kind: "rate_limited".into(),
                        msg: "HTTP 429".into(),
                    }),
                    401 | 403 => Some(EgressError {
                        kind: "auth".into(),
                        msg: format!("HTTP {status}"),
                    }),
                    _ => None,
                };
                let body = resp.text().await.ok();
                EgressResponse {
                    id: req.id,
                    ok,
                    status,
                    headers,
                    body,
                    error,
                }
            }
            Err(e) => {
                let kind = if e.is_timeout() || e.is_connect() {
                    "network"
                } else {
                    "other"
                };
                EgressResponse {
                    id: req.id,
                    ok: false,
                    status: 0,
                    headers: vec![],
                    body: None,
                    error: Some(EgressError {
                        kind: kind.into(),
                        msg: e.to_string(),
                    }),
                }
            }
        }
    }
}

async fn detect_egress_ip(local: Option<IpAddr>, interface: Option<&str>) -> Option<String> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
    if let Some(ip) = local {
        builder = builder.local_address(ip);
    }
    builder = apply_interface(builder, interface);
    let c = builder.build().ok()?;
    let text = c
        .get("https://api.ipify.org")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    Some(text.trim().to_string())
}

/// 提前多久视为「已过期」——留出续期余量,避免正好在执行任务时凭据到点。
const EXPIRY_SLACK_SECS: i64 = 60;

/// 取一份可用的 exec 凭据：本地有且没过期就直接用；否则走「申请 → 等你在面板批准」。
///
/// **不会因为没凭据就退出进程**：对方机器上跑的是常驻进程,卡在等待批准是正常状态,
/// 期间出口代理面照常工作。
async fn ensure_credential(
    paths: &config::Paths,
    cfg: &config::WorkerConfig,
    controller: &str,
) -> Result<config::StoredSecret> {
    let now = toolkit_now();
    if let Some(s) = config::load_secret(&paths.secret)? {
        if !s.is_expired(now, EXPIRY_SLACK_SECS) {
            match s.expires_at {
                Some(exp) => log::info!("[exec] 已有凭据,剩余 {} 分钟", (exp - now) / 60),
                None => log::info!("[exec] 已有长期凭据(无到期时间)"),
            }
            return Ok(s);
        }
        log::warn!("[exec] 凭据已过期,重新申请授权");
    }

    let client = reqwest::Client::new();
    let fresh = identity::acquire_credential(&client, controller, cfg).await?;
    config::save_secret(&paths.secret, &fresh)?;
    if let Some(exp) = fresh.expires_at {
        log::info!(
            "[exec] 已获批准,有效期至 unix {exp}(约 {} 分钟)",
            (exp - toolkit_now()) / 60
        );
    }
    Ok(fresh)
}

/// 当前 unix 秒。
fn toolkit_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `status` 子命令:一屏看完本机状态(路径 / 身份 / 凭据剩余 / controller 连通性)。
async fn run_status() -> Result<()> {
    let paths = config::Paths::resolve()?;
    let cfg = config::load(&paths.config)?;
    println!("workspace : {}", paths.root.display());
    println!(
        "配置文件  : {} {}",
        paths.config.display(),
        if paths.config.exists() {
            ""
        } else {
            "(尚未生成,首次 run 时创建)"
        }
    );
    println!(
        "worker id : {}",
        if cfg.worker_id.is_empty() {
            format!("(尚未派生,预计为 {})", identity::derive_id())
        } else {
            cfg.worker_id.clone()
        }
    );
    println!(
        "label     : {}",
        if cfg.label.is_empty() {
            identity::hostname()
        } else {
            cfg.label.clone()
        }
    );
    println!("controller: {}", cfg.controller);
    println!(
        "远程执行  : {}",
        if cfg.allow_exec { "启用" } else { "关闭" }
    );

    match config::load_secret(&paths.secret)? {
        None => println!("凭据      : 无(下次 run 会自动申请)"),
        Some(s) => {
            let now = toolkit_now();
            match s.expires_at {
                None => println!("凭据      : 长期有效(手工签发)"),
                Some(exp) if exp > now => {
                    println!("凭据      : 有效,剩余 {} 分钟", (exp - now) / 60)
                }
                Some(_) => println!("凭据      : 已过期(下次 run 会自动重新申请)"),
            }
        }
    }

    let url = format!("{}/api/web/health", cfg.controller.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => println!("controller: 可达 (HTTP {})", r.status()),
        Ok(r) => println!("controller: 不可达 (HTTP {})", r.status()),
        Err(e) => println!("controller: 不可达 ({e})"),
    }
    Ok(())
}

/// `run` 子命令的覆盖项(全部可选;不传就用配置里的值)。
#[derive(Default)]
struct RunOverrides {
    controller: Option<String>,
    label: Option<String>,
    token: Option<String>,
    no_exec: bool,
    egress_ip: Option<String>,
    local_address: Option<IpAddr>,
    interface: Option<String>,
}

/// `run` 子命令:读配置 →(缺凭据就申请并等批准)→ register → 心跳 → 长轮询主循环。
///
/// **零参数可跑**:身份、controller、凭据全在 workspace 里。首次启动的完整路径是
/// 「派生 id → 写配置 → 提交申请 → 每 10s 轮询 → 你在面板批准 → 落凭据 → 进主循环」;
/// 凭据过期时同样回到这条路续期,进程不退出(见 [`identity::acquire_credential`])。
async fn run_worker(ov: RunOverrides) -> Result<()> {
    // systemd watchdog 心跳(Type=notify 需要 READY=1;非 systemd 环境下自动 no-op)。
    let _watchdog = linux_service().spawn_watchdog();

    // ---------- 配置：加载 → 合并覆盖 → 补齐身份 → 回写 ----------
    let paths = config::Paths::resolve()?;
    paths.ensure_dir()?;
    let mut cfg = config::load(&paths.config)?;
    let mut dirty = false;
    if let Some(c) = ov.controller {
        let c = c.trim_end_matches('/').to_string();
        if cfg.controller != c {
            cfg.controller = c;
            dirty = true;
        }
    }
    if let Some(t) = ov.token {
        cfg.egress_token = t;
        dirty = true;
    }
    if ov.no_exec && cfg.allow_exec {
        cfg.allow_exec = false;
        dirty = true;
    }
    if ov.egress_ip.is_some() {
        cfg.egress_ip = ov.egress_ip.clone();
        dirty = true;
    }
    if ov.local_address.is_some() {
        cfg.local_address = ov.local_address.map(|ip| ip.to_string());
        dirty = true;
    }
    if ov.interface.is_some() {
        cfg.interface = ov.interface.clone();
        dirty = true;
    }
    dirty |= identity::ensure_identity(&mut cfg, ov.label.as_deref());
    if dirty {
        config::save(&paths.config, &cfg)?;
    }

    let controller = cfg.controller.trim_end_matches('/').to_string();
    let worker_id = cfg.worker_id.clone();
    let token = cfg.egress_token.clone();
    let local_address: Option<IpAddr> = cfg.local_address.as_deref().and_then(|s| s.parse().ok());
    let interface = cfg.interface.clone();
    if interface.is_some() && cfg!(not(target_os = "linux")) {
        log::warn!("网卡绑定仅 Linux 支持,已忽略配置里的 interface");
    }
    log::info!(
        "workspace={} id={worker_id} label={} controller={controller}",
        paths.root.display(),
        cfg.label
    );

    // ---------- exec 凭据：没有 / 已过期就地申请，等你在面板批准 ----------
    let exec_secret = if cfg.allow_exec {
        exec::validate_exec_controller(&controller)
            .context("exec 面 controller 传输安全校验失败")?;
        Some(ensure_credential(&paths, &cfg, &controller).await?)
    } else {
        log::info!("配置里 allow_exec=false:只跑出口代理,不启用远程命令执行面");
        None
    };

    // exec 面用的 controller/worker_id 副本:下面 `Worker` 结构体会把 egress 面的
    // `controller` 移进去,exec 面循环是完全独立的任务,需要自己的一份。
    let exec_controller = controller.clone();
    let exec_worker_id = worker_id.clone();
    let egress_ip = match cfg.egress_ip.clone() {
        Some(ip) => ip,
        None => detect_egress_ip(local_address, interface.as_deref())
            .await
            .unwrap_or_else(|| "unknown".to_string()),
    };
    log::info!("egress_ip={egress_ip} local_address={local_address:?} interface={interface:?}");

    let mut anon_builder = reqwest::Client::builder();
    if let Some(ip) = local_address {
        anon_builder = anon_builder.local_address(ip);
    }
    anon_builder = apply_interface(anon_builder, interface.as_deref());
    let anon = anon_builder.build().context("build anon client")?;

    let worker = Arc::new(Worker {
        controller,
        token,
        worker_id: worker_id.clone(),
        ctrl: reqwest::Client::new(),
        anon,
        sessions: Mutex::new(HashMap::new()),
        local_address,
        interface,
    });

    // 首次注册(重试直至成功)。
    loop {
        match worker.register(&egress_ip).await {
            Ok(()) => {
                log::info!("registered");
                break;
            }
            Err(e) => {
                log::warn!("register failed: {e}; retry in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }

    // 心跳后台任务:404 → 重新注册。
    {
        let w = worker.clone();
        let egress_ip = egress_ip.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
                let path = format!("/workers/{}/heartbeat", w.worker_id);
                match w
                    .ctrl_post(&path)
                    .json(&serde_json::json!({}))
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().as_u16() == 404 => {
                        log::warn!("heartbeat 404 → re-register");
                        let _ = w.register(&egress_ip).await;
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("heartbeat error: {e}"),
                }
            }
        });
    }

    // exec 面(remote-exec 第一期):独立 task 跑,与上面 egress 面互不干扰;
    // 配置里 allow_exec=false 时完全不 spawn。
    if let Some(secret) = exec_secret {
        let root = paths.exec_root.clone();
        log::info!("[exec] 已启用远程命令执行面,工作根目录={root:?}");
        tokio::spawn(async move {
            let opts = exec::ExecOpts {
                controller: exec_controller,
                secret: secret.secret,
                worker_id: exec_worker_id,
                root,
            };
            if let Err(e) = exec::run_exec_loop(opts).await {
                log::error!("[exec] exec 循环异常退出: {e:#}");
            }
        });
    }

    // 主循环:长轮询取活儿 → 代发 → 回传。
    let next_path = format!("/egress/next?worker_id={worker_id}");
    loop {
        let resp = worker
            .ctrl_get(&next_path)
            .timeout(LONG_POLL_TIMEOUT)
            .send()
            .await;
        match resp {
            Ok(r) => match r.status().as_u16() {
                200 => match r.json::<EgressRequest>().await {
                    Ok(req) => {
                        let w = worker.clone();
                        // 并发代发,不阻塞下一次长轮询。
                        tokio::spawn(async move {
                            let out = w.execute(req).await;
                            if let Err(e) = w
                                .ctrl_post("/egress/result")
                                .json(&out)
                                .timeout(Duration::from_secs(30))
                                .send()
                                .await
                            {
                                log::warn!("post result failed: {e}");
                            }
                        });
                    }
                    Err(e) => log::warn!("bad job payload: {e}"),
                },
                204 => { /* 空转,立即再轮询 */ }
                404 => {
                    log::warn!("next 404 → re-register");
                    let _ = worker.register(&egress_ip).await;
                }
                other => {
                    log::warn!("next unexpected status {other}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            },
            Err(e) => {
                log::warn!("long-poll error: {e}; retry in 3s");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

/// 在出站 `TcpSocket` 上按平台绑定网卡(`SO_BINDTODEVICE`,仅 Linux)。非 Linux 分支静默跳过
/// (调用前应已在别处打印过一次性警告,见 `run_proxy`)。
#[cfg(target_os = "linux")]
fn bind_device(socket: &tokio::net::TcpSocket, interface: Option<&str>) -> Result<()> {
    if let Some(name) = interface {
        let sref = socket2::SockRef::from(socket);
        sref.bind_device(Some(name.as_bytes()))
            .with_context(|| format!("bind_device({name}) 失败"))?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn bind_device(_socket: &tokio::net::TcpSocket, _interface: Option<&str>) -> Result<()> {
    Ok(())
}

/// 建一条出口绑定的出站 TCP 连接:解析 `host:port` → 按目标地址族建 socket → 按平台绑
/// 网卡(仅 Linux)/绑源 IP(与目标地址族一致时)→ connect。
async fn dial_upstream(
    host: &str,
    port: u16,
    interface: Option<&str>,
    local_address: Option<IpAddr>,
) -> Result<tokio::net::TcpStream> {
    let target_addr = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("解析目标地址失败: {host}:{port}"))?
        .next()
        .with_context(|| format!("目标地址无解析结果: {host}:{port}"))?;

    let socket = if target_addr.is_ipv6() {
        tokio::net::TcpSocket::new_v6()
    } else {
        tokio::net::TcpSocket::new_v4()
    }
    .context("建出站 socket 失败")?;

    bind_device(&socket, interface)?;

    // 仅当与目标地址族一致时才绑本地源 IP(v4 绑 v4 / v6 绑 v6),否则跳过避免报错。
    if let Some(ip) = local_address {
        let same_family = ip.is_ipv4() == target_addr.is_ipv4();
        if same_family {
            socket
                .bind(std::net::SocketAddr::new(ip, 0))
                .with_context(|| format!("绑定本地源 IP {ip} 失败"))?;
        }
    }

    socket
        .connect(target_addr)
        .await
        .with_context(|| format!("连接目标失败: {target_addr}"))
}

/// 从累积的字节缓冲里找出 `\r\n\r\n`(请求行 + headers 结束标记)。
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// 从请求行 + headers 文本里解析 `Host:` 头(明文 HTTP 分支,absolute-form 缺 host 时兜底用)。
fn parse_host_header(head: &str) -> Option<(String, u16)> {
    for line in head.lines().skip(1) {
        if let Some(rest) = line
            .strip_prefix("Host:")
            .or_else(|| line.strip_prefix("host:"))
        {
            let hostport = rest.trim();
            return split_host_port(hostport, 80);
        }
    }
    None
}

/// 拆分 `host:port` / `[v6]:port` / 纯 `host`(用 `default_port`)为 `(host, port)`。
fn split_host_port(s: &str, default_port: u16) -> Option<(String, u16)> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[') {
        // [ipv6]:port 或 [ipv6]
        let end = rest.find(']')?;
        let host = rest[..end].to_string();
        let after = &rest[end + 1..];
        let port = after
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(default_port);
        return Some((host, port));
    }
    match s.rsplit_once(':') {
        Some((host, port_str)) => match port_str.parse::<u16>() {
            Ok(port) => Some((host.to_string(), port)),
            Err(_) => Some((s.to_string(), default_port)), // 冒号可能是 ipv6 裸地址的一部分,当无端口处理
        },
        None => Some((s.to_string(), default_port)),
    }
}

/// 处理单条客户端连接:读首段请求头,按 `CONNECT`(HTTPS 隧道)或明文 HTTP(absolute-form)
/// 分流,建出口绑定的出站连接后双向转发。任何错误都干净返回,不 panic。
async fn handle_conn(
    mut client: tokio::net::TcpStream,
    interface: Option<String>,
    local_address: Option<IpAddr>,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 读到 \r\n\r\n 为止(请求行 + headers),留意可能一次 read 读到多个字节块。
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let header_end = loop {
        let mut chunk = [0u8; 4096];
        let n = client.read(&mut chunk).await.context("读客户端请求失败")?;
        if n == 0 {
            anyhow::bail!("客户端连接提前关闭");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = find_header_end(&buf) {
            break end;
        }
        if buf.len() > 64 * 1024 {
            anyhow::bail!("请求头过大,拒绝处理");
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    if method.eq_ignore_ascii_case("CONNECT") {
        // --- HTTPS 隧道:target 形如 host:port ---
        let (host, port) = split_host_port(&target, 443)
            .with_context(|| format!("CONNECT 目标解析失败: {target}"))?;
        let mut upstream = dial_upstream(&host, port, interface.as_deref(), local_address)
            .await
            .with_context(|| format!("CONNECT 建连失败: {host}:{port}"))?;
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .context("回写 200 失败")?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .context("隧道双向转发失败")?;
        Ok(())
    } else {
        // --- 明文 HTTP(absolute-form,尽力而为):从绝对 URI 或 Host 头取 host ---
        let (host, port, origin_path) = if let Some(rest) = target
            .strip_prefix("http://")
            .or_else(|| target.strip_prefix("HTTP://"))
        {
            let (authority, path) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, "/"),
            };
            let (host, port) = split_host_port(authority, 80)
                .with_context(|| format!("absolute-form host 解析失败: {authority}"))?;
            (host, port, path.to_string())
        } else {
            let (host, port) = parse_host_header(&head)
                .context("明文 HTTP 请求缺少可解析的 host(既无绝对 URI 也无 Host 头)")?;
            (host, port, target.clone())
        };

        let mut upstream = dial_upstream(&host, port, interface.as_deref(), local_address)
            .await
            .with_context(|| format!("明文 HTTP 建连失败: {host}:{port}"))?;

        // 把请求行改写成 origin-form,其余 headers 原样透传;已读到的 body 残余(header_end 之后)
        // 一并转发。
        let http_version = request_line
            .split_whitespace()
            .next_back()
            .unwrap_or("HTTP/1.1");
        let rewritten_line = format!("{method} {origin_path} {http_version}\r\n");
        let headers_rest = head
            .split_once("\r\n")
            .map(|(_, rest)| rest)
            .unwrap_or("")
            .to_string();

        upstream
            .write_all(rewritten_line.as_bytes())
            .await
            .context("写改写后的请求行失败")?;
        upstream
            .write_all(headers_rest.as_bytes())
            .await
            .context("写 headers 失败")?;
        if buf.len() > header_end {
            upstream
                .write_all(&buf[header_end..])
                .await
                .context("写 body 残余失败")?;
        }

        tokio::io::copy_bidirectional(&mut client, &mut upstream)
            .await
            .context("明文转发失败")?;
        Ok(())
    }
}

/// `proxy` 子命令:监听 TCP,逐连接 spawn task 处理(CONNECT 隧道 / 明文 HTTP 尽力而为),
/// 出站流量按 `--interface`(仅 Linux)/`--local-address` 绑定出口。
async fn run_proxy(
    listen: String,
    interface: Option<String>,
    local_address: Option<IpAddr>,
) -> Result<()> {
    if interface.is_some() && cfg!(not(target_os = "linux")) {
        log::warn!("接口绑定仅 Linux 支持,已忽略 --interface");
    }

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("监听 {listen} 失败"))?;
    log::info!(
        "proxy listening on {listen} interface={interface:?} local_address={local_address:?}"
    );

    loop {
        let (client, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                log::warn!("accept failed: {e}");
                continue;
            }
        };
        let interface = interface.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(client, interface, local_address).await {
                log::warn!("connection {peer} error: {e:#}");
            }
        });
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ =
        custom_utils::logger::logger_feature("toolkit-worker", "info,reqwest=warn", Info, false)
            .build();

    init_trace();

    let cli = Cli::parse();

    match cli.command {
        Command::Run {
            controller,
            label,
            token,
            no_exec,
            egress_ip,
            local_address,
            #[cfg(target_os = "linux")]
            interface,
        } => {
            run_worker(RunOverrides {
                controller,
                label,
                token,
                no_exec,
                egress_ip,
                local_address,
                #[cfg(target_os = "linux")]
                interface,
                #[cfg(not(target_os = "linux"))]
                interface: None,
            })
            .await
        }
        Command::Status => run_status().await,
        Command::Install {
            dry_run,
            workspace,
            controller,
            token,
            local_address,
            interface,
            env,
        } => {
            let mut svc = linux_service();
            if let Some(c) = controller {
                svc = svc.env("EGRESS_CONTROLLER", c);
            }
            if let Some(t) = token {
                svc = svc.env("EGRESS_WORKER_TOKEN", t);
            }
            if let Some(ip) = local_address {
                svc = svc.env("EGRESS_LOCAL_ADDRESS", ip.to_string());
            }
            if let Some(name) = interface {
                svc = svc.env("EGRESS_INTERFACE", name);
            }
            match svc
                .dispatch(DeployCommand::Install {
                    dry_run,
                    workspace,
                    env,
                })
                .await
                .context("安装失败")?
            {
                CliAction::DryRun(unit) => println!("{unit}"),
                CliAction::Handled => log::info!("install ok"),
                _ => {}
            }
            Ok(())
        }
        Command::Update { force } => {
            linux_service()
                .dispatch(DeployCommand::Update { force })
                .await
                .context("自更新失败")?;
            Ok(())
        }
        Command::Proxy {
            listen,
            interface,
            local_address,
        } => run_proxy(listen, interface, local_address).await,
    }
}
