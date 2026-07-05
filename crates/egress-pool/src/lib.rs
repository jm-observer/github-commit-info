//! egress-pool:中心程序「借远程 worker 出口 IP」的轻模型核心。
//!
//! 痛点是 **IP 维度反风控**(不是算力),所以本 crate 不分发爬取逻辑,只让中心进程里
//! 正常跑的程序把**出站 HTTP 请求**从别的 IP 发出去。对外只有两个原语:
//!
//! - [`Pool::fetch`] —— 匿名短租:随手挑一个在线 worker/IP 发,IP 轮换白送。
//! - [`Pool::session`] —— 钉死长租:拿一个 [`Session`] 句柄,其内所有请求走同一台
//!   worker(同一出口 IP + 连续 cookie),支持跨作用域按 `account` 复用 cookie。
//!
//! 共用策略:**同类型独占、类型间共用**。一台 worker 上,同一个 `typ`(=同站点/同账号类)
//! 至多一个活跃 session;不同 `typ` 可同住一台。匿名 [`Pool::fetch`] 不占用、随便共用。
//!
//! 数据面走**长轮询拉取**(NAT 友好):worker 主动连中心,[`Registry::next_request`]
//! 把待发请求交给它,worker 本机发出后 [`Registry::complete`] 回传。请求不落库(dumb pipe)。
//!
//! P0 为 in-memory,单 controller 存活期内有效;持久化(跨重启复用)见 P1。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

/// 心跳超过此时长视为失联(worker 每 10s 一跳)。
pub const ONLINE_TTL: Duration = Duration::from_secs(30);
/// 单次请求从派发到回传的等待上限。
const ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(60);

/// 一次要 worker 代发的出站请求(P0 仅文本 body;二进制透传如下载见 P1)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressRequest {
    pub id: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
    /// 有值 = 钉死到某 session(worker 侧按此复用 cookie jar);None = 匿名。
    #[serde(default)]
    pub session_id: Option<String>,
}

/// worker 代发后的响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressResponse {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub error: Option<EgressError>,
}

/// worker 侧对失败的归类(为 P2 的出口健康/冷却预留)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressError {
    /// `rate_limited` | `network` | `auth` | `other`
    pub kind: String,
    pub msg: String,
}

/// 借出口时可能的失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolError {
    /// 没有在线 worker 可用(优雅降级,非 panic)。
    NoWorker,
    /// 目标 worker 已消失(通道关闭)。
    WorkerGone,
    /// 等待回传超时。
    Timeout,
    /// worker 未回传就断开。
    Closed,
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PoolError::NoWorker => "no online worker available",
            PoolError::WorkerGone => "target worker gone",
            PoolError::Timeout => "egress round-trip timed out",
            PoolError::Closed => "worker disconnected before responding",
        };
        f.write_str(s)
    }
}
impl std::error::Error for PoolError {}

/// 长轮询取下一个待发请求的结果。
pub enum NextResult {
    /// 有活儿:交给 worker 执行。
    Job(EgressRequest),
    /// 本轮空转(超时无请求),worker 应立即再轮询。
    Idle,
    /// 未知 worker(未注册 / 已被替换),worker 应重新 register。
    Unknown,
}

/// 观测用的 worker 快照(供调试端点)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub id: String,
    pub egress_ip: String,
    pub online: bool,
    pub seconds_since_heartbeat: u64,
    /// 最近一次心跳的绝对时间戳(unix epoch 毫秒,wall-clock),供前端展示「最近心跳时间」。
    pub last_heartbeat_ms: i64,
    /// 当前被占用的 type 列表(同类型独占的可见状态)。
    pub types_held: Vec<String>,
    /// worker 代发绑定的网卡名(Linux `--interface`),无绑定则 None。
    pub interface: Option<String>,
    /// worker 代发绑定的本地源 IP(`--local-address`),无绑定则 None。
    pub local_address: Option<String>,
}

