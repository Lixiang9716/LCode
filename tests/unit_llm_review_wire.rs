//! Regression tests for the review-batch wire/parse/config fixes:
//! endpoint host matching, JSON-lock success path, search-result caps,
//! orphan id uniqueness, and config merge polarity.

use lcode::config::{Config, MemoryConfig, ReasoningEffort};
use lcode::llm::{ChatMessage, FinishReason, LlmResponse, Usage};

// --- host matching (review m7/m6) ---

#[test]
fn deepseek_host_matching_is_exact() {
    assert!(lcode::llm::is_deepseek_endpoint("https://api.deepseek.com"));
    assert!(lcode::llm::is_deepseek_endpoint("https://api.deepseek.com/anthropic"));
    assert!(lcode::llm::is_deepseek_endpoint("api.deepseek.com"));
    assert!(!lcode::llm::is_deepseek_endpoint("https://api.deepseek.com.evil.io"));
    assert!(!lcode::llm::is_deepseek_endpoint("https://llm-gateway.internal"));
    assert!(!lcode::llm::is_deepseek_endpoint(""));
}

// --- json_lock success path (review M2) ---

#[tokio::test]
async fn json_lock_sends_prefix_and_consumes_json() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|messages: &[ChatMessage], _tools| {
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].prefix, Some(true));
        assert_eq!(messages[1].content, "[");
        Ok(LlmResponse {
            content:
                r#"[{"name": "prefers-tabs", "description": "tabs", "tags": [], "body": "tabs"}]"#
                    .to_string(),
            tool_calls: None,
            server_results: Vec::new(),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        })
    });

    let tmp = tempfile::TempDir::new().unwrap();
    let store = lcode::agent::MemoryStore::with_config(
        tmp.path(),
        &MemoryConfig { json_lock: true, ..MemoryConfig::default() },
    )
    .unwrap();
    let written = store.extract("user: use tabs please", &mock).await.expect("extract succeeds");
    assert_eq!(written, 1, "prefix-locked JSON is parsed and written");
}

// --- search result length caps (review M2 security) ---

#[test]
fn search_results_are_length_capped() {
    use lcode::llm::anthropic::parse_anthropic_response;
    let big = "x".repeat(30_000);
    let data = serde_json::json!({
        "content": [
            { "type": "web_search_tool_result", "tool_use_id": "sr-1",
              "content": [ { "type": "text", "text": big } ] }
        ],
        "stop_reason": "tool_use"
    });
    let response = parse_anthropic_response(&data).expect("parses");
    assert!(
        response.server_results[0].content.len() <= 20_000,
        "capped at 20k chars, got {}",
        response.server_results[0].content.len()
    );
}

#[test]
fn orphan_results_get_unique_fallback_ids() {
    use lcode::llm::anthropic::parse_anthropic_response;
    let data = serde_json::json!({
        "content": [
            { "type": "web_search_tool_result", "content": [ { "type": "text", "text": "a" } ] },
            { "type": "web_search_tool_result", "content": [ { "type": "text", "text": "b" } ] }
        ],
        "stop_reason": "tool_use"
    });
    let response = parse_anthropic_response(&data).expect("parses");
    let calls = response.tool_calls.expect("synthesized calls");
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].id, calls[1].id, "fallback ids must be unique");
}

// --- config merge semantics (review m1) ---

#[test]
fn merge_new_llm_fields_follow_documented_polarity() {
    let mut base = Config::default();
    base.llm.internal_thinking_disabled = false;
    base.llm.reasoning_effort = Some(ReasoningEffort::Low);

    let mut other = Config::default();
    other.llm.internal_thinking_disabled = true;
    other.llm.reasoning_effort = Some(ReasoningEffort::Max);
    lcode::config::merge_config(&mut base, other);

    // false-wins (documented): a project layer can only relax internal
    // no-thinking, never tighten it — even an explicit `true` cannot be
    // told apart from the default.
    assert!(!base.llm.internal_thinking_disabled);
    // Some-wins for reasoning_effort.
    assert_eq!(base.llm.reasoning_effort, Some(ReasoningEffort::Max));
}
