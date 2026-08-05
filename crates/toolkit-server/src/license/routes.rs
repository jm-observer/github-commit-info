//! `/api/license/*`（公共面，免 Bearer，见 `auth::is_exempt`）+ `/api/web/license/*`
//! （管理面，走既有全局 `TOOLKIT_API_TOKEN` Bearer，与 `llm::routes` 同风格、不额外挂中间件）。
//!
//! 服务端**只签发、不验签**（设计 §6.2）：`refresh` 现签一份新的续期 `TKL1` + `TKR1` 封套，
//! 真正的信任判断留给客户端的 `evaluate`/`check_renewal_against_anchor`。这里的职责是：
//! - 台账定位 + 状态检查（存在 / 未吊销 / product 匹配 / 机器一致性）；
//! - **续期字段全部取自台账**，绝不采信请求体里除定位用途外的任何字段（避免客户端伪造扩权）；
//! - 限流 + 不记录敏感明细的审计日志。

use crate::license::store::{self, LicenseRow};
use crate::state::AppState;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use custom_utils::license::{
    decode_mreq1, issue::random_nonce, LicensePayload, MachineFingerprint,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;

// ============================================================
// 公共面：POST /api/license/refresh（免 Bearer，见 auth::is_exempt）
// ============================================================

pub fn public_router() -> Router<AppState> {
    Router::new().route("/refresh", post(refresh))
}

/// 请求体上限：`lic_id`/`product`/`machine_id`/`client_nonce` 都是短标识符，给一个宽松但
/// 有限的长度上限，防止调用方塞超大字符串进来占内存/占限流器 key 空间。
const MAX_FIELD_LEN: usize = 256;

#[derive(Debug, Deserialize)]
struct RefreshReq {
    lic_id: String,
    product: String,
    /// 仅用于定位 + 限流 + 一致性校验（该机是否在台账的 `machine_ids` 里），**不作鉴权**
    /// （设计 §6.2：`lic_id + machine + nonce` 不是自证或秘密）。
    #[serde(default)]
    machine_id: String,
    client_nonce: String,
    #[serde(default)]
    ver: Option<u32>,
}

#[derive(Debug, Serialize)]
struct RefreshOkBody {
    tkr1: String,
    cert: String,
}

enum RefreshError {
    NotConfigured,
    RateLimited,
    InvalidRequest(&'static str),
    NotFound,
    Revoked,
    ProductMismatch,
    MachineMismatch,
    SignFailed,
}

impl IntoResponse for RefreshError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            RefreshError::NotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "license_renewal_not_configured",
            ),
            RefreshError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            RefreshError::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            RefreshError::NotFound => (StatusCode::NOT_FOUND, "license_not_found"),
            RefreshError::Revoked => (StatusCode::FORBIDDEN, "revoked"),
            RefreshError::ProductMismatch => (StatusCode::BAD_REQUEST, "product_mismatch"),
            RefreshError::MachineMismatch => (StatusCode::BAD_REQUEST, "machine_mismatch"),
            RefreshError::SignFailed => (StatusCode::INTERNAL_SERVER_ERROR, "sign_failed"),
        };
        (status, Json(json!({ "error": error }))).into_response()
    }
}

async fn refresh(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<RefreshReq>,
) -> Result<Response, RefreshError> {
    let Some(signer) = state.license_signer.clone() else {
        return Err(RefreshError::NotConfigured);
    };

    // 限流先于一切业务逻辑（含 DB 查询）：每 IP + 每 lic_id 各一把滑动窗口，任一超限即拒绝。
    // lic_id 此时还未校验合法性，但作为限流 key 无妨——枚举 lic_id 打点本身就是限流要挡的对象。
    let ip_key = format!("ip:{}", peer.ip());
    let lic_key = format!("lic:{}", body.lic_id);
    if !state.license_rate_limiter.check(&ip_key) || !state.license_rate_limiter.check(&lic_key) {
        return Err(RefreshError::RateLimited);
    }

    let resp = do_refresh(&state, &signer, &body).await?;
    Ok((StatusCode::OK, Json(resp)).into_response())
}

