//! L4 解密会话管理（抓包设计 §17.4/§17.5/§17.8）：CA 生命周期 + 解密会话状态机 + 存储。
//!
//! 把 L4 从诊断子命令变成**正式协议能力**：`DecryptCaStatus/Create/ConfirmInstalled/Remove` +
//! `DecryptStart/Stop/Get/List/Delete/Read`。引擎用自研 `net-policy-mitm`（显式代理 + 上游按域名链
//! mihomo，方案 B 已真机 E2E，ADR §6.4）。
//!
//! CA 公钥由 GUI 装入调用者的 `CurrentUser\Root`，agent 用真实管道客户端 SID 绑定 owner；私钥只以
//! DPAPI machine-scope 密文落盘。会话监听先完成 bind，再由 server 事务化 reload mihomo 导流规则。

use crate::decrypt_sink::DecryptSink;
use anyhow::{Context, Result};
use net_policy_core::decrypt::{
    format_session_id, is_valid_session_id, CaState, CaStatus, DecryptArtifact, DecryptOpts,
    DecryptSession, DecryptState, DecryptTarget,
};
use net_policy_core::protocol::{ErrorKind, ProtocolError};
use net_policy_mitm::shutdown::ShutdownToken;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 单会话 loopback MITM 监听端口（固定；单会话互斥）。
const MITM_PORT: u16 = 18081;
/// 上游 mihomo 混合端口（按域名链，解 fake-ip）。
const UPSTREAM_URL: &str = "http://127.0.0.1:7890";
/// 明文产物总字节配额（§17.6）。
const QUOTA_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// 完成会话数上限（§17.6）。
const QUOTA_MAX_SESSIONS: usize = 5;

fn internal(e: anyhow::Error) -> ProtocolError {
    ProtocolError::new(ErrorKind::Internal, e.to_string())
}

/// SHA-256 十六进制（大写，与 Windows certutil thumbprint 风格一致）。
fn sha256_hex_upper(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(data);
    d.iter().map(|b| format!("{b:02X}")).collect()
}

