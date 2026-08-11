//! 热词候选挖掘（P1.5，纯函数层）。
//!
//! 把「我说了但系统不认识的词」捞出来，产物是候选清单，**人工审读后才进
//! `asr.hotwords`**——沿用 P2a 的既定取舍：只挖掘、不自动上线（design §10 取舍 1）。
//!
//! 设计权威源：docs/2026-07-24-homophone-correction/design.md §4.2 / §4.3。
//! 读数发现：真实纠错样本里真同音字错只有 3 条，主导错误是**专名/术语没进热词表**
//! （携程、驻留、签名池、zuche、mihomo…）。热词是「提示」不是「替换」，**没有过纠风险**，
//! 因此这条线不受 §9.4 负样本门槛的约束，可以先做。
//!
//! 两路输入，互不干扰：
//! - [`mine_corrections`]：纠错样本 `(O, Y')`。用户特地改出来的词就是最该进热词的词。
//!   复用 [`super::homophone`] 的同一套 LCS diff（不再写第二份对齐逻辑），取 **Y' 侧**
//!   的 insert / replace 片段。**与同音挖掘的取舍相反**：那边只要等长纯汉字 replace，
//!   这边恰恰要 insert 和不等长 replace——「多出来的词」才是词表缺口。
//! - [`mine_scenes`]：场景记录的交付文本（无金标、量大）。**无分词依赖**的新词发现：
//!   拉丁/混合 token 直接抽；中文候选走 n-gram 频次 + 左右邻字熵（边界稳定性）+
//!   凝固度（PMI），只留高频且边界稳的串。
//!
//! **不碰 `→` 语法**（design §2.4）：热词条目只写词面，绝不承载替换规则。

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use super::homophone::{context_snippet, diff_blocks, is_han, Block};

/// 候选来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// 来自纠错样本的 Y' 侧（用户手改出来的词），可信度最高。
    Correction,
    /// 来自场景记录交付文本的新词发现，需人工甄别。
    Scene,
}

/// 一条热词候选。
#[derive(Debug, Clone, Serialize)]
pub struct HotwordCandidate {
    /// 词面。**只有词面**，不含任何 `wrong→right` 结构。
    pub term: String,
    pub source: Source,
    /// 出现次数（纠错侧=被改出来的次数，场景侧=在交付文本里的频次）。
    pub freq: usize,
    /// 排序信号，越大越值得人工先看。跨 source 不可比。
    pub score: f64,
    /// 最多 3 条上下文片段，命中词用〔〕括出。
    pub contexts: Vec<String>,
}

/// 词面长度上限（字符）。超长的是句子不是词。
const MAX_TERM_CHARS: usize = 8;
/// 同 homophone：DP 是 O(n·m)，超长输入直接放弃。
const MAX_DIFF_CHARS: usize = 2000;
/// 「词条式 correction」判据：correction 短于此且远短于交付文本时，整条就是一个词面
/// （现有 `label="hotword"` 的填法，如 correction 直接写 "驻留" / "zuche"）。
const TERM_LIKE_MAX_CHARS: usize = 12;

/// 中文 n-gram 的长度范围。
const NGRAM_MIN: usize = 2;
const NGRAM_MAX: usize = 4;
/// 场景侧频次门槛：低于此的串没有统计意义，噪声压不住。
const SCENE_MIN_FREQ: usize = 5;
/// 文档频次门槛：**必须出现在多次不同的交付里**。只在一两段话里反复出现的，多半是
/// 那次讨论的临时说法，不是长期需要的词。
const SCENE_MIN_DOC_FREQ: usize = 3;
/// 左右邻字熵门槛（bit）：低于此说明这个串总黏在同一个上下文里，多半是更长词的碎片。
const MIN_ENTROPY: f64 = 1.2;
/// 凝固度门槛：log2(PMI)，低于此说明只是常用字碰巧连在一起。
const MIN_PMI: f64 = 5.0;
/// 拉丁 token 频次门槛（比中文低：拉丁串本身就少见，出现两次多半是专名）。
const LATIN_MIN_FREQ: usize = 3;
/// 场景侧导出上限：产物是给人一条条看的，几百条等于没导。
const SCENE_MAX_OUT: usize = 80;

