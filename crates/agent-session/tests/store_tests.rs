//! Plan 1 单测：用 `tests/fixtures/` 下自造的小样本 JSONL 验证 store 解析，
//! 不读真实用户 `~/.codex` / `~/.claude`。

use agent_session::store::Store;
use agent_session::{Provider, SessionStatus};
use std::path::PathBuf;

fn fixtures_home() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn store() -> Store {
    Store::with_home(fixtures_home())
}

const CODEX_DONE: &str = "11111111-aaaa-bbbb-cccc-000000000001";
const CODEX_RUNNING: &str = "22222222-aaaa-bbbb-cccc-000000000002";
/// fixture：在 session_index.jsonl 中保留，但 rollout 已搬到 archived_sessions/。
/// 应被 list() 过滤掉，但仍可被 locate() 找到。
const CODEX_ARCHIVED: &str = "44444444-aaaa-bbbb-cccc-000000000004";
// 新源 fixture：含 response_item（developer/environment_context/多轮 user+assistant/
// reasoning/function_call/output）+ event_msg 双写。不在 session_index.jsonl（不进 list）。
const CODEX_RESUME: &str = "33333333-aaaa-bbbb-cccc-000000000003";
const CLAUDE_DONE: &str = "aaaabbbb-cccc-dddd-eeee-000000000009";
const CLAUDE_PROCESSING: &str = "bbbbcccc-dddd-eeee-ffff-000000000010";

#[test]
fn list_returns_both_providers_sorted() {
    let rows = store().list(20).unwrap();
    // 两个 codex + 两个 claude
    let codex: Vec<_> = rows
        .iter()
        .filter(|r| r.provider == Provider::Codex)
        .collect();
    let claude: Vec<_> = rows
        .iter()
        .filter(|r| r.provider == Provider::Claude)
        .collect();
    assert_eq!(codex.len(), 2, "应列出两个 codex 会话");
    assert_eq!(claude.len(), 2, "应列出两个 claude 会话");

    // codex 按 updated_at 倒序：done(10:30) 在 running(09:00) 前
    assert_eq!(codex[0].id, CODEX_DONE);
    assert_eq!(codex[0].title, "复核设计文档 codeloop");
    assert_eq!(codex[0].status, SessionStatus::Idle);
    assert_eq!(codex[1].id, CODEX_RUNNING);
    assert_eq!(codex[1].status, SessionStatus::Generating);

    // claude 标题来自 ai-title
    let done = claude.iter().find(|r| r.id == CLAUDE_DONE).unwrap();
    assert_eq!(done.title, "Codeloop 双栏视图实现");
    assert_eq!(done.status, SessionStatus::Idle);
}

#[test]
fn list_filters_out_codex_archived_sessions() {
    // session_index 里有三条 codex 行，其中 ARCHIVED 的 rollout 在 archived_sessions/。
    // list() 应只返回两条 active 的，不让归档会话出现在配对下拉里。
    let rows = store().list(20).unwrap();
    let codex_ids: Vec<&str> = rows
        .iter()
        .filter(|r| r.provider == Provider::Codex)
        .map(|r| r.id.as_str())
        .collect();
    assert!(codex_ids.contains(&CODEX_DONE));
    assert!(codex_ids.contains(&CODEX_RUNNING));
    assert!(
        !codex_ids.contains(&CODEX_ARCHIVED),
        "已归档 session 不应出现在 list 输出里：实际 ids={codex_ids:?}"
    );
}

#[test]
fn locate_still_finds_archived_session() {
    // locate() 仍跨 active + archived 两个根查找——归档会话「按 id 定位」仍可用，
    // 仅在「列表」环节被过滤。保持现状以兼容 snapshot/messages 等其它入口。
    let path = store()
        .locate(Provider::Codex, CODEX_ARCHIVED)
        .unwrap()
        .expect("归档 session 仍能被 locate 定位到");
    assert!(path.to_string_lossy().contains("archived_sessions"));
}