/// 从 CERTIFICATE PEM 提取第一个证书的 DER（base64 解 body）。指纹算的是**存盘证书本体**，稳定。
fn pem_cert_to_der(pem: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let start = pem.find(begin)? + begin.len();
    let stop = pem[start..].find(end)? + start;
    let b64: String = pem[start..stop].split_whitespace().collect();
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

// ── CaStore：专用调试 CA 文件 + 安装确认状态（§17.4）─────────────────────────

/// 记录 GUI 在用户上下文安装公钥后回传的信息（`ca_confirm` 写；§17.4/§17.8）。
#[derive(serde::Serialize, serde::Deserialize)]
struct InstallRecord {
    thumbprint: String,
    owner_sid: String,
    store_scope: String,
    #[serde(default)]
    confirmed: bool,
}

struct CaStore {
    dir: PathBuf, // <ws>/mitm
}

impl CaStore {
    fn new(workspace: &Path) -> Self {
        Self {
            dir: workspace.join("mitm"),
        }
    }
    fn ca_crt(&self) -> PathBuf {
        self.dir.join("ca.crt")
    }
    /// DPAPI machine-scope 密文私钥（§17.4）：`<ws>/mitm/private/ca.key.dpapi`。**磁盘上从不明文**。
    fn ca_key_dpapi(&self) -> PathBuf {
        self.dir.join("private").join("ca.key.dpapi")
    }
    /// 旧版明文私钥路径（迁移用；见 [`Self::migrate_plaintext_key`]）。
    fn legacy_ca_key(&self) -> PathBuf {
        self.dir.join("ca.key")
    }
    fn install_json(&self) -> PathBuf {
        self.dir.join("ca-install.json")
    }

    /// 计算 CA 证书 DER 的 SHA-256 指纹（须 CA 已生成）。**从存盘的 ca.crt PEM 直接解 DER** ——
    /// 不能走 `CertAuthority::load`（它会重签证书，serial/签名带随机，DER 每次不同 → 指纹不稳定）。
    fn thumbprint(&self) -> Result<String> {
        let pem = std::fs::read_to_string(self.ca_crt()).context("read ca.crt")?;
        let der = pem_cert_to_der(&pem).context("parse ca.crt PEM")?;
        Ok(sha256_hex_upper(&der))
    }

    fn read_install(&self) -> Option<InstallRecord> {
        std::fs::read_to_string(self.install_json())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// 当前 CA 状态（§17.4）。
    fn status(&self) -> CaStatus {
        let crt = self.ca_crt();
        if !crt.exists() {
            return CaStatus {
                state: CaState::Absent,
                thumbprint: None,
                subject: None,
                not_after_ms: None,
                owner_sid: None,
                store_scope: None,
            };
        }
        if !self.ca_key_dpapi().exists() && !self.legacy_ca_key().exists() {
            // 证书在但私钥缺 → broken（§17.4：引导移除重建）。
            return CaStatus {
                state: CaState::Broken,
                thumbprint: None,
                subject: Some("net-policy-mitm CA".into()),
                not_after_ms: None,
                owner_sid: None,
                store_scope: None,
            };
        }
        let thumbprint = self.thumbprint().ok();
        let install = self.read_install();
        CaStatus {
            state: CaState::Installed,
            thumbprint,
            subject: Some("net-policy-mitm CA".into()),
            not_after_ms: None, // 有效期解析为后续增量
            owner_sid: install
                .as_ref()
                .filter(|record| record.confirmed)
                .map(|record| record.owner_sid.clone()),
            store_scope: install
                .as_ref()
                .filter(|record| record.confirmed)
                .map(|record| record.store_scope.clone()),
        }
    }

    /// 生成新 CA 并落盘（§17.4：设备唯一，不内置/不跨设备复用）。**不装信任库**。
    /// 公钥证书 `ca.crt` 明文落盘（非秘密）；私钥经 DPAPI machine-scope 加密后落
    /// `private/ca.key.dpapi`，**磁盘上从不出现明文私钥**。
    fn create(&self, owner_sid: &str) -> Result<CaStatus> {
        std::fs::create_dir_all(&self.dir)?;
        let priv_dir = self.dir.join("private");
        std::fs::create_dir_all(&priv_dir)?;
        // private/ 收紧到 SYSTEM + Administrators（best-effort；workspace 根已 ACL 收紧）。
        crate::security::lock_down_dir_system_only(&priv_dir);

        let ca = net_policy_mitm::cert::ca::CertAuthority::generate().context("generate CA")?;
        // 公钥证书（非秘密）。
        std::fs::write(self.ca_crt(), &ca.ca_cert_pem).context("write ca.crt")?;
        // 私钥 → DPAPI 密文。
        let enc = crate::dpapi::protect_machine(ca.ca_key_pem().as_bytes())
            .context("DPAPI 加密 CA 私钥")?;
        std::fs::write(self.ca_key_dpapi(), &enc).context("write ca.key.dpapi")?;
        // 清掉可能存在的旧明文私钥（迁移/防残留）。
        let _ = std::fs::remove_file(self.legacy_ca_key());
        let record = InstallRecord {
            thumbprint: self.thumbprint()?,
            owner_sid: owner_sid.to_string(),
            store_scope: "current_user".to_string(),
            confirmed: false,
        };
        std::fs::write(self.install_json(), serde_json::to_vec_pretty(&record)?)?;
        Ok(self.status())
    }

    /// 装配用于起会话的 `CertAuthority`：读 `ca.crt` + DPAPI 解密私钥，**在内存构造**（从不写明文）。
    /// 若只剩旧版明文私钥则先迁移到 DPAPI 密文再删明文。
    fn load_authority(&self) -> Result<net_policy_mitm::cert::ca::CertAuthority, ProtocolError> {
        self.migrate_plaintext_key();
        let cert_pem = std::fs::read_to_string(self.ca_crt())
            .map_err(|_| ProtocolError::new(ErrorKind::DecryptCaMissing, "CA 证书缺失"))?;
        let enc = std::fs::read(self.ca_key_dpapi())
            .map_err(|_| ProtocolError::new(ErrorKind::DecryptCaBroken, "CA 私钥缺失"))?;
        let key_pem = crate::dpapi::unprotect(&enc).map_err(|e| {
            ProtocolError::new(ErrorKind::DecryptCaBroken, format!("解密私钥失败：{e}"))
        })?;
        let key_pem = String::from_utf8(key_pem)
            .map_err(|_| ProtocolError::new(ErrorKind::DecryptCaBroken, "私钥非 UTF-8"))?;
        net_policy_mitm::cert::ca::CertAuthority::from_pem(&cert_pem, &key_pem)
            .map_err(|e| ProtocolError::new(ErrorKind::DecryptCaBroken, e.to_string()))
    }

    /// 一次性迁移：若存在旧明文 `ca.key` 且无 DPAPI 密文，则加密后落 `private/`，删明文。
    fn migrate_plaintext_key(&self) {
        let legacy = self.legacy_ca_key();
        if !legacy.exists() || self.ca_key_dpapi().exists() {
            return;
        }
        let Ok(pem) = std::fs::read(&legacy) else {
            return;
        };
        let Ok(enc) = crate::dpapi::protect_machine(&pem) else {
            return;
        };
        if std::fs::create_dir_all(self.dir.join("private")).is_ok() {
            crate::security::lock_down_dir_system_only(&self.dir.join("private"));
            if std::fs::write(self.ca_key_dpapi(), &enc).is_ok() {
                let _ = std::fs::remove_file(&legacy);
                log::info!("已把旧明文 CA 私钥迁移为 DPAPI 密文");
            }
        }
    }

    /// GUI 安装公钥到 `CurrentUser\Root` 后复核：回传指纹须与本地 CA 实际指纹一致，否则拒（§17.8）。
    fn confirm(&self, thumbprint: &str, owner_sid: &str) -> Result<CaStatus, ProtocolError> {
        if !self.ca_crt().exists() {
            return Err(ProtocolError::new(ErrorKind::DecryptCaMissing, "CA 未创建"));
        }
        let actual = self
            .thumbprint()
            .map_err(|_| ProtocolError::new(ErrorKind::DecryptCaBroken, "CA 私钥缺失/损坏"))?;
        if !actual.eq_ignore_ascii_case(thumbprint) {
            return Err(ProtocolError::new(
                ErrorKind::DecryptCaBroken,
                format!("回传指纹与本地 CA 不符（期望 {actual}）"),
            ));
        }
        if let Some(record) = self.read_install() {
            if !record.owner_sid.eq_ignore_ascii_case(owner_sid) {
                return Err(ProtocolError::new(
                    ErrorKind::DecryptConflict,
                    "CA 属于另一名调用者",
                ));
            }
        }
        let rec = InstallRecord {
            thumbprint: actual,
            owner_sid: owner_sid.to_string(),
            store_scope: "current_user".to_string(),
            confirmed: true,
        };
        std::fs::write(
            self.install_json(),
            serde_json::to_vec_pretty(&rec).map_err(|e| internal(e.into()))?,
        )
        .map_err(|e| internal(e.into()))?;
        Ok(self.status())
    }

    /// 删除本产品 CA（文件 + 安装记录）。信任库里的证书由 GUI 按 thumbprint 精确删（§17.4，真机）。
    fn remove(&self) -> Result<CaStatus> {
        for path in [
            self.ca_crt(),
            self.ca_key_dpapi(),
            self.legacy_ca_key(),
            self.install_json(),
        ] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("删除 {} 失败", path.display())),
            }
        }
        Ok(self.status())
    }

    /// 是否可启动解密：CA 已生成**且**已确认装进信任库（否则客户端不信任伪造叶子证书）。
    fn ready_for_session(&self) -> Result<(), ProtocolError> {
        let st = self.status();
        match st.state {
            CaState::Absent => Err(ProtocolError::new(
                ErrorKind::DecryptCaMissing,
                "请先创建并安装专用调试 CA",
            )),
            CaState::Broken => Err(ProtocolError::new(
                ErrorKind::DecryptCaBroken,
                "CA 私钥缺失/指纹不符，请移除后重建",
            )),
            CaState::Installed if !self.read_install().is_some_and(|record| record.confirmed) => {
                Err(ProtocolError::new(
                    ErrorKind::DecryptCaMissing,
                    "CA 已创建但未确认安装到信任库（请在 GUI 安装后再试）",
                ))
            }
            CaState::Installed => Ok(()),
        }
    }
}