/// 高频虚词/功能字：作为候选的**首字或尾字**即判为切碎，直接丢。
/// 只用于边界判定，不做词内过滤（「这个」的「这」在词首要丢，但「哪里」的「里」不丢）。
/// 含方位/量词尾（下、里、面、上、中、时…）——「看一下」「库里面」「服务器上」这类
/// 粘连尾巴靠它清掉。代价是「线下」这种真词也会被误杀：**热词表宁缺勿滥**，漏一个
/// 手动加即可，混进一堆通用词会让人放弃审读整份清单。
const STOP_EDGE: &str = "的了是我你他她它这那些什么么就都很和跟然后但因为所以还也要会能不没有在给对把被让说着过吧呢啊哦嗯个们其之与及或者如果一二三四五六七八九十再又更最只而且于以到从向由为按当下里面上中时后前内外间已还把用做去来看想每种些位条张台个话怎哪";

/// 纯噪声词：ASR 口水词与万能搭配，不该进热词表。
const NOISE_TERMS: [&str; 56] = [
    "然后", "这个", "那个", "就是", "什么", "怎么", "可以", "我们", "你们", "他们", "现在", "一下",
    "一个", "这样", "那样", "问题", "东西", "时候", "地方", "情况", "方式", "内容", "需要", "可能",
    "应该", "知道", "直接", "实际", "肯定", "非常", "另外", "全部", "好像", "导致", "已经", "还有",
    "或者", "而且", "如果", "所以", "因为", "但是", "不是", "没有", "觉得", "感觉", "意思", "办法",
    "结果", "开始", "继续", "确认", "考虑", "整体", "具体", "目前",
];

/// 常见英文虚词/通用词：拉丁 token 侧的停用表。热词表要的是专名，不是英语课本。
const LATIN_STOP: [&str; 40] = [
    "the", "and", "for", "you", "this", "that", "with", "have", "not", "but", "are", "was", "can",
    "will", "all", "one", "two", "get", "got", "out", "now", "new", "old", "yes", "no", "ok",
    "okay", "http", "https", "www", "com", "cn", "org", "net", "id", "url", "api", "app", "user",
    "test",
];

/// 从纠错样本挖候选。`pairs` = `(交付文本 O, 纠正文本 Y')`，附带样本 id 供人工回溯。
pub fn mine_corrections(pairs: &[(i64, String, String)]) -> Vec<HotwordCandidate> {
    let mut agg: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    for (_id, o, y) in pairs {
        for (term, ctx) in extract_from_pair(o, y) {
            let e = agg.entry(term).or_insert_with(|| (0, Vec::new()));
            e.0 += 1;
            if e.1.len() < 3 {
                e.1.push(ctx);
            }
        }
    }
    let mut out: Vec<HotwordCandidate> = agg
        .into_iter()
        .map(|(term, (freq, contexts))| HotwordCandidate {
            // 用户手改出来的词，频次本身就是强信号，score 直接取频次。
            score: freq as f64,
            term,
            source: Source::Correction,
            freq,
            contexts,
        })
        .collect();
    sort_candidates(&mut out);
    out
}

/// 单对 `(O, Y')` 的候选词面 + 上下文（Y' 侧）。
fn extract_from_pair(o: &str, y: &str) -> Vec<(String, String)> {
    let y_trim = y.trim();
    if y_trim.is_empty() {
        return Vec::new();
    }
    // 词条式 correction：整条就是一个词面（现有 hotword 标签的填法）。此时不跑 diff——
    // 拿一个词跟整段话 diff 只会得到一堆碎片。
    let o_chars = o.chars().count();
    let y_chars = y_trim.chars().count();
    if y_chars <= TERM_LIKE_MAX_CHARS && (o_chars >= y_chars * 3 || o_chars == 0) {
        return match normalize_term(y_trim) {
            Some(t) => vec![(t, format!("〔{y_trim}〕"))],
            None => Vec::new(),
        };
    }

    let a: Vec<char> = o.chars().collect();
    let b: Vec<char> = y.chars().collect();
    if a.len() > MAX_DIFF_CHARS || b.len() > MAX_DIFF_CHARS {
        return Vec::new();
    }
    // 同音挖掘**实际认领**的块让给它，避免「涵数→函数」被当成词表缺口。
    // 注意是按认领结果排除，不是按「等长纯汉字」这个形状排除——「助理→驻留」也是等长纯汉字，
    // 但读音不等价（理 lǐ / 留 liú），homophone 会整块丢弃，那它就该由热词侧接住。
    let claimed: HashSet<(String, String)> = super::homophone::mine(o, y)
        .into_iter()
        .map(|c| (c.wrong, c.right))
        .collect();
    let mut out = Vec::new();
    for block in diff_blocks(&a, &b) {
        let Block::Change {
            del,
            ins,
            ins_start,
            ..
        } = block
        else {
            continue;
        };
        if ins.is_empty() {
            // 纯删除：用户删掉了一段，跟词表缺口无关。
            continue;
        }
        let ins_text: String = ins.iter().collect();
        let del_text: String = del.iter().collect();
        if claimed.contains(&(del_text, ins_text.clone())) {
            continue;
        }
        // diff 可能把拉丁词切在中间（`hello`→`hell`+`o` 只剩 `ll`）。按 b 侧的 token
        // 边界补回完整词面，否则挖出来的是 `mih` / `mo` 这种碎片。
        let (start, len) = expand_to_token(&b, ins_start, ins.len());
        let term_text: String = b[start..start + len].iter().collect();
        if let Some(term) = normalize_term(&term_text) {
            out.push((term, context_snippet(&b, start, len)));
        }
    }
    out
}

