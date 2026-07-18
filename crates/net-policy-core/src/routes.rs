//! 生效路由计算（含**优先级**与**临时直连覆盖**）。
//!
//! mihomo 规则**首个命中生效**，故列表位置即优先级。这里把「内置 LAN 直连 + 临时例外 + 程序组 +
//! 用户规则 + 兜底 MATCH」按最终匹配顺序展开成结构化 [`RouteEntry`]（供 UI 看优先级/来源/删除），
//! 同一份顺序也渲染成 mihomo 规则行（[`to_lines`]），保证「看到的」与「跑的」一致。

use crate::config::{
    DecryptDivert, NetPolicySettings, ProcessRef, Route, Rule, RuleSet, TempDirect,
};
use crate::egress::EgressRuntimeView;
use crate::types::RouteEntry;

/// loopback MITM outbound 代理名（L4 解密自动导流，§17.3 方案 B）。
pub const MITM_OUT: &str = "mitm-out";

/// 内置内网/保留段直连（避免依赖 geoip 数据库）。始终最高优先，保证本机/局域网互访不受策略影响。
const BUILTIN_LAN: [&str; 6] = [
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "224.0.0.0/4",
];

fn entry(kind: &str, value: &str, route: Route, source: &str, deletable: bool) -> RouteEntry {
    RouteEntry {
        priority: 0,
        kind: kind.into(),
        value: value.into(),
        route,
        applied_route: None,
        source: source.into(),
        deletable,
    }
}

/// 计算生效路由（全部出口视为可用）。等价于 [`effective_routes_with`] 传
/// [`EgressRuntimeView::all_available`]。
pub fn effective_routes(
    settings: &NetPolicySettings,
    rules: &RuleSet,
    temp: &TempDirect,
) -> Vec<RouteEntry> {
    effective_routes_with(settings, rules, temp, &EgressRuntimeView::all_available())
}

/// 计算生效路由（按匹配顺序；`priority` 已填）。顺序：内置 LAN → 临时例外(Blackhole) → 程序组 →
/// 用户规则 → 兜底 MATCH（temp 激活时兜底 = DIRECT）。
///
/// `view` 是出口运行态：被停用/不可用的出口，其规则的 `applied_route` 会按 fallback 改写
/// （默认 `Blackhole` fail-closed，**不隐式回落直连**，设计 §6.1/§8.5）。
pub fn effective_routes_with(
    settings: &NetPolicySettings,
    rules: &RuleSet,
    temp: &TempDirect,
    view: &EgressRuntimeView,
) -> Vec<RouteEntry> {
    let mut out: Vec<RouteEntry> = Vec::new();

    for cidr in BUILTIN_LAN {
        out.push(entry("ip-cidr", cidr, Route::Direct, "builtin_lan", false));
    }

    // 临时直连时，例外进程强制 Blackhole（放在 LAN 之后、其它规则之前，优先级高但不挡本机互访）。
    if temp.active {
        for p in &temp.except {
            match p {
                ProcessRef::ProcessPath(v) => out.push(entry(
                    "process-path",
                    v,
                    Route::Blackhole,
                    "temp_except",
                    false,
                )),
                ProcessRef::ProcessName(v) => out.push(entry(
                    "process-name",
                    v,
                    Route::Blackhole,
                    "temp_except",
                    false,
                )),
            }
        }
    }

    for g in &rules.groups {
        for v in &g.root_paths {
            out.push(entry("process-path", v, g.route, "group", false));
        }
        for c in &g.known_children {
            match c {
                ProcessRef::ProcessPath(v) => {
                    out.push(entry("process-path", v, g.route, "group", false))
                }
                ProcessRef::ProcessName(v) => {
                    out.push(entry("process-name", v, g.route, "group", false))
                }
            }
        }
    }

    for r in &rules.rules {
        let (kind, value) = rule_kind_value(r);
        out.push(entry(kind, value, r.route(), "rule", true));
    }

    // 兜底：temp 激活 → DIRECT；否则用户设定的默认出口。
    let default = if temp.active {
        Route::Direct
    } else {
        settings.default_route
    };
    out.push(entry("match", "", default, "default", false));

    for (i, e) in out.iter_mut().enumerate() {
        e.priority = i;
        let applied = view.resolve(e.route);
        // 只在真的被改写时填，保持「None = 未降级」的语义与旧客户端兼容。
        e.applied_route = (applied != e.route).then_some(applied);
    }
    out
}

fn rule_kind_value(r: &Rule) -> (&'static str, &str) {
    match r {
        Rule::ProcessPath { value, .. } => ("process-path", value),
        Rule::ProcessName { value, .. } => ("process-name", value),
        Rule::DomainSuffix { value, .. } => ("domain-suffix", value),
        Rule::DomainKeyword { value, .. } => ("domain-keyword", value),
        Rule::IpCidr { value, .. } => ("ip-cidr", value),
    }
}

/// 把一条 RouteEntry 渲染成 mihomo 规则行。**以 `applied_route` 为准**——出口不可用时渲染的是
/// 降级后的目标（默认 `REJECT-DROP`），保证「看到的降级」与「跑的规则」一致。
pub fn render_line(e: &RouteEntry) -> String {
    let ob = e.applied_route.unwrap_or(e.route).outbound();
    match e.kind.as_str() {
        "match" => format!("  - MATCH,{ob}"),
        "ip-cidr" => format!("  - IP-CIDR,{},{ob},no-resolve", e.value),
        "process-path" => format!("  - PROCESS-PATH,{},{ob}", e.value),
        "process-name" => format!("  - PROCESS-NAME,{},{ob}", e.value),
        "domain-suffix" => format!("  - DOMAIN-SUFFIX,{},{ob}", e.value),
        "domain-keyword" => format!("  - DOMAIN-KEYWORD,{},{ob}", e.value),
        _ => format!("  - MATCH,{ob}"),
    }
}

