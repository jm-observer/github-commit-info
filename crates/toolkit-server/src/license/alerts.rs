//! 软件授权 · 临期邮件提醒（设计 `docs/license-impl-design.md` §4.3/§7）。
//!
//! **纯带外提醒**：这里发的邮件不参与任何授权判定，可丢可伪造，也不驱动客户端/服务端的任何
//! 授权状态——它只是"提醒人去操作"（续期/联系客户）。真正的信任判断全在 `license::routes`/
//! custom-utils 的验签与 `licenses` 台账里，本模块不碰那些。
//!
//! 配置全走环境变量，**未配置就不发**（同 TTS/LLM 未配置时的既有约定）：`SMTP_HOST` /
//! `SMTP_PORT` / `SMTP_USER` / `SMTP_PASS` / `SMTP_FROM` / `LICENSE_ALERT_TO`。缺
//! `SMTP_HOST`/`SMTP_FROM`/`LICENSE_ALERT_TO` 任一 → 提醒功能整体关闭（`from_env` 返回
//! `Ok(None)`，调用方打一条 info 日志，不 spawn 后台任务）。`SMTP_USER`/`SMTP_PASS`
//! 是一对：只设置其中一个视为配置错误（`Err`），因为多数 SMTP relay 需要成对的用户名/密码，
//! 只给一半大概率是打字漏了，不该被静默当成"匿名 relay"。

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use rusqlite::params;
use std::time::Duration;
use toolkit_core::SqlitePool;

use crate::license::store;

/// 命中检查的阈值（距 `business_deadline` 的剩余天数），降序排列，设计 §7 明确列出的五档。
pub const THRESHOLDS_DAYS: [i64; 5] = [30, 14, 7, 3, 1];

/// 两轮扫描间隔：24h（设计 §7「每天一次」）。
const SCAN_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// 首次启动后的延迟：避免和进程启动时的其它装配工作挤在一起。
const FIRST_RUN_DELAY: Duration = Duration::from_secs(5 * 60);

/// 环境变量名。
pub const ENV_SMTP_HOST: &str = "SMTP_HOST";
pub const ENV_SMTP_PORT: &str = "SMTP_PORT";
pub const ENV_SMTP_USER: &str = "SMTP_USER";
pub const ENV_SMTP_PASS: &str = "SMTP_PASS";
pub const ENV_SMTP_FROM: &str = "SMTP_FROM";
pub const ENV_LICENSE_ALERT_TO: &str = "LICENSE_ALERT_TO";

const DEFAULT_SMTP_PORT: u16 = 587;

/// 装配好的告警配置。`from_env` 是唯一构造入口；三态同 [`crate::license::Signer::from_env`]：
/// 齐全 → `Ok(Some)`；`SMTP_HOST`/`SMTP_FROM`/`LICENSE_ALERT_TO` 任一缺失 → `Ok(None)`（未启用，
/// 非错误）；设了但格式非法（端口不是数字、user/pass 只给一半）→ `Err`（配置错误，不能静默当
/// 未启用，否则运维会以为提醒在跑但实际没生效）。
#[derive(Debug, Clone)]
pub struct AlertConfig {
    host: String,
    port: u16,
    credentials: Option<Credentials>,
    from: Mailbox,
    alert_to: Mailbox,
}

impl AlertConfig {
    pub fn from_env() -> Result<Option<AlertConfig>> {
        let host = std::env::var(ENV_SMTP_HOST)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let from = std::env::var(ENV_SMTP_FROM)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let alert_to = std::env::var(ENV_LICENSE_ALERT_TO)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let (host, from, alert_to) = match (host, from, alert_to) {
            (Some(h), Some(f), Some(t)) => (h, f, t),
            _ => return Ok(None),
        };

        let port = match std::env::var(ENV_SMTP_PORT)
            .ok()
            .filter(|s| !s.trim().is_empty())
        {
            Some(p) => p
                .trim()
                .parse::<u16>()
                .with_context(|| format!("{ENV_SMTP_PORT}={p:?} 不是合法端口号"))?,
            None => DEFAULT_SMTP_PORT,
        };

        let user = std::env::var(ENV_SMTP_USER)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let pass = std::env::var(ENV_SMTP_PASS)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let credentials = match (user, pass) {
            (Some(u), Some(p)) => Some(Credentials::new(u, p)),
            (None, None) => None,
            _ => anyhow::bail!(
                "{ENV_SMTP_USER}/{ENV_SMTP_PASS} 必须成对设置（只给了一个，大概率是配置漏填）"
            ),
        };

        let from_mailbox: Mailbox = from
            .trim()
            .parse()
            .with_context(|| format!("{ENV_SMTP_FROM}={from:?} 不是合法邮箱地址"))?;
        let alert_to_mailbox: Mailbox = alert_to
            .trim()
            .parse()
            .with_context(|| format!("{ENV_LICENSE_ALERT_TO}={alert_to:?} 不是合法邮箱地址"))?;

        Ok(Some(AlertConfig {
            host,
            port,
            credentials,
            from: from_mailbox,
            alert_to: alert_to_mailbox,
        }))
    }