/// 若命中片段的两端切在拉丁 token 中间，向两侧扩到完整 token 边界。
/// 纯汉字片段原样返回（汉字没有 token 边界可言，扩了反而把上下文粘进来）。
fn expand_to_token(b: &[char], start: usize, len: usize) -> (usize, usize) {
    if len == 0
        || !b[start..start + len]
            .iter()
            .any(|c| c.is_ascii_alphanumeric())
    {
        return (start, len);
    }
    let mut lo = start;
    while lo > 0 && is_latin_body(b[lo - 1]) && is_latin_body(b[lo]) {
        lo -= 1;
    }
    let mut hi = start + len;
    while hi < b.len() && is_latin_body(b[hi]) && is_latin_body(b[hi - 1]) {
        hi += 1;
    }
    (lo, hi - lo)
}

/// 词面归一：剥首尾空白/标点，拒绝空串、超长、纯数字、纯标点、噪声词、首尾虚词。
fn normalize_term(raw: &str) -> Option<String> {
    let t: String = raw
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && !is_han(c))
        .to_string();
    if t.is_empty() {
        return None;
    }
    let chars: Vec<char> = t.chars().collect();
    if chars.len() > MAX_TERM_CHARS {
        return None;
    }
    if chars.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if NOISE_TERMS.contains(&t.as_str()) {
        return None;
    }
    // 单字候选一律不要：单字进热词表几乎必然误伤。
    if chars.len() < 2 {
        return None;
    }
    if has_stop_edge(&chars) {
        return None;
    }
    Some(t)
}

/// 首字或尾字是高频虚词 → 判为从句子里切碎的片段。纯拉丁串不适用此规则。
fn has_stop_edge(chars: &[char]) -> bool {
    let first = chars[0];
    let last = chars[chars.len() - 1];
    if !is_han(first) && !is_han(last) {
        return false;
    }
    STOP_EDGE.contains(first) || STOP_EDGE.contains(last)
}

/// 从场景记录的交付文本挖新词。`texts` = 每次交付的整段文本。
pub fn mine_scenes(texts: &[String]) -> Vec<HotwordCandidate> {
    let mut out = mine_scene_latin(texts);
    out.extend(mine_scene_han(texts));
    sort_candidates(&mut out);
    out
}

