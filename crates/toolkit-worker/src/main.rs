//! toolkit-worker:出口代理节点(轻模型「借出口」的执行端)。
//!
//! pull 模型(NAT 友好):worker 主动连 controller —— register → 心跳(10s)→ 长轮询
//! `egress/next` 取待发请求 → 用本机(本节点出口 IP)reqwest 代发 → `egress/result` 回传。
//!
//! per-session cookie:请求带 `session_id` 时用一个带 cookie jar 的独立 client(按 session_id
//! 惰性建、复用),使同一 session 的登录态在多次请求间连续;匿名请求走无 cookie 的共享 client。
//!
//! 出口选择(F1):本机可能有多个网卡/网关(如「直连」和「VPN」)。`--local-address`(或环境变量
//! `EGRESS_LOCAL_ADDRESS`)指定代发流量绑定的本地源 IP —— 源 IP 决定实际走哪个网关。该选项作用于
//! 所有**代发**client(`anon` / per-session)以及出口 IP 探测(`detect_egress_ip`),但**不**作用于
//! 连 controller 的 `ctrl` client(controller 可能在另一网段,绑定会导致连不通)。不传则行为与之前
//! 完全一致(不绑定,走系统默认路由)。
//!
//! 出口选择(F2,仅 Linux):`--interface <name>`(或环境变量 `EGRESS_INTERFACE`)按网卡名绑定
//! (`SO_BINDTODEVICE`,`reqwest` 的 `ClientBuilder::interface`),比 `--local-address` 更贴近
//! 「就是要走这张网卡」的语义,且能绑定没有固定 IP 的网卡。**该方法只有 Linux/Android 才有**——
//! 本 crate 在 Windows 开发机上也要能编译,所以所有 `.interface(..)` 调用都 `#[cfg(target_os =
//! "linux")]` 包起来,非 Linux 分支打印警告并忽略。`--interface` 与 `--local-address` 可以同传
//! (两者都会被调用,不冲突),`--interface` 优先级更高(语义上更精确)。
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
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

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
    /// 启动 worker(register → 心跳 → 长轮询代发)。
    Run {
        /// controller 基址,如 http://127.0.0.1:8788
        #[arg(long, env = "EGRESS_CONTROLLER")]
        controller: String,
        /// 共享鉴权 token(须与 controller 的 EGRESS_WORKER_TOKEN 一致;为空则不带)
        #[arg(long, env = "EGRESS_WORKER_TOKEN", default_value = "")]
        token: String,
        /// worker id 持久化文件(重启保持同一 id → cookie 绑定可复用)
        #[arg(long, default_value = "egress-worker-id")]
        id_file: String,
        /// 出口 IP 覆盖(默认启动时探测 https://api.ipify.org)
        #[arg(long)]
        egress_ip: Option<String>,
        /// 代发流量绑定的本地源 IP(决定走哪个网关;不传则不绑定,走系统默认路由)
        #[arg(long, env = "EGRESS_LOCAL_ADDRESS")]
        local_address: Option<IpAddr>,
        /// 代发流量绑定的网卡名(`SO_BINDTODEVICE`,仅 Linux;与 --local-address 可并存,
        /// 优先级更高)。非 Linux 传了此项会打印警告并忽略。
        #[arg(long, env = "EGRESS_INTERFACE")]
        interface: Option<String>,
    },
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
    /// 列出本机网卡 IP,辅助挑选 `run --local-address <IP>`。
    List,
    /// 扫描本机可用出口网卡 + ping controller 候选,打印可直接复制运行的 `run` 命令。
    Scan {
        /// controller 基址,可重复传多个一起探测;总会额外并入内置默认候选。
        #[arg(long)]
        controller: Vec<String>,
        /// 共享鉴权 token;不传则取环境变量 `EGRESS_WORKER_TOKEN`,都没有则命令里填占位符。
        #[arg(long)]
        token: Option<String>,
    },
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

/// 读取 `path` 里已有的非空 id;不存在/为空返回 `None`(不写文件,交给上层决定写什么)。
fn read_existing_id(path: &str) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 取网卡 MAC 地址的规范化短串(去冒号、转小写),拿不到则 `None`。
fn mac_suffix(interface: &str) -> Option<String> {
    match mac_address::mac_address_by_name(interface) {
        Ok(Some(mac)) => Some(mac.to_string().replace(':', "").to_lowercase()),
        _ => None,
    }
}

/// 按 `local_address` 反查其所属网卡名(`if_addrs` 枚举匹配),找不到则 `None`。
fn interface_name_for_ip(ip: IpAddr) -> Option<String> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find(|i| i.ip() == ip)
        .map(|i| i.name)
}

