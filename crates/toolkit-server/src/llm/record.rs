//! 大模型会话记录助手：把一次「user 输入 → assistant 输出」的业务调用落成一个 session。
//!
//! 设计取舍：本助手接收**已经拿到的 reply**（LLM 调用与错误处理留在各调用点），只负责把
//! 这次交换写进 `llm_sessions` / `llm_messages`。记录失败只 `warn!`，**绝不影响**业务结果——
//! 落记录是旁路审计，不该拖垮整理/总结/对话本身。

use serde_json::Value;
use toolkit_core::llm_sessions::{self, NewSession};
use toolkit_core::SqlitePool;

/// 记录一次单轮交换（user + assistant 各一条消息）。best-effort。
#[allow(clippy::too_many_arguments)]
pub fn record_exchange(
    pool: &SqlitePool,
    kind: &str,
    title: &str,
    model: &str,
    prompt_name: &str,
    user_content: &str,
    assistant_content: &str,
    session_meta: Value,
) {
    let res = (|| -> anyhow::Result<()> {
        let id = llm_sessions::create_session(
            pool,
            NewSession {
                kind,
                title,
                model: Some(model),
                prompt_name: Some(prompt_name),
                metadata: Some(&session_meta.to_string()),
            },
        )?;
        llm_sessions::append_message(pool, &id, "user", user_content, None)?;
        llm_sessions::append_message(pool, &id, "assistant", assistant_content, None)?;
        Ok(())
    })();
    if let Err(e) = res {
        log::warn!("记录 LLM 会话失败（{kind}）: {e:#}");
    }
}