/// 生效路由渲染为 mihomo 规则行序列（全部出口视为可用）。
pub fn to_lines(settings: &NetPolicySettings, rules: &RuleSet, temp: &TempDirect) -> Vec<String> {
    to_lines_with(settings, rules, temp, &EgressRuntimeView::all_available())
}

/// 同 [`to_lines`]，但按出口运行态做 fail-closed 改写。
pub fn to_lines_with(
    settings: &NetPolicySettings,
    rules: &RuleSet,
    temp: &TempDirect,
    view: &EgressRuntimeView,
) -> Vec<String> {
    effective_routes_with(settings, rules, temp, view)
        .iter()
        .map(render_line)
        .collect()
}

/// L4 解密自动导流规则行（§17.3 方案 B / §17.7）。**置于所有用户/策略规则之上**（由
/// [`crate::mihomo::generate_config`] 拼在 rules 段最前），保证目标进程的 HTTP(S) 先被截获。
///
/// 生成的规则（每个目标进程）：
/// - `AND((PROCESS),(NETWORK,tcp),(DST-PORT,80|443)) → mitm-out`：只导流 TCP HTTP(S)，其余端口/
///   协议不动。防环：只匹配目标进程，MITM 上游连接由 agent 进程发起不会再命中。
/// - 仅当 `force_tcp_for_quic`：`AND((PROCESS),(NETWORK,udp),(DST-PORT,443),(DOMAIN-SUFFIX,d)) →
///   REJECT`（每个 allowlist 域名一条），逼 QUIC 回退 TCP；**严格限进程+域名**，不扩大为全机 UDP/443。
///
/// `active=false` 或无目标 → 空（不注入任何规则，行为与未启用一致）。
pub fn divert_lines(divert: &DecryptDivert) -> Vec<String> {
    if !divert.active || divert.targets.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for t in &divert.targets {
        let (pk, pv) = match t {
            ProcessRef::ProcessPath(v) => ("PROCESS-PATH", v.as_str()),
            ProcessRef::ProcessName(v) => ("PROCESS-NAME", v.as_str()),
        };
        for domain in &divert.domains {
            for port in [80u16, 443] {
                out.push(format!(
                    "  - AND,(({pk},{pv}),(NETWORK,tcp),(DST-PORT,{port}),(DOMAIN-SUFFIX,{domain})),{MITM_OUT}"
                ));
            }
        }
        if divert.force_tcp_for_quic {
            for d in &divert.domains {
                out.push(format!(
                    "  - AND,(({pk},{pv}),(NETWORK,udp),(DST-PORT,443),(DOMAIN-SUFFIX,{d})),REJECT"
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_direct_overrides_default_and_blackholes_except() {
        let settings = NetPolicySettings {
            default_route: Route::Blackhole,
            ..Default::default()
        };
        let rules = RuleSet::default();
        let temp = TempDirect {
            active: true,
            except: vec![ProcessRef::ProcessName("secret.exe".into())],
        };
        let routes = effective_routes(&settings, &rules, &temp);
        // 兜底应变 DIRECT。
        let last = routes.last().unwrap();
        assert_eq!(last.kind, "match");
        assert_eq!(last.route, Route::Direct);
        // 例外进程应被 Blackhole。
        assert!(routes
            .iter()
            .any(|r| r.source == "temp_except" && r.route == Route::Blackhole));
        // 优先级连续。
        for (i, r) in routes.iter().enumerate() {
            assert_eq!(r.priority, i);
        }
    }

    #[test]
    fn divert_lines_empty_when_inactive() {
        assert!(divert_lines(&DecryptDivert::default()).is_empty());
        let inactive = DecryptDivert {
            active: false,
            targets: vec![ProcessRef::ProcessName("x.exe".into())],
            ..DecryptDivert::default()
        };
        assert!(divert_lines(&inactive).is_empty(), "active=false 不出规则");
    }

    #[test]
    fn divert_lines_process_path_variant_and_tcp_only_by_default() {
        let d = DecryptDivert {
            active: true,
            targets: vec![ProcessRef::ProcessPath(r"C:\app\a.exe".into())],
            domains: vec!["example.com".into()],
            mitm_port: 18081,
            force_tcp_for_quic: false,
            proxy_username: "net-policy".into(),
            proxy_password: "secret".into(),
        };
        let lines = divert_lines(&d);
        // ProcessPath → PROCESS-PATH；80/443 各一条；默认无 UDP REJECT。
        assert_eq!(lines.len(), 2);
        assert!(lines
            .iter()
            .any(|l| l.contains(r"(PROCESS-PATH,C:\app\a.exe)") && l.contains("(DST-PORT,443)")));
        assert!(lines.iter().all(|l| !l.contains("NETWORK,udp")));
    }

    #[test]
    fn user_rules_are_deletable_builtins_are_not() {
        let rules = RuleSet {
            rules: vec![Rule::DomainSuffix {
                value: "example.com".into(),
                route: Route::Direct,
            }],
            groups: vec![],
        };
        let routes = effective_routes(
            &NetPolicySettings::default(),
            &rules,
            &TempDirect::default(),
        );
        assert!(routes.iter().any(|r| r.source == "rule" && r.deletable));
        assert!(routes
            .iter()
            .all(|r| r.source != "builtin_lan" || !r.deletable));
    }
}
