//! 抓包真实后端（抓包设计 §7–§9；Phase 2a 全 TUN 抓包）。真机 spike 见
//! docs/net-policy/net-policy-capture-validation-report.md（pktmon→pcapng 管道已验证）。
//!
//! 分工：机器无关的 DTO/校验/过滤器预算/状态机在 `net_policy_core::capture`；本模块是**有副作用**
//! 的一侧——shell 调 `pktmon`（参数数组，**禁拼 shell 字符串**，§13）、解析组件、落盘 manifest、配额。
//!
//! **范围**：`All`（全 TUN）+ 定向（Process/Domain/Ip → [`resolve_endpoints`] 按 `/connections` 解析
//! 包面端点 → `plan_filters` → pktmon 命名过滤器）+ Stop/List/Get/Delete/Read。定向的 fake-ip 包面口径
//! （§3.1）取连接表 destinationIP（fake-ip 模式下即 TUN 包面地址），真机 golden probe 未做前 manifest
//! known_limits 标注该口径未经真机确认；空结果返回 `capture_target_empty`（不静默抓空）。
//!
//! 单会话：[`CaptureManager`] 持 `Mutex<Option<Active>>`，同一时刻只允许一个会话（§7）。

use anyhow::{bail, Context, Result};
use net_policy_core::capture::{
    format_session_id, is_valid_session_id, plan_filters, CaptureEndpoint, CaptureManifest,
    CaptureOpts, CaptureProtocol, CaptureSession, CaptureState, CaptureStopReason, CaptureTarget,
    EndpointSource, FilterPlanError, CAPTURE_SCHEMA_VERSION,
};
use net_policy_core::config::ProcessRef;
use net_policy_core::protocol::{ErrorKind, ProtocolError, Version};
use net_policy_core::types::Connection;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// 机器级配额（§9）：完成会话总字节 + 数量上限。
const QUOTA_TOTAL_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB
const QUOTA_MAX_SESSIONS: usize = 10;

/// mihomo Meta TUN 适配器描述匹配子串（§8#3：按 alias/description 联合匹配 TUN 组件）。
/// mihomo gvisor TUN 默认适配器名 `Meta`；WireGuard/Mihomo 变体也一并纳入候选。
const TUN_MATCHERS: &[&str] = &["Meta", "Mihomo", "mihomo", "WireGuard Tunnel"];

/// `System32\PktMon.exe` 绝对路径（§13：只传固定二进制，不依赖 PATH）。
pub fn pktmon_exe() -> PathBuf {
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    Path::new(&sysroot).join("System32").join("PktMon.exe")
}

/// pktmon 是否可用（探测：`available()` 决定是否声明 `capture_v1`）。
pub fn available() -> bool {
    cfg!(windows) && pktmon_exe().exists()
}

