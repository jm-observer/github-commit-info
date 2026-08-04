//! remote-exec 第一期：worker 侧「命令执行面」网络循环。
//!
//! 与文件顶部 egress 面（借出口）共享稳定 `worker_id`，但**凭据、路由、循环、审计全部独立**：
//!
//! - egress 面：`/api/internal/egress/*`，共享 token（`x-egress-token`），可并发代发。
//! - exec 面：`/api/internal/exec/*`，per-worker 专属 secret（`x-exec-secret` +
//!   `x-worker-id` + `x-instance-id`），第一期单任务槽（同一时刻只执行一条，不并发）。
//!
//! 两面各自 `tokio::spawn` 独立跑，互不阻塞；进程级 Ctrl+C 统一处理（先杀 exec 在跑的进程树，
//! 再退出进程——egress 面没有需要显式清理的状态）。
//!
//! 默认完全不启用：只有显式传 `--allow-exec` 才会走到本模块的任何网络请求。

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use worker_core::proto::{
    self, ExecHeartbeatReq, ExecRegisterReq, ExecResponse, HDR_EXEC_SECRET, HDR_INSTANCE_ID,
    HDR_WORKER_ID,
};
use worker_core::{ExecRequest, Executor};

/// exec 长轮询客户端超时（须大于 controller 端 25s 挂起上限，留出网络往返余量）。
const EXEC_LONG_POLL_TIMEOUT: Duration = Duration::from_secs(40);
/// exec 心跳间隔（设计 §5.1）。
const EXEC_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
/// result 上传失败时的有限重试次数。
const RESULT_UPLOAD_RETRIES: u32 = 3;

/// exec 面启动参数，由 `main.rs` 的 CLI 解析后传入。
pub struct ExecOpts {
    /// controller 基址（与 egress 面共用同一个 `--controller`，已在上层 trim 尾部 `/`）。
    pub controller: String,
    /// exec 专用密钥明文（从 `--exec-secret-file` 读出并 trim）。
    pub secret: String,
    /// worker id（与 egress 面共用同一个稳定 id）。
    pub worker_id: String,
    /// exec 工作根目录（临时脚本 + 本地审计落这里）。
    pub root: PathBuf,
}

/// 校验「启用 exec 时 controller 必须是 https，仅 loopback 主机允许 http」（设计 §4.4）。
///
/// 返回 `Ok(())` 表示放行；否则错误信息里带上具体 URL 便于排查。
pub fn validate_exec_controller(controller: &str) -> Result<()> {
    let url = reqwest::Url::parse(controller)
        .with_context(|| format!("controller 不是合法 URL: {controller}"))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            // `Url::host_str()` 对 IPv6 主机返回带方括号的形式（如 `[::1]`），解析成
            // `IpAddr` 前需要先去掉方括号。
            let is_loopback = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
                || url
                    .host_str()
                    .map(|h| h.trim_start_matches('[').trim_end_matches(']'))
                    .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                    .map(|ip| ip.is_loopback())
                    .unwrap_or(false);
            if is_loopback {
                Ok(())
            } else {
                bail!(
                    "启用 --allow-exec 时 controller 必须是 https://（仅 127.0.0.1/localhost 允许 http://）: {controller}"
                );
            }
        }
        other => bail!("controller URL scheme 不支持: {other} ({controller})"),
    }
}

