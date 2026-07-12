//! net-policy 配置与规则模型 + 持久化。
//!
//! 落盘布局（见 docs/net-policy-validation-report.md §14.8）：
//! `{workspace}/net-policy/{settings.json, rules.json}`。
//! 安全约定：WireGuard 私钥不入 `rules.json`；存在 `settings.json` 的 `wg`
//! 段，后续可迁到 Windows Credential Manager（首版直存，已在文档标注）。

use super::valid;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 出口路由。作为**单条规则**的命中出口时取 `Direct`/`Wg`（把程序/域名/IP 白名单到直连，
/// 或显式指向 SBN 海外）；作为**默认出口**（`NetPolicySettings::default_route`，未命中任何规则的
/// 兜底）时取 `Direct`（默认，观察模式，原样直连）、`Wg`（全走海外）或 `Blackhole`（全阻断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// 本地直连（绕过隧道）。
    Direct,
    /// 走 WireGuard 海外出口（默认 per-rule 出口）。
    #[default]
    Wg,
    /// 黑洞：静默丢弃（mihomo `REJECT-DROP`）。"空出口"——什么都上不去，不依赖任何 SBN。
    Blackhole,
}

impl Route {
    /// 映射到 mihomo 规则的 outbound 名。
    pub fn outbound(self) -> &'static str {
        match self {
            Route::Direct => "DIRECT",
            Route::Wg => "wg-out",
            // REJECT-DROP：静默丢包（真"空出口"），比 REJECT（回 RST）更贴"网络什么都上不去"。
            Route::Blackhole => "REJECT-DROP",
        }
    }
}

/// 单条分流规则。`kind` 决定匹配维度，`value` 是匹配值，`route` 是命中后的出口。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Rule {
    /// 进程完整路径（`PROCESS-PATH`，反斜杠，大小写不敏感）。
    ProcessPath { value: String, route: Route },
    /// 进程名（`PROCESS-NAME`，仅 exe 文件名）。
    ProcessName { value: String, route: Route },
    /// 域名后缀（`DOMAIN-SUFFIX`）。
    DomainSuffix { value: String, route: Route },
    /// IP/CIDR（`IP-CIDR`）。
    IpCidr { value: String, route: Route },
}

impl Rule {
    /// 渲染为一行 mihomo rule。
    pub fn to_mihomo_line(&self) -> String {
        match self {
            Rule::ProcessPath { value, route } => {
                format!("  - PROCESS-PATH,{value},{}", route.outbound())
            }
            Rule::ProcessName { value, route } => {
                format!("  - PROCESS-NAME,{value},{}", route.outbound())
            }
            Rule::DomainSuffix { value, route } => {
                format!("  - DOMAIN-SUFFIX,{value},{}", route.outbound())
            }
            Rule::IpCidr { value, route } => {
                format!("  - IP-CIDR,{value},{},no-resolve", route.outbound())
            }
        }
    }

    /// 取本规则命中后的出口（用于「规则指向海外但 WG 未配」的一致性校验）。
    pub fn route(&self) -> Route {
        match self {
            Rule::ProcessPath { route, .. }
            | Rule::ProcessName { route, .. }
            | Rule::DomainSuffix { route, .. }
            | Rule::IpCidr { route, .. } => *route,
        }
    }