/// 运行 pktmon 子命令（参数数组），返回 stdout。非零退出码报错带 stderr。
fn run_pktmon(args: &[&str]) -> Result<String> {
    let mut cmd = Command::new(pktmon_exe());
    cmd.args(args);
    crate::proc::hide_console(&mut cmd);
    let out = cmd
        .output()
        .with_context(|| format!("spawn pktmon {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "pktmon {} 失败（{}）：{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── `pktmon list --json` 组件解析（纯逻辑，可单测）───────────────────────────

/// `pktmon list --json` 一个组件（只取需要的字段；Id 兼容数字/字符串）。
#[derive(Debug, Clone, Deserialize)]
struct RawComponent {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Id", default)]
    id: serde_json::Value,
}

/// `pktmon list --json` 一个网卡分组。
#[derive(Debug, Clone, Deserialize)]
struct RawGroup {
    #[serde(rename = "Group", default)]
    group: String,
    #[serde(rename = "Components", default)]
    components: Vec<RawComponent>,
}

/// 解析后的一个 miniport 组件（代表整块网卡，是抓包点）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunComponent {
    pub id: String,
    pub group: String,
}

fn id_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// 从 `pktmon list --json` 定位 TUN 的 **miniport** 组件（§8#3）。
///
/// 结构（真机实测，见验证报告）：顶层按网卡分组 `{Group, Components:[{Name,Id,...}]}`；每组第一个
/// **miniport** 组件的 `Name` 等于 `Group`（语言中立判据，避开本地化的 `Type`）。匹配 `Group` 含
/// [`TUN_MATCHERS`] 任一子串的分组，取其 miniport 组件 Id。**零个或多个候选均拒绝**（不退化抓物理网卡）。
pub fn find_tun_component(json: &str) -> Result<TunComponent, ErrorKind> {
    let groups: Vec<RawGroup> = serde_json::from_str(json).map_err(|_| ErrorKind::Internal)?;
    let mut hits: Vec<TunComponent> = Vec::new();
    for g in &groups {
        let is_tun = TUN_MATCHERS.iter().any(|m| g.group.contains(m));
        if !is_tun {
            continue;
        }
        // miniport = Name 等于 Group 的组件。
        for c in &g.components {
            if c.name == g.group {
                if let Some(id) = id_to_string(&c.id) {
                    hits.push(TunComponent {
                        id,
                        group: g.group.clone(),
                    });
                }
            }
        }
    }
    hits.dedup();
    match hits.len() {
        1 => Ok(hits.into_iter().next().unwrap()),
        _ => Err(ErrorKind::CaptureComponentNotFound),
    }
}

// ── pktmon shell 操作（§4 命令形态，spike 验证）─────────────────────────────

/// `pktmon status` 是否显示 capture 正在运行（§8#1：已有会话则拒绝，绝不 stop）。
/// 判据：输出含运行态关键词而非「没有运行」。中英文环境都覆盖。
pub fn status_running() -> Result<bool> {
    let out = run_pktmon(&["status"])?;
    let lower = out.to_lowercase();
    let not_running = out.contains("没有运行") || lower.contains("not running");
    Ok(!not_running
        && (lower.contains("pktmon") || out.contains("记录程序") || lower.contains("logger")))
}

/// `pktmon filter list` 是否已有任意过滤器（§8#2：有则拒绝，绝不 remove 他人过滤器）。
pub fn filters_present() -> Result<bool> {
    let out = run_pktmon(&["filter", "list"])?;
    // 空时输出含「无」/「no filters」；有过滤器则出现编号行。
    let empty = out.contains("无") || out.to_lowercase().contains("no filter");
    Ok(!empty)
}

/// 列出 pktmon 组件（原始 JSON），供 [`find_tun_component`]。
pub fn list_components_json() -> Result<String> {
    run_pktmon(&["list", "--json"])
}

/// 全 TUN 抓包 start（§4）：`--capture --comp <id> --pkt-size <n> --file-name <etl> --file-size <MiB>
/// --log-mode circular`。snap_len=0 表示完整包。
pub fn start_capture(component: &str, opts: &CaptureOpts, etl: &Path) -> Result<()> {
    let pkt = opts.snap_len.to_string();
    let fsize = opts.file_size_mib.to_string();
    let etl_s = etl.to_string_lossy();
    run_pktmon(&[
        "start",
        "--capture",
        "--comp",
        component,
        "--pkt-size",
        &pkt,
        "--file-name",
        &etl_s,
        "--file-size",
        &fsize,
        "--log-mode",
        "circular",
    ])
    .map(|_| ())
}

/// `pktmon stop`（封口 ETL）。
pub fn stop_capture() -> Result<()> {
    run_pktmon(&["stop"]).map(|_| ())
}

/// 加一条命名过滤器（§4：`filter add <name> -i <ip> -p <port> -t <TCP|UDP>`）。参数数组，无注入面。
pub fn add_filter(f: &net_policy_core::capture::CaptureFilter) -> Result<()> {
    let port = f.port.to_string();
    run_pktmon(&[
        "filter",
        "add",
        &f.name,
        "-i",
        &f.capture_ip,
        "-p",
        &port,
        "-t",
        f.network.pktmon_flag(),
    ])
    .map(|_| ())
}

/// 清除所有 pktmon 过滤器（§4：`filter remove`）。**仅在本会话开始前确认过滤器为空、取得所有权后调用**。
pub fn remove_filters() -> Result<()> {
    run_pktmon(&["filter", "remove"]).map(|_| ())
}

/// `pktmon etl2pcap <etl> --out <pcapng> --component-id <id>`（§4）。
pub fn etl2pcap(etl: &Path, pcapng: &Path, component: &str) -> Result<()> {
    let etl_s = etl.to_string_lossy();
    let out_s = pcapng.to_string_lossy();
    run_pktmon(&[
        "etl2pcap",
        &etl_s,
        "--out",
        &out_s,
        "--component-id",
        component,
    ])
    .map(|_| ())
}

// ── Phase 2b：定向 target → 包面端点解析（§5.1）──────────────────────────────

/// 域名规范化（小写 + 去尾点）用于 suffix 匹配。
fn norm_host(h: &str) -> String {
    h.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// 把定向 target 按**当前连接快照**解析为包面端点（§5.1）。`All` 不该进这里（返回空）。
///
/// **fake-ip 口径（§3.1）**：fake-ip 模式下应用发往 TUN 的包目标就是 `/connections.metadata.destinationIP`
/// （198.18.x.x fake-ip），即 TUN 包面地址，故直接取 `destination_ip` 作 `capture_ip`、source 记 `Connection`。
/// 真机 golden probe 未做前，manifest 的 known_limits 标注该口径未经真机确认。空结果返回
/// `CaptureTargetEmpty`（提示先产生流量 / 改全 TUN 短抓，**不静默抓空**）。
pub fn resolve_endpoints(
    target: &CaptureTarget,
    conns: &[Connection],
) -> Result<Vec<CaptureEndpoint>, ErrorKind> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<CaptureEndpoint>, c: &Connection, source: EndpointSource| {
        let ip = c.destination_ip.trim();
        if ip.is_empty() {
            return;
        }
        let Ok(port) = c.destination_port.trim().parse::<u16>() else {
            return;
        };
        let Some(network) = CaptureProtocol::from_network(&c.network) else {
            return;
        };
        out.push(CaptureEndpoint {
            capture_ip: ip.to_string(),
            port,
            network,
            source,
        });
    };
    match target {
        CaptureTarget::All => {}
        CaptureTarget::Process(p) => {
            for c in conns {
                let hit = match p {
                    ProcessRef::ProcessPath(v) => c.process_path.eq_ignore_ascii_case(v),
                    ProcessRef::ProcessName(v) => c.process.eq_ignore_ascii_case(v),
                };
                if hit {
                    push(&mut out, c, EndpointSource::Connection);
                }
            }
        }
        CaptureTarget::Domain(d) => {
            let dn = norm_host(d);
            for c in conns {
                let host = norm_host(&c.host);
                // 精确或子域后缀命中（host == d 或 host 以 ".d" 结尾）。
                if host == dn || host.ends_with(&format!(".{dn}")) {
                    push(&mut out, c, EndpointSource::Connection);
                }
            }
        }
        CaptureTarget::Ip(ip) => {
            let want = ip.trim().to_ascii_lowercase();
            for c in conns {
                if c.destination_ip.trim().eq_ignore_ascii_case(&want) {
                    push(&mut out, c, EndpointSource::UserInput);
                }
            }
        }
    }
    if out.is_empty() {
        return Err(ErrorKind::CaptureTargetEmpty);
    }
    Ok(out)
}

// ── CaptureStore：会话目录 / manifest / 配额 / 分块读（§9）─────────────────────

pub struct CaptureStore {
    root: PathBuf,
}

impl CaptureStore {
    pub fn new(workspace: &Path) -> Self {
        Self {
            root: workspace.join("captures"),
        }
    }

    fn session_dir(&self, id: &str) -> Option<PathBuf> {
        // 防路径穿越：id 必须严格合法（§9/§13），否则拒绝定位。
        is_valid_session_id(id).then(|| self.root.join(id))
    }

    fn ensure_root(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("创建 captures 根失败：{}", self.root.display()))
    }

    /// 原子写 manifest（.tmp → rename）。
    fn write_manifest(&self, m: &CaptureManifest) -> Result<()> {
        let dir = self.session_dir(&m.session_id).context("非法 session id")?;
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("manifest.json.tmp");
        let final_ = dir.join("manifest.json");
        std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
        std::fs::rename(&tmp, &final_)?;
        Ok(())
    }

    fn read_manifest(&self, id: &str) -> Result<CaptureManifest> {
        let dir = self.session_dir(id).context("非法 session id")?;
        let s = std::fs::read_to_string(dir.join("manifest.json"))?;
        Ok(serde_json::from_str(&s)?)
    }

    /// pcapng 绝对路径（供分块读；不外泄给客户端）。
    pub fn pcapng_path(&self, id: &str) -> Option<PathBuf> {
        self.session_dir(id).map(|d| d.join("capture.pcapng"))
    }

    /// 列出所有会话的 manifest（倒序按开始时间）。
    pub fn list(&self) -> Vec<CaptureManifest> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.root) {
            for e in rd.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    if is_valid_session_id(name) {
                        if let Ok(m) = self.read_manifest(name) {
                            out.push(m);
                        }
                    }
                }
            }
        }
        out.sort_by_key(|m| std::cmp::Reverse(m.started_ms));
        out
    }

    /// 删除一个会话目录（§6：运行态由调用方先挡 `capture_busy`）。
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.session_dir(id).context("非法 session id")?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// 配额清理（§9）：先删最旧的 done/failed，直到总字节 < 上限且会话数 < 上限。绝不删活跃会话
    /// （调用方保证 active 不在列表里，或列表来自磁盘完成态）。返回删除的会话数。
    pub fn enforce_quota(&self, active_id: Option<&str>) -> usize {
        let mut sessions = self.list();
        // 只考虑终态（done/failed/orphaned）且非活跃会话作为可回收对象。
        let mut removed = 0;
        // 计算总字节。
        let total: u64 = sessions.iter().map(|m| m.pcapng_bytes + m.etl_bytes).sum();
        let over_bytes = total > QUOTA_TOTAL_BYTES;
        let over_count = sessions.len() > QUOTA_MAX_SESSIONS;
        if !over_bytes && !over_count {
            return 0;
        }
        // 最旧优先。
        sessions.sort_by_key(|m| m.started_ms);
        let mut running_total = total;
        let mut running_count = sessions.len();
        for m in &sessions {
            if running_total <= QUOTA_TOTAL_BYTES && running_count <= QUOTA_MAX_SESSIONS {
                break;
            }
            if Some(m.session_id.as_str()) == active_id {
                continue; // 绝不删活跃
            }
            if self.delete(&m.session_id).is_ok() {
                running_total = running_total.saturating_sub(m.pcapng_bytes + m.etl_bytes);
                running_count -= 1;
                removed += 1;
            }
        }
        removed
    }
}