/// 拉丁 / 混合 token（zuche、mihomo、G10、MCP…）：直接按 token 频次抽。
fn mine_scene_latin(texts: &[String]) -> Vec<HotwordCandidate> {
    // key = 小写形，value = (频次, 最常见原形计数, 上下文)
    let mut agg: HashMap<String, (usize, HashMap<String, usize>, Vec<String>)> = HashMap::new();
    for text in texts {
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            if !is_latin_start(chars[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && is_latin_body(chars[i]) {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let token = token.trim_matches(|c: char| !c.is_alphanumeric());
            if token.chars().count() < 2 || token.chars().count() > MAX_TERM_CHARS * 3 {
                continue;
            }
            if !token.chars().any(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            let lower = token.to_ascii_lowercase();
            if LATIN_STOP.contains(&lower.as_str()) {
                continue;
            }
            let e = agg
                .entry(lower)
                .or_insert_with(|| (0, HashMap::new(), Vec::new()));
            e.0 += 1;
            *e.1.entry(token.to_string()).or_insert(0) += 1;
            if e.2.len() < 3 {
                e.2.push(context_snippet(&chars, start, i - start));
            }
        }
    }
    agg.into_iter()
        .filter(|(_, (freq, _, _))| *freq >= LATIN_MIN_FREQ)
        .map(|(_lower, (freq, forms, contexts))| {
            // 展示用原形取出现最多的那个（Claude 而不是 claude）。
            let term = forms
                .into_iter()
                .max_by_key(|(form, n)| (*n, std::cmp::Reverse(form.clone())))
                .map(|(form, _)| form)
                .unwrap_or_default();
            HotwordCandidate {
                score: freq as f64,
                term,
                source: Source::Scene,
                freq,
                contexts,
            }
        })
        .collect()
}

/// 中文新词发现：n-gram 频次 + 左右邻字熵 + 凝固度（PMI）。无分词依赖。
fn mine_scene_han(texts: &[String]) -> Vec<HotwordCandidate> {
    // 单字频次（算 PMI 用）与 n-gram 统计。
    let mut uni: HashMap<char, usize> = HashMap::new();
    let mut total_chars = 0usize;
    // gram → 统计。`docs`/`last_doc` 是「出现在多少条不同交付里」的增量计数。
    #[derive(Default)]
    struct GramStat {
        freq: usize,
        left: HashMap<char, usize>,
        right: HashMap<char, usize>,
        contexts: Vec<String>,
        docs: usize,
        last_doc: Option<usize>,
    }
    let mut grams: HashMap<String, GramStat> = HashMap::new();

    for (doc_idx, text) in texts.iter().enumerate() {
        let chars: Vec<char> = text.chars().collect();
        // 汉字连续段：n-gram 不跨越标点/拉丁/空白，天然挡掉「话...题」这类跨句拼接。
        let mut seg_start = 0usize;
        let mut i = 0usize;
        while i <= chars.len() {
            let at_end = i == chars.len();
            if at_end || !is_han(chars[i]) {
                if i > seg_start {
                    let seg = &chars[seg_start..i];
                    for &c in seg {
                        *uni.entry(c).or_insert(0) += 1;
                        total_chars += 1;
                    }
                    for n in NGRAM_MIN..=NGRAM_MAX {
                        if seg.len() < n {
                            break;
                        }
                        for s in 0..=seg.len() - n {
                            let g: String = seg[s..s + n].iter().collect();
                            let e = grams.entry(g).or_default();
                            e.freq += 1;
                            if e.last_doc != Some(doc_idx) {
                                e.last_doc = Some(doc_idx);
                                e.docs += 1;
                            }
                            if s > 0 {
                                *e.left.entry(seg[s - 1]).or_insert(0) += 1;
                            } else {
                                // 段首/段尾也是一种「邻字」，且是最自由的那种。
                                *e.left.entry('\u{0}').or_insert(0) += 1;
                            }
                            if s + n < seg.len() {
                                *e.right.entry(seg[s + n]).or_insert(0) += 1;
                            } else {
                                *e.right.entry('\u{0}').or_insert(0) += 1;
                            }
                            if e.contexts.len() < 3 {
                                e.contexts.push(context_snippet(&chars, seg_start + s, n));
                            }
                        }
                    }
                }
                seg_start = i + 1;
            }
            i += 1;
        }
    }

    if total_chars == 0 {
        return Vec::new();
    }

    // 过频次/文档频次门槛 + 边界/凝固度筛。
    let mut kept: Vec<(String, usize, f64)> = Vec::new();
    for (gram, st) in &grams {
        if st.freq < SCENE_MIN_FREQ || st.docs < SCENE_MIN_DOC_FREQ {
            continue;
        }
        let chars: Vec<char> = gram.chars().collect();
        if NOISE_TERMS.contains(&gram.as_str()) || has_stop_edge(&chars) {
            continue;
        }
        if entropy(&st.left).min(entropy(&st.right)) < MIN_ENTROPY {
            continue;
        }
        let pmi = pmi_score(&chars, st.freq, &uni, total_chars);
        if pmi < MIN_PMI {
            continue;
        }
        // 排序偏凝固度而非频次：高频通用词（「记录」「测试」）压不下去的话，
        // 前几十条就全被它们占满，真专名反而看不到。
        // 双字中文再降权：没有通用词词典可对比，而双字恰是通用词的主要形态
        // （数据/文档/分析…），三字以上的串是专名的准确率高得多。
        let bigram_penalty = if chars.len() == 2 { 0.6 } else { 1.0 };
        kept.push((
            gram.clone(),
            st.freq,
            pmi * (st.docs as f64).ln() * bigram_penalty,
        ));
    }

    // 去嵌套：短串被更长的保留串包含且频次相近（≤1.2 倍）→ 短串只是长词的一部分，丢短的。
    //
    // 反方向（「长串是子串 + 粘连字」，如「台服务器」之于「服务器」）**故意不按频次比处理**：
    // 试过「子串频次 ≥ 父串 2 倍即丢父串」，它会连「请求头」一起杀掉（「请求」141 次 vs
    // 「请求头」24 次），而「请求头」恰恰是要的词。量词/方位粘连由 STOP_EDGE 的首尾字
    // 判定挡住（「台」「张」在表内），比频次比精准得多。
    let kept_clone = kept.clone();
    kept.retain(|(gram, freq, _)| {
        let n = gram.chars().count();
        let swallowed_by_longer = kept_clone.iter().any(|(other, ofreq, _)| {
            other.chars().count() > n
                && other.contains(gram.as_str())
                && (*freq as f64) <= (*ofreq as f64) * 1.2
        });
        !swallowed_by_longer
    });

    kept.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    kept.truncate(SCENE_MAX_OUT);
    kept.into_iter()
        .map(|(gram, freq, score)| {
            let contexts = grams
                .get(&gram)
                .map(|st| st.contexts.clone())
                .unwrap_or_default();
            HotwordCandidate {
                term: gram,
                source: Source::Scene,
                freq,
                score,
                contexts,
            }
        })
        .collect()
}

/// 邻字分布的香农熵（bit）。分布越散说明这个串的边界越自由，越可能是个完整的词。
fn entropy(counts: &HashMap<char, usize>) -> f64 {
    let total: usize = counts.values().sum();
    if total == 0 {
        return 0.0;
    }
    let total = total as f64;
    -counts
        .values()
        .map(|&c| {
            let p = c as f64 / total;
            p * p.log2()
        })
        .sum::<f64>()
}

/// 凝固度：所有二分切法里最小的 log2 互信息。取最小值 = 最保守的那种切法也粘得住。
fn pmi_score(chars: &[char], freq: usize, uni: &HashMap<char, usize>, total: usize) -> f64 {
    let total = total as f64;
    let p_gram = freq as f64 / total;
    let mut min_pmi = f64::MAX;
    for split in 1..chars.len() {
        // 子串概率用「组成字概率之积」近似（不额外维护子串表，n≤4 时够用且偏保守）。
        let p_left: f64 = chars[..split]
            .iter()
            .map(|c| *uni.get(c).unwrap_or(&1) as f64 / total)
            .product();
        let p_right: f64 = chars[split..]
            .iter()
            .map(|c| *uni.get(c).unwrap_or(&1) as f64 / total)
            .product();
        if p_left <= 0.0 || p_right <= 0.0 {
            continue;
        }
        min_pmi = min_pmi.min((p_gram / (p_left * p_right)).log2());
    }
    if min_pmi == f64::MAX {
        0.0
    } else {
        min_pmi
    }
}

/// 排序：score 降序，同分按词面稳定排序（导出结果可复现）。
fn sort_candidates(v: &mut [HotwordCandidate]) {
    v.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.term.cmp(&b.term))
    });
}