// ── DecryptStore：会话目录 / manifest(=DecryptSession) / http.jsonl / 配额 / 分块读 ──────

struct DecryptStore {
    root: PathBuf, // <ws>/decrypt
}

impl DecryptStore {
    fn new(workspace: &Path) -> Self {
        Self {
            root: workspace.join("decrypt"),
        }
    }
    fn session_dir(&self, id: &str) -> Option<PathBuf> {
        is_valid_session_id(id).then(|| self.root.join(id))
    }
    fn http_jsonl(&self, id: &str) -> Option<PathBuf> {
        self.session_dir(id).map(|d| d.join("http.jsonl"))
    }
    fn ensure_root(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("创建 decrypt 根失败：{}", self.root.display()))
    }
    /// 原子写 manifest（= DecryptSession DTO）。
    fn write(&self, s: &DecryptSession) -> Result<()> {
        let dir = self.session_dir(&s.id).context("非法 session id")?;
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("manifest.json.tmp");
        let fin = dir.join("manifest.json");
        std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
        std::fs::rename(&tmp, &fin)?;
        Ok(())
    }
    fn read(&self, id: &str) -> Result<DecryptSession> {
        let dir = self.session_dir(id).context("非法 session id")?;
        Ok(serde_json::from_str(&std::fs::read_to_string(
            dir.join("manifest.json"),
        )?)?)
    }
    fn list(&self) -> Vec<DecryptSession> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.root) {
            for e in rd.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    if is_valid_session_id(name) {
                        if let Ok(s) = self.read(name) {
                            out.push(s);
                        }
                    }
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.started_ms));
        out
    }
    fn delete(&self, id: &str) -> Result<()> {
        if let Some(dir) = self.session_dir(id) {
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
        }
        Ok(())
    }
    /// artifact 绝对路径（§17.8：客户端只传枚举，不给文件名）。
    fn artifact_path(&self, id: &str, artifact: DecryptArtifact) -> Option<PathBuf> {
        self.session_dir(id).map(|d| d.join(artifact.file_name()))
    }
    /// 配额清理（§17.6）：删最旧的非活跃终态会话直到 ≤ 上限。返回删除数。
    fn enforce_quota(&self, active_id: Option<&str>) -> usize {
        let mut sessions = self.list();
        let total: u64 = sessions
            .iter()
            .map(|s| {
                self.http_jsonl(&s.id)
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len())
                    .unwrap_or(0)
            })
            .sum();
        if total <= QUOTA_TOTAL_BYTES && sessions.len() <= QUOTA_MAX_SESSIONS {
            return 0;
        }
        sessions.sort_by_key(|s| s.started_ms); // 最旧优先
        let mut count = sessions.len();
        let mut running_total = total;
        let mut removed = 0;
        for s in &sessions {
            if count <= QUOTA_MAX_SESSIONS && running_total <= QUOTA_TOTAL_BYTES {
                break;
            }
            if Some(s.id.as_str()) == active_id {
                continue;
            }
            let bytes = self
                .http_jsonl(&s.id)
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if self.delete(&s.id).is_ok() {
                count -= 1;
                running_total = running_total.saturating_sub(bytes);
                removed += 1;
            }
        }
        removed
    }
}

