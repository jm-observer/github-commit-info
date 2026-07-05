//! egress-tester —— 出口代理 worker 功能自检工具(F3)。
//!
//! 独立测试程序,用公共 client crate `egress-client` 连 controller(`toolkit-server`),
//! 针对出口代理 worker 跑一遍功能自检:
//!
//! 1. 列 worker(交叉核对在线数 / 出口 IP)。
//! 2. 匿名短租 + IP 轮换探测。
//! 3. 具名 session 钉死(同一 session 内 IP 不变;同账号复用同一 worker)。
//! 4. 混合(session 钉死 + 匿名轮换交错)展示。
//! 5. 释放 session。
//! 6. 汇总 PASS/FAIL,任一 FAIL 则非零退出码。
//!
//! 这是诊断工具,不是 CLI 契约工具:输出走 stdout 人类可读文本(`println!`),不是 JSON。

use std::collections::BTreeSet;

use anyhow::Result;
use clap::Parser;
use egress_client::{EgressClient, EgressResponse};

#[derive(Parser)]
#[command(
    name = "egress-tester",
    version,
    about = "出口代理 worker 功能自检(取外网 IP / 匿名短租轮换 / 具名 session 钉死 / 混合)"
)]
struct Args {
    /// controller 基址,如 http://127.0.0.1:8788
    #[arg(long, env = "EGRESS_CONTROLLER")]
    controller: String,
    /// 共享鉴权 token(与 controller 的 EGRESS_WORKER_TOKEN 一致;不传则不带)
    #[arg(long, env = "EGRESS_WORKER_TOKEN")]
    token: Option<String>,
    /// 用于回显外网 IP 的目标 URL(该 URL 直接回显调用方公网 IP)
    #[arg(long, default_value = "https://api.ipify.org")]
    url: String,
    /// session 测试用的类型标签
    #[arg(long, default_value = "tester")]
    typ: String,
    /// session 测试用的具名账号
    #[arg(long, default_value = "probe-acc")]
    account: String,
    /// 匿名轮换探测轮数;不传则取「在线 worker 数 × 2」(至少 2)
    #[arg(long)]
    rounds: Option<usize>,
}

/// 汇总用的一条断言结果。
struct Check {
    name: &'static str,
    pass: bool,
    detail: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = EgressClient::new(args.controller.clone(), args.token.clone());

    println!("========================================");
    println!("egress-tester —— 出口代理功能自检");
    println!("controller = {}", args.controller);
    println!("回显 URL   = {}", args.url);
    println!("========================================\n");

    let mut checks: Vec<Check> = Vec::new();

    // ---------- 1. 列 worker ----------
    println!("【第 1 步】列 worker");
    let workers = client.workers().await?;
    let mut online_count = 0usize;
    let mut reported_ips: BTreeSet<String> = BTreeSet::new();
    for w in &workers {
        if w.online {
            online_count += 1;
        }
        reported_ips.insert(w.egress_ip.clone());
        println!(
            "  - id={} egress_ip={} online={} 最近心跳={}s前",
            w.id, w.egress_ip, w.online, w.seconds_since_heartbeat
        );
    }
    println!("  共 {} 台,其中在线 {} 台\n", workers.len(), online_count);

    if online_count == 0 {
        eprintln!("[FAIL] 无在线 worker,请先起 toolkit-worker 再跑本工具。");
        std::process::exit(1);
    }

    // ---------- 2. 匿名短租 + IP 轮换 ----------
    let rounds = args.rounds.unwrap_or_else(|| (online_count * 2).max(2));
    println!("【第 2 步】匿名短租 + IP 轮换探测(共 {} 轮)", rounds);
    let mut anon_ips: Vec<String> = Vec::new();
    let mut anon_all_ok = true;
    for i in 1..=rounds {
        match client.fetch("GET", &args.url, vec![], None).await {
            Ok(resp) => {
                let ip = extract_ip(&resp);
                println!("  第 {i} 轮: ok status={} ip={:?}", resp.status, ip);
                if let Some(ip) = ip {
                    anon_ips.push(ip);
                }
            }
            Err(e) => {
                anon_all_ok = false;
                println!("  第 {i} 轮: 失败 —— {e}");
            }
        }
    }
    let unique_ips: BTreeSet<String> = anon_ips.iter().cloned().collect();
    println!("  去重后的出口 IP 集合: {:?}", unique_ips);
    println!(
        "  观测: {} 轮成功回显 IP,{} 个不同 IP({}台在线 worker)",
        anon_ips.len(),
        unique_ips.len(),
        online_count
    );
    // 与第 1 步上报的 egress_ip 交叉核对(仅信息性,不作硬失败)。
    let matched: Vec<&String> = unique_ips.intersection(&reported_ips).collect();
    if matched.is_empty() {
        println!(
            "  交叉核对: 观测到的 IP 与 worker 上报的 egress_ip 均不匹配(本机跑时上报 IP \
             可能是占位,仅信息性,不影响判定)"
        );
    } else {
        println!(
            "  交叉核对: 有 {} 个 IP 与 worker 上报的 egress_ip 一致: {:?}",
            matched.len(),
            matched
        );
    }
    checks.push(Check {
        name: "匿名短租全部成功",
        pass: anon_all_ok,
        detail: format!("{rounds} 轮中成功 {} 轮", anon_ips.len()),
    });
    println!();