/// 纯逻辑部分，脱离 axum 提取器，便于单测直接调用（见文件末尾 `#[cfg(test)]`）。
async fn do_refresh(
    state: &AppState,
    signer: &crate::license::Signer,
    body: &RefreshReq,
) -> Result<RefreshOkBody, RefreshError> {
    if body.lic_id.trim().is_empty() || body.lic_id.len() > MAX_FIELD_LEN {
        return Err(RefreshError::InvalidRequest("invalid lic_id"));
    }
    if body.product.trim().is_empty() || body.product.len() > MAX_FIELD_LEN {
        return Err(RefreshError::InvalidRequest("invalid product"));
    }
    if body.client_nonce.trim().is_empty() || body.client_nonce.len() > MAX_FIELD_LEN {
        return Err(RefreshError::InvalidRequest("invalid client_nonce"));
    }
    if body.machine_id.len() > MAX_FIELD_LEN {
        return Err(RefreshError::InvalidRequest("invalid machine_id"));
    }

    let pool = state.pool.clone();
    let lic_id = body.lic_id.clone();
    let row = tokio::task::spawn_blocking(move || store::get(&pool, &lic_id))
        .await
        .map_err(|_| RefreshError::SignFailed)?
        .map_err(|_| RefreshError::SignFailed)?;
    let Some(row) = row else {
        return Err(RefreshError::NotFound);
    };

    if row.revoked_at.is_some() {
        return Err(RefreshError::Revoked);
    }
    if row.product != body.product {
        return Err(RefreshError::ProductMismatch);
    }
    // 一致性校验（不是鉴权边界，见模块文档）：非空 machine_id 必须命中台账里的某条机器指纹 id。
    if !body.machine_id.is_empty() && !row.machine_ids.iter().any(|m| m.id == body.machine_id) {
        return Err(RefreshError::MachineMismatch);
    }

    let not_before = parse_dt(&row.not_before).map_err(|_| RefreshError::SignFailed)?;
    let business_deadline =
        parse_dt(&row.business_deadline).map_err(|_| RefreshError::SignFailed)?;

    let now = Utc::now();
    let expires_at = std::cmp::min(
        now + ChronoDuration::days(row.grant_window_days.max(0)),
        business_deadline,
    );
    let lease_until = row
        .lease_days
        .map(|d| std::cmp::min(now + ChronoDuration::days(d.max(0)), business_deadline));

    let payload = LicensePayload {
        ver: body.ver.unwrap_or(1),
        product: row.product.clone(),
        lic_id: row.lic_id.clone(),
        subject: row.subject.clone(),
        machine: row.machine_ids.clone(),
        issued_at: now,
        not_before,
        business_deadline,
        expires_at,
        lease_until,
        grace_days: row.grace_days,
        features: row.features.clone(),
        max_version: row.max_version.clone(),
        nonce: random_nonce(),
    };

    let signed = signer
        .sign_refresh(&payload, now, body.client_nonce.clone())
        .map_err(|e| {
            log::warn!(
                "license refresh: sign failed for lic_id={}: {e:#}",
                row.lic_id
            );
            RefreshError::SignFailed
        })?;

    log::info!(
        "license refresh ok: lic_id={} expires_at={} lease_until={:?}",
        row.lic_id,
        expires_at,
        lease_until
    );

    Ok(RefreshOkBody {
        tkr1: signed.tkr1,
        cert: signed.cert,
    })
}

fn parse_dt(s: &str) -> anyhow::Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

// ============================================================
// 管理面：/api/web/license/*（Bearer，见 auth::is_exempt 未豁免此前缀）
// ============================================================

