//! audio-store HTTP 路由（挂在 `/api/web/audio` 前缀下，故相对路径写 `/store`）：
//!   - `POST /store?source=`           → 上传音频字节（raw body），返回 `{id, bytes, duration}`
//!   - `GET  /store/{id}`              → 下载（支持 `Range` 单区间分块）
//!
//! 上传走内容寻址（同字节幂等去重，见 [`super::store`]）。下载支持 HTTP Range，便于
//! `<audio>` 拖动定位 / 断点续传，且大文件分块读盘不占内存。

use std::io::{Read, Seek, SeekFrom};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::json;

use crate::audiostore::store::{self, StorePaths};
use crate::state::AppState;

/// 上传 body 上限 64MiB（axum 默认仅 2MiB，会挡长录音）。
const UPLOAD_BODY_LIMIT: usize = 64 * 1024 * 1024;

/// `source` 白名单：标记 blob 来源，便于运维统计 / 清理策略。
const SOURCE_WHITELIST: &[&str] = &["forge", "manual", "import", "other"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/store",
            post(upload).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route("/store/{id}", get(download))
}

#[derive(Debug, Deserialize)]
struct UploadParams {
    /// blob 来源标记（白名单 `forge|manual|import|other`），缺省 `other`。
    #[serde(default)]
    source: Option<String>,
}

fn bad_request(msg: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.into() })),
    )
        .into_response()
}

/// `POST /api/web/audio/store?source=`：内容寻址写入音频字节。
async fn upload(State(s): State<AppState>, Query(p): Query<UploadParams>, body: Bytes) -> Response {
    if body.is_empty() {
        return bad_request("音频 body 为空");
    }
    let source = p.source.unwrap_or_else(|| "other".to_string());
    if !SOURCE_WHITELIST.contains(&source.as_str()) {
        return bad_request(format!(
            "非法 source: {source}（应为 forge|manual|import|other）"
        ));
    }

    match store::put(&s.pool, &s.workspace, &body, &source) {
        Ok(r) => Json(json!({
            "id": r.id,
            "bytes": r.bytes,
            "duration": r.duration,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("{e:#}") })),
        )
            .into_response(),
    }
}

/// `GET /api/web/audio/store/{id}`：下载 blob，支持单区间 `Range`。
async fn download(
    State(s): State<AppState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !is_safe_segment(&id) {
        return bad_request("非法 id");
    }

    let path = StorePaths::new(&s.workspace).blob_path(&id);
    let size = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        // 文件不存在 → 404（不泄露路径细节）。
        Err(_) => {
            return (StatusCode::NOT_FOUND, Json(json!({ "error": "未找到" }))).into_response()
        }
    };

    match parse_range(headers.get(header::RANGE), size) {
        // 全量（无 Range / 语法不可解析 / multi-range 降级）。
        RangeOutcome::Full => match std::fs::read(&path) {
            Ok(bytes) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "audio/wav".to_string()),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_LENGTH, size.to_string()),
                ],
                Bytes::from(bytes),
            )
                .into_response(),
            Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "error": "未找到" }))).into_response(),
        },
        // 区间不可满足 → 416 + Content-Range: bytes */{size}。
        RangeOutcome::Unsatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{size}"))],
        )
            .into_response(),
        // 单区间 → 206，仅读该区间字节。
        RangeOutcome::Partial { start, end } => match read_range(&path, start, end) {
            Ok(chunk) => {
                let len = end - start + 1;
                (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (header::CONTENT_TYPE, "audio/wav".to_string()),
                        (header::ACCEPT_RANGES, "bytes".to_string()),
                        (header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
                        (header::CONTENT_LENGTH, len.to_string()),
                    ],
                    Bytes::from(chunk),
                )
                    .into_response()
            }
            Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "error": "未找到" }))).into_response(),
        },
    }
}

/// Range 解析结果。
#[derive(Debug, PartialEq, Eq)]
enum RangeOutcome {
    /// 无 Range / 不可解析 / multi-range → 全量 200。
    Full,
    /// 单区间 → 206，闭区间 `[start, end]`。
    Partial { start: u64, end: u64 },
    /// 不可满足 → 416。
    Unsatisfiable,
}