// ── CaptureManager：单会话状态机（§6/§7）─────────────────────────────────────

/// 活跃会话的内部句柄。
struct Active {
    id: String,
    component: String,
    etl: PathBuf,
    manifest: CaptureManifest,
    /// 本会话是否加过 pktmon 过滤器（stop 时需 best-effort 清除，§8 finally）。
    has_filters: bool,
}

pub struct CaptureManager {
    store: CaptureStore,
    /// 单会话槽：`Some` 表示 preparing/running/stopping/converting 中。
    active: Mutex<Option<Active>>,
}

impl CaptureManager {
    pub fn new(workspace: &Path) -> Self {
        Self {
            store: CaptureStore::new(workspace),
            active: Mutex::new(None),
        }
    }

    pub fn store(&self) -> &CaptureStore {
        &self.store
    }

    /// 当前是否有活跃会话 id。
    pub fn active_id(&self) -> Option<String> {
        self.active.lock().unwrap().as_ref().map(|a| a.id.clone())
    }

    /// 开始一次抓包（§8 顺序：探测占用 → 定位组件 → 加过滤器 → start → 确认 running）。
    /// `endpoints` 为定向抓包已解析的包面端点（`All` 传空）；返回 `running` 态或结构化错误码。
    pub fn start(
        &self,
        target: CaptureTarget,
        opts: CaptureOpts,
        endpoints: Vec<CaptureEndpoint>,
        rand16: [u8; 16],
        now_ms: u64,
        mihomo_version: String,
    ) -> Result<CaptureSession, ProtocolError> {
        opts.validate()
            .map_err(|e| ProtocolError::new(ErrorKind::Validation, e.to_string()))?;
        target
            .validate()
            .map_err(|e| ProtocolError::new(ErrorKind::Validation, e.to_string()))?;
        // 定向 target 必须已解析出端点（server 侧按 /connections 解析；空由 server 转 target_empty）。
        if target.is_directed() && endpoints.is_empty() {
            return Err(ProtocolError::new(
                ErrorKind::CaptureTargetEmpty,
                "定向目标未解析到任何包面端点（先产生流量，或改用全 TUN 短抓）",
            ));
        }
        // §5.2：去重 + 排序 → 过滤器；超 32 条拒绝（不静默退化全量）。
        let filters = if endpoints.is_empty() {
            Vec::new()
        } else {
            plan_filters(&endpoints).map_err(|e| match e {
                FilterPlanError::TooMany { deduped } => ProtocolError::new(
                    ErrorKind::CaptureFilterLimit,
                    format!("定向端点去重后 {deduped} 条，超 pktmon 32 条上限；请收窄目标"),
                ),
            })?
        };
        if !available() {
            return Err(ProtocolError::new(
                ErrorKind::CaptureUnsupported,
                "pktmon 不可用",
            ));
        }
        let mut slot = self.active.lock().unwrap();
        if let Some(a) = slot.as_ref() {
            return Err(ProtocolError::new(
                ErrorKind::CaptureConflict,
                format!("已有抓包会话进行中：{}", a.id),
            ));
        }
        // §8#1/#2：pktmon 已占用 / 已有过滤器 → 拒绝，绝不 stop/remove 他人资源。
        if status_running().map_err(internal)? {
            return Err(ProtocolError::new(
                ErrorKind::CaptureEngineBusy,
                "pktmon 已有 capture/trace 在运行（非本产品），拒绝接管",
            ));
        }
        if filters_present().map_err(internal)? {
            return Err(ProtocolError::new(
                ErrorKind::CaptureFiltersBusy,
                "pktmon 已有过滤器（非本产品），拒绝清除",
            ));
        }
        // §8#3：定位 TUN 组件（零个/多个候选均拒绝，不退化抓物理网卡）。
        let json = list_components_json().map_err(internal)?;
        let comp = find_tun_component(&json).map_err(|k| {
            ProtocolError::new(
                k,
                "未能唯一定位 mihomo TUN 组件（策略未 apply / TUN 未起栈？）",
            )
        })?;

        // 磁盘配额预清理 + 会话目录。
        self.store.ensure_root().map_err(internal)?;
        self.store.enforce_quota(None);
        let id = format_session_id(rand16);
        let dir = self
            .store
            .session_dir(&id)
            .ok_or_else(|| internal(anyhow::anyhow!("session id 生成非法")))?;
        std::fs::create_dir_all(&dir).map_err(|e| internal(e.into()))?;
        let etl = dir.join("capture.etl");

        // 定向：本会话取得所有权后加命名过滤器（开始前已确认过滤器为空，§8）。
        // 任一步失败 → best-effort 清掉本会话已加的过滤器，不留残留（§14）。
        for f in &filters {
            if let Err(e) = add_filter(f) {
                let _ = remove_filters();
                return Err(internal(e));
            }
        }

        // start capture。
        if let Err(e) = start_capture(&comp.id, &opts, &etl) {
            let _ = remove_filters();
            return Err(internal(e));
        }
        // 确认进入 running（§8#5：status 确认后才算 running）。
        if !status_running().map_err(internal)? {
            let _ = stop_capture();
            let _ = remove_filters();
            return Err(ProtocolError::new(
                ErrorKind::Internal,
                "pktmon start 后未进入 capture 运行态",
            ));
        }

        let mut known_limits = default_known_limits(&opts);
        if !filters.is_empty() {
            known_limits.push(
                "定向端点取自连接表 destinationIP（fake-ip 口径未经真机 golden probe 确认，§3.1）"
                    .to_string(),
            );
        }
        let has_filters = !filters.is_empty();
        let manifest = CaptureManifest {
            schema_version: CAPTURE_SCHEMA_VERSION,
            session_id: id.clone(),
            target: target.clone(),
            endpoints,
            opts,
            filters,
            tun_component: comp.id.clone(),
            mihomo_version,
            protocol: format!("{}.{}", Version::CURRENT.major, Version::CURRENT.minor),
            started_ms: now_ms,
            ended_ms: None,
            stop_reason: None,
            etl_bytes: 0,
            pcapng_bytes: 0,
            convert_ok: false,
            known_limits,
        };
        self.store.write_manifest(&manifest).map_err(internal)?;
        let session = manifest.to_session(CaptureState::Running, None);
        *slot = Some(Active {
            id,
            component: comp.id,
            etl,
            manifest,
            has_filters,
        });
        Ok(session)
    }