    /// 两条规则是否指向同一目标（kind + value 相同，忽略 route）。upsert / 按值删除用：
    /// 规则没有稳定 ID，前端此前按数组下标删规则，多行并发操作会因下标前移删错——按
    /// (kind, value) 定位没有这个竞态；同时保证同一目标只有一条规则（mihomo 首条命中，
    /// 旧规则在前会遮蔽改路后的新规则）。
    pub fn same_target(&self, other: &Rule) -> bool {
        use Rule::*;
        match (self, other) {
            (ProcessPath { value: a, .. }, ProcessPath { value: b, .. })
            | (ProcessName { value: a, .. }, ProcessName { value: b, .. })
            | (DomainSuffix { value: a, .. }, DomainSuffix { value: b, .. })
            | (IpCidr { value: a, .. }, IpCidr { value: b, .. }) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }

    /// 校验规则值（防注入 + 格式，P1-3）。
    pub fn validate(&self) -> Result<()> {
        match self {
            Rule::ProcessPath { value, .. } => valid::process_path(value),
            Rule::ProcessName { value, .. } => valid::process_name(value),
            Rule::DomainSuffix { value, .. } => valid::domain(value),
            Rule::IpCidr { value, .. } => valid::ip_or_cidr(value),
        }
    }
}

/// 程序组：用户选一个主程序，系统观察其子进程并允许确认加入同组（§14.4）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramGroup {
    pub id: String,
    pub name: String,
    pub root_paths: Vec<String>,
    #[serde(default)]
    pub known_children: Vec<ProcessRef>,
    #[serde(default)]
    pub route: Route,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ProcessRef {
    ProcessPath(String),
    ProcessName(String),
}

/// WireGuard outbound（mihomo userspace WG）。`server` 必须是 IP（避免鸡生蛋解析）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WgConfig {
    pub server: String,
    pub port: u16,
    /// 隧道内本机地址（如 10.66.66.5）。
    pub ip: String,
    pub private_key: String,
    pub public_key: String,
    #[serde(default)]
    pub pre_shared_key: String,
    #[serde(default = "default_mtu")]
    pub mtu: u32,
}

fn default_mtu() -> u32 {
    1420
}

impl WgConfig {
    /// 从标准 wg-quick `.conf`（INI 风格）文本解析出 net-policy 需要的字段：
    /// `[Interface]` 的 `PrivateKey`/`Address`/`MTU`，`[Peer]` 的
    /// `PublicKey`/`PresharedKey`/`Endpoint`。其余键（AllowedIPs/DNS/…）忽略——
    /// net-policy 自有分流与 DNS 策略。
    ///
    /// 只做解析与字段抽取，**不校验**：解析结果交前端合并到设置后由用户确认保存，
    /// 保存时再走 [`WgConfig::validate`]（例如 Endpoint 为域名会在保存时被拒，
    /// 给出可读报错而非在导入处直接失败）。
    pub fn from_wg_quick(text: &str) -> Result<WgConfig> {
        #[derive(PartialEq)]
        enum Section {
            None,
            Interface,
            Peer,
        }
        let mut section = Section::None;
        let mut private_key = String::new();
        let mut address = String::new();
        let mut mtu: Option<u32> = None;
        let mut public_key = String::new();
        let mut pre_shared_key = String::new();
        let mut endpoint = String::new();

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = match name.trim().to_ascii_lowercase().as_str() {
                    "interface" => Section::Interface,
                    "peer" => Section::Peer,
                    _ => Section::None,
                };
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let val = val.trim().to_string();
            match (&section, key.as_str()) {
                (Section::Interface, "privatekey") => private_key = val,
                (Section::Interface, "address") => address = val,
                (Section::Interface, "mtu") => mtu = val.parse().ok(),
                (Section::Peer, "publickey") => public_key = val,
                (Section::Peer, "presharedkey") => pre_shared_key = val,
                (Section::Peer, "endpoint") => endpoint = val,
                _ => {}
            }
        }

        if private_key.is_empty() {
            anyhow::bail!("配置缺少 [Interface] 段的 PrivateKey");
        }
        if public_key.is_empty() {
            anyhow::bail!("配置缺少 [Peer] 段的 PublicKey");
        }
        if endpoint.is_empty() {
            anyhow::bail!("配置缺少 [Peer] 段的 Endpoint");
        }

        // Address 可能逗号分隔 v4/v6，取第一个并剥掉 CIDR 前缀（mihomo 的 ip 字段要纯地址）。
        let ip = address
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .split('/')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if ip.is_empty() {
            anyhow::bail!("配置缺少 [Interface] 段的 Address");
        }

