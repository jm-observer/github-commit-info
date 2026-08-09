//! 公共大模型层：连接配置解析 + 可配提示词的「内置默认 + DB 覆盖」目录 + HTTP 路由。
//!
//! - **连接配置解析**（[`resolve_config`]）：DB（`llm_config` 表）优先，缺失回退环境变量
//!   （`LLM_BASE_URL` / `LLM_MODEL` / `LLM_API_KEY`）。两者都没有 → 明确报错。
//! - **提示词目录**：各功能在 [`builtins`] 注册内置默认（name + 语义版本 + 默认文本）。运行时
//!   解析（[`resolve_prompt`]）DB 行优先，缺失用内置默认；控制台改了就写 DB 覆盖、删 DB 行即
//!   「恢复内置默认」。
//!
//! 各功能（douyin 整理、对话总结、ASR 润色…）都经此层取配置/提示词，不再各自读 env /
//! `include_str!`。

pub mod record;
pub mod routes;

use anyhow::{anyhow, Context, Result};
use toolkit_core::llm_store;
use toolkit_core::SqlitePool;
use toolkit_llm::{prompt_hash, LlmClient, LlmConfig};

// ---- 内置提示词名字（其他模块引用，避免裸字符串散落）----
pub const NAME_DOUYIN_REFINE: &str = "douyin_refine";
pub const NAME_CHAT_SUMMARY: &str = "chat_summary";
/// ASR 中文优化系统提示词（orchestrator 的 vLLM 润色调用）。orchestrator 经裸字符串
/// `"asr_optimize_zh"` 读取（不依赖 toolkit-server crate）；两边名称必须一致。
pub const NAME_ASR_OPTIMIZE_ZH: &str = "asr_optimize_zh";
/// ASR 翻译系统提示词（orchestrator 的英文翻译调用）。
pub const NAME_ASR_TRANSLATE: &str = "asr_translate";

/// ASR 中文优化提示词内置默认（节 B 加固后）。orchestrator 的 `DEFAULT_OPTIMIZE_PROMPT`
/// 是同份文本的运行时回退；改这里时同步改那边。`PROMPT_VERSION` 用 `v2` 反映节 B 改动。
pub const ASR_OPTIMIZE_ZH_PROMPT: &str = "你是中文口语转写规整器。任务:仅修正口语病(去除\"那/就是/啊/什么的\"等口头语、合并自我重复如\"最左侧是最左侧是\"、补齐缺失标点、改正同音错字),输出通顺的书面中文。严格保留原句所有信息点和原有顺序;禁止归纳、概括、合并要点、改写为列表或重排语序;长句保持长句,不要为了简洁而压缩。\
\n规则:\
\n- 英文单词、代码标识符(驼峰/蛇形/含数字)保持原样,不要意译或音译,例如 Tauri 不要写成\"塔里\"。\
\n- 数字、日期、金额、版本号统一阿拉伯数字与标准写法,例如\"二零二六年六月\"→\"2026 年 6 月\"、\"v 一点零\"→\"v1.0\"。\
\n- 逐句对齐原文,不要合并/压缩/总结,不要删减信息。\
\n严格要求:只输出整理后的文本本身;不要解释、不要选项、不要markdown、不要追问、不要任何前后缀;若已通顺则原样返回。";

/// ASR 翻译提示词内置默认。
pub const ASR_TRANSLATE_PROMPT: &str =
    "Translate the user's sentence into natural English. Output ONLY the translation itself — no explanations, no options, no quotes, no markdown.";

/// 对话总结内置 prompt。`{CONVERSATION}` 占位符在调用时替换为粘贴的会话文本。
pub const CHAT_SUMMARY_PROMPT: &str =
    "你是会话总结助手。请阅读下面的对话内容，输出简洁的中文总结：\
先用一句话概述主题，再用要点列出关键结论 / 决定 / 待办（无则省略对应小节）。\
只输出总结本身，不要复述原文，不要编造对话中没有的信息。\n\n对话内容：\n{CONVERSATION}";

/// 一条内置提示词定义（编译期默认）。
pub struct BuiltinPrompt {
    pub name: &'static str,
    /// 人类可读说明（控制台展示）。
    pub description: &'static str,
    /// 语义版本（与功能自身的 PROMPT_VERSION 对齐）。
    pub version: &'static str,
    /// 占位符列表（仅用于控制台提示，例如 `{TRANSCRIPT}` / `{CONVERSATION}`）。
    pub placeholders: &'static [&'static str],
    /// 编译期默认文本。
    pub default_text: &'static str,
}

