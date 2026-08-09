//! `/api/web/llm` 路由集成测试：连接配置 upsert/读回 + 提示词覆盖/重置。
//! 不触真实 LLM（不测 ping/summarize），只验证 DB 持久层经 HTTP 的行为。

mod common;
use common::TestServer;

use serde_json::{json, Value};

async fn get_json(client: &reqwest::Client, url: &str) -> Value {
    client.get(url).send().await.unwrap().json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_upsert_and_readback() {
    // 隔离环境变量影响：清掉 LLM_* 让初始来源为 none。
    std::env::remove_var("LLM_BASE_URL");
    std::env::remove_var("LLM_MODEL");
    std::env::remove_var("LLM_API_KEY");

    let s = TestServer::start().await;
    let client = reqwest::Client::new();

    // 初始无 DB 行、无 env → source=none。
    let v = get_json(&client, &s.url("/api/web/llm/config")).await;
    assert_eq!(v["source"], "none");
    assert_eq!(v["db_configured"], false);

    // PUT 写配置（带 key），末尾斜杠应被规整。
    let resp = client
        .put(s.url("/api/web/llm/config"))
        .json(&json!({ "base_url": "http://gb10:8000/v1/", "model": "qwen", "api_key": "sk-x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let v = get_json(&client, &s.url("/api/web/llm/config")).await;
    assert_eq!(v["source"], "db");
    assert_eq!(v["base_url"], "http://gb10:8000/v1");
    assert_eq!(v["model"], "qwen");
    assert_eq!(v["has_api_key"], true);

    // 不带 api_key 的 PUT 应保留原 key；改了 model。
    client
        .put(s.url("/api/web/llm/config"))
        .json(&json!({ "base_url": "http://gb10:8000/v1", "model": "qwen2" }))
        .send()
        .await
        .unwrap();
    let v = get_json(&client, &s.url("/api/web/llm/config")).await;
    assert_eq!(v["model"], "qwen2");
    assert_eq!(v["has_api_key"], true, "省略 api_key 应保留原值");

    // 空串 api_key → 清空。
    client
        .put(s.url("/api/web/llm/config"))
        .json(&json!({ "base_url": "http://gb10:8000/v1", "model": "qwen2", "api_key": "" }))
        .send()
        .await
        .unwrap();
    let v = get_json(&client, &s.url("/api/web/llm/config")).await;
    assert_eq!(v["has_api_key"], false);

    // base_url 空 → 400。
    let resp = client
        .put(s.url("/api/web/llm/config"))
        .json(&json!({ "base_url": "", "model": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_override_and_reset() {
    let s = TestServer::start().await;
    let client = reqwest::Client::new();

    // 列表含内置 douyin_refine / chat_summary，初始 source=builtin。
    let v = get_json(&client, &s.url("/api/web/llm/prompts")).await;
    let prompts = v["prompts"].as_array().unwrap();
    let refine = prompts
        .iter()
        .find(|p| p["name"] == "douyin_refine")
        .expect("内置目录应含 douyin_refine");
    assert_eq!(refine["source"], "builtin");
    assert_eq!(refine["modified"], false);
    assert!(prompts.iter().any(|p| p["name"] == "chat_summary"));

    // 覆盖 douyin_refine。
    let resp = client
        .put(s.url("/api/web/llm/prompts/douyin_refine"))
        .json(&json!({ "text": "自定义整理提示词 {TRANSCRIPT}" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let v = get_json(&client, &s.url("/api/web/llm/prompts/douyin_refine")).await;
    assert_eq!(v["source"], "db");
    assert_eq!(v["modified"], true);
    assert_eq!(v["text"], "自定义整理提示词 {TRANSCRIPT}");
    assert!(v["builtin_text"].as_str().unwrap().contains("{TRANSCRIPT}"));

    // 重置（DELETE）→ 回到内置。
    let resp = client
        .delete(s.url("/api/web/llm/prompts/douyin_refine"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v = get_json(&client, &s.url("/api/web/llm/prompts/douyin_refine")).await;
    assert_eq!(v["source"], "builtin");
    assert_eq!(v["modified"], false);

    // 未知提示词 → 404。
    let resp = client
        .get(s.url("/api/web/llm/prompts/does_not_exist"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
