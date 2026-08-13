//! Unit tests for provider resolution and multi-provider aliases
//! (`src/agent/mod.rs::build_provider`, `src/llm/anthropic.rs`).

use lcode::agent::build_provider;
use lcode::config::{Config, LlmConfig};

/// Config with a dummy (non-empty) key so provider construction
/// succeeds and the resolved provider kind can be asserted.
fn config_with(provider: &str, api_key: &str) -> Config {
    Config {
        llm: LlmConfig {
            provider: provider.to_string(),
            api_key: api_key.to_string(),
            model: "test-model".to_string(),
            ..LlmConfig::default()
        },
        ..Config::default()
    }
}

#[test]
fn anthropic_aliases_resolve_to_anthropic_provider() {
    // deepseek / kimi are Anthropic-compatible endpoints.
    for alias in ["deepseek", "kimi"] {
        let provider = build_provider(&config_with(alias, "test-key"))
            .unwrap_or_else(|e| panic!("alias {alias} should build: {e}"));
        assert_eq!(provider.name(), "anthropic", "alias {alias} must use the Anthropic client");
    }
}

#[test]
fn openai_aliases_resolve_to_openai_provider() {
    // minimax / glm are OpenAI-compatible endpoints.
    for alias in ["minimax", "glm"] {
        let provider = build_provider(&config_with(alias, "test-key"))
            .unwrap_or_else(|e| panic!("alias {alias} should build: {e}"));
        assert_eq!(provider.name(), "openai", "alias {alias} must use the OpenAI client");
    }
}

#[test]
fn base_aliases_still_work() {
    for alias in ["openai", "openai_compatible"] {
        let provider = build_provider(&config_with(alias, "test-key")).expect("openai alias");
        assert_eq!(provider.name(), "openai");
    }
    for alias in ["anthropic", "claude"] {
        let provider = build_provider(&config_with(alias, "test-key")).expect("anthropic alias");
        assert_eq!(provider.name(), "anthropic");
    }
}

#[test]
fn unknown_provider_errors_with_supported_list() {
    let err = match build_provider(&config_with("gpt-999", "test-key")) {
        Ok(_) => panic!("unknown provider must be rejected"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.contains("Unknown LLM provider: gpt-999"), "unexpected message: {msg}");
    assert!(msg.contains("deepseek") && msg.contains("glm"), "message lists aliases: {msg}");
}

#[test]
fn openai_compatible_alias_without_key_mentions_openai() {
    // OpenAI-compatible providers do not consult env vars, so this error
    // is deterministic: an anthropic alias would also check
    // ANTHROPIC_API_KEY (env-dependent), which is why only the OpenAI
    // path is asserted here.
    let err = match build_provider(&config_with("glm", "")) {
        Ok(_) => panic!("missing key must fail"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("OpenAI API key is required"), "got: {err}");
}

#[test]
fn anthropic_provider_defaults_to_official_api_base() {
    let cfg = LlmConfig { api_key: "test-key".to_string(), api_base: None, ..LlmConfig::default() };
    let provider = lcode::llm::anthropic::AnthropicProvider::new(&cfg).unwrap();
    assert_eq!(provider.api_base(), "https://api.anthropic.com/v1");
}

#[test]
fn anthropic_provider_uses_custom_api_base() {
    let cfg = LlmConfig {
        api_key: "test-key".to_string(),
        api_base: Some("https://api.deepseek.com/anthropic".to_string()),
        ..LlmConfig::default()
    };
    let provider = lcode::llm::anthropic::AnthropicProvider::new(&cfg).unwrap();
    assert_eq!(provider.api_base(), "https://api.deepseek.com/anthropic");
}

#[test]
fn explicit_api_base_wins_over_alias_default() {
    let mut cfg = config_with("deepseek", "test-key");
    cfg.llm.api_base = Some("https://gateway.example.com/anthropic".to_string());
    let provider = build_provider(&cfg).expect("custom api_base must be accepted");
    assert_eq!(provider.name(), "anthropic");
}

// ---------------------------------------------------------------------------
// Anthropic wire serialization: multi-tool-call turns (E2E regression —
// DeepSeek's strict Anthropic endpoint rejected separate user messages
// for each tool_result with a 400).
// ---------------------------------------------------------------------------

use lcode::llm::anthropic::anthropic_messages_to_json;
use lcode::llm::{ChatMessage, FunctionCall, ToolCallRequest};

/// Consecutive tool results merge into ONE user message whose
/// `content` array pairs every tool_use in order.
#[test]
fn anthropic_serialization_merges_parallel_tool_results() {
    let mut assistant = ChatMessage::assistant(String::new());
    assistant.tool_calls = Some(vec![
        ToolCallRequest {
            id: "call-1".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall { name: "read_file".to_string(), arguments: "{}".to_string() },
        },
        ToolCallRequest {
            id: "call-2".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall { name: "list_dir".to_string(), arguments: "{}".to_string() },
        },
    ]);
    let messages = [
        ChatMessage::user("task"),
        assistant,
        ChatMessage::tool("result-1".to_string(), "call-1".to_string()),
        ChatMessage::tool("result-2".to_string(), "call-2".to_string()),
    ];
    let refs: Vec<&ChatMessage> = messages.iter().collect();
    let json = anthropic_messages_to_json(&refs, false);

    // user(task) + assistant(tool_use x2) + ONE merged user message.
    assert_eq!(json.len(), 3, "tool results merge into one user message: {json:?}");
    let merged = &json[2];
    assert_eq!(merged["role"], "user");
    let blocks = merged["content"].as_array().expect("content is a block array");
    assert_eq!(blocks.len(), 2, "both tool_result blocks in one message");
    assert_eq!(blocks[0]["type"], "tool_result");
    assert_eq!(blocks[0]["tool_use_id"], "call-1");
    assert_eq!(blocks[0]["content"], "result-1");
    assert_eq!(blocks[1]["tool_use_id"], "call-2");
    assert_eq!(blocks[1]["content"], "result-2");

    // The assistant message still carries both tool_use blocks.
    let assistant_json = &json[1];
    let uses = assistant_json["content"].as_array().expect("assistant blocks");
    assert_eq!(uses.len(), 2);
    assert_eq!(uses[0]["id"], "call-1");
    assert_eq!(uses[1]["id"], "call-2");
}

/// A lone tool result keeps its single-block shape (no behavior change
/// for single tool calls).
#[test]
fn anthropic_serialization_keeps_single_tool_result_shape() {
    let mut assistant = ChatMessage::assistant(String::new());
    assistant.tool_calls = Some(vec![ToolCallRequest {
        id: "call-1".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall { name: "grep".to_string(), arguments: "{}".to_string() },
    }]);
    let messages = [
        ChatMessage::user("task"),
        assistant,
        ChatMessage::tool("one".to_string(), "call-1".to_string()),
    ];
    let refs: Vec<&ChatMessage> = messages.iter().collect();
    let json = anthropic_messages_to_json(&refs, false);
    assert_eq!(json.len(), 3);
    assert_eq!(json[2]["content"][0]["tool_use_id"], "call-1");
    assert_eq!(json[2]["content"][0]["content"], "one");
}
