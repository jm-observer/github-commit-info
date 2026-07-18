//! mihomo 配置生成（纯逻辑：规则集 + 设置 → `config.yaml` 文本）。
//!
//! 从 zero-desktop `engine.rs::generate_config` 提级到 core——它只依赖 config 类型、无副作用，
//! 是可单测的纯函数。进程生命周期（start/stop/reload）留在 agent（有副作用）。
//!
//! 落地的验证结论（docs/net-policy-validation-report.md）：
//! - TUN(gvisor) + WG userspace outbound，§9#1 实测无路由环，无需手动 route-exclude host route。
//! - `strict-route: true` + `dns-hijack: any:53`（§0.6 拦系统 DNS，含显式 8.8.8.8）。
//! - DNS 模式 A：上游 nameserver 走物理 bootstrap（§0.8.1），kill-switch 放行其 IP。

use crate::config::{
    DecryptDivert, NetPolicySettings, Route, RuleSet, TempDirect, WgDialerProxyKind,
};
use crate::egress::{EgressRuntimeView, EGRESS_PROXY, EGRESS_WG};
use crate::routes;

/// mihomo 外部控制器端口（loopback）。
pub const CONTROLLER: &str = "127.0.0.1:9090";

fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

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
    divert: &DecryptDivert,
) -> String {
    generate_config_with(
        settings,
        rules,
        secret,
        temp,
        divert,
        &EgressRuntimeView::all_available(),
    )
}