        // Endpoint = host:port，从右切一刀（兼容 IPv6 字面量 [::1]:51820）。
        let endpoint = endpoint.split(',').next().unwrap_or("").trim();
        let (host, port_s) = endpoint
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("Endpoint 缺少端口：{endpoint}"))?;
        let host = host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let port: u16 = port_s
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("Endpoint 端口非法：{port_s}"))?;

        Ok(WgConfig {
            server: host,
            port,
            ip,
            private_key,
            public_key,
            pre_shared_key,
            mtu: mtu.unwrap_or_else(default_mtu),
        })
    }

    /// WG 是否未配置（三关键字段皆空）——用于判断「默认黑洞、不依赖 SBN」时可跳过 WG 校验。
    pub fn is_blank(&self) -> bool {
        self.server.trim().is_empty()
            && self.private_key.trim().is_empty()
            && self.public_key.trim().is_empty()
    }

    /// 校验 WG 配置（格式 + 防注入）。
    pub fn validate(&self) -> Result<()> {
        valid::ip(&self.server).context("WG server 必须是合法 IP（不能是域名）")?;
        if self.port == 0 {
            anyhow::bail!("WG 端口非法");
        }
        valid::ip_or_cidr(&self.ip).context("WG 隧道内地址非法")?;
        valid::wg_key(&self.private_key).context("WG 私钥非法")?;
        valid::wg_key(&self.public_key).context("WG 公钥非法")?;
        if !self.pre_shared_key.is_empty() {
            valid::wg_key(&self.pre_shared_key).context("WG 预共享密钥非法")?;
        }
        Ok(())
    }
}

/// net-policy 设置（与 rules 分文件存）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetPolicySettings {
    pub wg: WgConfig,
    /// mihomo 上游 DNS bootstrap（必为 UDP IP）。§0.8.1 实测：走物理，kill-switch 必放行。
    #[serde(default = "default_dns_bootstrap")]
    pub dns_bootstrap: Vec<String>,
    /// 局域网保留段（防火墙白名单 + TUN route-exclude）。
    #[serde(default = "default_lan_ranges")]
    pub lan_ranges: Vec<String>,
    /// 是否启用防火墙 kill-switch（fail-closed）。**默认开启**——"未知流量必须海外 /
    /// fail-closed 不可妥协"是核心约束（P0-1）；关闭即"不受保护预览"模式。
    #[serde(default = "default_true")]
    pub killswitch_enabled: bool,
    /// 首版默认阻断 IPv6 公网（§0.8 / VP-12）。
    #[serde(default = "default_true")]
    pub block_ipv6: bool,
    /// **默认出口**（未命中任何规则的兜底）：`Direct`（默认，观察模式，原样直连只看不管）、
    /// `Wg`（全走海外）或 `Blackhole`（全阻断）。默认直连 → "observer-first，进页即可观察，不改流量"。
    #[serde(default = "default_default_route")]
    pub default_route: Route,
    /// 主开关：是否在 `setup` 时**自动应用**策略（启动即生效）。默认 false——首次安装不擅自改全局
    /// 防火墙；用户在 UI 显式启用后持久化为 true，此后每次启动自动恢复上次策略。
    #[serde(default)]
    pub enabled: bool,
}

/// 默认出口缺省值 = 直连·观察（observer-first：进页即可观察流量，不改路由，不依赖 SBN）。
fn default_default_route() -> Route {
    Route::Direct
}

fn default_dns_bootstrap() -> Vec<String> {
    vec!["223.5.5.5".into(), "119.29.29.29".into()]
}

fn default_lan_ranges() -> Vec<String> {
    vec![
        "192.168.0.0/16".into(),
        "10.0.0.0/8".into(),
        "172.16.0.0/12".into(),
        "169.254.0.0/16".into(),
    ]
}

