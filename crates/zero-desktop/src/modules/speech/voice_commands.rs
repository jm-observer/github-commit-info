//! 本地轻量语音命令：固定短语表 → 本机动作（见 docs/llm-and-voice-enhancement-plan.md §节 C）。
//!
//! 不接 LLM：短而无歧义的命令走字符串精确匹配，零延迟。复杂自然语言意图请走远程通道
//! （见 voice-command-agent-design.md）。

use tracing::info;

use crate::modules::speech::paste_watch;

/// 命令上限长度（字符数）。超过即视为正常听写，跳过命令匹配，避免长句误触发。
const MAX_COMMAND_CHARS: usize = 8;

/// 已支持的命令。新增命令只要：① 加一个枚举分支；② 在 [`COMMAND_PHRASES`] 加触发短语；
/// ③ 在 [`dispatch`] 加执行分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    /// 向焦点窗口发一次回车（典型场景：聊天框 / 表单提交）。
    SendEnter,
}

/// 命令匹配结果。优化稿里命令出现的位置不同处理方式不同：
///
/// - [`CommandMatch::Whole`]：整段就是命令（用户单独说"发送"）。执行命令，**不写剪贴板/不粘贴**。
/// - [`CommandMatch::Tail`]：命令挂在正文末尾（用户说"你好，发送"），分隔符前是正文。正文走
///   正常剪贴板/粘贴链路；命令仅在 `auto_paste` 开启时才会被派发——剪贴板模式下用户还要
///   手动 Ctrl+V，提前敲回车会误提交空内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandMatch {
    Whole(VoiceCommand),
    Tail {
        prefix: String,
        command: VoiceCommand,
    },
}

/// 视作"命令前分隔符"的字符集（中英文标点 + 空格 + 全角空格）。ASR 优化稿在用户明显停顿处
/// 通常会插入这些标点之一，故"前有分隔符的尾部命令"对应"用户说命令前停顿了一下"。
const SEPARATORS: &[char] = &[
    '，', '。', '、', '；', '：', '！', '？', ',', '.', ';', ':', '!', '?', ' ', '\u{3000}',
];

/// 触发短语表。匹配前已对优化稿做归一化（去标点 / 全角转半角 / trim / 小写化），
/// 故此处条目也用归一化形式书写。
///
/// 多对一映射：同一个 [`VoiceCommand`] 可挂多条短语。
const COMMAND_PHRASES: &[(&str, VoiceCommand)] = &[
    ("发送", VoiceCommand::SendEnter),
    ("发送一下", VoiceCommand::SendEnter),
    ("回车", VoiceCommand::SendEnter),
    ("确认发送", VoiceCommand::SendEnter),
    ("send", VoiceCommand::SendEnter),
    ("enter", VoiceCommand::SendEnter),
];

/// 把优化稿归一化：剥离中英文常见标点、全角空格 → 半角、英文转小写、trim。
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        // 中文常见标点 + ASCII 标点：剥离。
        if matches!(
            ch,
            '。' | '，'
                | '！'
                | '？'
                | '、'
                | '：'
                | '；'
                | '．'
                | '.'
                | ','
                | '!'
                | '?'
                | ':'
                | ';'
                | '\''
                | '"'
        ) {
            continue;
        }
        // 全角空格 → 半角空格。
        let mapped = if ch == '\u{3000}' { ' ' } else { ch };
        for low in mapped.to_lowercase() {
            out.push(low);
        }
    }
    out.trim().to_string()
}

/// 把候选子串（已 trim）尝试解析为命令短语。短语长度上限内、归一化非空、命中短语表。
fn match_phrase(candidate: &str) -> Option<VoiceCommand> {
    if candidate.chars().count() > MAX_COMMAND_CHARS {
        return None;
    }
    let norm = normalize(candidate);
    if norm.is_empty() {
        return None;
    }
    COMMAND_PHRASES
        .iter()
        .find(|(phrase, _)| *phrase == norm)
        .map(|(_, cmd)| *cmd)
}

