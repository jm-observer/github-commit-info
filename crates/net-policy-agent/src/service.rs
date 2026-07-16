//! Windows 服务形态：以 LocalSystem 在 **Session 0** 常驻，**开机即起、不依赖用户登录**，根治
//! 「登录会话结束 → agent+mihomo 被杀 → 防火墙残留断网」(D-1)。SCM 通过 `run-service` 子命令拉起本入口。
//!
//! 要点：
//! - 管道 ACL 在服务(LocalSystem)下放行交互用户 IU（见 [`crate::security`]），GUI 才能跨会话连上；
//! - workspace 固定 `%ProgramData%\net-policy`（机器级，不属于任何用户 profile）；
//! - Stop/Shutdown：控制处理器**只发停止信号并返回 NoError**（关键：不能在返回前 exit，否则 SCM 判定
//!   异常终止 → CouldNotStopService）；主线程收到信号后报 Stopped 再退出。mihomo 作为孤儿子进程存活、
//!   防火墙/TUN 保持（与「关窗=保持」一致），服务下次启动由 `setup` 自动接管。

#[cfg(windows)]
pub fn run() -> anyhow::Result<()> {
    imp::run()
}

#[cfg(not(windows))]
pub fn run() -> anyhow::Result<()> {
    anyhow::bail!("net-policy-agent 服务仅支持 Windows")
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::sync::mpsc;
    use std::time::Duration;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;

    pub const SERVICE_NAME: &str = "net-policy-agent";

    windows_service::define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .map_err(|e| anyhow::anyhow!("service_dispatcher::start 失败：{e}"))
    }

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            log::error!("net-policy-agent 服务失败：{e:#}");
            std::process::exit(1);
        }
    }

    fn run_service() -> anyhow::Result<()> {
        // 控制处理器：Stop/Shutdown 只把信号送出并**立即返回 NoError**（SCM 得到"已受理"应答）；
        // 真正的退出在主线程收到信号后进行。
        let (tx, rx) = mpsc::channel::<()>();
        let handle =
            service_control_handler::register(SERVICE_NAME, move |control| match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = tx.send(());
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            })
            .map_err(|e| anyhow::anyhow!("注册服务控制器失败：{e}"))?;

        let set = |state: ServiceState, accept: ServiceControlAccept, wait_ms: u64| {
            handle.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accept,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::from_millis(wait_ms),
                process_id: None,
            })
        };

        set(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            30_000,
        )
        .map_err(|e| anyhow::anyhow!("上报 StartPending 失败：{e}"))?;

        // 机器级 workspace（服务不属任何用户 profile）。
        let ws = crate::paths::service_workspace()
            .to_string_lossy()
            .into_owned();

        // 只有收到“管道已创建”后才向 SCM 报 Running；启动失败则进程非零退出，让 failure action 重启。
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = crate::run_with_ready(false, Some(ws), Some(ready_tx))
                .map_err(|e| format!("{e:#}"));
            let _ = exit_tx.send(result);
        });

        match ready_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => {}
            Ok(Err(message)) => anyhow::bail!("agent 启动失败：{message}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                anyhow::bail!("agent 启动超时：30 秒内未创建控制面管道")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let detail = exit_rx
                    .try_recv()
                    .ok()
                    .and_then(Result::err)
                    .unwrap_or_else(|| "启动线程提前退出".to_string());
                anyhow::bail!("agent 启动失败：{detail}");
            }
        }

        set(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
        )
        .map_err(|e| anyhow::anyhow!("上报 Running 失败：{e}"))?;

        loop {
            if rx.try_recv().is_ok() {
                break;
            }
            match exit_rx.try_recv() {
                Ok(Ok(())) => anyhow::bail!("agent 主循环意外正常退出"),
                Ok(Err(message)) => anyhow::bail!("agent 主循环退出：{message}"),
                Err(mpsc::TryRecvError::Disconnected) => anyhow::bail!("agent 主循环线程断开"),
                Err(mpsc::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(200)),
            }
        }

        // 报 Stopped 后退出。mihomo 孤儿子进程存活、防火墙/TUN 保持；下次启动 setup 自动接管。
        let _ = set(ServiceState::Stopped, ServiceControlAccept::empty(), 0);
        std::process::exit(0);
    }
}