pub fn admin_router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_licenses).post(create_license))
        .route(
            "/{lic_id}",
            axum::routing::put(update_license).delete(delete_license),
        )
        .route("/{lic_id}/revoke", post(revoke_license))
}

fn admin_err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

async fn list_licenses(State(state): State<AppState>) -> Response {
    let pool = state.pool.clone();
    match tokio::task::spawn_blocking(move || store::list(&pool)).await {
        Ok(Ok(rows)) => (StatusCode::OK, Json(json!({ "licenses": rows }))).into_response(),
        _ => admin_err(StatusCode::INTERNAL_SERVER_ERROR, "list_failed"),
    }
}

/// `POST /api/web/license` 请求体：新建/覆盖一份授权台账。`machine_requests` 是客户端导出的
/// `MREQ1...` 串数组（设计 §5），服务端解码成 `MachineFingerprint` 再落库——**不接受调用方
/// 直接传 `{id, components}` 结构**，统一走 MREQ1 入口，避免两条解析路径互相漂移。
#[derive(Debug, Deserialize)]
struct CreateReq {
    lic_id: String,
    product: String,
    subject: String,
    #[serde(default)]
    contact_email: Option<String>,
    machine_requests: Vec<String>,
    not_before: DateTime<Utc>,
    business_deadline: DateTime<Utc>,
    grant_window_days: i64,
    #[serde(default)]
    lease_days: Option<i64>,
    #[serde(default)]
    grace_days: Option<i64>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    max_version: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

async fn create_license(State(state): State<AppState>, Json(body): Json<CreateReq>) -> Response {
    if body.lic_id.trim().is_empty() {
        return admin_err(StatusCode::UNPROCESSABLE_ENTITY, "lic_id required");
    }
    if body.product.trim().is_empty() || body.subject.trim().is_empty() {
        return admin_err(StatusCode::UNPROCESSABLE_ENTITY, "product/subject required");
    }
    if body.machine_requests.is_empty() {
        // 设计 §3.2："面向客户签发时 machine 不能为空；一期不支持不绑机授权"。
        return admin_err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "machine_requests must not be empty",
        );
    }
    if body.not_before > body.business_deadline {
        return admin_err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_before must be <= business_deadline",
        );
    }

    let mut machine_ids: Vec<MachineFingerprint> = Vec::with_capacity(body.machine_requests.len());
    for mreq in &body.machine_requests {
        match decode_mreq1(mreq) {
            Ok(fp) => machine_ids.push(fp),
            Err(e) => {
                return admin_err(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("bad machine_requests entry: {e}"),
                )
            }
        }
    }

    let pool = state.pool.clone();
    let lic_id = body.lic_id.clone();
    let existing = match tokio::task::spawn_blocking({
        let pool = pool.clone();
        let lic_id = lic_id.clone();
        move || store::get(&pool, &lic_id)
    })
    .await
    {
        Ok(Ok(r)) => r,
        _ => return admin_err(StatusCode::INTERNAL_SERVER_ERROR, "lookup_failed"),
    };

    let row = LicenseRow {
        lic_id: body.lic_id,
        product: body.product,
        subject: body.subject,
        contact_email: body.contact_email,
        machine_ids,
        not_before: body.not_before.to_rfc3339(),
        business_deadline: body.business_deadline.to_rfc3339(),
        grant_window_days: body.grant_window_days,
        lease_days: body.lease_days,
        grace_days: body.grace_days.unwrap_or(14),
        features: body.features,
        max_version: body.max_version,
        // upsert 的 SQL 不会用这两个字段覆盖已有行（见 store::upsert 注释），首建时才生效。
        revoked_at: existing.as_ref().and_then(|r| r.revoked_at.clone()),
        note: body.note.unwrap_or_default(),
        created_at: existing
            .map(|r| r.created_at)
            .unwrap_or_else(toolkit_core::now_iso8601),
    };

    match tokio::task::spawn_blocking(move || store::upsert(&pool, &row)).await {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "lic_id": lic_id })),
        )
            .into_response(),
        _ => admin_err(StatusCode::INTERNAL_SERVER_ERROR, "upsert_failed"),
    }
}