    // ---------- 3. 具名 session 钉死 ----------
    println!(
        "【第 3 步】具名 session 钉死(typ={}, account={})",
        args.typ, args.account
    );
    let session1 = client.session(&args.typ, Some(&args.account)).await?;
    println!("  拿到 session1,worker_id={}", session1.worker_id());
    let r1 = session1.fetch("GET", &args.url, vec![], None).await?;
    let ip1 = extract_ip(&r1);
    println!("  session1 第 1 次请求: ip={:?}", ip1);
    let r2 = session1.fetch("GET", &args.url, vec![], None).await?;
    let ip2 = extract_ip(&r2);
    println!("  session1 第 2 次请求: ip={:?}", ip2);

    let pinned_same_ip = ip1.is_some() && ip1 == ip2;
    checks.push(Check {
        name: "同一 session 内两次请求 IP 相同(钉死)",
        pass: pinned_same_ip,
        detail: format!("ip1={ip1:?} ip2={ip2:?}"),
    });

    let session2 = client.session(&args.typ, Some(&args.account)).await?;
    println!(
        "  再次申请同账号 session2,worker_id={}",
        session2.worker_id()
    );
    let same_worker = session1.worker_id() == session2.worker_id();
    checks.push(Check {
        name: "具名身份复用同一 worker",
        pass: same_worker,
        detail: format!(
            "session1.worker_id={} session2.worker_id={}",
            session1.worker_id(),
            session2.worker_id()
        ),
    });
    println!();

    // ---------- 4. 混合(hybrid) ----------
    println!("【第 4 步】混合探测(session 钉死 + 匿名轮换交错)");
    for i in 1..=4 {
        if i % 2 == 1 {
            let r = session1.fetch("GET", &args.url, vec![], None).await;
            match r {
                Ok(resp) => println!("  第 {i} 轮 [session]: ip={:?}", extract_ip(&resp)),
                Err(e) => println!("  第 {i} 轮 [session]: 失败 —— {e}"),
            }
        } else {
            let r = client.fetch("GET", &args.url, vec![], None).await;
            match r {
                Ok(resp) => println!("  第 {i} 轮 [匿名]: ip={:?}", extract_ip(&resp)),
                Err(e) => println!("  第 {i} 轮 [匿名]: 失败 —— {e}"),
            }
        }
    }
    println!("  说明: session 那几轮 IP 应恒定,匿名那几轮可能轮换。\n");

    // ---------- 5. 释放 session ----------
    println!("【第 5 步】释放 session");
    session1.release().await?;
    println!("  session1 已释放");
    session2.release().await?;
    println!("  session2 已释放\n");

    // ---------- 6. 汇总 ----------
    println!("========================================");
    println!("汇总");
    println!("========================================");
    let mut any_fail = false;
    for c in &checks {
        let mark = if c.pass { "PASS" } else { "FAIL" };
        if !c.pass {
            any_fail = true;
        }
        println!("  [{mark}] {} —— {}", c.name, c.detail);
    }

    if any_fail {
        eprintln!("\n存在 FAIL 项,自检未通过。");
        std::process::exit(1);
    } else {
        println!("\n全部通过。");
        Ok(())
    }
}

/// 从 `EgressResponse.body` 里取出回显的 IP(去除首尾空白;`api.ipify.org` 直接回纯文本 IP)。
fn extract_ip(resp: &EgressResponse) -> Option<String> {
    resp.body
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
