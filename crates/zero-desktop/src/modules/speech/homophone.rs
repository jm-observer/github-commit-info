//! 同音字候选挖掘（P2a，纯函数层）。
//!
//! 输入一对文本 `(O, Y')`（交付出去的优化文 / 用户纠正后的文本），产出同音替换候选
//! `wrong → right`。**只挖掘、不替换、不落表**——产物仅供人工审读（§6.3：读音等价 ≠
//! 同音纠错，用户改专名/改习惯也读音等价，必须人工 approve 才算数）。
//!
//! 设计权威源：docs/2026-07-24-homophone-correction/design.md §6。要点：
//! - §6.1 按字符 LCS diff，**只接受** replace 块且须同时满足：两侧字符数相同、纯汉字、
//!   ≤6 字、**前后都存在 equal 块**（整段 replace 拒绝，哪怕位于文本首尾——v3 堵死了
//!   v2 的首尾豁免）。insert/delete 一律丢弃（反例甲：朴素配对会把 insert 错位配成
//!   假同音对）。
//! - §6.2 读音三级：`exact`（带声调读音一致且两侧均非多音字）只有它能自动进候选；
//!   `polyphone`（仅多音字读音集合交集非空）与 `fuzzy`（需模糊音归并 zh/z ch/c sh/s
//!   n/l an/ang in/ing en/eng 或忽略声调）只导出给人工看。
//! - 丢弃是廉价的：宁可丢一批可疑的，不放一条脏对进词表。

use pinyin::{ToPinyin, ToPinyinMulti};
use serde::Serialize;

/// 读音匹配等级（严格程度递减）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// 带声调读音完全一致，且两侧均非多音字。唯一可自动进候选的等级。
    Exact,
    /// 仅在「多音字读音集合交集非空」的放宽下等价。仅供人工审读。
    Polyphone,
    /// 需模糊音归并（zh/z、ch/c、sh/s、n/l、an/ang、in/ing、en/eng）或忽略声调。仅供人工审读。
    Fuzzy,
}

impl MatchKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Polyphone => "polyphone",
            MatchKind::Fuzzy => "fuzzy",
        }
    }
}

/// 一条挖掘出的候选（尚未聚合、未过 hits 门槛）。
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// O 侧（交付文本里）的错词。
    pub wrong: String,
    /// Y' 侧（用户纠正后）的对词。
    pub right: String,
    /// 对词的主读音（带声调，按字空格分隔）。exact 级即双方共同读音。
    pub reading: String,
    pub match_kind: MatchKind,
    /// O 侧命中位置前后各 ~5 字的上下文片段，供人工判断。
    pub context: String,
}

/// 防御性上限：diff 是 O(n·m) 的 DP，交付文本都是句子级，超长输入直接放弃挖掘。
const MAX_DIFF_CHARS: usize = 2000;
/// §6.1 条件 5：replace 块两侧长度上限。
const MAX_BLOCK_CHARS: usize = 6;