/// `PUT /{lic_id}` 请求体：改续期窗口 / 在线租约 / 联系人。**从简的存在性语义**（管理端点，
/// 走 Bearer，非公网攻击面）：`clear_lease=true` 显式清空 `lease_days`（转纯离线模式）；
/// `contact_email` 传空串等价于清空。其余字段缺省即不改动对应列。
#[derive(Debug, Deserialize, Default)]
struct UpdateReq {
    #[serde(default)]
    grant_window_days: Option<i64>,
    #[serde(default)]
    lease_days: Option<i64>,
    #[serde(default)]
    clear_lease: Option<bool>,
    #[serde(default)]
    contact_email: Option<String>,
}

async fn update_license(
    State(state): State<AppState>,
    Path(lic_id): Path<String>,
    body: Option<Json<UpdateReq>>,
) -> Response {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let pool = state.pool.clone();

    let lic = lic_id.clone();
    let exists = match tokio::task::spawn_blocking({
        let pool = pool.clone();
        move || store::get(&pool, &lic)
    })
    .await
    {
        Ok(Ok(r)) => r,
        _ => return admin_err(StatusCode::INTERNAL_SERVER_ERROR, "lookup_failed"),
    };
    if exists.is_none() {
        return admin_err(StatusCode::NOT_FOUND, "license_not_found");
    }

    if let Some(days) = body.grant_window_days {
        let pool = pool.clone();
        let lic = lic_id.clone();
        if tokio::task::spawn_blocking(move || store::set_grant_window(&pool, &lic, days))
            .await
            .ok()
            .and_then(|r| r.ok())
            != Some(true)
        {
            return admin_err(StatusCode::INTERNAL_SERVER_ERROR, "update_failed");
        }
    }

    if body.clear_lease == Some(true) {
        let pool = pool.clone();
        let lic = lic_id.clone();
        let _ = tokio::task::spawn_blocking(move || store::set_lease(&pool, &lic, None)).await;
    } else if let Some(days) = body.lease_days {
        let pool = pool.clone();
        let lic = lic_id.clone();
        let _ =
            tokio::task::spawn_blocking(move || store::set_lease(&pool, &lic, Some(days))).await;
    }

    if let Some(email) = body.contact_email {
        let pool = pool.clone();
        let lic = lic_id.clone();
        let email_opt = if email.trim().is_empty() {
            None
        } else {
            Some(email)
        };
        let _ = tokio::task::spawn_blocking(move || {
            store::set_contact_email(&pool, &lic, email_opt.as_deref())
        })
        .await;
    }

    match tokio::task::spawn_blocking(move || store::get(&pool, &lic_id)).await {
        Ok(Ok(Some(row))) => (StatusCode::OK, Json(row)).into_response(),
        _ => admin_err(StatusCode::INTERNAL_SERVER_ERROR, "reread_failed"),
    }
}

async fn revoke_license(State(state): State<AppState>, Path(lic_id): Path<String>) -> Response {
    let pool = state.pool.clone();
    match tokio::task::spawn_blocking(move || store::revoke(&pool, &lic_id)).await {
        Ok(Ok(true)) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Ok(Ok(false)) => admin_err(StatusCode::NOT_FOUND, "license_not_found"),
        _ => admin_err(StatusCode::INTERNAL_SERVER_ERROR, "revoke_failed"),
    }
}