#[test]
fn locate_finds_files_by_id() {
    let s = store();
    assert!(s.locate(Provider::Codex, CODEX_DONE).unwrap().is_some());
    assert!(s.locate(Provider::Claude, CLAUDE_DONE).unwrap().is_some());
    assert!(s.locate(Provider::Codex, "no-such-id").unwrap().is_none());
    assert!(s.locate(Provider::Claude, "no-such-id").unwrap().is_none());
}

#[test]
fn codex_snapshot_parses_status_reply_and_cwd() {
    let snap = store().snapshot(Provider::Codex, CODEX_DONE).unwrap();
    assert_eq!(snap.status, SessionStatus::Idle);
    // 最近回复取末个 task_complete.last_agent_message
    assert!(snap.latest_reply.contains("VERDICT: PASS"));
    // cwd 来自 session_meta.cwd
    assert_eq!(
        snap.cwd,
        PathBuf::from("D:/git/github-commit-info-codeloop/docs")
    );
}

#[test]
fn codex_cwd_falls_back_to_turn_context() {
    // session 2 只有 turn_context.cwd，无 session_meta
    let snap = store().snapshot(Provider::Codex, CODEX_RUNNING).unwrap();
    assert_eq!(snap.status, SessionStatus::Generating);
    assert_eq!(
        snap.cwd,
        PathBuf::from("D:/git/github-commit-info-codeloop/crates/agent-session")
    );
}

#[test]
fn claude_snapshot_parses_status_reply_and_cwd() {
    let snap = store().snapshot(Provider::Claude, CLAUDE_DONE).unwrap();
    assert_eq!(snap.status, SessionStatus::Idle);
    assert_eq!(
        snap.latest_reply,
        "已完成 codeloop.html 的实现，新增了左右双栏布局。"
    );
    assert_eq!(
        snap.cwd,
        PathBuf::from("D:/git/github-commit-info-codeloop")
    );
}

#[test]
fn claude_processing_status_when_last_event_is_user() {
    let snap = store()
        .snapshot(Provider::Claude, CLAUDE_PROCESSING)
        .unwrap();
    assert_eq!(snap.status, SessionStatus::Processing);
    assert_eq!(
        snap.cwd,
        PathBuf::from("D:/git/github-commit-info-codeloop/crates/toolkit-server")
    );
}

#[test]
fn codex_messages_render_user_and_assistant() {
    let page = store().messages(Provider::Codex, CODEX_DONE, 0).unwrap();
    let roles: Vec<_> = page.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
    assert!(page.messages[0].text.contains("请复核"));
    assert!(page.messages.last().unwrap().text.contains("VERDICT: PASS"));
    // 游标 = 总行数（含坏行），下次增量空
    assert!(page.cursor > 0);
    let next = store()
        .messages(Provider::Codex, CODEX_DONE, page.cursor)
        .unwrap();
    assert!(next.messages.is_empty());
    assert_eq!(next.cursor, page.cursor);
}

#[test]
fn claude_messages_render_blocks() {
    let page = store().messages(Provider::Claude, CLAUDE_DONE, 0).unwrap();
    // user / assistant(tool_use 行) / user(tool_result，无文本被跳过) / assistant(end_turn)
    let texts: Vec<_> = page.messages.iter().map(|m| m.text.clone()).collect();
    // 第一条 user
    assert_eq!(page.messages[0].role, "user");
    assert!(texts[0].contains("双栏视图"));
    // 含 thinking 折叠标记与 tool_use 标记
    assert!(texts.iter().any(|t| t.contains("[thinking]")));
    assert!(texts.iter().any(|t| t.contains("[tool_use: Read]")));
    // 末条 assistant 文本
    assert!(texts.last().unwrap().contains("已完成 codeloop.html"));
}