// ── DecryptManager：单会话状态机（§17.5）────────────────────────────────────

struct Active {
    id: String,
    session: DecryptSession,
    sink: Arc<DecryptSink>,
    shutdown: ShutdownToken,
    divert_active: bool,
    proxy_password: String,
}

pub struct DecryptManager {
    ca: CaStore,
    store: DecryptStore,
    active: Mutex<Option<Active>>,
}

impl DecryptManager {
    pub fn new(workspace: &Path) -> Self {
        Self {
            ca: CaStore::new(workspace),
            store: DecryptStore::new(workspace),
            active: Mutex::new(None),
        }
    }

    pub fn active_id(&self) -> Option<String> {
        self.active.lock().unwrap().as_ref().map(|a| a.id.clone())
    }

    /// 当前活跃会话对应的 mihomo 自动导流描述（§17.3 方案 B）。无会话 → inactive（default）。
    /// agent 在 apply/reload 写 mihomo 配置时取用：有会话即把目标进程 TCP HTTP(S) 导到 loopback MITM。
    pub fn active_divert(&self) -> net_policy_core::config::DecryptDivert {
        use net_policy_core::config::{DecryptDivert, ProcessRef};
        let slot = self.active.lock().unwrap();
        let Some(a) = slot.as_ref().filter(|a| a.divert_active) else {
            return DecryptDivert::default();
        };
        // 目标进程按完整路径匹配（PROCESS-PATH，精确到实例路径；同名多实例是 MVP 已知限制）。
        let targets = vec![ProcessRef::ProcessPath(
            a.session.target.process.path.clone(),
        )];
        let domains = a
            .session
            .target
            .normalized_domains()
            .unwrap_or_else(|_| a.session.target.domains.clone());
        DecryptDivert {
            active: true,
            targets,
            domains,
            mitm_port: MITM_PORT,
            force_tcp_for_quic: a.session.opts.force_tcp_for_quic,
            proxy_username: "net-policy".to_string(),
            proxy_password: a.proxy_password.clone(),
        }
    }

