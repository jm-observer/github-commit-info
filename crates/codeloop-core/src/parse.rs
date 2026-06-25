//! 复核回复解析：VERDICT 行 + ASK_USER 结构化标记。纯函数，便于单测。

use serde::{Deserialize, Serialize};

/// Codex 复核结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// 无明显错误，终止循环。
    Pass,
    /// 上方有问题清单，继续修订（含解析不到时的保守兜底）。
    NeedsWork,
}

/// 一个待用户拍板的结构化问题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUser {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// 解析 VERDICT：取**最后一次**出现的 `VERDICT: PASS|NEEDS_WORK`。
///
/// 解析不到 → 保守视为 `NeedsWork`（由调用方据「连续 N 轮解析失败」判 AbortedParse）。
/// 返回 `None` 表示根本没有 VERDICT 行（用于上层统计连续解析失败）。
pub fn parse_verdict(reply: &str) -> Option<Verdict> {
    let mut found = None;
    for line in reply.lines() {
        let t = line.trim();
        // 前缀大小写不敏感（容 agent 偶尔写 `Verdict:` / `verdict:`）。
        const PFX: usize = "VERDICT:".len();
        if t.len() < PFX || !t.is_char_boundary(PFX) || !t[..PFX].eq_ignore_ascii_case("VERDICT:") {
            continue;
        }
        let rest = &t[PFX..];
        match rest.trim().to_ascii_uppercase().as_str() {
            "PASS" => found = Some(Verdict::Pass),
            "NEEDS_WORK" => found = Some(Verdict::NeedsWork),
            _ => {}
        }
    }
    found
}

/// 解析 ASK_USER：取**第一行**以 `ASK_USER:` 开头者，后接一段 JSON。
///
/// JSON 可在同一行，也可换行/美化（pretty-print）跨多行——从标记处取到文末，
/// 先整段试 JSON，再用花括号配平抽出首个完整 JSON 对象试 JSON。
/// 全部失败兜底：把 `ASK_USER:` 之后整段当纯文本 question（options 空）。
/// 无 ASK_USER 行返回 `None`。
pub fn parse_ask_user(reply: &str) -> Option<AskUser> {
    // 定位标记：单行 trim 后以 `ASK_USER:` 开头。记下标记冒号之后到文末的整段，
    // 以便容纳 JSON 跨行的情形。
    let mut tail = None;
    for line in reply.lines() {
        if let Some(rest) = line.trim().strip_prefix("ASK_USER:") {
            // 同一行可能就是完整 JSON；也可能为空（JSON 在后续行）。
            // 用原文中该行之后的剩余文本拼出 tail，避免丢掉换行的 JSON。
            let after_marker = rest.trim_start();
            tail = Some((after_marker.to_string(), line));
            break;
        }
    }
    let (same_line, marker_line) = tail?;

    // 从标记行之后取整段剩余原文（含换行），用于跨行 JSON。
    let rest_from_marker = reply
        .split_once(marker_line)
        .map(|(_, after)| after)
        .unwrap_or("");
    let multiline = format!("{same_line}\n{rest_from_marker}");
    let multiline = multiline.trim();

    // 1) 同行整段直接试。
    if !same_line.is_empty() {
        if let Ok(parsed) = serde_json::from_str::<AskUser>(&same_line) {
            return Some(parsed);
        }
    }
    // 2) 跨行：花括号配平抽出首个完整 JSON 对象再试。
    if let Some(obj) = extract_first_json_object(multiline) {
        if let Ok(parsed) = serde_json::from_str::<AskUser>(obj) {
            return Some(parsed);
        }
    }
    // 3) 兜底：取首个非空文本行当纯文本问题（绝不返回空串）。
    let question = multiline
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    Some(AskUser {
        question,
        options: Vec::new(),
    })
}