    /// 停止活跃会话并转换为 pcapng（§6：running → stopping → converting → done）。
    /// 幂等：无活跃会话时按 id 从磁盘返回当前态。
    pub fn stop(
        &self,
        id: &str,
        reason: CaptureStopReason,
        now_ms: u64,
    ) -> Result<CaptureSession, ProtocolError> {
        let mut slot = self.active.lock().unwrap();
        let is_active = slot.as_ref().map(|a| a.id == id).unwrap_or(false);
        if !is_active {
            // 幂等：非活跃 → 返回磁盘上的当前态。
            return self.get(id);
        }
        let active = slot.take().unwrap();
        // §8 finally：本会话若加过过滤器，无论成败都 best-effort 清除（开始前已确认过滤器为空，
        // 故此处 remove 只会清掉本会话自己加的）。
        if active.has_filters {
            let _ = remove_filters();
        }
        // stop → etl2pcap → 校验 → 写 manifest。失败标 failed 并保留可诊断错误。
        let result = (|| -> Result<CaptureManifest> {
            stop_capture()?;
            let dir = self.store.session_dir(&active.id).context("session dir")?;
            let pcapng = dir.join("capture.pcapng");
            let etl_bytes = std::fs::metadata(&active.etl).map(|m| m.len()).unwrap_or(0);
            etl2pcap(&active.etl, &pcapng, &active.component)?;
            let pcapng_bytes = std::fs::metadata(&pcapng)
                .map(|m| m.len())
                .context("转换后 pcapng 不存在")?;
            if pcapng_bytes < 8 {
                bail!("pcapng 过小（{pcapng_bytes} 字节），疑转换失败");
            }
            // 校验 pcapng SHB 魔数（0x0A0D0D0A）。
            let head = std::fs::read(&pcapng)?;
            if head.len() < 4 || head[0..4] != [0x0A, 0x0D, 0x0D, 0x0A] {
                bail!("pcapng 头魔数非法（非 0x0A0D0D0A）");
            }
            let mut m = active.manifest.clone();
            m.ended_ms = Some(now_ms);
            m.stop_reason = Some(reason);
            m.etl_bytes = etl_bytes;
            m.pcapng_bytes = pcapng_bytes;
            m.convert_ok = true;
            self.store.write_manifest(&m)?;
            // done 后删 ETL（§9：转换成功即删）。
            let _ = std::fs::remove_file(&active.etl);
            Ok(m)
        })();

        match result {
            Ok(m) => {
                self.store.enforce_quota(None);
                Ok(m.to_session(CaptureState::Done, None))
            }
            Err(e) => {
                // 失败：写 failed manifest，清 ETL/临时 pcapng。
                let mut m = active.manifest.clone();
                m.ended_ms = Some(now_ms);
                m.stop_reason = Some(CaptureStopReason::Error);
                m.convert_ok = false;
                let _ = self.store.write_manifest(&m);
                let err = ProtocolError::new(ErrorKind::CaptureConvertFailed, e.to_string());
                Ok(m.to_session(CaptureState::Failed, Some(err)))
            }
        }
    }

