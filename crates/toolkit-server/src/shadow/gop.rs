//! GOP 发音评测后端：把用户录音 + 参考文本转发到 GB10 发音评测服务
//! （`:8098 POST /assess`，streaming-speech 仓维护），把返回映射进既有 [`ScoreResult`]。
//!
//! **传输分层**（见 docs/english-shadow-gop-design.md §3）：外部桌面端 → toolkit-server 仍是
//! raw body + query（不动）；**只有这一跳** toolkit-server → `:8098` 才把 raw 音频 + `ref_text`
//! 组装成 **multipart**。multipart 只存在于本模块。
//!
//! 端点契约权威源：streaming-speech `docs/pronunciation-assess-api.md`（Phase A 落地后）。

use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use super::{PhoneResult, ScoreResult, ShadowKind, WordResult};

/// `/assess` 总超时。发音评测比纯 ASR 重（对齐 + 后验），目标 < 2s，给 60s 兜底。
const ASSESS_TIMEOUT: Duration = Duration::from_secs(60);

/// `/assess` 成功响应（契约见设计 §4）。所有分值 0~1（已标定）。
#[derive(Debug, Clone, Deserialize)]
struct AssessResponse {
    /// 参考文本（流式 `final` 事件带；批量 `/assess` 响应也带，但批量路径用入参 ref_text）。
    #[serde(default)]
    ref_text: Option<String>,
    /// CTC 反推近似文本，optional、非稳定 ASR，仅回看用。
    #[serde(default)]
    transcript: Option<String>,
    /// 句级发音分 0~1。
    sentence_score: f64,
    #[serde(default)]
    words: Vec<AssessWord>,
    /// 严重错读音素总数，供 passed 判定。
    #[serde(default)]
    bad_phone_count: Option<u32>,
    /// 评测模型标识，如 `wav2vec2-gop-v1`。
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AssessWord {
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    score: Option<f64>,
    /// 发音三档 `ok|warn|bad`。
    #[serde(default)]
    pron_status: Option<String>,
    #[serde(default)]
    phones: Option<Vec<AssessPhone>>,
}

#[derive(Debug, Clone, Deserialize)]
struct AssessPhone {
    ph: String,
    score: f64,
    #[serde(default)]
    pron_status: Option<String>,
    #[serde(default)]
    expected_ph: Option<String>,
    #[serde(default)]
    actual_ph: Option<String>,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    reliable: Option<bool>,
    #[serde(default)]
    t_start: Option<f64>,
    #[serde(default)]
    t_end: Option<f64>,
    #[serde(default)]
    peak_t: Option<f64>,
    #[serde(default)]
    gop_raw: Option<f64>,
}

/// 调 `:8098 /assess` 做发音评测，映射进 [`ScoreResult`]。
///
/// - `base`：`GOP_BASE_URL`（已去尾斜杠）。
/// - `kind`：决定上游 `granularity`（裁剪返回详尽度，与落库单元正交，见设计 §4）。
/// - 返回 `Err` 时由 handler 回 502（显式配置但上游不可达/报错）。
pub async fn assess(
    base: &str,
    audio: Vec<u8>,
    mime: &str,
    file_name: &'static str,
    ref_text: &str,
    kind: ShadowKind,
    threshold: f64,
) -> Result<ScoreResult> {
    let client = reqwest::Client::builder()
        .timeout(ASSESS_TIMEOUT)
        .build()
        .context("build reqwest client")?;

    let part = Part::bytes(audio)
        .file_name(file_name)
        .mime_str(mime)
        .context("构造 multipart audio part")?;
    let form = Form::new()
        .part("audio", part)
        .text("ref_text", ref_text.to_string())
        .text("granularity", kind.as_str().to_string());

    let url = format!("{base}/assess");
    let resp = client
        .post(&url)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("调发音评测 /assess ({url})"))?;
    let status = resp.status();
    let body = resp.text().await.context("读 /assess 响应 body")?;
    if !status.is_success() {
        bail!(
            "发音评测 /assess {status}: {}",
            body.chars().take(200).collect::<String>()
        );
    }
    let parsed: AssessResponse = serde_json::from_str(&body).with_context(|| {
        format!(
            "解析 /assess 响应失败,前 200 字符: {}",
            body.chars().take(200).collect::<String>()
        )
    })?;

    Ok(map_response(parsed, ref_text, threshold))
}

/// 把流式 WS 的 `final` 事件 JSON 映射进 `ScoreResult`（供中继落库,见 shadow/stream.rs）。
/// `final` 事件即批量 `/assess` 响应形状(自带 ref_text);解析失败返回 None。
pub fn score_result_from_final(v: &serde_json::Value, threshold: f64) -> Option<ScoreResult> {
    let parsed: AssessResponse = serde_json::from_value(v.clone()).ok()?;
    let ref_text = parsed.ref_text.clone().unwrap_or_default();
    Some(map_response(parsed, &ref_text, threshold))
}