/// exec 面主入口：register → 心跳(独立 task) → 单任务长轮询循环。
///
/// `ctrl_c_rx` 用于收到 Ctrl+C 时优雅结束（由 `main.rs` 的信号处理触发）——本函数内部也会
/// 自行监听 `tokio::signal::ctrl_c()`，`main.rs` 里对 exec 面无需重复传信号，直接
/// `tokio::spawn` 本函数即可，Ctrl+C 到达时函数自行 kill_all 后返回。
pub async fn run_exec_loop(opts: ExecOpts) -> Result<()> {
    let ExecOpts {
        controller,
        secret,
        worker_id,
        root,
    } = opts;

    validate_exec_controller(&controller)?;

    let executor = Arc::new(
        Executor::new(&root).with_context(|| format!("初始化 exec 执行器失败: {root:?}"))?,
    );
    let instance_id = uuid::Uuid::new_v4().to_string();
    let client = reqwest::Client::new();

    let ctx = Arc::new(ExecCtx {
        controller: controller.trim_end_matches('/').to_string(),
        secret,
        worker_id,
        instance_id,
        client,
    });

    // 首次注册，失败则重试（与 egress 面首次 register 的取舍一致）。
    loop {
        match register(&ctx).await {
            Ok(()) => {
                log::info!("[exec] registered instance_id={}", ctx.instance_id);
                break;
            }
            Err(e) => {
                log::warn!("[exec] register failed: {e:#}; retry in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }

    // 心跳后台任务：401 → 停止 exec 循环（凭据被吊销）；404 → 重新 register。
    let heartbeat_stop = Arc::new(tokio::sync::Notify::new());
    {
        let ctx = ctx.clone();
        let stop = heartbeat_stop.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(EXEC_HEARTBEAT_INTERVAL) => {}
                    _ = stop.notified() => break,
                }
                match heartbeat(&ctx).await {
                    Ok(HeartbeatOutcome::Ok) => {}
                    Ok(HeartbeatOutcome::NeedReregister) => {
                        log::warn!("[exec] heartbeat 404 → re-register");
                        if let Err(e) = register(&ctx).await {
                            log::warn!("[exec] re-register after heartbeat 404 failed: {e:#}");
                        }
                    }
                    Ok(HeartbeatOutcome::Revoked) => {
                        log::error!("[exec] heartbeat 401 → 凭据已吊销，停止 exec 心跳");
                        break;
                    }
                    Err(e) => log::warn!("[exec] heartbeat error: {e:#}"),
                }
            }
        });
    }

    // Ctrl+C：杀当前进程树后结束 exec 循环。main 里对整个进程的退出仍是常规路径，
    // 这里只负责 exec 自己的收尾（egress 面没有需要清理的进程状态）。
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let next_url = format!(
        "{}/api/internal/exec/next?worker_id={}",
        ctx.controller, ctx.worker_id
    );

    loop {
        let poll = ctx
            .client
            .get(&next_url)
            .header(HDR_WORKER_ID, &ctx.worker_id)
            .header(HDR_EXEC_SECRET, &ctx.secret)
            .header(HDR_INSTANCE_ID, &ctx.instance_id)
            .timeout(EXEC_LONG_POLL_TIMEOUT)
            .send();

        tokio::select! {
            _ = &mut ctrl_c => {
                log::info!("[exec] Ctrl+C 收到，杀当前进程树后退出整个 worker 进程");
                executor.kill_all();
                heartbeat_stop.notify_one();
                // 设计 §6.2/§8.2 要求 Ctrl+C 杀树后退出进程,而不仅是结束 exec
                // 循环——egress 面的主循环没有自己的信号处理,一旦本任务注册了
                // `tokio::signal::ctrl_c()`,进程默认的 SIGINT 终止行为就被接管,
                // 所以这里必须显式 `process::exit`,否则 egress 面会继续裸跑。
                std::process::exit(0);
            }
            resp = poll => {
                match resp {
                    Ok(r) => {
                        let status = r.status().as_u16();
                        match status {
                            200 => match r.json::<ExecRequest>().await {
                                Ok(req) => {
                                    // 单任务：本次 next 到收到结果为止，不再发起下一次 next。
                                    let outcome = handle_one(&executor, &req).await;
                                    if let Err(e) = post_result(&ctx, &outcome).await {
                                        log::warn!(
                                            "[exec] post result failed after retries: {e:#}"
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::warn!("[exec] bad ExecRequest payload: {e}");
                                }
                            },
                            204 => { /* 空转，立即再轮询 */ }
                            404 => {
                                log::warn!("[exec] next 404 → re-register");
                                if let Err(e) = register(&ctx).await {
                                    log::warn!("[exec] re-register after next 404 failed: {e:#}");
                                }
                            }
                            409 => {
                                log::warn!("[exec] next 409 stale_instance → re-register");
                                if let Err(e) = register(&ctx).await {
                                    log::warn!("[exec] re-register after 409 failed: {e:#}");
                                }
                            }
                            401 => {
                                log::error!("[exec] next 401 → 凭据已吊销，停止 exec 循环");
                                heartbeat_stop.notify_one();
                                return Ok(());
                            }
                            other => {
                                log::warn!("[exec] next unexpected status {other}");
                                tokio::time::sleep(Duration::from_secs(2)).await;
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("[exec] long-poll error: {e}; retry in 3s");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                }
            }
        }
    }
}

/// 执行一条请求：先本地 `validate`，不过直接回传 `spawn_failed`；通过则交给 `Executor::run`。
async fn handle_one(executor: &Executor, req: &ExecRequest) -> ExecResponse {
    if let Err(e) = proto::validate(req) {
        log::warn!("[exec] request {} 未通过 validate: {e}", req.id);
        return ExecResponse::spawn_failed(req.id.clone(), format!("validate failed: {e}"), 0);
    }
    executor.run(req).await
}

/// exec 面共享上下文：controller 地址、凭据三件套、HTTP client。
struct ExecCtx {
    controller: String,
    secret: String,
    worker_id: String,
    instance_id: String,
    client: reqwest::Client,
}

async fn register(ctx: &ExecCtx) -> Result<()> {
    let (powershell, hostname) = detect_powershell_and_hostname().await;
    let body = ExecRegisterReq {
        worker_id: ctx.worker_id.clone(),
        instance_id: ctx.instance_id.clone(),
        powershell,
        hostname,
    };
    let url = format!("{}/api/internal/exec/register", ctx.controller);
    ctx.client
        .post(url)
        .header(HDR_WORKER_ID, &ctx.worker_id)
        .header(HDR_EXEC_SECRET, &ctx.secret)
        .header(HDR_INSTANCE_ID, &ctx.instance_id)
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("register 请求失败")?
        .error_for_status()
        .context("register 返回非成功状态")?;
    Ok(())
}

enum HeartbeatOutcome {
    Ok,
    NeedReregister,
    Revoked,
}

async fn heartbeat(ctx: &ExecCtx) -> Result<HeartbeatOutcome> {
    let body = ExecHeartbeatReq {
        worker_id: ctx.worker_id.clone(),
        instance_id: ctx.instance_id.clone(),
    };
    let url = format!("{}/api/internal/exec/heartbeat", ctx.controller);
    let resp = ctx
        .client
        .post(url)
        .header(HDR_WORKER_ID, &ctx.worker_id)
        .header(HDR_EXEC_SECRET, &ctx.secret)
        .header(HDR_INSTANCE_ID, &ctx.instance_id)
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("heartbeat 请求失败")?;
    match resp.status().as_u16() {
        404 => Ok(HeartbeatOutcome::NeedReregister),
        401 => Ok(HeartbeatOutcome::Revoked),
        _ => Ok(HeartbeatOutcome::Ok),
    }
}

/// 回传结果，失败有限重试（指数退避），最终失败只本地 warn（不拖垮主循环）。
async fn post_result(ctx: &ExecCtx, outcome: &ExecResponse) -> Result<()> {
    let url = format!("{}/api/internal/exec/result", ctx.controller);
    let mut backoff = Duration::from_millis(500);
    let mut last_err = None;
    for attempt in 1..=RESULT_UPLOAD_RETRIES {
        let res = ctx
            .client
            .post(&url)
            .header(HDR_WORKER_ID, &ctx.worker_id)
            .header(HDR_EXEC_SECRET, &ctx.secret)
            .header(HDR_INSTANCE_ID, &ctx.instance_id)
            .json(outcome)
            .timeout(Duration::from_secs(30))
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => {
                last_err = Some(anyhow::anyhow!("result 返回非成功状态: {}", r.status()));
            }
            Err(e) => {
                last_err = Some(anyhow::anyhow!(e).context("result 请求失败"));
            }
        }
        if attempt < RESULT_UPLOAD_RETRIES {
            tokio::time::sleep(backoff).await;
            backoff *= 2;
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("result 上传失败，原因未知")))
}

/// 探测本机 PowerShell 版本串（`$PSVersionTable.PSVersion` 简化取法），探测不到则 `None`；
/// 附带取 hostname。都是尽力而为，不影响 register 流程。
async fn detect_powershell_and_hostname() -> (Option<String>, Option<String>) {
    let powershell = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let hostname = hostname_best_effort();

    (powershell, hostname)
}

fn hostname_best_effort() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write;

    #[test]
    fn https_controller_ok() {
        assert!(validate_exec_controller("https://example.com").is_ok());
    }

    #[test]
    fn http_non_loopback_rejected() {
        let err = validate_exec_controller("http://example.com").unwrap_err();
        assert!(err.to_string().contains("https"));
    }

    #[test]
    fn http_loopback_allowed() {
        assert!(validate_exec_controller("http://127.0.0.1:8788").is_ok());
        assert!(validate_exec_controller("http://localhost:8788").is_ok());
    }

    #[test]
    fn http_ipv6_loopback_allowed() {
        assert!(validate_exec_controller("http://[::1]:8788").is_ok());
    }

    #[test]
    fn invalid_url_rejected() {
        assert!(validate_exec_controller("not a url").is_err());
    }
}