async fn delete_license(State(state): State<AppState>, Path(lic_id): Path<String>) -> Response {
    let pool = state.pool.clone();
    match tokio::task::spawn_blocking(move || store::delete(&pool, &lic_id)).await {
        Ok(Ok(true)) => (StatusCode::OK, Json(json!({ "ok": true }))).into_response(),
        Ok(Ok(false)) => admin_err(StatusCode::NOT_FOUND, "license_not_found"),
        _ => admin_err(StatusCode::INTERNAL_SERVER_ERROR, "delete_failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::license::signer::Signer;
    use custom_utils::license::delegation::DelegationCert;
    use custom_utils::license::issue::{generate_keypair, sign_delegation};
    use custom_utils::license::keys::{key_table_from, KeyEntry, Role};
    use custom_utils::license::response::decode_and_verify_tkr1;
    use custom_utils::license::state::check_renewal_against_anchor;
    use custom_utils::license::token::decode_and_verify_license_delegated;
    use custom_utils::license::RevokedSet;

    fn test_state(dir: &std::path::Path) -> AppState {
        let cfg = crate::test_config(dir.to_path_buf());
        crate::bootstrap(&cfg).unwrap()
    }

    /// 组一把 root + 一把 renewal，root 签一张委托证书，构造 `Signer`（跳过 env/文件系统）。
    /// 返回 (root KeyEntry 供客户端 baked 表用, signer, root anchor 用的 root SigningKey)。
    fn setup_signer() -> (Vec<KeyEntry>, Signer, ed25519_dalek::SigningKey) {
        let (root_sk, root_vk) = generate_keypair();
        let (renewal_sk, renewal_vk) = generate_keypair();
        let root_kid = "root-test";
        let renewal_kid = "renewal-test";

        let cert = DelegationCert {
            ver: 1,
            sub_kid: renewal_kid.to_string(),
            role: Role::Renewal,
            sub_pubkey_hex: custom_utils::license::keys::hex_encode(renewal_vk.as_bytes()),
            not_before: Utc::now() - ChronoDuration::days(1),
            expires_at: Utc::now() + ChronoDuration::days(30),
            nonce: random_nonce(),
        };
        let cert_token = sign_delegation(&root_sk, root_kid, &cert).unwrap();

        let table_str = format!(
            "{root_kid}:root:{}",
            custom_utils::license::keys::hex_encode(root_vk.as_bytes())
        );
        let table = key_table_from(&table_str).unwrap();

        let signer = Signer::from_parts(renewal_sk, renewal_kid.to_string(), cert_token);
        (table, signer, root_sk)
    }

    #[tokio::test]
    async fn refresh_happy_path_produces_verifiable_tkr1() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(tmp.path());
        let (table, signer, root_sk) = setup_signer();

        // 客户端本机指纹（这里直接手搓一条，测试不依赖真实硬件读取）。
        let machine = MachineFingerprint {
            id: "m-1".to_string(),
            components: std::collections::BTreeMap::from([(
                "machineguid".to_string(),
                "h1".to_string(),
            )]),
        };

        let not_before = Utc::now() - ChronoDuration::days(10);
        let business_deadline = Utc::now() + ChronoDuration::days(180);

        // 台账入台（等价 admin POST）。
        let row = LicenseRow {
            lic_id: "L-TEST-1".to_string(),
            product: "zero-desktop".to_string(),
            subject: "测试客户".to_string(),
            contact_email: None,
            machine_ids: vec![machine.clone()],
            not_before: not_before.to_rfc3339(),
            business_deadline: business_deadline.to_rfc3339(),
            grant_window_days: 30,
            lease_days: Some(14),
            grace_days: 14,
            features: vec!["speech".to_string()],
            max_version: None,
            revoked_at: None,
            note: "".to_string(),
            created_at: toolkit_core::now_iso8601(),
        };
        store::upsert(&state.pool, &row).unwrap();

        // root 现签一份 anchor TKL1，模拟客户端本地持有的锚（用于事后 check_renewal_against_anchor）。
        let anchor_payload = LicensePayload {
            ver: 1,
            product: row.product.clone(),
            lic_id: row.lic_id.clone(),
            subject: row.subject.clone(),
            machine: vec![machine.clone()],
            issued_at: Utc::now() - ChronoDuration::days(10),
            not_before,
            business_deadline,
            expires_at: Utc::now() + ChronoDuration::days(5),
            lease_until: Some(Utc::now() + ChronoDuration::days(3)),
            grace_days: 14,
            features: vec!["speech".to_string()],
            max_version: None,
            nonce: random_nonce(),
        };
        let anchor_token =
            custom_utils::license::issue::sign_license(&root_sk, "root-test", &anchor_payload)
                .unwrap();
        let revoked = RevokedSet::new();
        let anchor =
            custom_utils::license::decode_and_verify_license(&anchor_token, &table, &revoked)
                .unwrap();

        let body = RefreshReq {
            lic_id: row.lic_id.clone(),
            product: row.product.clone(),
            machine_id: machine.id.clone(),
            client_nonce: "client-nonce-abc".to_string(),
            ver: Some(1),
        };

        let resp = do_refresh(&state, &signer, &body)
            .await
            .ok()
            .expect("refresh should succeed");

        // 客户端视角验证：TKR1 外层（委托链）+ echo_nonce 回绑。
        let tkr1_payload = decode_and_verify_tkr1(
            &resp.tkr1,
            &resp.cert,
            &table,
            &revoked,
            Utc::now(),
            "client-nonce-abc",
        )
        .expect("TKR1 must verify");
        assert_eq!(tkr1_payload.echo_nonce, "client-nonce-abc");

        // 内层续期 TKL1（委托签名）验证 + 与 anchor 的续期约束比对。
        let renewed = decode_and_verify_license_delegated(
            &tkr1_payload.license,
            &resp.cert,
            &table,
            &revoked,
            Utc::now(),
        )
        .expect("inner renewal TKL1 must verify");
        check_renewal_against_anchor(&anchor, &renewed)
            .expect("renewal must satisfy anchor constraints");

        assert_eq!(renewed.lic_id, "L-TEST-1");
        assert!(renewed.expires_at <= business_deadline);
    }

    #[tokio::test]
    async fn refresh_rejects_unknown_lic_id() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(tmp.path());
        let (_table, signer, _root_sk) = setup_signer();
        let body = RefreshReq {
            lic_id: "does-not-exist".to_string(),
            product: "zero-desktop".to_string(),
            machine_id: "".to_string(),
            client_nonce: "n".to_string(),
            ver: None,
        };
        let err = do_refresh(&state, &signer, &body)
            .await
            .expect_err("must fail");
        assert!(matches!(err, RefreshError::NotFound));
    }

    #[tokio::test]
    async fn refresh_rejects_revoked() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(tmp.path());
        let (_table, signer, _root_sk) = setup_signer();
        let row = LicenseRow {
            lic_id: "L-REVOKED".to_string(),
            product: "zero-desktop".to_string(),
            subject: "x".to_string(),
            contact_email: None,
            machine_ids: vec![],
            not_before: (Utc::now() - ChronoDuration::days(1)).to_rfc3339(),
            business_deadline: (Utc::now() + ChronoDuration::days(30)).to_rfc3339(),
            grant_window_days: 30,
            lease_days: None,
            grace_days: 14,
            features: vec![],
            max_version: None,
            revoked_at: Some(toolkit_core::now_iso8601()),
            note: "".to_string(),
            created_at: toolkit_core::now_iso8601(),
        };
        store::upsert(&state.pool, &row).unwrap();
        let body = RefreshReq {
            lic_id: "L-REVOKED".to_string(),
            product: "zero-desktop".to_string(),
            machine_id: "".to_string(),
            client_nonce: "n".to_string(),
            ver: None,
        };
        let err = do_refresh(&state, &signer, &body)
            .await
            .expect_err("must fail");
        assert!(matches!(err, RefreshError::Revoked));
    }
}
