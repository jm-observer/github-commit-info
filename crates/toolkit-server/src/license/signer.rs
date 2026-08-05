//! 续期私钥装配 + 签发（设计 §3.4/§6.2）。持有 renewal `SigningKey`——**这是本 crate 唯一
//! 允许持私钥的地方**，与 custom-utils 的 `issue.rs` 一样只在内存、绝不落日志/回显。
//!
//! renewal 钥本身**不 bake 进客户端**（设计 §2.4）：客户端只认「随响应带的 `TKDC1` 委托证书 +
//! 该证书对应的 root 锚」，所以这里签的续期 `TKL1`/`TKR1` 用的 kid 不必出现在任何 baked 表里，
//! 只要 `cert` 是一张有效的、由离线 `root` 签给这个 `renewal_kid` 的委托证书即可。

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use custom_utils::license::issue::{load_seed, sign_license, sign_tkr1};
use custom_utils::license::{LicensePayload, Tkr1Payload};
use ed25519_dalek::SigningKey;
use std::path::Path;

/// 装配好的续期私钥 + 委托证书。`Signer::from_env` 是唯一构造入口；三个 env 任一缺失即
/// 返回 `Ok(None)`（而非报错——未启用在线续期是一个受支持的运行形态，同 TTS/LLM 未配置）。
pub struct Signer {
    renewal_sk: SigningKey,
    renewal_kid: String,
    /// root 签给 `renewal_kid` 的 `TKDC1` 委托证书串（离线 `tklic delegate` 生成，见设计 §2.4）。
    /// 随每次 refresh 响应原样带给客户端，客户端凭它+baked root 锚验证 `renewal_kid` 有权签名。
    cert: String,
}

/// 环境变量名：renewal 私钥 seed 文件路径（`custom_utils::util_license::issue::load_seed`
/// 读取的明文 framed seed，`0600`；加密/保管在文件外，见设计 §2.2）。
pub const ENV_RENEWAL_SEED: &str = "LICENSE_RENEWAL_SEED";
/// 环境变量名：该 renewal 私钥对应的 kid（如 `renewal-1`）。**私钥文件本身不含 kid**，必须
/// 显式配对，否则签出来的信封里 kid 与证书里的 `sub_kid` 对不上，客户端会拒绝。
pub const ENV_RENEWAL_KID: &str = "LICENSE_RENEWAL_KID";
/// 环境变量名：root 签的 `TKDC1` 委托证书文件路径（纯文本 token 串，公钥非秘密，可与代码
/// 一起分发；但证书本身仍需从离线 `tklic delegate` 产出，服务端不生成）。
pub const ENV_RENEWAL_CERT: &str = "LICENSE_RENEWAL_CERT";

impl Signer {
    /// 从 [`ENV_RENEWAL_SEED`] / [`ENV_RENEWAL_KID`] / [`ENV_RENEWAL_CERT`] 三个环境变量装配。
    /// 三者齐全 → `Ok(Some(signer))`；**任一缺失（未设置）→ `Ok(None)`**（不 panic，调用方按
    /// `bootstrap()` 里的约定打一条 info 日志）。文件存在但内容非法（seed 格式错/读不到）→
    /// `Err`——这种情况视为配置错误，不能静默当作"未启用"，否则运维会以为续期在跑但实际没生效。
    pub fn from_env() -> Result<Option<Signer>> {
        let (seed_path, kid, cert_path) = match (
            std::env::var(ENV_RENEWAL_SEED).ok(),
            std::env::var(ENV_RENEWAL_KID).ok(),
            std::env::var(ENV_RENEWAL_CERT).ok(),
        ) {
            (Some(s), Some(k), Some(c))
                if !s.trim().is_empty() && !k.trim().is_empty() && !c.trim().is_empty() =>
            {
                (s, k, c)
            }
            _ => return Ok(None),
        };

        let renewal_sk = load_seed(Path::new(&seed_path)).with_context(|| {
            format!("加载续期私钥 seed 失败：{seed_path}（{ENV_RENEWAL_SEED}）")
        })?;
        let cert = std::fs::read_to_string(&cert_path)
            .with_context(|| format!("读取续期委托证书失败：{cert_path}（{ENV_RENEWAL_CERT}）"))?
            .trim()
            .to_string();
        if cert.is_empty() {
            anyhow::bail!("续期委托证书文件 {cert_path}（{ENV_RENEWAL_CERT}）为空");
        }

        Ok(Some(Signer {
            renewal_sk,
            renewal_kid: kid,
            cert,
        }))
    }

    /// 供测试直接注入一把内存生成的密钥 + 证书串，绕开 env/文件系统。
    #[cfg(test)]
    pub fn from_parts(renewal_sk: SigningKey, renewal_kid: String, cert: String) -> Signer {
        Signer {
            renewal_sk,
            renewal_kid,
            cert,
        }
    }

    pub fn renewal_kid(&self) -> &str {
        &self.renewal_kid
    }

    /// 签一次续期响应：内层 renewal 签的 `TKL1` + 外层 `TKR1` 封套（设计 §3.4/§6.2）。
    /// `payload` 的全部字段须已经过调用方（`routes::refresh`）按台账 + 锚字段规则算好——本函数
    /// 只管签名，不做任何业务校验（那属于路由层，见 `routes.rs` 的注释）。
    pub fn sign_refresh(
        &self,
        payload: &LicensePayload,
        server_time: DateTime<Utc>,
        echo_nonce: String,
    ) -> Result<RefreshResponse> {
        let license = sign_license(&self.renewal_sk, &self.renewal_kid, payload)
            .context("签发续期 TKL1 失败")?;
        let tkr1_payload = Tkr1Payload {
            ver: 1,
            license,
            server_time,
            echo_nonce,
        };
        let tkr1 = sign_tkr1(&self.renewal_sk, &self.renewal_kid, &tkr1_payload)
            .context("签发 TKR1 响应封套失败")?;
        Ok(RefreshResponse {
            tkr1,
            cert: self.cert.clone(),
        })
    }
}

/// `/api/license/refresh` 成功响应体：`tkr1` = 双层签名（内层续期 TKL1 + 外层 TKR1 封套）的
/// 完整 token 串；`cert` = 随附的 root 签发委托证书（客户端两跳链验证的第一跳）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RefreshResponse {
    pub tkr1: String,
    pub cert: String,
}