/// 从文本里用花括号配平抽出**首个**完整 JSON 对象子串（含起止花括号）。
/// 简单跳过字符串字面量内的花括号与转义，足够应付 agent 输出的拍板 JSON。
fn extract_first_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 解析 Claude 回复里单行 `WORKTREE: <绝对路径>` 标记，取**最后一次**出现
/// （容 Claude 中途多次打印；以末次为准）。前缀大小写不敏感；冒号后整段 trim 为路径，
/// 空则跳过。无标记返回 `None`。
pub fn parse_worktree_path(reply: &str) -> Option<String> {
    let mut found = None;
    for line in reply.lines() {
        let t = line.trim();
        const PFX: usize = "WORKTREE:".len();
        if t.len() < PFX || !t.is_char_boundary(PFX) || !t[..PFX].eq_ignore_ascii_case("WORKTREE:")
        {
            continue;
        }
        let path = t[PFX..].trim();
        if !path.is_empty() {
            found = Some(path.to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_pass() {
        assert_eq!(
            parse_verdict("无问题。\nVERDICT: PASS"),
            Some(Verdict::Pass)
        );
    }

    #[test]
    fn verdict_needs_work_case_insensitive() {
        assert_eq!(
            parse_verdict("有问题\nverdict: needs_work"),
            Some(Verdict::NeedsWork)
        );
    }

    #[test]
    fn verdict_takes_last_occurrence() {
        let reply = "VERDICT: NEEDS_WORK\n继续\nVERDICT: PASS";
        assert_eq!(parse_verdict(reply), Some(Verdict::Pass));
    }

    #[test]
    fn verdict_none_when_absent() {
        assert_eq!(parse_verdict("没有结论行"), None);
    }

    #[test]
    fn ask_user_structured() {
        let reply = r#"ASK_USER: {"question": "用哪种方案？", "options": ["A", "B"]}"#;
        let q = parse_ask_user(reply).unwrap();
        assert_eq!(q.question, "用哪种方案？");
        assert_eq!(q.options, vec!["A", "B"]);
    }

    #[test]
    fn ask_user_no_options() {
        let reply = r#"ASK_USER: {"question": "要不要砍掉模块 X？"}"#;
        let q = parse_ask_user(reply).unwrap();
        assert_eq!(q.question, "要不要砍掉模块 X？");
        assert!(q.options.is_empty());
    }

    #[test]
    fn ask_user_fallback_to_plain_text() {
        let reply = "ASK_USER: 这不是合法 JSON 的问题";
        let q = parse_ask_user(reply).unwrap();
        assert_eq!(q.question, "这不是合法 JSON 的问题");
        assert!(q.options.is_empty());
    }

    #[test]
    fn ask_user_none_when_absent() {
        assert!(parse_ask_user("普通回复\nVERDICT: PASS").is_none());
    }

    #[test]
    fn ask_user_json_on_next_line() {
        // 标记单独成行、JSON 在下一行（旧实现会得到空 question）。
        let reply = "ASK_USER:\n{\"question\": \"用哪种方案？\", \"options\": [\"A\", \"B\"]}";
        let q = parse_ask_user(reply).unwrap();
        assert_eq!(q.question, "用哪种方案？");
        assert_eq!(q.options, vec!["A", "B"]);
    }

    #[test]
    fn ask_user_pretty_printed_multiline_json() {
        let reply = "ASK_USER:\n{\n  \"question\": \"要不要砍掉模块 X？\",\n  \"options\": [\n    \"砍\",\n    \"留\"\n  ]\n}\n";
        let q = parse_ask_user(reply).unwrap();
        assert_eq!(q.question, "要不要砍掉模块 X？");
        assert_eq!(q.options, vec!["砍", "留"]);
    }

    #[test]
    fn ask_user_marker_with_surrounding_reply_and_multiline_json() {
        // 复现 ID21 实际场景：回复里有铺垫文本、VERDICT 行、标记单独成行、
        // 美化后的 JSON 跨多行、选项含中文逗号/引号。旧实现会返回空 question。
        let reply = r#"我已查看现有实现，需要你拍板下一步走向。

VERDICT: NEEDS_WORK

ASK_USER:
{
  "question": "下一步走哪条路？",
  "options": [
    "方案 A：保持现状，仅补测试",
    "方案 B：重写解析层"
  ]
}

继续等待你的答复。"#;
        let q = parse_ask_user(reply).unwrap();
        assert_eq!(q.question, "下一步走哪条路？");
        assert_eq!(
            q.options,
            vec!["方案 A：保持现状，仅补测试", "方案 B：重写解析层"]
        );
    }

    #[test]
    fn ask_user_marker_then_plain_text_lines() {
        // 标记后无合法 JSON，跨行也只有纯文本：取首个非空行，绝不空串。
        let reply = "ASK_USER:\n这是个无 JSON 的问题\n第二行";
        let q = parse_ask_user(reply).unwrap();
        assert_eq!(q.question, "这是个无 JSON 的问题");
        assert!(q.options.is_empty());
    }

    #[test]
    fn worktree_path_basic() {
        let reply = "我已用子 agent 实现完毕。\nWORKTREE: D:/git/repo-wt-feat\n下面是改动概述。";
        assert_eq!(
            parse_worktree_path(reply),
            Some("D:/git/repo-wt-feat".to_string())
        );
    }

    #[test]
    fn worktree_path_takes_last_and_case_insensitive() {
        let reply = "worktree: /tmp/a\n...\nWORKTREE: /tmp/b";
        assert_eq!(parse_worktree_path(reply), Some("/tmp/b".to_string()));
    }

    #[test]
    fn worktree_path_none_when_absent_or_empty() {
        assert!(parse_worktree_path("没有标记").is_none());
        assert!(parse_worktree_path("WORKTREE:   ").is_none());
    }
}