/// 解析单区间 `Range` 头。仅支持单 range 的三种形态：
///   `bytes=start-end` / `bytes=start-`（到末尾）/ `bytes=-suffix`（末 N 字节）。
/// multi-range（含逗号）或语法不可解析 → [`RangeOutcome::Full`]（降级全量）。
fn parse_range(header: Option<&axum::http::HeaderValue>, size: u64) -> RangeOutcome {
    let Some(raw) = header.and_then(|v| v.to_str().ok()) else {
        return RangeOutcome::Full;
    };
    let raw = raw.trim();
    // 只认 `bytes=` 前缀。
    let Some(spec) = raw.strip_prefix("bytes=") else {
        return RangeOutcome::Full;
    };
    let spec = spec.trim();
    // multi-range（含逗号）→ 降级全量。
    if spec.contains(',') {
        return RangeOutcome::Full;
    }
    // 必须恰有一个 `-`。
    let Some((a, b)) = spec.split_once('-') else {
        return RangeOutcome::Full;
    };
    let a = a.trim();
    let b = b.trim();

    // 空文件：任何区间都不可满足。
    if size == 0 {
        return RangeOutcome::Unsatisfiable;
    }

    match (a.is_empty(), b.is_empty()) {
        // `bytes=-suffix`：末 N 字节。
        (true, false) => {
            let Ok(suffix) = b.parse::<u64>() else {
                return RangeOutcome::Full;
            };
            if suffix == 0 {
                return RangeOutcome::Unsatisfiable;
            }
            let suffix = suffix.min(size);
            let start = size - suffix;
            RangeOutcome::Partial {
                start,
                end: size - 1,
            }
        }
        // `bytes=start-`：start 到末尾。
        (false, true) => {
            let Ok(start) = a.parse::<u64>() else {
                return RangeOutcome::Full;
            };
            if start >= size {
                return RangeOutcome::Unsatisfiable;
            }
            RangeOutcome::Partial {
                start,
                end: size - 1,
            }
        }
        // `bytes=start-end`。
        (false, false) => {
            let (Ok(start), Ok(end)) = (a.parse::<u64>(), b.parse::<u64>()) else {
                return RangeOutcome::Full;
            };
            if start >= size {
                return RangeOutcome::Unsatisfiable;
            }
            // start > end → 不可满足。
            if start > end {
                return RangeOutcome::Unsatisfiable;
            }
            // end 超界 → clamp 到 size-1。
            let end = end.min(size - 1);
            RangeOutcome::Partial { start, end }
        }
        // `bytes=-` 两端皆空 → 语法无意义，降级全量。
        (true, true) => RangeOutcome::Full,
    }
}

/// 仅读闭区间 `[start, end]` 的字节（seek + 定长读，不读整文件）。
fn read_range(path: &std::path::Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let len = (end - start + 1) as usize;
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// 单段路径是否安全：非空、无路径分隔符、无 `..`、无盘符冒号、非 `.`。
/// （与 audioforge::routes::is_safe_segment 同规则。）
fn is_safe_segment(seg: &str) -> bool {
    !seg.is_empty()
        && !seg.contains('/')
        && !seg.contains('\\')
        && !seg.contains("..")
        && !seg.contains(':')
        && seg != "."
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hv(s: &str) -> HeaderValue {
        HeaderValue::from_str(s).unwrap()
    }

    #[test]
    fn range_none_is_full() {
        assert_eq!(parse_range(None, 100), RangeOutcome::Full);
    }

    #[test]
    fn range_start_end() {
        assert_eq!(
            parse_range(Some(&hv("bytes=0-3")), 100),
            RangeOutcome::Partial { start: 0, end: 3 }
        );
        assert_eq!(
            parse_range(Some(&hv("bytes=10-20")), 100),
            RangeOutcome::Partial { start: 10, end: 20 }
        );
    }

    #[test]
    fn range_start_to_end_of_file() {
        assert_eq!(
            parse_range(Some(&hv("bytes=50-")), 100),
            RangeOutcome::Partial { start: 50, end: 99 }
        );
    }

    #[test]
    fn range_suffix() {
        assert_eq!(
            parse_range(Some(&hv("bytes=-10")), 100),
            RangeOutcome::Partial { start: 90, end: 99 }
        );
        // suffix 超过文件 → clamp 到整文件。
        assert_eq!(
            parse_range(Some(&hv("bytes=-500")), 100),
            RangeOutcome::Partial { start: 0, end: 99 }
        );
    }

    #[test]
    fn range_end_overflow_clamps() {
        assert_eq!(
            parse_range(Some(&hv("bytes=90-1000")), 100),
            RangeOutcome::Partial { start: 90, end: 99 }
        );
    }

    #[test]
    fn range_multi_is_full() {
        assert_eq!(
            parse_range(Some(&hv("bytes=0-3,5-7")), 100),
            RangeOutcome::Full
        );
    }

    #[test]
    fn range_garbage_is_full() {
        assert_eq!(parse_range(Some(&hv("rubbish")), 100), RangeOutcome::Full);
        assert_eq!(parse_range(Some(&hv("bytes=abc")), 100), RangeOutcome::Full);
        assert_eq!(parse_range(Some(&hv("bytes=x-y")), 100), RangeOutcome::Full);
        // 两端皆空。
        assert_eq!(parse_range(Some(&hv("bytes=-")), 100), RangeOutcome::Full);
    }

    #[test]
    fn range_unsatisfiable() {
        // start >= size。
        assert_eq!(
            parse_range(Some(&hv("bytes=100-200")), 100),
            RangeOutcome::Unsatisfiable
        );
        assert_eq!(
            parse_range(Some(&hv("bytes=100-")), 100),
            RangeOutcome::Unsatisfiable
        );
        // suffix=0。
        assert_eq!(
            parse_range(Some(&hv("bytes=-0")), 100),
            RangeOutcome::Unsatisfiable
        );
        // start > end。
        assert_eq!(
            parse_range(Some(&hv("bytes=20-10")), 100),
            RangeOutcome::Unsatisfiable
        );
        // 空文件。
        assert_eq!(
            parse_range(Some(&hv("bytes=0-3")), 0),
            RangeOutcome::Unsatisfiable
        );
    }

    #[test]
    fn safe_segment_rules() {
        assert!(is_safe_segment("aud_deadbeefdeadbeef"));
        assert!(!is_safe_segment(""));
        assert!(!is_safe_segment(".."));
        assert!(!is_safe_segment("../etc"));
        assert!(!is_safe_segment("a/b"));
        assert!(!is_safe_segment("a\\b"));
        assert!(!is_safe_segment("C:"));
        assert!(!is_safe_segment("."));
    }
}