/// 尝试把一段优化稿解析为命令。
///
/// 算法：
/// 1. 先尝试整段是命令（用户单独说"发送"，可能带句号）→ [`CommandMatch::Whole`]
/// 2. 否则从右往左找分隔符，若分隔符后的子串是命令短语 → [`CommandMatch::Tail`]
///    （前置正文 = 分隔符前的非空内容，已 trim 掉末尾连续分隔符）
/// 3. 都不命中 → `None`（走正常听写流程）
///
/// 仅"前有分隔符"才算尾部命令——避免 `"我要发送邮件"` 这类连写误触发。
pub fn match_command(optimized_text: &str) -> Option<CommandMatch> {
    let trimmed = optimized_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(cmd) = match_phrase(trimmed) {
        return Some(CommandMatch::Whole(cmd));
    }
    // 从右往左扫所有分隔符位置：第一个让"分隔符后的尾巴"匹配上短语的就是答案。
    // 顺序从右往左是为了优先取最贴近末尾的命令（也能正确处理 `"a，发送。"` 这种末尾还有句号
    // 的情形——找到 `，` 时尾巴 `发送。` 归一化后即 `发送`）。
    for (byte_idx, ch) in trimmed.char_indices().rev() {
        if !SEPARATORS.contains(&ch) {
            continue;
        }
        let tail_start = byte_idx + ch.len_utf8();
        let tail = trimmed[tail_start..].trim();
        let Some(cmd) = match_phrase(tail) else {
            continue;
        };
        let prefix = trimmed[..byte_idx]
            .trim_end_matches(|c: char| SEPARATORS.contains(&c))
            .trim()
            .to_string();
        if prefix.is_empty() {
            // `"，发送"` / `" 发送"` 这类前缀其实为空 → 当 Whole 处理。
            return Some(CommandMatch::Whole(cmd));
        }
        return Some(CommandMatch::Tail {
            prefix,
            command: cmd,
        });
    }
    None
}

/// 执行命令。返回是否真的派发了一次按键（前台是本进程 / 平台不支持 → false，
/// 此时调用方可决定回落到默认链路）。
pub fn dispatch(cmd: VoiceCommand, raw_text: &str) -> bool {
    let acted = match cmd {
        VoiceCommand::SendEnter => paste_watch::press_enter_to_foreground(),
    };
    info!(
        target: "speech",
        "[voice_cmd] cmd={cmd:?} acted={acted} raw={raw_text:?}"
    );
    acted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(cmd: VoiceCommand) -> CommandMatch {
        CommandMatch::Whole(cmd)
    }
    fn tail(prefix: &str, cmd: VoiceCommand) -> CommandMatch {
        CommandMatch::Tail {
            prefix: prefix.to_string(),
            command: cmd,
        }
    }

    #[test]
    fn matches_send_synonyms_as_whole() {
        for s in ["发送", "发送。", "发送！", "  发送  ", "回车", "确认发送"] {
            assert_eq!(
                match_command(s),
                Some(whole(VoiceCommand::SendEnter)),
                "{s}"
            );
        }
    }

    #[test]
    fn matches_english_case_insensitive() {
        assert_eq!(match_command("Send"), Some(whole(VoiceCommand::SendEnter)));
        assert_eq!(
            match_command("ENTER."),
            Some(whole(VoiceCommand::SendEnter))
        );
    }

    #[test]
    fn matches_tail_with_separator() {
        // 中文逗号 / 句号
        assert_eq!(
            match_command("你好，发送"),
            Some(tail("你好", VoiceCommand::SendEnter))
        );
        assert_eq!(
            match_command("你好。发送。"),
            Some(tail("你好", VoiceCommand::SendEnter))
        );
        // 末尾还有标点
        assert_eq!(
            match_command("今天就这样吧，发送！"),
            Some(tail("今天就这样吧", VoiceCommand::SendEnter))
        );
        // 空格也算分隔符
        assert_eq!(
            match_command("hello send"),
            Some(tail("hello", VoiceCommand::SendEnter))
        );
        // 多个分隔符堆叠 → 取最近能匹配的
        assert_eq!(
            match_command("你好。，发送"),
            Some(tail("你好", VoiceCommand::SendEnter))
        );
    }

    #[test]
    fn no_separator_no_tail_match() {
        // 连写，前面无标点 → 不算尾部命令（避免"我要发送邮件"那种误触发）。
        assert_eq!(match_command("你好发送"), None);
        assert_eq!(match_command("我要发送"), None);
        // "发送邮件" 命令短语不在末尾 → 完全不匹配。
        assert_eq!(match_command("发送邮件"), None);
    }

    #[test]
    fn skips_long_phrase_candidates_only() {
        // "我要发送邮件了" 7 字符，整段不在短语表 + 末尾无分隔符 → None。
        assert_eq!(match_command("我要发送邮件了"), None);
        // 长正文 + 末尾尾部命令 → 仍能匹配（短语长度限制仅作用于候选短语本身）。
        assert_eq!(
            match_command("这是一段很长的正文内容，发送"),
            Some(tail("这是一段很长的正文内容", VoiceCommand::SendEnter))
        );
    }

    #[test]
    fn skips_empty_and_non_command() {
        assert_eq!(match_command(""), None);
        assert_eq!(match_command("你好"), None);
    }

    #[test]
    fn empty_prefix_becomes_whole() {
        // "，发送" 前缀空 → 当 Whole 处理（不该写空字符串进剪贴板）。
        assert_eq!(
            match_command("，发送"),
            Some(whole(VoiceCommand::SendEnter))
        );
        assert_eq!(match_command(" 发送"), Some(whole(VoiceCommand::SendEnter)));
    }
}