/// 全部内置提示词目录。新增可配提示词在此登记一行即可被控制台列出 / 编辑 / 重置。
pub fn builtins() -> Vec<BuiltinPrompt> {
    vec![
        BuiltinPrompt {
            name: NAME_DOUYIN_REFINE,
            description: "抖音 ASR 原文整理（纠错/去口语水词/分段/小结）",
            version: douyin::refine::PROMPT_VERSION,
            placeholders: &["{TRANSCRIPT}"],
            default_text: douyin::refine::REFINE_PROMPT,
        },
        BuiltinPrompt {
            name: NAME_CHAT_SUMMARY,
            description: "对话总结：粘贴会话文本 → 输出要点总结",
            version: "v1",
            placeholders: &["{CONVERSATION}"],
            default_text: CHAT_SUMMARY_PROMPT,
        },
        BuiltinPrompt {
            name: NAME_ASR_OPTIMIZE_ZH,
            description: "ASR 中文优化（口语病修正 + 英文/代码保留 + 数字规范化）",
            // v2 = 节 B 在 v1 基础上增补英文保留 / 数字规范化 / 逐句对齐三条规则。
            version: "v2",
            placeholders: &[],
            default_text: ASR_OPTIMIZE_ZH_PROMPT,
        },
        BuiltinPrompt {
            name: NAME_ASR_TRANSLATE,
            description: "ASR 中文→英文翻译",
            version: "v1",
            placeholders: &[],
            default_text: ASR_TRANSLATE_PROMPT,
        },
    ]
}

/// 查内置默认（按名字）。
pub fn builtin(name: &str) -> Option<BuiltinPrompt> {
    builtins().into_iter().find(|b| b.name == name)
}

/// 配置来源标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    Db,
    Env,
    None,
}

impl ConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigSource::Db => "db",
            ConfigSource::Env => "env",
            ConfigSource::None => "none",
        }
    }
}

/// 解析有效连接配置：DB 优先（base_url + model 非空才算有效），否则环境变量。
pub fn resolve_config(pool: &SqlitePool) -> Result<LlmConfig> {
    if let Some(c) = llm_store::get_config(pool)? {
        if !c.base_url.trim().is_empty() && !c.model.trim().is_empty() {
            return Ok(LlmConfig::new(c.base_url, c.model, c.api_key));
        }
    }
    LlmConfig::from_env()
        .context("未配置大模型：请在控制台填写地址/模型，或设置 LLM_BASE_URL/LLM_MODEL 环境变量")
}

/// 仅判断来源（不报错）。
pub fn config_source(pool: &SqlitePool) -> Result<ConfigSource> {
    if let Some(c) = llm_store::get_config(pool)? {
        if !c.base_url.trim().is_empty() && !c.model.trim().is_empty() {
            return Ok(ConfigSource::Db);
        }
    }
    Ok(match LlmConfig::from_env() {
        Ok(_) => ConfigSource::Env,
        Err(_) => ConfigSource::None,
    })
}

/// 装配可用的 LLM 客户端（解析配置 → LlmClient）。
pub fn resolve_client(pool: &SqlitePool) -> Result<LlmClient> {
    LlmClient::new(resolve_config(pool)?)
}

/// 解析有效提示词文本：DB 覆盖优先，否则内置默认；都没有则报错（未知 name）。
pub fn resolve_prompt(pool: &SqlitePool, name: &str) -> Result<String> {
    if let Some(p) = llm_store::get_prompt(pool, name)? {
        return Ok(p.text);
    }
    builtin(name)
        .map(|b| b.default_text.to_string())
        .ok_or_else(|| anyhow!("未知提示词 {name}（既无 DB 覆盖也无内置默认）"))
}

/// 解析提示词的版本号（DB 覆盖优先，否则内置版本）。落产物元信息用。
pub fn resolve_prompt_version(pool: &SqlitePool, name: &str) -> Result<String> {
    if let Some(p) = llm_store::get_prompt(pool, name)? {
        return Ok(p.version);
    }
    builtin(name)
        .map(|b| b.version.to_string())
        .ok_or_else(|| anyhow!("未知提示词 {name}"))
}

/// 当前生效提示词的短哈希（落产物元信息用，配合 version 溯源）。
pub fn resolve_prompt_hash(pool: &SqlitePool, name: &str) -> Result<String> {
    Ok(prompt_hash(&resolve_prompt(pool, name)?))
}
