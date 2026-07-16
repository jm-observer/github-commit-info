//! 生效路由计算（含**优先级**与**临时直连覆盖**）。
//!
//! mihomo 规则**首个命中生效**，故列表位置即优先级。这里把「内置 LAN 直连 + 临时例外 + 程序组 +
//! 用户规则 + 兜底 MATCH」按最终匹配顺序展开成结构化 [`RouteEntry`]（供 UI 看优先级/来源/删除），
//! 同一份顺序也渲染成 mihomo 规则行（[`to_lines`]），保证「看到的」与「跑的」一致。

use crate::config::{NetPolicySettings, ProcessRef, Route, Rule, RuleSet, TempDirect};
use crate::types::RouteEntry;

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
        source: source.into(),
        deletable,
    }
}

/// 计算生效路由（按匹配顺序；`priority` 已填）。顺序：内置 LAN → 临时例外(Blackhole) → 程序组 →
/// 用户规则 → 兜底 MATCH（temp 激活时兜底 = DIRECT）。
pub fn effective_routes(
    settings: &NetPolicySettings,
    rules: &RuleSet,
    temp: &TempDirect,
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

/// 把一条 RouteEntry 渲染成 mihomo 规则行。
pub fn render_line(e: &RouteEntry) -> String {
    let ob = e.route.outbound();
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

/// 生效路由渲染为 mihomo 规则行序列。
pub fn to_lines(settings: &NetPolicySettings, rules: &RuleSet, temp: &TempDirect) -> Vec<String> {
    effective_routes(settings, rules, temp)
        .iter()
        .map(render_line)
        .collect()
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