    // ── CA ──
    pub fn ca_status(&self) -> CaStatus {
        self.ca.status()
    }
    pub fn ca_owner_sid(&self) -> Option<String> {
        self.ca.read_install().map(|record| record.owner_sid)
    }
    pub fn ca_create(&self, owner_sid: &str) -> Result<CaStatus, ProtocolError> {
        if self.active_id().is_some() {
            return Err(ProtocolError::new(
                ErrorKind::DecryptConflict,
                "有解密会话进行中，先停止再改 CA",
            ));
        }
        self.ca.create(owner_sid).map_err(internal)
    }
    pub fn ca_confirm(&self, thumbprint: &str, owner_sid: &str) -> Result<CaStatus, ProtocolError> {
        self.ca.confirm(thumbprint, owner_sid)
    }
    /// 导出 CA 公钥证书 PEM（供 GUI 装信任库；非秘密）。CA 未创建 → `decrypt_ca_missing`。
    pub fn ca_export_public(&self) -> Result<String, ProtocolError> {
        std::fs::read_to_string(self.ca.ca_crt())
            .map_err(|_| ProtocolError::new(ErrorKind::DecryptCaMissing, "CA 未创建"))
    }
    pub fn ca_remove(&self) -> Result<CaStatus, ProtocolError> {
        if self.active_id().is_some() {
            return Err(ProtocolError::new(
                ErrorKind::DecryptConflict,
                "有解密会话进行中，先停止再删 CA",
            ));
        }
        self.ca.remove().map_err(internal)
    }

    // ── 会话 ──