/// 挖掘入口：`o` = 交付出去的文本（text_optimized，缺则 text_raw），`y` = 用户纠正文本。
pub fn mine(o: &str, y: &str) -> Vec<Candidate> {
    if o.is_empty() || y.is_empty() || o == y {
        return Vec::new();
    }
    let a: Vec<char> = o.chars().collect();
    let b: Vec<char> = y.chars().collect();
    if a.len() > MAX_DIFF_CHARS || b.len() > MAX_DIFF_CHARS {
        return Vec::new();
    }

    let blocks = diff_blocks(&a, &b);
    let mut out = Vec::new();
    for (idx, block) in blocks.iter().enumerate() {
        let Block::Change {
            del,
            ins,
            del_start,
            ..
        } = block
        else {
            continue;
        };
        // §6.1 条件 1/3/5：等长、纯汉字、≤6 字。
        if del.len() != ins.len() || del.is_empty() || del.len() > MAX_BLOCK_CHARS {
            continue;
        }
        if !del.iter().chain(ins.iter()).all(|&c| is_han(c)) {
            continue;
        }
        // §6.1 条件 4：前后都必须存在 equal 块（blocks 构造保证 equal/change 交替，
        // 只需不在首尾）。整段 replace 在此被拒。
        if idx == 0 || idx + 1 == blocks.len() {
            continue;
        }
        debug_assert!(matches!(blocks[idx - 1], Block::Equal));
        debug_assert!(matches!(blocks[idx + 1], Block::Equal));

        // §6.2 逐位判级，取整块最宽松位（任一位判不上 fuzzy → 整块丢弃）。
        let mut kind = MatchKind::Exact;
        let mut readings_right = Vec::with_capacity(ins.len());
        let mut ok = true;
        for (&w, &r) in del.iter().zip(ins.iter()) {
            match grade_char_pair(w, r) {
                Some((k, right_reading)) => {
                    kind = kind.max(k);
                    readings_right.push(right_reading);
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }

        let wrong: String = del.iter().collect();
        let right: String = ins.iter().collect();
        if wrong == right {
            continue;
        }
        out.push(Candidate {
            context: context_snippet(&a, *del_start, del.len()),
            wrong,
            right,
            reading: readings_right.join(" "),
            match_kind: kind,
        });
    }
    out
}

/// 判一对字符的读音等级；判不上任何等级（含非汉字/查不到读音）返回 None。
/// 返回 (等级, 右侧字的主读音)。
///
/// exact 的「非多音字（或读音可唯一确定）」落地为**主读音表相等**：heteronym 表收了
/// 大量罕用读音（池: chí/tuó/chè、涵: hán/hàn），严格按「读音集合是单元素」判会把
/// exact 层掏空；而真歧义的多音字（长 zhǎng/cháng vs 常 cháng）主读音必不同，
/// 自然落到 polyphone，不会被错判成 exact。
fn grade_char_pair(w: char, r: char) -> Option<(MatchKind, String)> {
    let rw = readings_of(w);
    let rr = readings_of(r);
    if rw.is_empty() || rr.is_empty() {
        return None;
    }
    let right_primary = rr[0].tone.clone();
    // exact：主读音（常用读音）带声调一致。
    if let (Some(pw), Some(pr)) = (primary_of(w), primary_of(r)) {
        if pw == pr {
            return Some((MatchKind::Exact, right_primary));
        }
    }
    // polyphone：带声调读音集合交集非空。
    if rw.iter().any(|x| rr.iter().any(|y| x.tone == y.tone)) {
        return Some((MatchKind::Polyphone, right_primary));
    }
    // fuzzy：模糊音归并 + 忽略声调后等价。
    if rw.iter().any(|x| {
        rr.iter()
            .any(|y| fuzzy_key(&x.plain) == fuzzy_key(&y.plain))
    }) {
        return Some((MatchKind::Fuzzy, right_primary));
    }
    None
}

struct Reading {
    /// 带声调（如 "hán"）。
    tone: String,
    /// 无声调（如 "han"）。
    plain: String,
}

/// 主读音（常用读音，带声调）：单读音表的取值。
fn primary_of(c: char) -> Option<String> {
    c.to_pinyin().map(|p| p.with_tone().to_string())
}

/// 一个字的全部读音（多音字含所有读音；查不到返回空）。
fn readings_of(c: char) -> Vec<Reading> {
    if let Some(multi) = c.to_pinyin_multi() {
        return multi
            .into_iter()
            .map(|p| Reading {
                tone: p.with_tone().to_string(),
                plain: p.plain().to_string(),
            })
            .collect();
    }
    // heteronym 表未收录时回退单读音表。
    if let Some(p) = c.to_pinyin() {
        return vec![Reading {
            tone: p.with_tone().to_string(),
            plain: p.plain().to_string(),
        }];
    }
    Vec::new()
}

/// 模糊音归一键：声母 zh→z / ch→c / sh→s / l→n，韵母 ang→an / eng→en / ing→in
/// （含 uang→uan、iang→ian 这类复韵母尾）。声调在 plain 里已剥离。
fn fuzzy_key(plain: &str) -> String {
    const INITIALS: [&str; 23] = [
        "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x",
        "r", "z", "c", "s", "y", "w",
    ];
    let (initial, final_part) = INITIALS
        .iter()
        .find(|ini| plain.starts_with(**ini))
        .map(|ini| (*ini, &plain[ini.len()..]))
        .unwrap_or(("", plain));
    let initial = match initial {
        "zh" => "z",
        "ch" => "c",
        "sh" => "s",
        "l" => "n",
        other => other,
    };
    let final_part = final_part
        .strip_suffix("ang")
        .map(|head| format!("{head}an"))
        .or_else(|| {
            final_part
                .strip_suffix("eng")
                .map(|head| format!("{head}en"))
        })
        .or_else(|| {
            final_part
                .strip_suffix("ing")
                .map(|head| format!("{head}in"))
        })
        .unwrap_or_else(|| final_part.to_string());
    format!("{initial}{final_part}")
}

pub(crate) fn is_han(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// O 侧命中位置前后各 5 字的上下文，命中词用〔〕括出。
pub(crate) fn context_snippet(a: &[char], start: usize, len: usize) -> String {
    const CTX: usize = 5;
    let lo = start.saturating_sub(CTX);
    let hi = (start + len + CTX).min(a.len());
    let mut s = String::new();
    s.extend(&a[lo..start]);
    s.push('〔');
    s.extend(&a[start..start + len]);
    s.push('〕');
    s.extend(&a[start + len..hi]);
    s
}

/// 字符级 diff 块。equal / change 严格交替（构造保证），change 内 del/ins 是同一
/// 变更区里两侧的原始字符序列。
///
/// `pub(crate)`：热词候选挖掘（[`super::hotword_mine`]）复用同一套 diff，避免两处各写一份
/// 对齐逻辑而结论不一致。
pub(crate) enum Block {
    /// 内容不需要，只需存在性（§6.1 条件 4 的前后 equal 判断）。
    Equal,
    Change {
        del: Vec<char>,
        ins: Vec<char>,
        /// del 在 a 中的起始下标（取上下文用）。
        del_start: usize,
        /// ins 在 b 中的起始下标（取 Y' 侧上下文用；同音挖掘不需要，热词挖掘要）。
        ins_start: usize,
    },
}

/// 经典 LCS DP + 回走，把对齐结果合并成 equal/change 交替的块序列。
pub(crate) fn diff_blocks(a: &[char], b: &[char]) -> Vec<Block> {
    let n = a.len();
    let m = b.len();
    // dp[i][j] = a[i..] 与 b[j..] 的 LCS 长度。
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut eq_len = 0usize;
    let mut del: Vec<char> = Vec::new();
    let mut ins: Vec<char> = Vec::new();
    let mut del_start = 0usize;
    let mut ins_start = 0usize;
    let (mut i, mut j) = (0usize, 0usize);

    macro_rules! flush_eq {
        () => {
            if eq_len > 0 {
                blocks.push(Block::Equal);
                eq_len = 0;
            }
        };
    }
    macro_rules! flush_change {
        () => {
            if !del.is_empty() || !ins.is_empty() {
                blocks.push(Block::Change {
                    del: std::mem::take(&mut del),
                    ins: std::mem::take(&mut ins),
                    del_start,
                    ins_start,
                });
            }
        };
    }

    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            flush_change!();
            eq_len += 1;
            i += 1;
            j += 1;
        } else {
            if del.is_empty() && ins.is_empty() {
                flush_eq!();
                del_start = i;
                ins_start = j;
            }
            // 已到某一侧末尾时只能走另一侧；否则按 DP 选保留更长 LCS 的方向。
            if j >= m || (i < n && dp[i + 1][j] >= dp[i][j + 1]) {
                del.push(a[i]);
                i += 1;
            } else {
                ins.push(b[j]);
                j += 1;
            }
        }
    }
    flush_change!();
    flush_eq!();
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mine_one(o: &str, y: &str) -> Candidate {
        let mut v = mine(o, y);
        assert_eq!(
            v.len(),
            1,
            "expected exactly one candidate for {o:?} -> {y:?}, got {v:?}"
        );
        v.remove(0)
    }

    /// exact：涵/函 均为唯一读音 hán，句中替换、前后有 equal。
    #[test]
    fn exact_single_char() {
        let c = mine_one("这个涵数有问题", "这个函数有问题");
        assert_eq!(c.wrong, "涵");
        assert_eq!(c.right, "函");
        assert_eq!(c.match_kind, MatchKind::Exact);
        assert_eq!(c.reading, "hán");
        assert!(c.context.contains("〔涵〕"), "context: {}", c.context);
    }

    /// 反例甲（§6.1）：insert 不是 replace，一律丢弃——朴素配对会把「参数/字段」
    /// 错位配成假同音对。
    #[test]
    fn insert_is_discarded() {
        assert!(mine("请输入函数参数", "请输入函数字段参数").is_empty());
    }

    /// 反例乙（§6.1 条件 4）：整段 replace 拒绝，哪怕恰好首尾即全文。
    #[test]
    fn whole_text_replace_is_rejected() {
        assert!(mine("函数", "寒暑").is_empty());
    }

    /// 条件 4 从严：文本开头的 replace（前面没有 equal 块）也拒绝——v3 堵死了
    /// v2 的「或位于文本首尾」豁免。
    #[test]
    fn replace_at_text_start_is_rejected() {
        assert!(mine("涵数有问题", "函数有问题").is_empty());
    }

    /// 真实样本（2026-07-25）：算数据库 → 双数据库。suàn/shuāng 需 s↔sh、uan↔uang
    /// 归并 + 忽略声调，判 fuzzy，仅供人工审读。
    #[test]
    fn real_case_suan_shuang_is_fuzzy() {
        let c = mine_one("能不能做成算数据库", "能不能做成双数据库");
        assert_eq!(c.wrong, "算");
        assert_eq!(c.right, "双");
        assert_eq!(c.match_kind, MatchKind::Fuzzy);
    }

    /// 真实样本：签名词 → 签名池。cí/chí 是 c↔ch 模糊音。
    #[test]
    fn real_case_ci_chi_is_fuzzy() {
        let c = mine_one("一换签名词就出错", "一换签名池就出错");
        assert_eq!(c.wrong, "词");
        assert_eq!(c.right, "池");
        assert_eq!(c.match_kind, MatchKind::Fuzzy);
    }

    /// 多音字：长(cháng/zhǎng) 与 常(cháng) 仅在读音集合交集意义下等价 → polyphone。
    #[test]
    fn polyphone_is_flagged_not_exact() {
        let c = mine_one("这个长识在这里", "这个常识在这里");
        assert_eq!(c.match_kind, MatchKind::Polyphone);
    }

    /// 读音完全不相干的 replace（口误/改写，§3 C 类）整块丢弃。
    #[test]
    fn unrelated_replace_is_discarded() {
        assert!(mine("我想吃苹果啊", "我想吃香蕉啊").is_empty());
    }

    /// 含非汉字的 replace 块丢弃（§6.1 条件 3）。
    #[test]
    fn non_han_replace_is_discarded() {
        assert!(mine("版本是A的问题", "版本是B的问题").is_empty());
    }

    /// 两侧长度不同的变更区（replace+insert 混在同一区）丢弃（§6.1 条件 1）。
    /// 注意与「独立的 insert 块不影响别处合法 replace 块」区分：这里 函库 与 涵
    /// 在同一变更区（后随 equal「数啊」），长度 2≠1，整块丢弃。
    #[test]
    fn unequal_length_change_is_discarded() {
        assert!(mine("这里有涵数啊", "这里有函库数啊").is_empty());
    }

    /// 超过 6 字的 replace 块丢弃（§6.1 条件 5）。
    #[test]
    fn overlong_block_is_discarded() {
        // 构造 7 字全同音替换不现实，用读音等价的短句验证长度闸门本身:
        // 两侧各 7 个汉字的变更区,即使逐位可比也拒绝。
        let o = "前缀一二三四五六七后缀";
        let y = "前缀壹贰叁肆伍陆柒后缀";
        assert!(mine(o, y).is_empty());
    }

    /// 一段文本里多个独立 replace 块各自产出候选。
    #[test]
    fn multiple_blocks_each_yield_candidate() {
        let v = mine("这个涵数和那个涵数都错了", "这个函数和那个函数都错了");
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|c| c.wrong == "涵" && c.right == "函"));
    }

    /// O == Y' / 空输入直接返回空。
    #[test]
    fn trivial_inputs_yield_nothing() {
        assert!(mine("一样的文本", "一样的文本").is_empty());
        assert!(mine("", "x").is_empty());
        assert!(mine("x", "").is_empty());
    }

    #[test]
    fn fuzzy_key_merges() {
        assert_eq!(fuzzy_key("zhang"), fuzzy_key("zan"));
        assert_eq!(fuzzy_key("chi"), fuzzy_key("ci"));
        assert_eq!(fuzzy_key("shuang"), fuzzy_key("suan"));
        assert_eq!(fuzzy_key("lin"), fuzzy_key("ning"));
        assert_ne!(fuzzy_key("hong"), fuzzy_key("hun"));
        assert_ne!(fuzzy_key("ma"), fuzzy_key("mo"));
    }
}