    fn build_transport(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.host)
            .with_context(|| format!("构造 SMTP relay 失败：{}", self.host))?
            .port(self.port);
        if let Some(creds) = self.credentials.clone() {
            builder = builder.credentials(creds);
        }
        Ok(builder.build())
    }
}

/// 给定到期日与当前时间，算距今剩余天数落在哪些阈值区间内（`days_left <= threshold`）。
/// 用 `<=` 而不是精确相等：定时任务每 24h 跑一次，若进程重启错过了某个精确的"第 N 天"，
/// `<=` 能在下一轮追上补发，代价是一次重启后的追赶轮可能对同一张 license 一口气命中多个
/// 阈值（各阈值仍各自只发一次，靠 `license_alerts` 去重表落库后不会再重复）。返回值按阈值从
/// 大到小排列（30 在前、1 在后），与 [`THRESHOLDS_DAYS`] 顺序一致。
pub fn thresholds_hit(business_deadline: DateTime<Utc>, now: DateTime<Utc>) -> Vec<i64> {
    let days_left = (business_deadline - now).num_days();
    THRESHOLDS_DAYS
        .iter()
        .copied()
        .filter(|&t| days_left <= t)
        .collect()
}

/// 去重表查询：`(lic_id, threshold_days)` 是否已经发过。
fn already_sent(pool: &SqlitePool, lic_id: &str, threshold_days: i64) -> Result<bool> {
    let conn = pool.get()?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM license_alerts WHERE lic_id = ?1 AND threshold_days = ?2",
        params![lic_id, threshold_days],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 去重表落库：标记 `(lic_id, threshold_days)` 已发送。仅在邮件真正发送成功后调用。
fn mark_sent(pool: &SqlitePool, lic_id: &str, threshold_days: i64) -> Result<()> {
    let conn = pool.get()?;
    conn.execute(
        "INSERT INTO license_alerts(lic_id, threshold_days, sent_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(lic_id, threshold_days) DO UPDATE SET sent_at = excluded.sent_at",
        params![lic_id, threshold_days, toolkit_core::now_iso8601()],
    )?;
    Ok(())
}

/// 启动一个每 24h 跑一次的后台任务，扫描 `licenses` 台账并按需发临期提醒。
/// `config` 由调用方在 `from_env()` 返回 `Some` 时才调用本函数；`None` 时根本不 spawn。
pub fn spawn_daily_scan(pool: SqlitePool, config: AlertConfig) {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_RUN_DELAY).await;
        loop {
            if let Err(e) = run_once(&pool, &config).await {
                log::warn!("license 临期提醒扫描失败: {e:#}");
            }
            tokio::time::sleep(SCAN_INTERVAL).await;
        }
    });
}

/// 跑一轮扫描：取全部未吊销 license → 逐条算命中阈值 → 逐阈值发信 + 去重落库。
/// 单条 license / 单个阈值失败不影响其它——发信失败只 warn，不写去重表（下一轮重试）。
async fn run_once(pool: &SqlitePool, config: &AlertConfig) -> Result<()> {
    let pool_for_list = pool.clone();
    let rows = tokio::task::spawn_blocking(move || store::list(&pool_for_list))
        .await
        .context("spawn_blocking store::list")??;

    let now = Utc::now();
    for row in rows {
        if row.revoked_at.is_some() {
            continue;
        }
        let deadline = match DateTime::parse_from_rfc3339(&row.business_deadline) {
            Ok(d) => d.with_timezone(&Utc),
            Err(e) => {
                log::warn!(
                    "license {} 的 business_deadline={:?} 解析失败，跳过临期检查：{e:#}",
                    row.lic_id,
                    row.business_deadline
                );
                continue;
            }
        };

        for threshold in thresholds_hit(deadline, now) {
            let lic_id = row.lic_id.clone();
            let pool_check = pool.clone();
            let sent = match tokio::task::spawn_blocking(move || {
                already_sent(&pool_check, &lic_id, threshold)
            })
            .await
            {
                Ok(Ok(sent)) => sent,
                Ok(Err(e)) => {
                    log::warn!(
                        "license {} 阈值 {threshold}d 去重表查询失败，跳过本次：{e:#}",
                        row.lic_id
                    );
                    continue;
                }
                Err(e) => {
                    log::warn!("license {} 去重表查询任务 join 失败：{e:#}", row.lic_id);
                    continue;
                }
            };
            if sent {
                continue;
            }

            let days_left = (deadline - now).num_days();
            match send_alert(config, &row, threshold, days_left, deadline).await {
                Ok(()) => {
                    let lic_id = row.lic_id.clone();
                    let pool_mark = pool.clone();
                    if let Err(e) = tokio::task::spawn_blocking(move || {
                        mark_sent(&pool_mark, &lic_id, threshold)
                    })
                    .await
                    .context("spawn_blocking mark_sent")?
                    {
                        log::warn!(
                            "license {} 阈值 {threshold}d 邮件已发但去重表落库失败（下一轮可能重发）：{e:#}",
                            row.lic_id
                        );
                    }
                }
                Err(e) => {
                    // 发信失败只 warn，不写去重表，下一轮重试（设计 §7）。
                    log::warn!(
                        "license {} 阈值 {threshold}d 临期提醒发送失败（下一轮重试）：{e:#}",
                        row.lic_id
                    );
                }
            }
        }
    }
    Ok(())
}