/// 当前 wall-clock 时间的 unix epoch 毫秒(仅用于展示,不参与 TTL 判定)。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct WorkerEntry {
    egress_ip: String,
    last_heartbeat: Instant,
    /// 与 `last_heartbeat` 并行的 wall-clock 毫秒时间戳(供观测展示绝对时间;TTL 判定仍用 Instant)。
    last_heartbeat_ms: i64,
    tx: mpsc::UnboundedSender<EgressRequest>,
    rx: Arc<AsyncMutex<mpsc::UnboundedReceiver<EgressRequest>>>,
    /// typ -> 当前占用者 account(None = 匿名临时 session)。承载「同类型独占」。
    occupied: HashMap<String, Option<String>>,
    /// worker 代发绑定的网卡名(Linux `--interface`),供观测展示。
    interface: Option<String>,
    /// worker 代发绑定的本地源 IP(`--local-address`),供观测展示。
    local_address: Option<String>,
}

#[derive(Default)]
struct Inner {
    workers: HashMap<String, WorkerEntry>,
    /// req_id -> 等待回传的 oneshot。
    pending: HashMap<String, oneshot::Sender<EgressResponse>>,
    /// (typ, account) -> worker_id,持久绑定(cookie 复用钉死同一出口)。
    bindings: HashMap<(String, String), String>,
    /// 匿名 fetch 的 round-robin 游标。
    rr: usize,
}

/// 进程内共享的 worker 注册表 + 请求路由表。既是算力面也是出口面。
#[derive(Default)]
pub struct Registry {
    inner: Mutex<Inner>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    // ---------- worker 侧 ----------

    /// worker 注册 / 重注册(重注册换新通道,旧的自然丢弃)。
    ///
    /// `interface` / `local_address` 是 worker 侧代发绑定的网卡名 / 本地源 IP(纯观测用途,
    /// 不参与路由决策),老版本 worker 不传则为 `None`。
    pub fn register(
        &self,
        worker_id: &str,
        egress_ip: &str,
        interface: Option<&str>,
        local_address: Option<&str>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut g = self.inner.lock().unwrap();
        g.workers.insert(
            worker_id.to_string(),
            WorkerEntry {
                egress_ip: egress_ip.to_string(),
                last_heartbeat: Instant::now(),
                last_heartbeat_ms: now_ms(),
                tx,
                rx: Arc::new(AsyncMutex::new(rx)),
                occupied: HashMap::new(),
                interface: interface.map(|s| s.to_string()),
                local_address: local_address.map(|s| s.to_string()),
            },
        );
        log::info!("egress worker registered: {worker_id} ip={egress_ip}");
    }

    /// 刷新心跳。返回该 worker 是否存在。
    pub fn heartbeat(&self, worker_id: &str) -> bool {
        let mut g = self.inner.lock().unwrap();
        match g.workers.get_mut(worker_id) {
            Some(w) => {
                w.last_heartbeat = Instant::now();
                w.last_heartbeat_ms = now_ms();
                true
            }
            None => false,
        }
    }

    /// 长轮询取下一个待发请求(顺带刷新心跳)。`wait` 内无请求返回 [`NextResult::Idle`]。
    pub async fn next_request(&self, worker_id: &str, wait: Duration) -> NextResult {
        let rx = {
            let mut g = self.inner.lock().unwrap();
            match g.workers.get_mut(worker_id) {
                Some(w) => {
                    w.last_heartbeat = Instant::now();
                    w.last_heartbeat_ms = now_ms();
                    w.rx.clone()
                }
                None => return NextResult::Unknown,
            }
        };
        // 单个 worker 同一时刻只有一个 in-flight 的 next_request,锁 rx 不会争用。
        let mut rx = rx.lock().await;
        match tokio::time::timeout(wait, rx.recv()).await {
            Ok(Some(req)) => NextResult::Job(req),
            Ok(None) => NextResult::Unknown, // 通道被替换/关闭
            Err(_) => NextResult::Idle,
        }
    }

