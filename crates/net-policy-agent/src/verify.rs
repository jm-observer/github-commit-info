//! 出口与泄漏验证（§14.9 的 UI 化最小版）：出口 IP + DNS 劫持 + 引擎在线。

use crate::win::run_ps;
use anyhow::Result;
use net_policy_core::config::Route;
use net_policy_core::mihomo::CONTROLLER;
use net_policy_core::types::{VerifyCase, VerifyReport};

fn case(id: &str, name: &str, status: &str, observed: String) -> VerifyCase {
    VerifyCase {
        id: id.into(),
        name: name.into(),
        status: status.into(),
        observed,
    }
}

/// 执行轻量验证用例。`default_route` 决定各用例的**期望方向**（黑洞下出不去=通过）。
pub fn run(secret: &str, default_route: Route) -> Result<VerifyReport> {
    let mihomo_running = crate::engine::running(secret);
    let mut cases = Vec::new();

    match run_ps("try{ (Invoke-RestMethod https://api.ipify.org -TimeoutSec 10).Trim() }catch{ 'FAIL:'+$_.Exception.Message }")
    {
        Ok(ip) => {
            let ip = ip.trim().to_string();
            let reachable = !ip.starts_with("FAIL") && !ip.is_empty();
            let (status, observed) = match default_route {
                Route::Blackhole => {
                    if reachable {
                        ("failed", format!("{ip}（黑洞姿态下仍能出公网——存在泄漏！）"))
                    } else {
                        ("passed", "出口请求被阻断（黑洞姿态预期行为）".to_string())
                    }
                }
                _ => {
                    if reachable {
                        ("passed", ip)
                    } else {
                        ("failed", ip)
                    }
                }
            };
            cases.push(case("exit-ip", "当前公网出口 IP", status, observed));
        }
        Err(e) => cases.push(case("exit-ip", "当前公网出口 IP", "failed", format!("{e:#}"))),
    }

    match run_ps(
        "try{ ((Resolve-DnsName example.com -Type A -Server 8.8.8.8 -DnsOnly -EA Stop | Where-Object Type -eq A).IPAddress -join ',') }catch{ 'FAIL:'+$_.Exception.Message }",
    ) {
        Ok(ans) => {
            let ans = ans.trim().to_string();
            let status = if ans.contains("198.18.") {
                "passed"
            } else if ans.starts_with("FAIL") {
                "failed"
            } else {
                "unknown"
            };
            cases.push(case("dns-hijack", "DNS 劫持(fake-ip)", status, ans));
        }
        Err(e) => cases.push(case("dns-hijack", "DNS 劫持(fake-ip)", "failed", format!("{e:#}"))),
    }

    cases.push(case(
        "engine",
        "mihomo 控制器",
        if mihomo_running { "passed" } else { "failed" },
        format!("http://{CONTROLLER}/version"),
    ));

    Ok(VerifyReport {
        mihomo_running,
        cases,
    })
}