/// 发一封提醒邮件。收件人 = `LICENSE_ALERT_TO` + 该行非空 `contact_email`（后者格式非法则跳过
/// 该收件人、不影响发给 `LICENSE_ALERT_TO`）。内容纯文本，不含私钥/令牌/机器串等敏感信息。
async fn send_alert(
    config: &AlertConfig,
    row: &store::LicenseRow,
    threshold_days: i64,
    days_left: i64,
    deadline: DateTime<Utc>,
) -> Result<()> {
    let subject = format!(
        "[授权临期提醒] {} ({}) 还剩 {} 天到期",
        row.subject, row.lic_id, days_left
    );
    let body = format!(
        "license: {}\n客户: {}\n到期日(business_deadline): {}\n剩余天数: {}\n\n\
         请及时联系客户续期。\n\n（本邮件为自动带外提醒，不代表授权状态判定。）",
        row.lic_id,
        row.subject,
        deadline.to_rfc3339(),
        days_left,
    );

    let mut builder = Message::builder()
        .from(config.from.clone())
        .to(config.alert_to.clone())
        .subject(subject);

    if let Some(contact) = row
        .contact_email
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        match contact.trim().parse::<Mailbox>() {
            Ok(mailbox) => builder = builder.to(mailbox),
            Err(e) => log::warn!(
                "license {} 的 contact_email={contact:?} 不是合法邮箱，跳过该收件人：{e:#}",
                row.lic_id
            ),
        }
    }

    let email = builder.body(body).context("构造提醒邮件失败")?;
    let transport = config.build_transport()?;
    transport.send(email).await.with_context(|| {
        format!(
            "发送 license {} 阈值 {threshold_days}d 提醒邮件失败",
            row.lic_id
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(dir: &std::path::Path) -> SqlitePool {
        let p = toolkit_core::open_pool(&dir.join("t.db")).unwrap();
        toolkit_core::migrate(&p).unwrap();
        p
    }

    #[test]
    fn thresholds_hit_picks_all_at_or_below_days_left() {
        let now = Utc::now();
        // 40 天后到期：一个阈值都不命中。
        assert_eq!(
            thresholds_hit(now + chrono::Duration::days(40), now),
            Vec::<i64>::new()
        );
        // 恰好 30 天：命中 30。
        assert_eq!(
            thresholds_hit(now + chrono::Duration::days(30), now),
            vec![30]
        );
        // 20 天：仍只命中 30（14 更严格，20 > 14 不命中）。
        assert_eq!(
            thresholds_hit(now + chrono::Duration::days(20), now),
            vec![30]
        );
        // 10 天：命中 30、14。
        assert_eq!(
            thresholds_hit(now + chrono::Duration::days(10), now),
            vec![30, 14]
        );
        // 5 天：命中 30、14、7。
        assert_eq!(
            thresholds_hit(now + chrono::Duration::days(5), now),
            vec![30, 14, 7]
        );
        // 2 天：命中 30、14、7、3。
        assert_eq!(
            thresholds_hit(now + chrono::Duration::days(2), now),
            vec![30, 14, 7, 3]
        );
        // 0 天（今天到期）：全命中。
        assert_eq!(thresholds_hit(now, now), vec![30, 14, 7, 3, 1]);
        // 已过期（负数）：全命中（仍要提醒，事已过期更该催）。
        assert_eq!(
            thresholds_hit(now - chrono::Duration::days(5), now),
            vec![30, 14, 7, 3, 1]
        );
    }

    #[test]
    fn dedup_table_crud() {
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path());

        assert!(!already_sent(&p, "L-1", 30).unwrap());
        mark_sent(&p, "L-1", 30).unwrap();
        assert!(already_sent(&p, "L-1", 30).unwrap());
        // 不同阈值互不影响。
        assert!(!already_sent(&p, "L-1", 14).unwrap());
        // 不同 lic_id 互不影响。
        assert!(!already_sent(&p, "L-2", 30).unwrap());

        // 重复 mark_sent 幂等（更新 sent_at，不报错、不产生第二行）。
        mark_sent(&p, "L-1", 30).unwrap();
        let conn = p.get().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM license_alerts WHERE lic_id = 'L-1' AND threshold_days = 30",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn from_env_none_when_unset() {
        // 不并发跑（env 是进程全局），单测内顺序执行足够。
        for k in [
            ENV_SMTP_HOST,
            ENV_SMTP_PORT,
            ENV_SMTP_USER,
            ENV_SMTP_PASS,
            ENV_SMTP_FROM,
            ENV_LICENSE_ALERT_TO,
        ] {
            std::env::remove_var(k);
        }
        assert!(AlertConfig::from_env().unwrap().is_none());
    }
}