    /// worker 回传结果,唤醒对应的等待方。
    pub fn complete(&self, resp: EgressResponse) {
        let tx = {
            let mut g = self.inner.lock().unwrap();
            g.pending.remove(&resp.id)
        };
        if let Some(tx) = tx {
            let _ = tx.send(resp);
        }
    }

    /// 观测快照。
    pub fn workers_snapshot(&self) -> Vec<WorkerStatus> {
        let g = self.inner.lock().unwrap();
        let now = Instant::now();
        let mut v: Vec<WorkerStatus> = g
            .workers
            .iter()
            .map(|(id, w)| {
                let since = now.duration_since(w.last_heartbeat);
                WorkerStatus {
                    id: id.clone(),
                    egress_ip: w.egress_ip.clone(),
                    online: since < ONLINE_TTL,
                    seconds_since_heartbeat: since.as_secs(),
                    last_heartbeat_ms: w.last_heartbeat_ms,
                    types_held: w.occupied.keys().cloned().collect(),
                    interface: w.interface.clone(),
                    local_address: w.local_address.clone(),
                }
            })
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    // ---------- 消费侧(供 Pool 调用) ----------

    fn is_online(w: &WorkerEntry, now: Instant) -> bool {
        now.duration_since(w.last_heartbeat) < ONLINE_TTL
    }

    /// 派发一个请求到某 worker(None = 匿名轮询挑),返回等待回传的 receiver。
    fn dispatch(
        &self,
        worker: Option<String>,
        req: EgressRequest,
    ) -> Result<oneshot::Receiver<EgressResponse>, PoolError> {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();

        let worker_id = match worker {
            Some(w) => w,
            None => {
                let mut ids: Vec<String> = g
                    .workers
                    .iter()
                    .filter(|(_, w)| Self::is_online(w, now))
                    .map(|(id, _)| id.clone())
                    .collect();
                if ids.is_empty() {
                    return Err(PoolError::NoWorker);
                }
                ids.sort();
                let idx = g.rr % ids.len();
                g.rr = g.rr.wrapping_add(1);
                ids[idx].clone()
            }
        };

        let tx = match g.workers.get(&worker_id) {
            Some(w) => w.tx.clone(),
            None => return Err(PoolError::WorkerGone),
        };
        let id = req.id.clone();
        let (otx, orx) = oneshot::channel();
        g.pending.insert(id.clone(), otx);
        if tx.send(req).is_err() {
            g.pending.remove(&id);
            return Err(PoolError::WorkerGone);
        }
        Ok(orx)
    }

    async fn round_trip(
        &self,
        worker: Option<String>,
        req: EgressRequest,
    ) -> Result<EgressResponse, PoolError> {
        let rx = self.dispatch(worker, req)?;
        match tokio::time::timeout(ROUND_TRIP_TIMEOUT, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(PoolError::Closed),
            Err(_) => Err(PoolError::Timeout),
        }
    }

    /// 按「同类型独占、类型间共用 + account 绑定复用」挑一个 worker,返回 (worker_id, session_id)。
    fn acquire_session(
        &self,
        typ: &str,
        account: Option<&str>,
    ) -> Result<(String, String), PoolError> {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();

        // 1. account 已绑定且在线 → 复用同一 worker(cookie 跟着它)。
        if let Some(acc) = account {
            let key = (typ.to_string(), acc.to_string());
            if let Some(wid) = g.bindings.get(&key).cloned() {
                let online = g
                    .workers
                    .get(&wid)
                    .map(|w| Self::is_online(w, now))
                    .unwrap_or(false);
                if online {
                    g.workers
                        .get_mut(&wid)
                        .unwrap()
                        .occupied
                        .insert(typ.to_string(), Some(acc.to_string()));
                    return Ok((wid, session_id_for(typ, Some(acc))));
                }
                // 绑定的 worker 掉线 → 解绑,重新挑选(老 cookie 从新 IP 也用不了)。
                g.bindings.remove(&key);
            }
        }

        // 2. 挑一个「该 typ 无占用」的在线 worker(同类型独占)。
        let mut ids: Vec<String> = g
            .workers
            .iter()
            .filter(|(_, w)| Self::is_online(w, now) && !w.occupied.contains_key(typ))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        let wid = ids.into_iter().next().ok_or(PoolError::NoWorker)?;

        g.workers
            .get_mut(&wid)
            .unwrap()
            .occupied
            .insert(typ.to_string(), account.map(|s| s.to_string()));
        if let Some(acc) = account {
            g.bindings
                .insert((typ.to_string(), acc.to_string()), wid.clone());
        }
        Ok((wid, session_id_for(typ, account)))
    }

    /// 释放 session 占用。匿名(临时)释放占用;具名 account 保留占用 + 绑定(钉死复用)。
    fn release_session(&self, worker_id: &str, typ: &str, account: Option<&str>) {
        if account.is_some() {
            return; // 绑定持久:为 cookie 复用钉死这台 worker,不在 drop 时释放。
        }
        let mut g = self.inner.lock().unwrap();
        if let Some(w) = g.workers.get_mut(worker_id) {
            w.occupied.remove(typ);
        }
    }
}

/// 具名/匿名 session 的稳定 id:具名 → `typ:account`(跨作用域稳定 → worker 复用同一 cookie jar);
/// 匿名 → 每次唯一(每次新 cookie)。
fn session_id_for(typ: &str, account: Option<&str>) -> String {
    match account {
        Some(a) => format!("{typ}:{a}"),
        None => format!("{typ}:anon:{}", uuid::Uuid::new_v4()),
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 爬取代码用的进程内句柄(廉价可 clone)。
#[derive(Clone)]
pub struct Pool {
    reg: Arc<Registry>,
}

impl Pool {
    pub fn new(reg: Arc<Registry>) -> Self {
        Self { reg }
    }

    /// 匿名短租:随手挑一个在线 worker 代发(IP 轮换)。无在线 worker → [`PoolError::NoWorker`]。
    pub async fn fetch(
        &self,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<String>,
    ) -> Result<EgressResponse, PoolError> {
        let req = EgressRequest {
            id: new_id(),
            method: method.to_string(),
            url: url.to_string(),
            headers,
            body,
            session_id: None,
        };
        self.reg.round_trip(None, req).await
    }

    /// 钉死长租:拿一个 [`Session`]。`account=Some` 走具名身份(跨作用域复用 cookie + 出口);
    /// `account=None` 为临时钉定(本作用域内同一 IP,结束即释放)。
    pub fn session(&self, typ: &str, account: Option<&str>) -> Result<Session, PoolError> {
        let (worker_id, session_id) = self.reg.acquire_session(typ, account)?;
        Ok(Session {
            reg: self.reg.clone(),
            worker_id,
            session_id,
            typ: typ.to_string(),
            account: account.map(|s| s.to_string()),
        })
    }
}

/// 钉死到某一台 worker 的会话句柄。drop 时释放占用(具名身份保留绑定以便复用)。
pub struct Session {
    reg: Arc<Registry>,
    worker_id: String,
    session_id: String,
    typ: String,
    account: Option<String>,
}

impl Session {
    /// 当前钉定的 worker id。
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// 经钉定 worker 代发(同一 IP + 连续 cookie)。
    pub async fn fetch(
        &self,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<String>,
    ) -> Result<EgressResponse, PoolError> {
        let req = EgressRequest {
            id: new_id(),
            method: method.to_string(),
            url: url.to_string(),
            headers,
            body,
            session_id: Some(self.session_id.clone()),
        };
        self.reg.round_trip(Some(self.worker_id.clone()), req).await
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reg
            .release_session(&self.worker_id, &self.typ, self.account.as_deref());
    }
}