fn default_true() -> bool {
    true
}

impl Default for NetPolicySettings {
    fn default() -> Self {
        Self {
            wg: WgConfig::default(),
            dns_bootstrap: default_dns_bootstrap(),
            lan_ranges: default_lan_ranges(),
            killswitch_enabled: true,
            block_ipv6: true,
            default_route: Route::Direct,
            enabled: false,
        }
    }
}

impl NetPolicySettings {
    /// 校验设置（WG + DNS bootstrap + LAN 段，防注入 P1-3）。
    ///
    /// WG 与 SBN **解耦**：仅在「默认出口=海外」或「WG 已填」时强校验 WG；默认黑洞且 WG 留空时
    /// 允许通过（纯黑洞，不依赖 SBN）。「某条规则指向海外但 WG 缺失」的跨表一致性在 apply/reload
    /// 时由 `validate_combined` 再查（此处拿不到 rules）。
    pub fn validate(&self) -> Result<()> {
        if self.default_route == Route::Wg {
            self.wg
                .validate()
                .context("默认出口=海外(SBN)，需有效的 WireGuard 配置")?;
        } else if !self.wg.is_blank() {
            self.wg.validate()?;
        }
        // IPv6 旁路防护：mihomo 配置 `ipv6:false` 时 TUN 不接管 v6 路由，v6 的封锁完全靠
        // 防火墙 KS-IPv6Block（仅 block_ipv6=true 时创建）。若姿态是黑洞/海外却关掉 block_ipv6，
        // IPv6 公网流量会绕过策略直接出物理网卡——黑洞/全VPN 对 v6 静默失效。禁止该组合。
        if self.default_route != Route::Direct && !self.block_ipv6 {
            anyhow::bail!(
                "「阻断/海外」姿态必须开启「阻断 IPv6 公网」：引擎当前不接管 IPv6 流量，关闭后 IPv6 会绕过黑洞/隧道直接出物理网卡"
            );
        }
        if self.dns_bootstrap.is_empty() {
            anyhow::bail!("DNS bootstrap 不能为空（mihomo 上游解析需要）");
        }
        for d in &self.dns_bootstrap {
            valid::ip(d).context("DNS bootstrap 必须是 UDP IP")?;
        }
        for l in &self.lan_ranges {
            valid::ip_or_cidr(l).context("LAN 段非法")?;
        }
        Ok(())
    }
}

/// 规则集合（与 settings 分文件存）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleSet {
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub groups: Vec<ProgramGroup>,
}

impl RuleSet {
    /// 展开为 mihomo rules（程序组先展开成 PROCESS-* 规则，再追加普通规则，最后
    /// `MATCH,<default_route>`——默认出口由 `NetPolicySettings::default_route` 决定，黑洞或海外）。
    pub fn to_mihomo_rules(&self, default_route: Route) -> Vec<String> {
        let mut lines = Vec::new();
        // 内网/保留地址直连——用显式 IP-CIDR 而非 GEOIP,private，避免依赖 geoip 数据库
        // （0.228 实测：fresh 机器若无 geoip.metadb，mihomo 会去 GitHub 下载，国内慢/失败）。
        for cidr in [
            "127.0.0.0/8",
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "169.254.0.0/16",
            "224.0.0.0/4",
        ] {
            lines.push(format!("  - IP-CIDR,{cidr},DIRECT,no-resolve"));
        }
        // 程序组展开
        for g in &self.groups {
            for p in &g.root_paths {
                lines.push(format!("  - PROCESS-PATH,{p},{}", g.route.outbound()));
            }
            for c in &g.known_children {
                match c {
                    ProcessRef::ProcessPath(v) => {
                        lines.push(format!("  - PROCESS-PATH,{v},{}", g.route.outbound()))
                    }
                    ProcessRef::ProcessName(v) => {
                        lines.push(format!("  - PROCESS-NAME,{v},{}", g.route.outbound()))
                    }
                }
            }
        }
        // 普通规则
        for r in &self.rules {
            lines.push(r.to_mihomo_line());
        }
        // fail-closed 核心：未命中任何规则的兜底 = 默认出口（默认黑洞，或全走海外）。
        lines.push(format!("  - MATCH,{}", default_route.outbound()));
        lines
    }