/// 派生/加载 worker id,优先级:
///
/// 1. `id_file` 里已有非空 id → 直接用(重启保持同一 id,cookie 绑定可复用)。
/// 2. 否则若指定了 `--interface <name>` → `w-{name}-{mac}`(mac 拿不到则退化 `w-{name}`)。
/// 3. 否则若指定了 `--local-address <ip>` → 反查所属网卡取 mac → `w-{ip}-{mac}`
///    (网卡名/mac 任一拿不到则退化 `w-{ip}`)。
/// 4. 都没有 → 退回随机 `w_<uuid>` 并持久化到 `id_file`。
///
/// 分支 2/3 派生出的 id 会写回 `id_file`(下次重启命中分支 1,保持稳定)。
fn resolve_worker_id(
    id_file: &str,
    interface: Option<&str>,
    local_address: Option<IpAddr>,
) -> Result<String> {
    if let Some(id) = read_existing_id(id_file) {
        return Ok(id);
    }

    let derived = if let Some(name) = interface {
        Some(match mac_suffix(name) {
            Some(mac) => format!("w-{name}-{mac}"),
            None => format!("w-{name}"),
        })
    } else if let Some(ip) = local_address {
        Some(
            match interface_name_for_ip(ip)
                .and_then(|name| mac_suffix(&name).map(|mac| (name, mac)))
            {
                Some((_, mac)) => format!("w-{ip}-{mac}"),
                None => format!("w-{ip}"),
            },
        )
    } else {
        None
    };

    let id = match derived {
        Some(id) => id,
        None => format!("w_{}", uuid::Uuid::new_v4()),
    };
    std::fs::write(id_file, &id).with_context(|| format!("write worker id file {id_file}"))?;
    Ok(id)
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

/// `run` 子命令:register → 心跳 → 长轮询代发主循环。实际网络逻辑与升级前一字不改。
async fn run_worker(
    controller: String,
    token: String,
    id_file: String,
    egress_ip: Option<String>,
    local_address: Option<IpAddr>,
    interface: Option<String>,
) -> Result<()> {
    // systemd watchdog 心跳(Type=notify 需要 READY=1;非 systemd 环境下自动 no-op)。
    let _watchdog = linux_service().spawn_watchdog();

    if interface.is_some() && cfg!(not(target_os = "linux")) {
        log::warn!("接口绑定仅 Linux 支持,已忽略 --interface");
    }

    let controller = controller.trim_end_matches('/').to_string();
    let worker_id = resolve_worker_id(&id_file, interface.as_deref(), local_address)?;
    let egress_ip = match egress_ip {
        Some(ip) => ip,
        None => detect_egress_ip(local_address, interface.as_deref())
            .await
            .unwrap_or_else(|| "unknown".to_string()),
    };
    log::info!(
        "id={worker_id} egress_ip={egress_ip} controller={controller} local_address={local_address:?} interface={interface:?}"
    );

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

/// `list-egress` 子命令:枚举本机网卡 IPv4 地址,辅助挑选 `--local-address`。
fn list_egress() -> Result<()> {
    let ifaces = if_addrs::get_if_addrs().context("枚举本机网卡失败")?;
    println!("本机可用出口(源 IP)——挑一个传给 `run --local-address <IP>`:");
    for iface in ifaces {
        let IpAddr::V4(ip) = iface.ip() else {
            continue; // 只列 IPv4,--local-address 场景够用
        };
        if iface.is_loopback() {
            println!("  {}\t{ip}\t[loopback,一般不用]", iface.name);
        } else {
            println!("  {}\t{ip}", iface.name);
        }
    }
    Ok(())
}

/// 内置 controller 候选(局域网常见地址),`scan` 时与用户传入的 `--controller` 并集探测。
const DEFAULT_CONTROLLERS: &[&str] = &["http://127.0.0.1:8788", "http://192.168.0.68:8788"];

/// `scan` 子命令:一步选出「哪个 controller 能连 + 哪张网卡当出口」,打印可直接复制的 `run` 命令。
///
/// 分三段:1) 并集去重后逐个 ping controller 候选的 `/api/web/health`;2) 枚举本机非回环
/// IPv4 网卡,逐个探测其外网出口 IP(Linux 用 `--interface` 绑定,非 Linux 退化用
/// `--local-address` 近似)、算建议 id、按外网 IP 去重(重复出口只保留第一个);
/// 3) 打印三段人类可读的报告 + 可直接复制运行的 `run` 命令。
async fn run_scan(controllers: Vec<String>, token: Option<String>) -> Result<()> {
    // ---------- 1. controller 候选:并集去重(保留首次出现顺序)+ 逐个 ping ----------
    let mut candidates: Vec<String> = Vec::new();
    for c in controllers
        .into_iter()
        .chain(DEFAULT_CONTROLLERS.iter().map(|s| s.to_string()))
    {
        let c = c.trim_end_matches('/').to_string();
        if !candidates.contains(&c) {
            candidates.push(c);
        }
    }

    println!("=== controller 候选(一起 ping) ===");
    let ping_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .context("build ping client")?;
    let mut reachable: Option<String> = None;
    for url in &candidates {
        let health_url = format!("{url}/api/web/health");
        let start = Instant::now();
        match ping_client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let ms = start.elapsed().as_millis();
                println!("  ✓ {url}\thealth ok ({ms}ms)");
                if reachable.is_none() {
                    reachable = Some(url.clone());
                }
            }
            Ok(resp) => {
                println!("  ✗ {url}\tHTTP {}", resp.status().as_u16());
            }
            Err(e) => {
                let desc = if e.is_timeout() {
                    "超时".to_string()
                } else {
                    e.to_string()
                };
                println!("  ✗ {url}\t{desc}");
            }
        }
    }
    let chosen_controller = match &reachable {
        Some(url) => {
            println!("  → 下面命令用 {url}");
            url.clone()
        }
        None => {
            let fallback = candidates
                .first()
                .cloned()
                .unwrap_or_else(|| DEFAULT_CONTROLLERS[0].to_string());
            println!("  均不可达,默认使用第一个候选,请检查 controller 是否在运行 → {fallback}");
            fallback
        }
    };

    let token_str = token
        .or_else(|| std::env::var("EGRESS_WORKER_TOKEN").ok())
        .filter(|t| !t.is_empty());
    let token_display = token_str.as_deref().unwrap_or("<TOKEN>");

    // ---------- 2. 枚举出口网卡,探测各自的外网出口 IP + 建议 id ----------
    println!();
    println!("=== 出口接口 ===");
    println!("  接口\t源IP\t\t外网出口IP\t建议 id");

    struct IfaceProbe {
        name: String,
        source_ip: IpAddr,
        egress: Result<String, String>,
        suggested_id: String,
    }

    let ifaces = if_addrs::get_if_addrs().context("枚举本机网卡失败")?;
    let mut probes: Vec<IfaceProbe> = Vec::new();
    for iface in &ifaces {
        if iface.is_loopback() {
            continue;
        }
        let IpAddr::V4(v4) = iface.ip() else {
            continue; // 只看 IPv4
        };
        if v4.octets()[0] == 169 && v4.octets()[1] == 254 {
            continue; // link-local,跳过
        }
        let ip = IpAddr::V4(v4);

        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(8));
        #[cfg(target_os = "linux")]
        {
            builder = apply_interface(builder, Some(iface.name.as_str()));
        }
        #[cfg(not(target_os = "linux"))]
        {
            builder = builder.local_address(ip);
        }
        let egress = match builder.build() {
            Ok(c) => match c.get("https://api.ipify.org").send().await {
                Ok(resp) => match resp.text().await {
                    Ok(text) => Ok(text.trim().to_string()),
                    Err(e) => Err(format!("探测失败:{e}")),
                },
                Err(e) => Err(format!("探测失败:{e}")),
            },
            Err(e) => Err(format!("探测失败:{e}")),
        };

        let suggested_id = match mac_suffix(&iface.name) {
            Some(mac) => format!("w-{}-{mac}", iface.name),
            None => format!("w-{}", iface.name),
        };

        probes.push(IfaceProbe {
            name: iface.name.clone(),
            source_ip: ip,
            egress,
            suggested_id,
        });
    }

    // 按外网 IP 去重:外网IP -> 首个使用它的接口名。
    let mut first_owner: HashMap<String, String> = HashMap::new();
    let mut notes: Vec<String> = Vec::new();
    let mut usable: Vec<&IfaceProbe> = Vec::new();
    for p in &probes {
        match &p.egress {
            Ok(ip) => {
                if let Some(owner) = first_owner.get(ip) {
                    println!(
                        "  {}\t{}\t{ip}\t(与 {owner} 同出口,跳过)",
                        p.name, p.source_ip
                    );
                    notes.push(format!("{} 外网IP与 {owner} 相同 = 同出口", p.name));
                } else {
                    println!("  {}\t{}\t{ip}\t{}", p.name, p.source_ip, p.suggested_id);
                    first_owner.insert(ip.clone(), p.name.clone());
                    usable.push(p);
                }
            }
            Err(e) => {
                println!("  {}\t{}\t{e}\t-", p.name, p.source_ip);
                notes.push(format!("{} 探测失败", p.name));
            }
        }
    }
    if !notes.is_empty() {
        println!("  (跳过:lo 回环;{})", notes.join(";"));
    } else {
        println!("  (跳过:lo 回环)");
    }

    // ---------- 3. 打印可复制运行的命令 ----------
    println!();
    println!("=== 挑一个,复制运行 ===");
    if usable.is_empty() {
        println!("  (没有可用的出口网卡,无法生成命令)");
    }
    for p in &usable {
        let egress_ip = p.egress.as_deref().unwrap_or("?");
        println!("  # {} → 出口 {egress_ip}", p.name);
        println!(
            "  toolkit-worker run --controller {chosen_controller} --token {token_display} --interface {}",
            p.name
        );
    }

    Ok(())
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
        let headers_rest = head.splitn(2, "\r\n").nth(1).unwrap_or("").to_string();

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
            token,
            id_file,
            egress_ip,
            local_address,
            interface,
        } => {
            run_worker(
                controller,
                token,
                id_file,
                egress_ip,
                local_address,
                interface,
            )
            .await
        }
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
        Command::List => list_egress(),
        Command::Scan { controller, token } => run_scan(controller, token).await,
        Command::Proxy {
            listen,
            interface,
            local_address,
        } => run_proxy(listen, interface, local_address).await,
    }
}