/// 同 [`generate_config`]，但按出口运行态生成：**被停用/不可用的出口不再渲染其 outbound**，
/// 指向它的规则按 fallback 改写（默认 `REJECT-DROP`）。这让「Stop 一个出口」是真的数据面动作，
/// 而不只是一个显示状态；同时保证配置里不会残留悬空的 outbound 引用（设计 §4/§6.1）。
pub fn generate_config_with(
    settings: &NetPolicySettings,
    rules: &RuleSet,
    secret: &str,
    temp: &TempDirect,
    divert: &DecryptDivert,
    view: &EgressRuntimeView,
) -> String {
    let wg = &settings.wg;
    let wg_available = !view.is_unavailable(EGRESS_WG);
    let proxy_available = !view.is_unavailable(EGRESS_PROXY);
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
    // L4 解密导流规则置于所有用户/策略规则之上（§17.3 方案 B）；未启用时为空。
    let mut rule_vec = routes::divert_lines(divert);
    rule_vec.extend(routes::to_lines_with(settings, rules, temp, view));
    let rule_lines = rule_vec.join("\n");
    let ipv6 = !settings.block_ipv6; // block_ipv6=true → mihomo ipv6:false

    // DNS nameserver：**海外姿态 + WG 就绪**时，被墙域名走隧道内 DoH（`#wg-out`）解析——否则用国内
    // bootstrap 解墙外域名会被污染，现象是「纯 IP 通、google 等域名解不出」（真机实测）。国内直连
    // 域名仍走下方 direct-nameserver（国内）。其余姿态（直连/阻断）没有隧道可走，仍用国内 bootstrap。
    // 出口被停用时不能再把 DNS 指向它的 outbound——否则配置引用悬空、解析全挂。
    let use_tunnel_dns =
        settings.default_route == Route::Wg && wg.validate().is_ok() && wg_available;
    let nameserver = match settings.default_route {
        Route::Wg if use_tunnel_dns => {
            "\"https://1.1.1.1/dns-query#wg-out\", \"https://8.8.8.8/dns-query#wg-out\"".to_string()
        }
        Route::Proxy
            if proxy_available && settings.proxy_subscriptions.active_subscription().is_some() =>
        {
            "\"https://1.1.1.1/dns-query#subscription-out\", \"https://8.8.8.8/dns-query#subscription-out\""
                .to_string()
        }
        _ => dns_bootstrap.clone(),
    };

    // L2 域名嗅探（抓包设计 §11 Phase 1）：仅在 `sniffer_enabled` 时输出 `sniffer` 块。
    // override-destination 恒 false（顶层 + 协议级），只观察不改路由/目标；parse-pure-ip 只对
    // 缺域名的连接尝试。端口取常见 HTTP/TLS/QUIC。默认关闭 → 不输出该块（mihomo 默认即不嗅探）。
    let sniffer = if settings.sniffer_enabled {
        "sniffer:\n  enable: true\n  parse-pure-ip: true\n  override-destination: false\n  sniff:\n    HTTP:\n      ports: [80, 8080-8880]\n    TLS:\n      ports: [443, 8443]\n    QUIC:\n      ports: [443, 8443]\n\n"
    } else {
        ""
    };

    // L4 解密导流的 loopback MITM outbound（§17.3 方案 B）：http 代理指向本机 MITM 监听端口。
    // MITM 解密后其上游连接再链回 mihomo，保留原出口语义（防环见 `DecryptDivert` 文档）。
    let mitm_out = if divert.active {
        format!(
            "\n  - name: {name}\n    type: http\n    server: 127.0.0.1\n    port: {port}\n    username: {username}\n    password: {password}",
            name = routes::MITM_OUT,
            port = divert.mitm_port,
            username = divert.proxy_username,
            password = divert.proxy_password,
        )
    } else {
        String::new()
    };

    // WG 已配 → 输出 wg-out outbound；未配 → 空 proxies（黑洞/直连仍可运行，不强制 SBN）。
    // WG 停用时连它的拨号代理一起不渲染，避免留下无人引用的 wg-dialer 组。
    let dialer_subscription_slot = wg_available
        .then(|| {
            wg.dialer_proxy
                .as_ref()
                .and_then(|proxy| proxy.subscription_slot)
                .map(|slot| settings.proxy_subscriptions.active.unwrap_or(slot))
        })
        .flatten();
    let provider_slot = proxy_available
        .then_some(settings.proxy_subscriptions.active)
        .flatten()
        .or(dialer_subscription_slot);
    let provider_name = provider_slot.map(|slot| format!("net-policy-sub-{}", slot + 1));
    let provider_config = provider_slot
        .and_then(|slot| settings.proxy_subscriptions.get(slot).map(|sub| (slot, sub)))
        .map(|(slot, sub)| {
            let provider = format!("net-policy-sub-{}", slot + 1);
            format!(
                "proxy-providers:\n  {provider}:\n    type: http\n    url: {url}\n    path: ./providers/{provider}.yaml\n    interval: {interval}\n",
                url = yaml_quote(&sub.url),
                interval = sub.interval_secs,
            )
        })
        .unwrap_or_default();

    let mut groups = Vec::new();
    if proxy_available && settings.proxy_subscriptions.active_subscription().is_some() {
        if let Some(provider) = &provider_name {
            groups.push(format!(
                "  - name: subscription-out\n    type: select\n    use:\n      - {provider}"
            ));
        }
    }
    if dialer_subscription_slot.is_some() {
        if let Some(provider) = &provider_name {
            groups.push(format!(
                "  - name: wg-dialer\n    type: select\n    use:\n      - {provider}"
            ));
        }
    }
    let proxy_groups = if groups.is_empty() {
        "proxy-groups: []".to_string()
    } else {
        format!("proxy-groups:\n{}", groups.join("\n"))
    };

    let wg_proxy = if wg.validate().is_ok() && wg_available {
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
        let dialer = wg.dialer_proxy.as_ref().filter(|p| p.subscription_slot.is_none()).map(|proxy| {
            let kind = match proxy.kind {
                WgDialerProxyKind::Socks5 => "socks5",
                WgDialerProxyKind::Http => "http",
            };
            format!(
                "\n  - name: wg-dialer\n    type: {kind}\n    server: {server}\n    port: {port}\n    username: {username}\n    password: {password}\n    udp: {udp}",
                kind = kind,
                server = yaml_quote(&proxy.server),
                port = proxy.port,
                username = yaml_quote(&proxy.username),
                password = yaml_quote(&proxy.password),
                udp = proxy.udp,
            )
        });
        let dialer_ref = if wg.dialer_proxy.is_some() {
            "\n    dialer-proxy: wg-dialer"
        } else {
            ""
        };
        format!(
            "{dialer}\n  - name: wg-out\n    type: wireguard\n    server: {server}\n    port: {port}\n    ip: {ip}\n    private-key: {priv}\n    public-key: {pubk}\n    pre-shared-key: {psk}\n    udp: true\n    persistent-keepalive: 25\n    mtu: {mtu}\n    remote-dns-resolve: false{dialer_ref}{amnezia}",
            server = wg.server,
            port = wg.port,
            ip = wg.ip,
            priv = wg.private_key,
            pubk = wg.public_key,
            psk = wg.pre_shared_key,
            mtu = wg.mtu,
            dialer = dialer.unwrap_or_default(),
            dialer_ref = dialer_ref,
        )
    } else {
        String::new()
    };

    // 合并 outbound 块：wg-out（可选）+ mitm-out（可选）。都无 → 空 proxies（黑洞/直连仍可运行）。
    let proxies = if wg_proxy.is_empty() && mitm_out.is_empty() {
        " []".to_string()
    } else {
        format!("{wg_proxy}{mitm_out}")
    };

    format!(
        r#"# 由 net-policy-agent 生成，请勿手改（改规则用 UI / apply）。
port: {http_port}
socks-port: {socks_port}
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
  nameserver: [{nameserver}]
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

{sniffer}proxies:{proxies}

{provider_config}{proxy_groups}

rules:
{rule_lines}
"#,
        http_port = settings.local_proxy.http_port,
        socks_port = settings.local_proxy.socks_port,
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
            &crate::config::DecryptDivert::default(),
        );
        assert!(cfg.contains("secret: \"sekret\""));
        assert!(cfg.contains("proxies: []"), "无 WG 应输出空 proxies");
        assert!(
            cfg.contains("MATCH,REJECT-DROP"),
            "黑洞兜底应为 REJECT-DROP"
        );
        assert!(cfg.contains("ipv6: false"), "默认 block_ipv6 → ipv6:false");
    }

    /// 一份 WG 合法、默认出口走 WG 的设置（出口停用相关用例的公共基线）。
    fn wg_settings() -> NetPolicySettings {
        use crate::config::WgConfig;
        NetPolicySettings {
            default_route: Route::Wg,
            wg: WgConfig {
                server: "38.209.122.38".into(),
                port: 29987,
                ip: "10.66.66.2".into(),
                private_key: "aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvMTI=".into(),
                public_key: "cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1Yj0=".into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn gen_with(settings: &NetPolicySettings, rules: &RuleSet, view: &EgressRuntimeView) -> String {
        generate_config_with(
            settings,
            rules,
            "s",
            &crate::config::TempDirect::default(),
            &crate::config::DecryptDivert::default(),
            view,
        )
    }

    #[test]
    fn stopped_wg_egress_is_not_rendered_and_traffic_is_blocked_not_leaked() {
        let settings = wg_settings();
        let rules = RuleSet {
            rules: vec![Rule::DomainSuffix {
                value: "example.com".into(),
                route: Route::Wg,
            }],
            groups: vec![],
        };
        // 基线：WG 可用时应有 wg-out、规则与兜底都指向它、DNS 走隧道。
        let up = gen_with(&settings, &rules, &EgressRuntimeView::all_available());
        assert!(up.contains("name: wg-out"));
        assert!(up.contains("DOMAIN-SUFFIX,example.com,wg-out"));
        assert!(up.contains("MATCH,wg-out"));
        assert!(up.contains("dns-query#wg-out"), "海外姿态应走隧道内 DoH");

        let mut view = EgressRuntimeView::all_available();
        view.unavailable.insert(EGRESS_WG.into());
        let down = gen_with(&settings, &rules, &view);
        assert!(
            !down.contains("wg-out"),
            "停用出口后配置里不得残留任何 wg-out 引用（含 outbound / 规则 / DNS）"
        );
        assert!(down.contains("proxies: []"), "唯一 outbound 被摘掉后应为空");
        assert!(
            down.contains("DOMAIN-SUFFIX,example.com,REJECT-DROP"),
            "指向停用出口的规则必须 fail-closed"
        );
        assert!(
            down.contains("MATCH,REJECT-DROP") && !down.contains("MATCH,DIRECT"),
            "兜底也必须阻断，绝不静默裸奔到直连"
        );
    }

    #[test]
    fn stopped_egress_falls_back_to_direct_only_when_explicitly_allowed() {
        let settings = wg_settings();
        let mut view = EgressRuntimeView::all_available();
        view.unavailable.insert(EGRESS_WG.into());
        view.fallback
            .insert(EGRESS_WG.into(), crate::egress::EgressFallback::Direct);
        let cfg = gen_with(&settings, &RuleSet::default(), &view);
        assert!(cfg.contains("MATCH,DIRECT"), "用户明确允许时才回落直连");
        assert!(!cfg.contains("wg-out"));
    }

    #[test]
    fn stopped_proxy_egress_drops_provider_and_group() {
        use crate::config::{ProxySubscription, ProxySubscriptions};
        let settings = NetPolicySettings {
            default_route: Route::Proxy,
            proxy_subscriptions: ProxySubscriptions {
                first: Some(ProxySubscription {
                    name: "sub".into(),
                    url: "https://example.com/sub".into(),
                    ..Default::default()
                }),
                second: None,
                active: Some(0),
            },
            ..Default::default()
        };
        let up = gen_with(
            &settings,
            &RuleSet::default(),
            &EgressRuntimeView::all_available(),
        );
        assert!(up.contains("name: subscription-out"));
        assert!(up.contains("proxy-providers:"));

        let mut view = EgressRuntimeView::all_available();
        view.unavailable.insert(EGRESS_PROXY.into());
        let down = gen_with(&settings, &RuleSet::default(), &view);
        assert!(
            !down.contains("subscription-out"),
            "停用代理出口后不得残留悬空的 select 组引用"
        );
        assert!(
            !down.contains("proxy-providers:"),
            "订阅 provider 一并不加载"
        );
        assert!(down.contains("MATCH,REJECT-DROP"));
    }

    #[test]
    fn direct_config_match_direct() {
        let settings = NetPolicySettings::default(); // default_route = Direct
        let cfg = generate_config(
            &settings,
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
            &crate::config::DecryptDivert::default(),
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
            &crate::config::DecryptDivert::default(),
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
            &crate::config::DecryptDivert::default(),
        );
        assert!(cfg.contains("type: wireguard"), "普通 WG 仍渲染");
        assert!(
            !cfg.contains("amnezia-wg-option"),
            "无 amnezia 不应出现该块"
        );
    }

    #[test]
    fn dialer_proxy_is_rendered_before_wireguard() {
        use crate::config::{WgConfig, WgDialerProxy, WgDialerProxyKind};
        let settings = NetPolicySettings {
            wg: WgConfig {
                server: "38.209.122.38".into(),
                port: 29987,
                ip: "10.66.66.2".into(),
                private_key: "aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvaGVsbG8=".into(),
                public_key: "cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1YmtleQ==".into(),
                dialer_proxy: Some(WgDialerProxy {
                    kind: WgDialerProxyKind::Socks5,
                    server: "127.0.0.1".into(),
                    port: 7891,
                    username: "user".into(),
                    password: "pass".into(),
                    udp: true,
                    subscription_slot: None,
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
            &crate::config::DecryptDivert::default(),
        );
        let dialer = cfg.find("name: wg-dialer").expect("应渲染上游代理");
        let wg = cfg.find("name: wg-out").expect("应渲染 WG");
        assert!(dialer < wg, "上游代理应在 WG outbound 前定义");
        assert!(cfg.contains("dialer-proxy: wg-dialer"));
        assert!(cfg.contains("server: \"127.0.0.1\"\n    port: 7891"));
        assert!(cfg.contains("udp: true"));
    }

    #[test]
    fn subscription_provider_is_rendered_for_selected_slot() {
        use crate::config::{
            ProxySubscription, ProxySubscriptions, WgConfig, WgDialerProxy, WgDialerProxyKind,
        };
        let settings = NetPolicySettings {
            wg: WgConfig {
                server: "38.209.122.38".into(),
                port: 29987,
                ip: "10.66.66.2".into(),
                private_key: "aGVsbG9oZWxsb2hlbGxvaGVsbG9oZWxsb2hlbGxvaGVsbG8=".into(),
                public_key: "cGVlcnB1YmtleXB1YmtleXB1YmtleXB1YmtleXB1YmtleQ==".into(),
                dialer_proxy: Some(WgDialerProxy {
                    kind: WgDialerProxyKind::Socks5,
                    server: String::new(),
                    port: 1,
                    username: String::new(),
                    password: String::new(),
                    udp: true,
                    subscription_slot: Some(1),
                }),
                ..Default::default()
            },
            proxy_subscriptions: ProxySubscriptions {
                first: Some(ProxySubscription {
                    name: "一".into(),
                    url: "https://one.example/sub".into(),
                    interval_secs: 3600,
                }),
                second: Some(ProxySubscription {
                    name: "二".into(),
                    url: "https://two.example/sub?a=1".into(),
                    interval_secs: 7200,
                }),
                active: Some(1),
            },
            default_route: Route::Wg,
            ..Default::default()
        };
        let cfg = generate_config(
            &settings,
            &RuleSet::default(),
            "s",
            &TempDirect::default(),
            &DecryptDivert::default(),
        );
        assert!(cfg.contains("proxy-providers:"));
        assert!(cfg.contains("url: \"https://two.example/sub?a=1\""));
        assert!(!cfg.contains("https://one.example/sub"));
        assert!(cfg.contains("      - net-policy-sub-2"));
        assert!(cfg.contains("dialer-proxy: wg-dialer"));
    }

    #[test]
    fn subscription_can_be_default_egress_with_explicit_local_ports() {
        use crate::config::{LocalProxyListeners, ProxySubscription, ProxySubscriptions};
        let settings = NetPolicySettings {
            proxy_subscriptions: ProxySubscriptions {
                first: Some(ProxySubscription {
                    name: "主订阅".into(),
                    url: "https://proxy.example/subscription".into(),
                    interval_secs: 1800,
                }),
                second: None,
                active: Some(0),
            },
            local_proxy: LocalProxyListeners {
                socks_port: 1080,
                http_port: 8080,
            },
            default_route: Route::Proxy,
            ..Default::default()
        };
        let cfg = generate_config(
            &settings,
            &RuleSet::default(),
            "s",
            &TempDirect::default(),
            &DecryptDivert::default(),
        );
        assert!(cfg.contains("port: 8080\nsocks-port: 1080"));
        assert!(cfg.contains("name: subscription-out"));
        assert!(cfg.contains("MATCH,subscription-out"));
        assert!(cfg.contains("https://1.1.1.1/dns-query#subscription-out"));
        assert!(cfg.contains("url: \"https://proxy.example/subscription\""));
    }

    #[test]
    fn sniffer_absent_by_default() {
        // 默认 sniffer_enabled=false → 不输出 sniffer 块（mihomo 默认即不嗅探）。
        let cfg = generate_config(
            &NetPolicySettings::default(),
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
            &crate::config::DecryptDivert::default(),
        );
        assert!(!cfg.contains("sniffer:"), "默认不应输出 sniffer 块");
    }

    #[test]
    fn sniffer_block_rendered_when_enabled() {
        let settings = NetPolicySettings {
            sniffer_enabled: true,
            ..Default::default()
        };
        let cfg = generate_config(
            &settings,
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
            &crate::config::DecryptDivert::default(),
        );
        assert!(
            cfg.contains("sniffer:\n  enable: true"),
            "应输出 sniffer 块"
        );
        // §11 硬约束：只观察不改路由——override-destination 必须为 false。
        assert!(
            cfg.contains("override-destination: false"),
            "sniffer 不得改路由/目标"
        );
        assert!(cfg.contains("parse-pure-ip: true"));
        assert!(cfg.contains("    TLS:\n      ports: [443, 8443]"));
        assert!(cfg.contains("    HTTP:\n      ports: [80, 8080-8880]"));
        assert!(cfg.contains("    QUIC:\n      ports: [443, 8443]"));
        // sniffer 块位于 tun 与 proxies 之间，proxies 仍在。
        let s_idx = cfg.find("sniffer:").unwrap();
        let p_idx = cfg.find("proxies:").unwrap();
        assert!(s_idx < p_idx, "sniffer 应在 proxies 之前");
    }

    #[test]
    fn rule_lines_rendered_in_order() {
        let rules = RuleSet {
            rules: vec![
                Rule::DomainSuffix {
                    value: "example.com".into(),
                    route: Route::Direct,
                },
                Rule::DomainKeyword {
                    value: "ctrip".into(),
                    route: Route::Direct,
                },
            ],
            groups: vec![],
        };
        let cfg = generate_config(
            &NetPolicySettings::default(),
            &rules,
            "s",
            &crate::config::TempDirect::default(),
            &crate::config::DecryptDivert::default(),
        );
        assert!(cfg.contains("DOMAIN-SUFFIX,example.com,DIRECT"));
        assert!(cfg.contains("DOMAIN-KEYWORD,ctrip,DIRECT"));
    }

    #[test]
    fn divert_inactive_emits_nothing() {
        let cfg = generate_config(
            &NetPolicySettings::default(),
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
            &crate::config::DecryptDivert::default(),
        );
        assert!(!cfg.contains("mitm-out"), "未启用导流不应出现 mitm-out");
        assert!(cfg.contains("proxies: []"), "无 WG/无导流应空 proxies");
    }

    #[test]
    fn divert_active_emits_mitm_out_proxy_and_top_rules() {
        use crate::config::{DecryptDivert, ProcessRef};
        let divert = DecryptDivert {
            active: true,
            targets: vec![ProcessRef::ProcessName("chrome.exe".into())],
            domains: vec!["example.com".into()],
            mitm_port: 18081,
            force_tcp_for_quic: false,
            proxy_username: "net-policy".into(),
            proxy_password: "secret".into(),
        };
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
            &divert,
        );
        // mitm-out http 代理输出。
        assert!(cfg.contains("name: mitm-out"), "应输出 mitm-out 代理");
        assert!(cfg.contains("type: http"));
        assert!(cfg.contains("server: 127.0.0.1"));
        assert!(cfg.contains("port: 18081"));
        // 导流规则：TCP 80/443 → mitm-out，且置于用户 DOMAIN-SUFFIX 规则之上。
        let divert443 = "AND,((PROCESS-NAME,chrome.exe),(NETWORK,tcp),(DST-PORT,443),(DOMAIN-SUFFIX,example.com)),mitm-out";
        assert!(cfg.contains(divert443), "应有 443 导流规则");
        assert!(
            cfg.contains("(DST-PORT,80),(DOMAIN-SUFFIX,example.com)),mitm-out"),
            "应有 80 导流规则"
        );
        let d_idx = cfg
            .find("mitm-out),")
            .or_else(|| cfg.find(divert443))
            .unwrap();
        let user_idx = cfg.find("DOMAIN-SUFFIX,example.com,DIRECT").unwrap();
        assert!(d_idx < user_idx, "导流规则应在用户规则之上");
        // 默认不 force-tcp：不出现 UDP REJECT。
        assert!(!cfg.contains("(NETWORK,udp)"), "默认不阻断 QUIC/UDP");
    }

    #[test]
    fn divert_force_tcp_rejects_scoped_udp443() {
        use crate::config::{DecryptDivert, ProcessRef};
        let divert = DecryptDivert {
            active: true,
            targets: vec![ProcessRef::ProcessName("app.exe".into())],
            domains: vec!["example.com".into(), "cdn.example.net".into()],
            mitm_port: 18081,
            force_tcp_for_quic: true,
            proxy_username: "net-policy".into(),
            proxy_password: "secret".into(),
        };
        let cfg = generate_config(
            &NetPolicySettings::default(),
            &RuleSet::default(),
            "s",
            &crate::config::TempDirect::default(),
            &divert,
        );
        // force-tcp：每个 allowlist 域名一条 进程+域名+UDP/443 REJECT（严格作用域，非全机）。
        assert!(cfg.contains(
            "AND,((PROCESS-NAME,app.exe),(NETWORK,udp),(DST-PORT,443),(DOMAIN-SUFFIX,example.com)),REJECT"
        ));
        assert!(cfg.contains(
            "AND,((PROCESS-NAME,app.exe),(NETWORK,udp),(DST-PORT,443),(DOMAIN-SUFFIX,cdn.example.net)),REJECT"
        ));
    }
}