    /// 开始解密会话（§17.5：checking_ca → preparing → decrypting）。起 loopback MITM 监听 +
    /// DecryptSink。mihomo 导流由 server 在本方法成功后事务化 reload；reload 失败会回滚本会话。
    /// 必须在 tokio 运行时上下文调用（内部 `tokio::spawn` run_proxy）。
    pub fn start(
        &self,
        mut target: DecryptTarget,
        opts: DecryptOpts,
        rand16: [u8; 16],
        now_ms: u64,
    ) -> Result<DecryptSession, ProtocolError> {
        // checking_ca
        self.ca.ready_for_session()?;
        opts.validate()
            .map_err(|e| ProtocolError::new(ErrorKind::Validation, e.to_string()))?;
        let domains = target
            .normalized_domains()
            .map_err(|e| ProtocolError::new(ErrorKind::Validation, e.to_string()))?;
        target.process = crate::security::resolve_process_instance(&target.process)
            .map_err(|e| ProtocolError::new(ErrorKind::DecryptTargetStale, e.to_string()))?;

        let mut slot = self.active.lock().unwrap();
        if let Some(a) = slot.as_ref() {
            return Err(ProtocolError::new(
                ErrorKind::DecryptConflict,
                format!("已有解密会话进行中：{}", a.id),
            ));
        }

        // preparing：会话目录 + sink + 引擎装配
        net_policy_mitm::install_crypto_provider();
        self.store.ensure_root().map_err(internal)?;
        self.store.enforce_quota(None);
        let id = format_session_id(rand16);
        let dir = self
            .store
            .session_dir(&id)
            .ok_or_else(|| internal(anyhow::anyhow!("session id 非法")))?;
        std::fs::create_dir_all(&dir).map_err(|e| internal(e.into()))?;
        let http_jsonl = dir.join("http.jsonl");
        let sink = Arc::new(DecryptSink::create(&http_jsonl, opts).map_err(internal)?);

        // CA → CertCache（DPAPI 解密私钥，内存装配，从不写明文）。
        let ca_auth = self.ca.load_authority()?;
        let cert_cache = Arc::new(net_policy_mitm::cert::site::CertCache::new(ca_auth));

        let allow = domains.clone();
        let should_intercept: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(move |h: &str| {
            let h = h.to_ascii_lowercase();
            allow
                .iter()
                .any(|d| h == *d || h.ends_with(&format!(".{d}")))
        });
        let proxy_password = hex::encode(rand::random::<[u8; 32]>());
        use base64::Engine;
        let expected_proxy_authorization = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("net-policy:{proxy_password}"))
        );
        let runtime = net_policy_mitm::proxy::ProxyRuntime {
            cert_cache,
            sink: sink.clone(),
            should_intercept,
            expected_proxy_authorization: Some(expected_proxy_authorization),
        };
        let upstream =
            Arc::new(net_policy_mitm::upstream::Upstream::parse(UPSTREAM_URL).map_err(internal)?);
        let shutdown = ShutdownToken::new();

        // 先同步 bind，成功后才允许会话进入 decrypting；避免后台 bind 失败却返回成功。
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", MITM_PORT))
            .map_err(|e| internal(e.into()))?;
        std_listener
            .set_nonblocking(true)
            .map_err(|e| internal(e.into()))?;
        let listener =
            tokio::net::TcpListener::from_std(std_listener).map_err(|e| internal(e.into()))?;
        let shutdown_child = shutdown.child_token();
        tokio::spawn(async move {
            if let Err(e) =
                net_policy_mitm::proxy::run_proxy_on(listener, upstream, runtime, shutdown_child)
                    .await
            {
                log::error!("L4 MITM run_proxy 退出：{e:#}");
            }
        });

        let session = DecryptSession {
            id: id.clone(),
            state: DecryptState::Decrypting,
            target,
            opts,
            started_ms: now_ms,
            ended_ms: None,
            per_domain: Default::default(),
            error: None,
        };
        if let Err(error) = self.store.write(&session) {
            shutdown.cancel();
            return Err(internal(error));
        }
        *slot = Some(Active {
            id,
            session: session.clone(),
            sink,
            shutdown,
            divert_active: true,
            proxy_password,
        });
        Ok(session)
    }

    /// 停止会话（§17.5：stopping → finalizing → done）：撤监听 + 从 sink 收每域名计数 + 落 manifest。
    /// 幂等：非活跃 → 读磁盘当前态。
    pub fn stop(&self, id: &str, now_ms: u64) -> Result<DecryptSession, ProtocolError> {
        let mut slot = self.active.lock().unwrap();
        let is_active = slot.as_ref().map(|a| a.id == id).unwrap_or(false);
        if !is_active {
            return self.get(id);
        }
        let active = slot.take().unwrap();
        self.finalize_active(active, now_ms)
    }

    /// 事务化停止第一阶段：先让 config-gen 看不到 divert，但保持代理监听；reload 成功后再 finish。
    pub fn begin_stop(&self, id: &str) -> Result<(), ProtocolError> {
        let mut slot = self.active.lock().unwrap();
        let active = slot
            .as_mut()
            .filter(|active| active.id == id)
            .ok_or_else(|| {
                ProtocolError::new(ErrorKind::DecryptTargetStale, "会话不存在或非活跃")
            })?;
        active.divert_active = false;
        Ok(())
    }

    /// reload 失败时恢复内存导流描述；当前 mihomo 配置仍指向存活代理，流量不中断。
    pub fn abort_stop(&self, id: &str) {
        if let Some(active) = self.active.lock().unwrap().as_mut().filter(|a| a.id == id) {
            active.divert_active = true;
        }
    }

    pub fn finish_stop(&self, id: &str, now_ms: u64) -> Result<DecryptSession, ProtocolError> {
        let mut slot = self.active.lock().unwrap();
        if slot.as_ref().map(|active| active.id.as_str()) != Some(id) {
            return Err(ProtocolError::new(
                ErrorKind::DecryptTargetStale,
                "会话不存在或非活跃",
            ));
        }
        let active = slot.take().ok_or_else(|| {
            ProtocolError::new(ErrorKind::DecryptTargetStale, "会话不存在或非活跃")
        })?;
        self.finalize_active(active, now_ms)
    }

    fn finalize_active(
        &self,
        active: Active,
        now_ms: u64,
    ) -> Result<DecryptSession, ProtocolError> {
        active.shutdown.cancel();
        let mut s = active.session;
        s.state = DecryptState::Done;
        s.ended_ms = Some(now_ms);
        s.per_domain = active.sink.per_domain();
        self.store.write(&s).map_err(internal)?;
        self.store.enforce_quota(None);
        Ok(s)
    }

    pub fn get(&self, id: &str) -> Result<DecryptSession, ProtocolError> {
        if let Some(a) = self.active.lock().unwrap().as_ref() {
            if a.id == id {
                let mut s = a.session.clone();
                s.per_domain = a.sink.per_domain();
                return Ok(s);
            }
        }
        self.store
            .read(id)
            .map_err(|_| ProtocolError::new(ErrorKind::DecryptTargetStale, "会话不存在"))
    }

    pub fn list(&self) -> Vec<DecryptSession> {
        let active = self.active.lock().unwrap();
        let active_id = active.as_ref().map(|a| a.id.clone());
        let mut out = self.store.list();
        // 活跃会话的 per_domain 用内存最新值覆盖。
        if let (Some(a), Some(aid)) = (active.as_ref(), active_id) {
            if let Some(s) = out.iter_mut().find(|s| s.id == aid) {
                s.state = DecryptState::Decrypting;
                s.per_domain = a.sink.per_domain();
            }
        }
        out
    }

    pub fn delete(&self, id: &str) -> Result<(), ProtocolError> {
        if self.active_id().as_deref() == Some(id) {
            return Err(ProtocolError::new(
                ErrorKind::DecryptConflict,
                "会话进行中，请先停止再删除",
            ));
        }
        self.store.delete(id).map_err(internal)
    }

    /// 分块读产物（§17.8：仅 done 会话；len ≤ 512 KiB）。返回 `(offset, 原始字节, eof)`。
    pub fn read(
        &self,
        id: &str,
        artifact: DecryptArtifact,
        offset: u64,
        len: u32,
    ) -> Result<(u64, Vec<u8>, bool), ProtocolError> {
        use std::io::{Read, Seek, SeekFrom};
        let session = self.get(id)?;
        if session.state != DecryptState::Done {
            return Err(ProtocolError::new(
                ErrorKind::DecryptTargetStale,
                "仅 done 会话可读产物",
            ));
        }
        let path = self
            .store
            .artifact_path(id, artifact)
            .ok_or_else(|| ProtocolError::new(ErrorKind::Validation, "非法 session id"))?;
        let file_len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        net_policy_core::decrypt::validate_read_window(offset, len, file_len)
            .map_err(|e| ProtocolError::new(ErrorKind::Validation, e.to_string()))?;
        let mut f = std::fs::File::open(&path)
            .map_err(|e| ProtocolError::new(ErrorKind::Internal, format!("open artifact：{e}")))?;
        f.seek(SeekFrom::Start(offset))
            .map_err(|e| internal(e.into()))?;
        let to_read = std::cmp::min(len as u64, file_len - offset) as usize;
        let mut buf = vec![0u8; to_read];
        f.read_exact(&mut buf).map_err(|e| internal(e.into()))?;
        let eof = offset + to_read as u64 >= file_len;
        Ok((offset, buf, eof))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net_policy_core::decrypt::ProcessInstanceRef;

    fn ws(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("np-dec-mgr-{}-{name}", std::process::id()))
    }
    fn target() -> DecryptTarget {
        DecryptTarget {
            process: ProcessInstanceRef {
                pid: 100,
                created_at_100ns: 1,
                path: r"C:\a.exe".into(),
            },
            domains: vec!["example.com".into()],
        }
    }

    #[test]
    fn ca_lifecycle_create_confirm_status_remove() {
        let w = ws("ca");
        let _ = std::fs::remove_dir_all(&w);
        let m = DecryptManager::new(&w);
        // 初始 Absent
        assert_eq!(m.ca_status().state, CaState::Absent);
        // create → Installed + 有指纹
        let st = m.ca_create("S-1-5-21-x").unwrap();
        assert_eq!(st.state, CaState::Installed);
        let tp = st.thumbprint.clone().unwrap();
        assert_eq!(tp.len(), 64);
        assert!(st.owner_sid.is_none(), "未确认安装");
        // 未确认安装 → 不能起会话
        assert_eq!(
            m.start(target(), DecryptOpts::default(), [1; 16], 1)
                .unwrap_err()
                .kind,
            ErrorKind::DecryptCaMissing
        );
        // confirm 指纹不符 → 拒
        assert!(m.ca_confirm("DEADBEEF", "S-1-5-21").is_err());
        // confirm 正确指纹 → 记录 owner_sid
        let st2 = m.ca_confirm(&tp, "S-1-5-21-x").unwrap();
        assert_eq!(st2.owner_sid.as_deref(), Some("S-1-5-21-x"));
        assert_eq!(st2.store_scope.as_deref(), Some("current_user"));
        // remove → Absent
        assert_eq!(m.ca_remove().unwrap().state, CaState::Absent);
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn create_stores_dpapi_key_no_plaintext_and_loads() {
        let w = ws("dpapi");
        let _ = std::fs::remove_dir_all(&w);
        let store = CaStore::new(&w);
        store.create("S-1-5-21-test").unwrap();
        // 磁盘上：公钥证书在，DPAPI 密文私钥在，明文 ca.key 不存在。
        assert!(store.ca_crt().exists());
        assert!(store.ca_key_dpapi().exists());
        assert!(!store.legacy_ca_key().exists(), "不得有明文私钥");
        // 密文文件不含 PEM 私钥标记（确已加密，非明文）。
        let enc = std::fs::read(store.ca_key_dpapi()).unwrap();
        assert!(
            !String::from_utf8_lossy(&enc).contains("PRIVATE KEY"),
            "私钥密文不得含明文 PEM 头"
        );
        // load_authority 解密装配成功。
        assert!(store.load_authority().is_ok());
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn migrates_legacy_plaintext_key_to_dpapi() {
        let w = ws("migrate");
        let _ = std::fs::remove_dir_all(&w);
        let store = CaStore::new(&w);
        // 造一个旧版明文布局：ca.crt + 明文 ca.key，无 DPAPI 密文。
        std::fs::create_dir_all(&store.dir).unwrap();
        let ca = net_policy_mitm::cert::ca::CertAuthority::generate().unwrap();
        std::fs::write(store.ca_crt(), &ca.ca_cert_pem).unwrap();
        std::fs::write(store.legacy_ca_key(), ca.ca_key_pem()).unwrap();
        assert!(store.legacy_ca_key().exists());
        // load_authority 触发迁移。
        assert!(store.load_authority().is_ok());
        assert!(store.ca_key_dpapi().exists(), "应生成 DPAPI 密文");
        assert!(!store.legacy_ca_key().exists(), "迁移后删明文");
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn start_rejects_without_ca() {
        let w = ws("noca");
        let _ = std::fs::remove_dir_all(&w);
        let m = DecryptManager::new(&w);
        let err = m
            .start(target(), DecryptOpts::default(), [2; 16], 1)
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::DecryptCaMissing);
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn store_manifest_roundtrip_list_delete() {
        let w = ws("store");
        let _ = std::fs::remove_dir_all(&w);
        let store = DecryptStore::new(&w);
        store.ensure_root().unwrap();
        let id = format_session_id([7; 16]);
        let s = DecryptSession {
            id: id.clone(),
            state: DecryptState::Done,
            target: target(),
            opts: DecryptOpts::default(),
            started_ms: 10,
            ended_ms: Some(20),
            per_domain: Default::default(),
            error: None,
        };
        store.write(&s).unwrap();
        assert_eq!(store.read(&id).unwrap().started_ms, 10);
        assert_eq!(store.list().len(), 1);
        // 路径穿越 id 拒绝定位
        assert!(store.session_dir("dec-../evil").is_none());
        store.delete(&id).unwrap();
        assert_eq!(store.list().len(), 0);
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn quota_removes_session_when_bytes_exceed_limit_even_under_count_limit() {
        let w = ws("quota-bytes");
        let _ = std::fs::remove_dir_all(&w);
        let store = DecryptStore::new(&w);
        store.ensure_root().unwrap();
        let id = format_session_id([9; 16]);
        let session = DecryptSession {
            id: id.clone(),
            state: DecryptState::Done,
            target: target(),
            opts: DecryptOpts::default(),
            started_ms: 1,
            ended_ms: Some(2),
            per_domain: Default::default(),
            error: None,
        };
        store.write(&session).unwrap();
        let file = std::fs::File::create(store.http_jsonl(&id).unwrap()).unwrap();
        file.set_len(QUOTA_TOTAL_BYTES + 1).unwrap();
        assert_eq!(store.enforce_quota(None), 1);
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(&w);
    }
}