    /// 取会话当前态（活跃优先 running，否则读磁盘 manifest → done/failed）。
    pub fn get(&self, id: &str) -> Result<CaptureSession, ProtocolError> {
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            if a.id == id {
                return Ok(a.manifest.to_session(CaptureState::Running, None));
            }
        }
        match self.store.read_manifest(id) {
            Ok(m) => {
                let state = if m.convert_ok {
                    CaptureState::Done
                } else {
                    CaptureState::Failed
                };
                Ok(m.to_session(state, None))
            }
            Err(_) => Err(ProtocolError::new(ErrorKind::CaptureNotFound, "会话不存在")),
        }
    }

    /// 列出所有会话 DTO。
    pub fn list(&self) -> Vec<CaptureSession> {
        let active = self.active.lock().unwrap();
        let active_id = active.as_ref().map(|a| a.id.clone());
        let mut out: Vec<CaptureSession> = self
            .store
            .list()
            .into_iter()
            .map(|m| {
                let state = if Some(&m.session_id) == active_id.as_ref() {
                    CaptureState::Running
                } else if m.convert_ok {
                    CaptureState::Done
                } else {
                    CaptureState::Failed
                };
                m.to_session(state, None)
            })
            .collect();
        out.sort_by_key(|m| std::cmp::Reverse(m.started_ms));
        out
    }

    /// 删除会话（运行态返回 `capture_busy`，不隐式 stop，§6）。
    pub fn delete(&self, id: &str) -> Result<(), ProtocolError> {
        if self.active_id().as_deref() == Some(id) {
            return Err(ProtocolError::new(
                ErrorKind::CaptureBusy,
                "会话运行中，请先停止再删除",
            ));
        }
        self.store
            .delete(id)
            .map_err(|e| ProtocolError::new(ErrorKind::Internal, e.to_string()))
    }
}