    /// 校验全部规则（任一非法即整体拒绝，P1-3）。
    pub fn validate(&self) -> Result<()> {
        for (i, r) in self.rules.iter().enumerate() {
            r.validate().with_context(|| format!("规则 #{i} 非法"))?;
        }
        for g in &self.groups {
            for p in &g.root_paths {
                valid::process_path(p).with_context(|| format!("程序组 {} 路径非法", g.name))?;
            }
            for c in &g.known_children {
                match c {
                    ProcessRef::ProcessPath(v) => valid::process_path(v)?,
                    ProcessRef::ProcessName(v) => valid::process_name(v)?,
                }
            }
        }
        Ok(())
    }
}

/// net-policy workspace 子目录。
pub fn net_policy_dir(workspace: &Path) -> PathBuf {
    workspace.join("net-policy")
}

fn settings_path(workspace: &Path) -> PathBuf {
    net_policy_dir(workspace).join("settings.json")
}

fn rules_path(workspace: &Path) -> PathBuf {
    net_policy_dir(workspace).join("rules.json")
}

/// 生成的 mihomo 配置路径。
pub fn mihomo_config_path(workspace: &Path) -> PathBuf {
    net_policy_dir(workspace)
        .join("generated")
        .join("config.yaml")
}

/// kill-switch 状态快照路径（Remove 时按原值恢复 DefaultOutboundAction）。
pub fn killswitch_state_path(workspace: &Path) -> PathBuf {
    net_policy_dir(workspace).join("killswitch-state.json")
}

/// 从已生成的 mihomo `config.yaml` 解析 external-controller secret（P1：应用重启后恢复鉴权口令，
/// 以便管理/急停仍在运行的旧 mihomo 实例）。secret 本就随配置写盘（`secret: "..."`），解析它
/// **不增加新的磁盘暴露面**——这也是 review 建议的"从生成的 config 解析恢复"路径。
/// 返回 `None` 表示无配置 / 未含非空 secret。
pub fn read_generated_secret(workspace: &Path) -> Option<String> {
    let cfg = std::fs::read_to_string(mihomo_config_path(workspace)).ok()?;
    for line in cfg.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("secret:") {
            let v = rest.trim().trim_matches('"').trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
            return None;
        }
    }
    None
}

pub fn load_settings(workspace: &Path) -> NetPolicySettings {
    try_load_settings(workspace).unwrap_or_default()
}