#[test]
fn messages_increment_by_cursor() {
    let s = store();
    let first = s.messages(Provider::Codex, CODEX_DONE, 0).unwrap();
    // 从第 5 行起（跳过 session_meta + 第一轮），仅拿后半段
    let partial = s.messages(Provider::Codex, CODEX_DONE, 5).unwrap();
    assert!(partial.messages.len() < first.messages.len());
    assert_eq!(partial.cursor, first.cursor);
}

#[test]
fn codex_response_item_source_shows_resume_user_rounds() {
    let page = store().messages(Provider::Codex, CODEX_RESUME, 0).unwrap();
    let texts: Vec<_> = page.messages.iter().map(|m| m.text.clone()).collect();

    // G1：resume 轮用户 prompt 不再丢失（旧逻辑只认 event_msg 会丢这条）。
    assert!(
        texts.iter().any(|t| t.contains("已按意见修订")),
        "resume 轮用户 prompt 应可见"
    );
    assert!(
        texts.iter().any(|t| t.contains("请以严格审阅者身份复核")),
        "首轮用户 prompt 应可见"
    );

    // developer 与每轮注入的 environment_context 块不展示。
    assert!(
        !texts.iter().any(|t| t.contains("environment_context")),
        "环境上下文块应被过滤"
    );
    assert!(
        !texts.iter().any(|t| t.contains("approval_policy")),
        "developer 说明应被过滤"
    );

    // 双写去重：assistant 回复来自 response_item，event_msg 不再重复计入。
    assert_eq!(
        texts.iter().filter(|t| t.contains("NEEDS_WORK")).count(),
        1,
        "首轮 assistant 回复不应因双写而重复"
    );
    assert_eq!(
        texts.iter().filter(|t| t.contains("VERDICT: PASS")).count(),
        1
    );

    // 恰好两条真实用户消息（developer / environment_context 不计）。
    let users = page.messages.iter().filter(|m| m.role == "user").count();
    assert_eq!(users, 2, "应有且仅有两轮真实用户 prompt");

    // G2：思考 / 工具调用渲染为标记，真实内容入 detail 可展开。
    let thinking = page
        .messages
        .iter()
        .find(|m| m.text == "[thinking]")
        .expect("应渲染 reasoning 为 [thinking]");
    assert!(thinking.detail.as_deref().unwrap_or("").contains("先通读"));
    let tool = page
        .messages
        .iter()
        .find(|m| m.text.contains("[tool_use: shell]"))
        .expect("应渲染 function_call 为 [tool_use]");
    assert!(tool.detail.as_deref().unwrap_or("").contains("cat"));
    let tool_out = page
        .messages
        .iter()
        .find(|m| m.text == "[tool_result]")
        .expect("应渲染 function_call_output 为 [tool_result]");
    assert!(tool_out.detail.as_deref().unwrap_or("").contains("plan.md"));

    // 游标连续翻页不重出。
    let next = store()
        .messages(Provider::Codex, CODEX_RESUME, page.cursor)
        .unwrap();
    assert!(next.messages.is_empty());
    assert_eq!(next.cursor, page.cursor);
}

#[test]
fn codex_old_event_msg_source_not_regressed() {
    // 纯 event_msg rollout（无 response_item）仍走旧源，逐轮 user/assistant 不变。
    let page = store().messages(Provider::Codex, CODEX_DONE, 0).unwrap();
    let roles: Vec<_> = page.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
}

#[test]
fn claude_image_block_rendered_not_dropped() {
    // G3：纯 image 用户消息渲染为 [image]，不再因正文为空被丢弃。
    let page = store().messages(Provider::Claude, CLAUDE_DONE, 0).unwrap();
    assert!(
        page.messages.iter().any(|m| m.text == "[image]"),
        "纯 image 消息应渲染为 [image]"
    );
}

#[test]
fn provider_parse_roundtrip() {
    assert_eq!(Provider::parse("codex"), Some(Provider::Codex));
    assert_eq!(Provider::parse("CLAUDE"), Some(Provider::Claude));
    assert_eq!(Provider::parse("nope"), None);
    assert_eq!(Provider::Codex.as_str(), "codex");
}