/// 把 `/assess` 响应映射进 `ScoreResult`：
/// - `score` = `sentence_score`；
/// - `passed` = `sentence_score >= threshold && bad_phone_count <= max_bad_phones(音素总数)`
///   （放宽,见 scoring-ui §2.4;配额按句长自适应,见 todo「标定实测发现」）；
/// - 词级 `status`（内容维度，给老前端回退渲染）从 `pron_status` 派生：`bad → wrong`，否则 `ok`。
fn map_response(r: AssessResponse, ref_text: &str, threshold: f64) -> ScoreResult {
    let bad_count = r
        .bad_phone_count
        .unwrap_or_else(|| count_bad_phones(&r.words));
    let phone_total: usize = r
        .words
        .iter()
        .map(|w| w.phones.as_ref().map_or(0, |p| p.len()))
        .sum();
    let words = r
        .words
        .into_iter()
        .map(|w| {
            let pron = w.pron_status;
            let status = content_status_from_pron(pron.as_deref());
            WordResult {
                reference: w.reference,
                status,
                score: w.score,
                pron_status: pron,
                phones: w.phones.map(|ps| ps.into_iter().map(map_phone).collect()),
            }
        })
        .collect();

    ScoreResult {
        transcript: r.transcript.unwrap_or_default(),
        ref_text: ref_text.trim().to_string(),
        score: r.sentence_score,
        // 放宽:句分达标 且 严重错读音素 ≤ 配额(uncertain 已不计入 bad_count;配额按句长自适应)。
        passed: r.sentence_score >= threshold && bad_count <= super::max_bad_phones(phone_total),
        words,
        bad_phone_count: Some(bad_count),
        model: r.model,
    }
}

fn map_phone(p: AssessPhone) -> PhoneResult {
    PhoneResult {
        ph: p.ph,
        score: p.score,
        pron_status: p.pron_status.unwrap_or_else(|| "ok".to_string()),
        expected_ph: p.expected_ph,
        actual_ph: p.actual_ph,
        hint: p.hint,
        reliable: p.reliable,
        t_start: p.t_start,
        t_end: p.t_end,
        peak_t: p.peak_t,
        gop_raw: p.gop_raw,
    }
}

/// 上游若没给 `bad_phone_count`，本地从音素 `pron_status==bad` 兜底统计。
fn count_bad_phones(words: &[AssessWord]) -> u32 {
    words
        .iter()
        .flat_map(|w| w.phones.iter().flatten())
        .filter(|p| p.pron_status.as_deref() == Some("bad"))
        .count() as u32
}

/// 发音三档 → v1 内容维度 `status`（给未升级的老前端回退上色）。
fn content_status_from_pron(pron: Option<&str>) -> &'static str {
    match pron {
        Some("bad") => "wrong",
        _ => "ok",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AssessResponse {
        serde_json::from_str(
            r#"{
              "transcript": "i think so",
              "ref_text": "I think so",
              "sentence_score": 0.71,
              "words": [
                { "ref": "I", "score": 0.95, "pron_status": "ok",
                  "phones": [ { "ph": "AY", "score": 0.95, "pron_status": "ok" } ] },
                { "ref": "think", "score": 0.42, "pron_status": "bad",
                  "phones": [
                    { "ph": "TH", "score": 0.18, "pron_status": "bad",
                      "expected_ph": "TH", "actual_ph": "S", "hint": "/θ/ 读成了 /s/" },
                    { "ph": "IH", "score": 0.71, "pron_status": "ok" }
                  ] },
                { "ref": "so", "score": 0.88, "pron_status": "ok" }
              ],
              "bad_phone_count": 1,
              "model": "wav2vec2-gop-v1"
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn maps_words_and_phones() {
        let r = map_response(sample(), "I think so", 0.6);
        assert_eq!(r.score, 0.71);
        assert_eq!(r.model.as_deref(), Some("wav2vec2-gop-v1"));
        assert_eq!(r.words.len(), 3);
        // think：pron_status=bad → 内容维度 status 派生为 wrong（老前端回退）。
        assert_eq!(r.words[1].status, "wrong");
        assert_eq!(r.words[1].pron_status.as_deref(), Some("bad"));
        let phones = r.words[1].phones.as_ref().unwrap();
        assert_eq!(phones[0].ph, "TH");
        assert_eq!(phones[0].actual_ph.as_deref(), Some("S"));
    }

    #[test]
    fn short_sentence_zero_bad_tolerance() {
        // 短句(<10 音素)零容忍:1 个 bad 即不通过(豁免规则已吸收冤案,单错读不得靠配额混过)。
        let r = map_response(sample(), "I think so", 0.6); // bad_phone_count=1, score 0.71
        assert_eq!(r.bad_phone_count, Some(1));
        assert!(!r.passed, "短句 1 个 bad 应不通过(配额=0)");

        let mut clean = sample();
        clean.bad_phone_count = Some(0);
        let r2 = map_response(clean, "x", 0.6);
        assert!(r2.passed, "零 bad + 句分达标应通过");
    }

    #[test]
    fn bad_quota_scales_with_sentence_length() {
        // 配额按句长自适应(每 10 音素 1 个):25 音素句容 2 个 bad,3 个不行。
        let mut long = sample();
        long.bad_phone_count = Some(2);
        let ph = long.words[0].phones.as_ref().unwrap()[0].clone();
        long.words[0].phones = Some(vec![ph.clone(); 25]);
        let r = map_response(long, "x", 0.6);
        assert!(r.passed, "25 音素句 2 个 bad 应通过(配额=2)");

        let mut over = sample();
        over.bad_phone_count = Some(3);
        over.words[0].phones = Some(vec![ph; 25]);
        let r2 = map_response(over, "x", 0.6);
        assert!(!r2.passed, "25 音素句 3 个 bad 应不通过");
    }

    #[test]
    fn passed_needs_score_above_threshold() {
        // 句分不够 → 不过(即便零 bad)。
        let mut resp = sample();
        resp.bad_phone_count = Some(0);
        resp.sentence_score = 0.5;
        let r = map_response(resp, "x", 0.6);
        assert!(!r.passed);
    }

    #[test]
    fn bad_count_falls_back_to_phone_scan() {
        let mut resp = sample();
        resp.bad_phone_count = None; // 上游没给 → 本地扫音素
        let r = map_response(resp, "x", 0.6);
        assert_eq!(r.bad_phone_count, Some(1));
    }
}