/// 剔除已在热词表里的词（大小写不敏感）。
pub fn drop_existing(
    v: Vec<HotwordCandidate>,
    existing: &HashSet<String>,
) -> Vec<HotwordCandidate> {
    let lower: HashSet<String> = existing.iter().map(|s| s.to_ascii_lowercase()).collect();
    v.into_iter()
        .filter(|c| !lower.contains(&c.term.to_ascii_lowercase()))
        .collect()
}

fn is_latin_start(c: char) -> bool {
    c.is_ascii_alphabetic()
}

fn is_latin_body(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '#')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(v: &[HotwordCandidate]) -> Vec<&str> {
        v.iter().map(|c| c.term.as_str()).collect()
    }

    /// 词条式 correction（现有 hotword 标签的填法）直接成词，不跑 diff。
    #[test]
    fn correction_term_like() {
        let pairs = vec![(
            1i64,
            "测试携程的时候通过网关去发起一个接口".to_string(),
            "驻留".to_string(),
        )];
        let got = mine_corrections(&pairs);
        assert_eq!(terms(&got), vec!["驻留"]);
    }

    /// 整句 correction：取 Y' 侧真正多出来的词面。
    #[test]
    fn correction_replace_block() {
        let pairs = vec![(
            1i64,
            "先把登录助理这个功能给他补全".to_string(),
            "先把登录驻留这个功能给他补全".to_string(),
        )];
        let got = mine_corrections(&pairs);
        assert!(terms(&got).contains(&"驻留"), "got {:?}", terms(&got));
    }

    /// 等长纯汉字 replace 是同音字错的形状，让给 homophone，这里不产出。
    #[test]
    fn correction_skips_homophone_shape() {
        let pairs = vec![(
            1i64,
            "给我编义的这个命令".to_string(),
            "给我编译的这个命令".to_string(),
        )];
        assert!(mine_corrections(&pairs).is_empty());
    }

    /// 纯删除不产候选。
    #[test]
    fn correction_pure_delete() {
        let pairs = vec![(
            1i64,
            "用来测试后面的我们的app".to_string(),
            "用来测试我们的app".to_string(),
        )];
        let got = mine_corrections(&pairs);
        assert!(terms(&got).is_empty(), "got {:?}", terms(&got));
    }

    /// 首尾虚词的碎片一律丢。
    #[test]
    fn normalize_rejects_stop_edge() {
        assert!(normalize_term("的驻留").is_none());
        assert!(normalize_term("驻留的").is_none());
        assert!(normalize_term("驻留").is_some());
    }

    #[test]
    fn normalize_rejects_junk() {
        assert!(normalize_term("").is_none());
        assert!(normalize_term("12345").is_none());
        assert!(normalize_term("，。！").is_none());
        assert!(normalize_term("然后").is_none());
        assert!(normalize_term("驻").is_none(), "单字不进热词表");
        assert!(normalize_term("这是一个非常长的句子不该成词").is_none());
    }

    /// 拉丁 token：够频次即抽，停用词滤掉，展示用最常见原形。
    #[test]
    fn scene_latin_tokens() {
        let texts: Vec<String> = (0..5)
            .map(|_| "这个 zuche 的网关要走 mihomo 的 the app".to_string())
            .collect();
        let got = mine_scenes(&texts);
        let t = terms(&got);
        assert!(t.contains(&"zuche"), "got {t:?}");
        assert!(t.contains(&"mihomo"), "got {t:?}");
        assert!(!t.contains(&"the"), "停用词不该出现: {t:?}");
        assert!(!t.contains(&"app"), "停用词不该出现: {t:?}");
    }

    /// 中文新词：高频且边界自由的专名能被捞出，口水词不会。
    #[test]
    fn scene_han_new_word() {
        let mut texts: Vec<String> = Vec::new();
        for i in 0..8 {
            texts.push(format!("然后这个签名池要重启一下第{i}次"));
            texts.push(format!("签名池的账号轮转有问题吗第{i}次"));
            texts.push(format!("我看了签名池，然后发现请求头缺失{i}"));
        }
        let got = mine_scenes(&texts);
        let t = terms(&got);
        assert!(t.contains(&"签名池"), "应挖出「签名池」: {t:?}");
        assert!(!t.contains(&"然后"), "口水词不该出现: {t:?}");
    }

    /// 去嵌套：长串保留时，频次相近的短碎片不重复出现。
    #[test]
    fn scene_han_drops_nested() {
        let texts: Vec<String> = (0..10)
            .map(|i| format!("重启签名池服务，检查签名池服务状态{i}"))
            .collect();
        let got = mine_scenes(&texts);
        let t = terms(&got);
        assert!(
            !t.contains(&"名池") && !t.contains(&"池服"),
            "碎片不该保留: {t:?}"
        );
    }

    /// 已在热词表里的词不再作为候选。
    #[test]
    fn drop_existing_works() {
        let v = vec![
            HotwordCandidate {
                term: "zuche".into(),
                source: Source::Scene,
                freq: 9,
                score: 9.0,
                contexts: vec![],
            },
            HotwordCandidate {
                term: "驻留".into(),
                source: Source::Correction,
                freq: 2,
                score: 2.0,
                contexts: vec![],
            },
        ];
        let existing: HashSet<String> = ["Zuche".to_string()].into_iter().collect();
        let got = drop_existing(v, &existing);
        assert_eq!(terms(&got), vec!["驻留"]);
    }

    /// 空输入不 panic、不产候选。
    #[test]
    fn empty_inputs() {
        assert!(mine_corrections(&[]).is_empty());
        assert!(mine_scenes(&[]).is_empty());
        assert!(mine_scenes(&["。，！".to_string()]).is_empty());
    }
}
