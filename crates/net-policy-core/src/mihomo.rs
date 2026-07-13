//! mihomo 配置生成（纯逻辑：规则集 + 设置 → `config.yaml` 文本）。
//!
//! 从 zero-desktop `engine.rs::generate_config` 提级到 core——它只依赖 config 类型、无副作用，
//! 是可单测的纯函数。进程生命周期（start/stop/reload）留在 agent（有副作用）。
//!
//! 落地的验证结论（docs/net-policy-validation-report.md）：
//! - TUN(gvisor) + WG userspace outbound，§9#1 实测无路由环，无需手动 route-exclude host route。
//! - `strict-route: true` + `dns-hijack: any:53`（§0.6 拦系统 DNS，含显式 8.8.8.8）。
//! - DNS 模式 A：上游 nameserver 走物理 bootstrap（§0.8.1），kill-switch 放行其 IP。

use crate::config::{NetPolicySettings, RuleSet, TempDirect};
use crate::routes;

/// mihomo 外部控制器端口（loopback）。
pub const CONTROLLER: &str = "127.0.0.1:9090";

/// 从规则集 + 设置生成 mihomo `config.yaml` 文本。`secret` 是 external-controller 鉴权口令，
/// `temp` 是临时直连覆盖（未激活传 `&TempDirect::default()`）。
///
/// **与 SBN 解耦**：仅当 WG 配置合法时才输出 `wg-out` proxy；否则 `proxies: []`（纯黑洞/直连
/// 也能跑，TUN 照常起）。规则/兜底由 [`routes::to_lines`] 统一展开（含临时直连覆盖）。
pub fn generate_config(
    settings: &NetPolicySettings,
    rules: &RuleSet,
    secret: &str,
    temp: &TempDirect,
) -> String {
    let wg = &settings.wg;
    let dns_bootstrap = settings
        .dns_bootstrap
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let lan_exclude = settings
        .lan_ranges
        .iter()
        .map(|s| format!("    - {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    let rule_lines = routes::to_lines(settings, rules, temp).join("\n");
    let ipv6 = !settings.block_ipv6; // block_ipv6=true → mihomo ipv6:false

    // WG 已配 → 输出 wg-out outbound；未配 → 空 proxies（黑洞/直连仍可运行，不强制 SBN）。
    let proxies = if wg.validate().is_ok() {
        // AmneziaWG 混淆：填了 amnezia 就追加 amnezia-wg-option 块，让 mihomo 以 AmneziaWG
        // 方式握手（改 magic header + 加垃圾包），破坏原生 WG 的固定特征以规避 DPI 丢包。
        // 全为数字，无字符串注入面。子字段缩进 6 空格（proxy 项字段 4 空格的下一层）。
        let amnezia = wg
            .amnezia
            .as_ref()
            .map(|a| {
                format!(
                    "\n    amnezia-wg-option:\n      jc: {}\n      jmin: {}\n      jmax: {}\n      s1: {}\n      s2: {}\n      s3: {}\n      s4: {}\n      h1: {}\n      h2: {}\n      h3: {}\n      h4: {}",
                    a.jc, a.jmin, a.jmax, a.s1, a.s2, a.s3, a.s4, a.h1, a.h2, a.h3, a.h4,
                )
            })
            .unwrap_or_default();
        format!(
            "\n  - name: wg-out\n    type: wireguard\n    server: {server}\n    port: {port}\n    ip: {ip}\n    private-key: {priv}\n    public-key: {pubk}\n    pre-shared-key: {psk}\n    udp: true\n    mtu: {mtu}\n    remote-dns-resolve: false{amnezia}",
            server = wg.server,
            port = wg.port,
            ip = wg.ip,
            priv = wg.private_key,
            pubk = wg.public_key,
            psk = wg.pre_shared_key,
            mtu = wg.mtu,
        )
    } else {
        " []".to_string()
    };

    format!(
        r#"# 由 net-policy-agent 生成，请勿手改（改规则用 UI / apply）。
mixed-port: 7890
allow-lan: false
mode: rule
log-level: info
ipv6: {ipv6}
find-process-mode: always
external-controller: {CONTROLLER}
secret: "{secret}"

dns:
  enable: true
  listen: 127.0.0.1:1053
  ipv6: {ipv6}
  enhanced-mode: fake-ip
  fake-ip-range: 198.18.0.1/16
  fake-ip-filter:
    - '*.lan'
    - '*.local'
    - '+.msftconnecttest.com'
    - '+.msftncsi.com'
  # DNS 模式 A：上游 bootstrap 走物理（§0.8.1，kill-switch 放行其 IP）。
  default-nameserver: [{dns_bootstrap}]
  nameserver: [{dns_bootstrap}]
  direct-nameserver: [{dns_bootstrap}]
  fallback: []

tun:
  enable: true
  stack: gvisor
  dns-hijack:
    - any:53
    - tcp://any:53
  auto-route: true
  auto-detect-interface: true
  strict-route: true
  route-exclude-address:
{lan_exclude}

proxies:{proxies}

proxy-groups: []

rules:
{rule_lines}
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NetPolicySettings, Route, Rule, RuleSet};

    #[test]
    fn blackhole_config_has_reject_match_and_empty_proxies() {
        let settings = NetPolicySettings {
            default_route: Route::Blackhole,
            ..Default::default()
        };
        let rules = RuleSet::default();
        let cfg = generate_config(
            &settings,
            &rules,
            "sekret",
            &crate::config::TempDirect::default(),
        );
        assert!(cfg.contains("secret: \"sekret\""));
        assert!(cfg.contains("proxies: []"), "无 WG 应输出空 proxies");
        assert!(
            cfg.contains("MATCH,REJECT-DROP"),
            "黑洞兜底应为 REJECT-DROP"
        );
        assert!(cfg.contains("ipv6: false"), "默认 block_ipv6 → ipv6:false");
    }

    #[test]
    fn direct_config_match_direct() {
        let settings = NetPolicySettings::default(); // default_route = Direct
        let cfg = generate_config(
            &settings,
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
        );
        assert!(cfg.contains("MATCH,DIRECT"));
    }

    #[test]
    fn amnezia_option_rendered_when_present() {
        use crate::config::{AmneziaConfig, WgConfig};
        let settings = NetPolicySettings {
            wg: WgConfig {
                server: "38.209.122.38".into(),
                port: 29987,
                ip: "10.66.66.2".into(),
                private_key: "aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvMTI=".into(),
                public_key: "cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1Yj0=".into(),
                amnezia: Some(AmneziaConfig {
                    jc: 4,
                    jmin: 40,
                    jmax: 70,
                    s1: 15,
                    s2: 20,
                    s3: 0,
                    s4: 0,
                    h1: 100,
                    h2: 200,
                    h3: 300,
                    h4: 400,
                }),
                ..Default::default()
            },
            default_route: Route::Wg,
            ..Default::default()
        };
        let cfg = generate_config(
            &settings,
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
        );
        assert!(cfg.contains("amnezia-wg-option:"), "应渲染 amnezia 块");
        assert!(cfg.contains("      jc: 4"));
        assert!(cfg.contains("      h1: 100"));
        assert!(cfg.contains("      h4: 400"));
        assert!(cfg.contains("MATCH,wg-out"), "默认海外应走 wg-out");
    }

    #[test]
    fn no_amnezia_option_when_absent() {
        use crate::config::WgConfig;
        let settings = NetPolicySettings {
            wg: WgConfig {
                server: "38.209.122.38".into(),
                port: 29987,
                ip: "10.66.66.2".into(),
                private_key: "aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvMTI=".into(),
                public_key: "cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1Yj0=".into(),
                amnezia: None,
                ..Default::default()
            },
            default_route: Route::Wg,
            ..Default::default()
        };
        let cfg = generate_config(
            &settings,
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
        );
        assert!(cfg.contains("type: wireguard"), "普通 WG 仍渲染");
        assert!(
            !cfg.contains("amnezia-wg-option"),
            "无 amnezia 不应出现该块"
        );
    }

    #[test]
    fn rule_lines_rendered_in_order() {
        let rules = RuleSet {
            rules: vec![Rule::DomainSuffix {
                value: "example.com".into(),
                route: Route::Direct,
            }],
            groups: vec![],
        };
        let cfg = generate_config(
            &NetPolicySettings::default(),
            &rules,
            "s",
            &crate::config::TempDirect::default(),
        );
        assert!(cfg.contains("DOMAIN-SUFFIX,example.com,DIRECT"));
    }
}
