//! English 跟读判分（Shadow Reading）。
//!
//! 用户跟读 → FunASR 转写（经 `asr-client`）→ 与参考文本词级对齐打分 → 落
//! `shadow_attempt` + 累加 `shadow_stat`。设计见 `docs/english-shadow-design.md`。
//!
//! 本模块只持有**判分内核**（归一化 / 对齐 / 打分，纯函数易测）与对外 [`routes`]、
//! 持久层 [`store`]。ASR 与 DB 在 handler 里编排。
//!
//! > v1 是 ASR 文本对齐——衡量「内容/可懂度」，非发音细腻度（ASR 较宽容）。发音级 GOP
//! > 属后续阶段，替换打分内核即可，接口形状不变。

pub mod routes;
pub mod store;

use serde::Serialize;

/// FunASR `/transcribe` 上游 base：env `ASR_BASE_URL`，缺省同机回环 9101。
/// 与 douyin 的 `asr_url` 默认对齐（那边带 `/transcribe` 后缀，这里走 asr-client 不带）。
pub fn asr_base() -> String {
    std::env::var("ASR_BASE_URL")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| asr_client::DEFAULT_BASE.to_string())
}

/// 默认「通过」阈值（内容命中率）。设计 §2 决策 1。
pub const DEFAULT_THRESHOLD: f64 = 0.9;

/// 跟读粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowKind {
    Sentence,
    Word,
}

impl ShadowKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sentence" => Some(Self::Sentence),
            "word" => Some(Self::Word),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sentence => "sentence",
            Self::Word => "word",
        }
    }
}

/// 单个参考词的判定结果（供前端逐词标色）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WordResult {
    /// 参考词原文（保留原始大小写/形态用于展示）。
    #[serde(rename = "ref")]
    pub reference: String,
    /// `ok` 读对 / `wrong` 读错（替换）/ `missing` 漏读。
    pub status: &'static str,
}

/// 一次跟读的判分结果。
#[derive(Debug, Clone, Serialize)]
pub struct ScoreResult {
    /// ASR 识别到的用户朗读全文。
    pub transcript: String,
    pub ref_text: String,
    /// 内容命中率 0~1。
    pub score: f64,
    /// `score >= threshold`。
    pub passed: bool,
    pub words: Vec<WordResult>,
}