fn internal(e: anyhow::Error) -> ProtocolError {
    ProtocolError::new(ErrorKind::Internal, e.to_string())
}

/// 每次抓包都成立的已知限制（§5.2/§12 隐私提示）。
fn default_known_limits(opts: &CaptureOpts) -> Vec<String> {
    let mut v = vec![
        "IP/端口过滤不区分源和目的".to_string(),
        "LAN（route-exclude）与未入 TUN 的 IPv6 不在本次抓包".to_string(),
        "HTTPS/QUIC 内容仍是密文（L3 不解密）".to_string(),
    ];
    if opts.is_full_packet() {
        v.push("完整包模式：可能含 Cookie/Authorization/明文 HTTP/DNS 查询等敏感数据".to_string());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    // 真机 `pktmon list --json` 的最小结构样本（含一个 TUN 分组 + 一个物理网卡分组）。
    const SAMPLE_JSON: &str = r#"[
      {"Group":"Intel(R) Wireless-AC 9260 160MHz","Components":[
        {"Name":"Intel(R) Wireless-AC 9260 160MHz","DriverName":"Netwtw08.sys","Id":4},
        {"Name":"WFP Native Filter","DriverName":"wfplwfs.sys","Id":17}
      ]},
      {"Group":"Meta","Components":[
        {"Name":"Meta","DriverName":"wintun.sys","Id":42},
        {"Name":"WFP Native Filter","DriverName":"wfplwfs.sys","Id":99}
      ]}
    ]"#;

    #[test]
    fn find_tun_component_picks_meta_miniport() {
        let c = find_tun_component(SAMPLE_JSON).expect("应定位 Meta miniport");
        assert_eq!(c.id, "42");
        assert_eq!(c.group, "Meta");
    }

    #[test]
    fn find_tun_component_rejects_when_absent() {
        let no_tun =
            r#"[{"Group":"Intel Ethernet","Components":[{"Name":"Intel Ethernet","Id":3}]}]"#;
        assert_eq!(
            find_tun_component(no_tun),
            Err(ErrorKind::CaptureComponentNotFound)
        );
    }

    #[test]
    fn find_tun_component_rejects_when_multiple() {
        let two = r#"[
          {"Group":"Meta","Components":[{"Name":"Meta","Id":1}]},
          {"Group":"Mihomo","Components":[{"Name":"Mihomo","Id":2}]}
        ]"#;
        assert_eq!(
            find_tun_component(two),
            Err(ErrorKind::CaptureComponentNotFound)
        );
    }

    #[test]
    fn id_string_variant_accepted() {
        let s = r#"[{"Group":"Meta","Components":[{"Name":"Meta","Id":"7"}]}]"#;
        assert_eq!(find_tun_component(s).unwrap().id, "7");
    }

    fn conn(host: &str, ip: &str, port: &str, proc: &str, path: &str, net: &str) -> Connection {
        Connection {
            id: "c1".into(),
            chains: vec![],
            outbound: "DIRECT".into(),
            host: host.into(),
            destination_ip: ip.into(),
            destination_port: port.into(),
            process: proc.into(),
            process_path: path.into(),
            rule: "".into(),
            network: net.into(),
        }
    }

    #[test]
    fn resolve_process_by_path_then_name() {
        let conns = vec![
            conn(
                "a.com",
                "1.1.1.1",
                "443",
                "chrome.exe",
                r"C:\chrome.exe",
                "tcp",
            ),
            conn("b.com", "2.2.2.2", "80", "edge.exe", r"C:\edge.exe", "tcp"),
        ];
        // 按完整路径命中
        let t = CaptureTarget::Process(ProcessRef::ProcessPath(r"C:\chrome.exe".into()));
        let eps = resolve_endpoints(&t, &conns).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].capture_ip, "1.1.1.1");
        assert_eq!(eps[0].port, 443);
        // 按进程名兜底
        let t2 = CaptureTarget::Process(ProcessRef::ProcessName("edge.exe".into()));
        assert_eq!(
            resolve_endpoints(&t2, &conns).unwrap()[0].capture_ip,
            "2.2.2.2"
        );
    }

    #[test]
    fn resolve_domain_suffix_and_empty() {
        let conns = vec![
            conn("api.example.com", "3.3.3.3", "443", "x", "x", "tcp"),
            conn("other.net", "4.4.4.4", "443", "x", "x", "tcp"),
        ];
        let t = CaptureTarget::Domain("example.com".into());
        let eps = resolve_endpoints(&t, &conns).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].capture_ip, "3.3.3.3");
        // 无命中 → target_empty
        let t2 = CaptureTarget::Domain("nope.com".into());
        assert_eq!(
            resolve_endpoints(&t2, &conns),
            Err(ErrorKind::CaptureTargetEmpty)
        );
    }

    #[test]
    fn resolve_ip_and_skips_unparseable() {
        let conns = vec![
            conn("a", "198.18.0.9", "443", "x", "x", "tcp"),
            conn("a", "198.18.0.9", "bad-port", "x", "x", "tcp"), // 端口不可解析 → 跳过
            conn("a", "198.18.0.9", "53", "x", "x", "udp"),
        ];
        let t = CaptureTarget::Ip("198.18.0.9".into());
        let eps = resolve_endpoints(&t, &conns).unwrap();
        assert_eq!(eps.len(), 2, "跳过端口不可解析的那条");
        assert_eq!(eps[0].source, EndpointSource::UserInput);
    }

    #[test]
    fn store_paths_reject_bad_session_id() {
        let dir = std::env::temp_dir().join(format!("np-cap-test-{}", std::process::id()));
        let store = CaptureStore::new(&dir);
        assert!(store.session_dir("cap-../evil").is_none());
        assert!(store.session_dir(&format_session_id([3; 16])).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_manifest_roundtrip_and_list() {
        let dir = std::env::temp_dir().join(format!("np-cap-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = CaptureStore::new(&dir);
        store.ensure_root().unwrap();
        let id = format_session_id([9; 16]);
        let m = CaptureManifest {
            schema_version: CAPTURE_SCHEMA_VERSION,
            session_id: id.clone(),
            target: CaptureTarget::All,
            endpoints: vec![],
            opts: CaptureOpts::default(),
            filters: vec![],
            tun_component: "42".into(),
            mihomo_version: "1.18".into(),
            protocol: "1.5".into(),
            started_ms: 111,
            ended_ms: Some(222),
            stop_reason: Some(CaptureStopReason::User),
            etl_bytes: 100,
            pcapng_bytes: 200,
            convert_ok: true,
            known_limits: vec![],
        };
        store.write_manifest(&m).unwrap();
        let got = store.read_manifest(&id).unwrap();
        assert_eq!(got.tun_component, "42");
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_id, id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quota_removes_oldest_over_count() {
        let dir = std::env::temp_dir().join(format!("np-cap-q-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = CaptureStore::new(&dir);
        store.ensure_root().unwrap();
        // 写 12 个完成会话（超 10 上限），started_ms 递增。
        for i in 0..12u8 {
            let m = CaptureManifest {
                schema_version: CAPTURE_SCHEMA_VERSION,
                session_id: format_session_id([i; 16]),
                target: CaptureTarget::All,
                endpoints: vec![],
                opts: CaptureOpts::default(),
                filters: vec![],
                tun_component: "1".into(),
                mihomo_version: "x".into(),
                protocol: "1.5".into(),
                started_ms: i as u64,
                ended_ms: Some(i as u64 + 1),
                stop_reason: Some(CaptureStopReason::User),
                etl_bytes: 0,
                pcapng_bytes: 1000,
                convert_ok: true,
                known_limits: vec![],
            };
            store.write_manifest(&m).unwrap();
        }
        assert_eq!(store.list().len(), 12);
        let removed = store.enforce_quota(None);
        assert!(removed >= 2, "应删到 ≤10 个");
        assert!(store.list().len() <= QUOTA_MAX_SESSIONS);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