/// 严格加载设置：仅“文件尚不存在”视为首次使用并返回默认值；已有文件读取或解析失败必须上抛，
/// 防止安全策略静默退化为默认直连。
pub fn try_load_settings(workspace: &Path) -> Result<NetPolicySettings> {
    let p = settings_path(workspace);
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("parse {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(NetPolicySettings::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

pub fn save_settings(workspace: &Path, s: &NetPolicySettings) -> Result<()> {
    let p = settings_path(workspace);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(s).context("serialize settings")?;
    std::fs::write(&p, json).with_context(|| format!("write {}", p.display()))
}

/// 严格加载规则：仅“文件尚不存在”返回空规则；已有文件损坏时拒绝继续应用。
pub fn try_load_rules(workspace: &Path) -> Result<RuleSet> {
    let p = rules_path(workspace);
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("parse {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RuleSet::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

pub fn save_rules(workspace: &Path, r: &RuleSet) -> Result<()> {
    let p = rules_path(workspace);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(r).context("serialize rules")?;
    std::fs::write(&p, json).with_context(|| format!("write {}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wg_quick_full() {
        let conf = "\
[Interface]
PrivateKey = aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvMTI=
Address = 10.66.66.5/32, fd00::5/128
MTU = 1380
DNS = 1.1.1.1

[Peer]
PublicKey = cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1Yj0=
PresharedKey = cHNrcHNrcHNrcHNrcHNrcHNrcHNrcHNrcHNrcHNrMTI=
Endpoint = 38.209.122.38:51227
AllowedIPs = 0.0.0.0/0, ::/0
";
        let wg = WgConfig::from_wg_quick(conf).expect("parse");
        assert_eq!(wg.server, "38.209.122.38");
        assert_eq!(wg.port, 51227);
        assert_eq!(wg.ip, "10.66.66.5"); // CIDR 前缀已剥离
        assert_eq!(wg.mtu, 1380);
        assert!(wg.private_key.starts_with("aGVsbG"));
        assert!(wg.public_key.starts_with("cGVlcn"));
        assert!(wg.pre_shared_key.starts_with("cHNr"));
        wg.validate().expect("解析出的配置应通过校验");
    }

    #[test]
    fn parse_wg_quick_minimal_defaults_mtu_and_optional_psk() {
        let conf = "\
[Interface]
PrivateKey = aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvMTI=
Address = 10.0.0.2

[Peer]
PublicKey = cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1Yj0=
Endpoint = 1.2.3.4:51820
";
        let wg = WgConfig::from_wg_quick(conf).expect("parse");
        assert_eq!(wg.ip, "10.0.0.2");
        assert_eq!(wg.port, 51820);
        assert_eq!(wg.mtu, default_mtu());
        assert_eq!(wg.pre_shared_key, "");
    }

    #[test]
    fn parse_wg_quick_missing_required_fails() {
        let conf = "[Interface]\nAddress = 10.0.0.2\n";
        assert!(WgConfig::from_wg_quick(conf).is_err());
    }

    #[test]
    fn same_target_matches_kind_and_value_ignores_route() {
        let a = Rule::DomainSuffix {
            value: "Example.com".into(),
            route: Route::Direct,
        };
        let b = Rule::DomainSuffix {
            value: "example.com".into(),
            route: Route::Blackhole,
        };
        let c = Rule::IpCidr {
            value: "example.com".into(),
            route: Route::Direct,
        };
        assert!(a.same_target(&b)); // 同 kind+value（大小写不敏感），route 不同也算同目标
        assert!(!a.same_target(&c)); // kind 不同即不同目标
    }

    #[test]
    fn non_direct_route_requires_block_ipv6() {
        let mut s = NetPolicySettings {
            default_route: Route::Blackhole,
            block_ipv6: false,
            ..Default::default()
        };
        assert!(
            s.validate().is_err(),
            "黑洞姿态关 block_ipv6 应被拒绝（IPv6 旁路）"
        );
        s.block_ipv6 = true;
        s.validate().expect("黑洞 + 阻断 IPv6 应通过");
        s.default_route = Route::Direct;
        s.block_ipv6 = false;
        s.validate().expect("直连观察姿态允许关 block_ipv6");
    }

    #[test]
    fn strict_load_rejects_corrupt_existing_files() {
        let workspace =
            std::env::temp_dir().join(format!("zero-net-policy-corrupt-{}", std::process::id()));
        let dir = net_policy_dir(&workspace);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(settings_path(&workspace), "{").unwrap();
        std::fs::write(rules_path(&workspace), "{").unwrap();

        assert!(try_load_settings(&workspace).is_err());
        assert!(try_load_rules(&workspace).is_err());

        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn strict_load_defaults_only_when_files_are_absent() {
        let workspace =
            std::env::temp_dir().join(format!("zero-net-policy-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);

        assert!(!try_load_settings(&workspace).unwrap().enabled);
        assert!(try_load_rules(&workspace).unwrap().rules.is_empty());
    }
}