/// 归一化为词序列：非字母数字一律视作分隔符，ASCII 转小写。撇号被剥离，故
/// `don't` / `dont` 归一一致。英文/数字场景足够；中文按字保留（极少用）。
fn normalize_tokens(s: &str) -> Vec<String> {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

/// 通用编辑距离（Levenshtein），作用于任意可比较序列。
fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// 句模式判分：参考词序列与识别词序列做词级对齐，逐参考词标 ok/wrong/missing，
/// 命中率 = ok 词数 / 参考词数。
pub fn score_sentence(ref_text: &str, hyp_text: &str, threshold: f64) -> ScoreResult {
    let ref_display = split_display_words(ref_text);
    let r = normalize_tokens(ref_text);
    let h = normalize_tokens(hyp_text);

    let statuses = align_statuses(&r, &h);
    // align_statuses 与归一化后的 r 一一对应；用原始词形展示。
    let words: Vec<WordResult> = ref_display
        .iter()
        .zip(statuses.iter())
        .map(|(disp, st)| WordResult {
            reference: disp.clone(),
            status: st,
        })
        .collect();

    let ok = statuses.iter().filter(|s| **s == "ok").count();
    let score = if r.is_empty() {
        0.0
    } else {
        ok as f64 / r.len() as f64
    };
    ScoreResult {
        transcript: hyp_text.trim().to_string(),
        ref_text: ref_text.trim().to_string(),
        score,
        passed: score >= threshold,
        words,
    }
}

/// 词模式判分：参考是单个词，取识别词中与之字符相似度最高者；相似度即得分
/// （字符级编辑距离归一），避免 ASR 把 `cat` 听成 `cap` 直接判 0 太苛刻。
pub fn score_word(ref_word: &str, hyp_text: &str, threshold: f64) -> ScoreResult {
    let ref_norm = normalize_tokens(ref_word);
    let ref_token = ref_norm.first().cloned().unwrap_or_default();
    let ref_chars: Vec<char> = ref_token.chars().collect();
    let hyp_tokens = normalize_tokens(hyp_text);

    let mut best = 0.0f64;
    for t in &hyp_tokens {
        let tc: Vec<char> = t.chars().collect();
        let dist = edit_distance(&ref_chars, &tc);
        let maxlen = ref_chars.len().max(tc.len()).max(1);
        let sim = 1.0 - dist as f64 / maxlen as f64;
        if sim > best {
            best = sim;
        }
    }
    if ref_chars.is_empty() {
        best = 0.0;
    }
    let passed = best >= threshold;
    let status = if passed { "ok" } else { "wrong" };
    ScoreResult {
        transcript: hyp_text.trim().to_string(),
        ref_text: ref_word.trim().to_string(),
        score: best,
        passed,
        words: vec![WordResult {
            reference: ref_word.trim().to_string(),
            status,
        }],
    }
}

/// 派发：按粒度选打分函数。
pub fn score(kind: ShadowKind, ref_text: &str, hyp_text: &str, threshold: f64) -> ScoreResult {
    match kind {
        ShadowKind::Sentence => score_sentence(ref_text, hyp_text, threshold),
        ShadowKind::Word => score_word(ref_text, hyp_text, threshold),
    }
}

/// 按空白切「展示词」（保留原始大小写/标点形态），与 `normalize_tokens` 词数对齐。
/// 注：normalize 用同样的空白边界，故两者词数一致。
fn split_display_words(s: &str) -> Vec<String> {
    // 与 normalize_tokens 同源：先把非字母数字换空格再切，保证一一对应。
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

/// 词级对齐：返回与 `r` 等长的状态序列（"ok"/"wrong"/"missing"）。
/// 走 Levenshtein DP + 回溯：匹配=ok、替换=wrong、删除(参考有识别无)=missing；
/// 插入(识别多出)不挂到参考词上（只影响不了命中率）。
fn align_statuses(r: &[String], h: &[String]) -> Vec<&'static str> {
    let n = r.len();
    let m = h.len();
    if n == 0 {
        return Vec::new();
    }
    if m == 0 {
        return vec!["missing"; n];
    }
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate().take(n + 1) {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if r[i - 1] == h[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    // 回溯
    let mut statuses = vec!["missing"; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let cost = if r[i - 1] == h[j - 1] { 0 } else { 1 };
        if dp[i][j] == dp[i - 1][j - 1] + cost {
            statuses[i - 1] = if cost == 0 { "ok" } else { "wrong" };
            i -= 1;
            j -= 1;
        } else if dp[i][j] == dp[i - 1][j] + 1 {
            statuses[i - 1] = "missing"; // 参考词被删（漏读）
            i -= 1;
        } else {
            j -= 1; // 识别多读，不挂参考词
        }
    }
    while i > 0 {
        statuses[i - 1] = "missing";
        i -= 1;
    }
    statuses
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_match_scores_one() {
        let r = score_sentence(
            "The quick brown fox",
            "the quick brown fox",
            DEFAULT_THRESHOLD,
        );
        assert_eq!(r.score, 1.0);
        assert!(r.passed);
        assert!(r.words.iter().all(|w| w.status == "ok"));
        assert_eq!(r.words.len(), 4);
        assert_eq!(r.words[0].reference, "The"); // 保留原始大小写
    }

    #[test]
    fn missing_tail_word_marked_missing() {
        let r = score_sentence("the quick brown fox jumps", "the quick brown fox", 0.9);
        assert_eq!(r.words.len(), 5);
        assert_eq!(r.words[4].status, "missing");
        assert!((r.score - 0.8).abs() < 1e-9);
        assert!(!r.passed);
    }

    #[test]
    fn substituted_word_marked_wrong() {
        let r = score_sentence("i have a cat", "i have a dog", 0.9);
        assert_eq!(r.words[3].status, "wrong");
        assert_eq!(r.words[0].status, "ok");
        assert!((r.score - 0.75).abs() < 1e-9);
    }

    #[test]
    fn extra_words_do_not_lower_hit_rate() {
        // 多读 "really" 不应拉低命中率：参考 3 词全 ok。
        let r = score_sentence("it is good", "it is really good", 0.9);
        assert_eq!(r.score, 1.0);
        assert!(r.words.iter().all(|w| w.status == "ok"));
    }

    #[test]
    fn punctuation_and_case_ignored() {
        let r = score_sentence("Hello, world!", "hello world", 0.9);
        assert_eq!(r.score, 1.0);
    }

    #[test]
    fn empty_hyp_all_missing() {
        let r = score_sentence("one two three", "", 0.9);
        assert!(r.words.iter().all(|w| w.status == "missing"));
        assert_eq!(r.score, 0.0);
    }

    #[test]
    fn word_mode_exact_and_close() {
        let exact = score_word("cat", "cat", 0.9);
        assert_eq!(exact.score, 1.0);
        assert!(exact.passed);

        // "cap" vs "cat"：3 字符差 1 → sim ≈ 0.667，默认 0.9 不过
        let close = score_word("cat", "cap", 0.9);
        assert!(close.score < 0.9);
        assert!(!close.passed);
        assert_eq!(close.words[0].status, "wrong");

        // 句中含目标词 → 取最佳
        let in_sentence = score_word("brown", "the brown one", 0.9);
        assert_eq!(in_sentence.score, 1.0);
        assert!(in_sentence.passed);
    }

    #[test]
    fn kind_parse() {
        assert_eq!(ShadowKind::parse("sentence"), Some(ShadowKind::Sentence));
        assert_eq!(ShadowKind::parse("WORD"), Some(ShadowKind::Word));
        assert_eq!(ShadowKind::parse("x"), None);
    }
}
