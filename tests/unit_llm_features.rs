//! Unit tests for the DeepSeek P0/P1 feature batch:
//! - reasoning_effort wiring (OpenAI top-level field, Anthropic
//!   `reasoning` block on third-party endpoints only)
//! - internal provider (thinking forced off for utility calls)
//! - beta prefix completion (URL routing, message flag, anthropic bail)
//! - server-side web_search (declaration gating, response parsing,
//!   executor recording)
//! - memory JSON lock (prefix + graceful fallback)

use lcode::agent::{
    BackgroundManager, ConversationMemory, CronScheduler, HookRegistry, McpRegistry, Planner,
    TodoManager,
};
use lcode::config::{Config, LlmConfig, MemoryConfig, ReasoningEffort};
use lcode::llm::anthropic::{parse_anthropic_response, AnthropicProvider};
use lcode::llm::openai::{completion_url, message_to_json, OpenAiProvider};
use lcode::llm::{
    ChatMessage, FinishReason, FunctionCall, FunctionDefinition, LlmResponse, ServerToolSpec,
    ToolCallRequest, ToolDefinition, Usage,
};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn dummy_openai_config() -> LlmConfig {
    LlmConfig {
        provider: "openai".to_string(),
        api_key: secrecy::SecretString::from("test-key"),
        model: "deepseek-v4-flash".to_string(),
        api_base: Some("https://api.deepseek.com".to_string()),
        ..LlmConfig::default()
    }
}

fn dummy_anthropic_config(api_base: Option<&str>) -> LlmConfig {
    LlmConfig {
        provider: "deepseek".to_string(),
        api_key: secrecy::SecretString::from("test-key"),
        model: "deepseek-v4-flash".to_string(),
        api_base: api_base.map(str::to_string),
        ..LlmConfig::default()
    }
}

// --- reasoning_effort ---

#[test]
fn openai_body_carries_reasoning_effort_when_configured() {
    let config =
        LlmConfig { reasoning_effort: Some(ReasoningEffort::Low), ..dummy_openai_config() };
    let provider = OpenAiProvider::new(&config).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false);
    assert_eq!(body["reasoning_effort"], "low");
    assert!(body.get("thinking").is_none());
}

#[test]
fn openai_thinking_disabled_wins_over_reasoning_effort() {
    let config = LlmConfig {
        thinking_disabled: true,
        reasoning_effort: Some(ReasoningEffort::Max),
        ..dummy_openai_config()
    };
    let provider = OpenAiProvider::new(&config).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false);
    assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn anthropic_effort_uses_output_config_only_on_deepseek() {
    // DeepSeek endpoint: `output_config: {effort}` is the only knob the
    // endpoint honours (a top-level `reasoning` field is ignored).
    let config = LlmConfig {
        reasoning_effort: Some(ReasoningEffort::Low),
        ..dummy_anthropic_config(Some("https://api.deepseek.com/anthropic"))
    };
    let provider = AnthropicProvider::new(&config).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false).expect("body builds");
    assert_eq!(body["output_config"], serde_json::json!({ "effort": "low" }));

    // Native Anthropic endpoint: no such field (the API would reject it).
    let config = LlmConfig {
        reasoning_effort: Some(ReasoningEffort::Low),
        ..dummy_anthropic_config(Some("https://api.anthropic.com/v1"))
    };
    let provider = AnthropicProvider::new(&config).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false).expect("body builds");
    assert!(body.get("output_config").is_none());
}

// --- internal provider (P0-1) ---

#[tokio::test]
async fn internal_provider_forces_thinking_disabled() {
    // Main provider: thinking enabled by default → no thinking key.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })))
        .mount(&server)
        .await;

    let llm = LlmConfig {
        provider: "openai_compatible".to_string(),
        api_key: secrecy::SecretString::from("test-key"),
        model: "deepseek-v4-flash".to_string(),
        api_base: Some(server.uri()),
        ..LlmConfig::default()
    };
    let mut config = Config { llm: llm.clone(), ..Config::default() };
    // The internal provider must force thinking off regardless.
    let internal = lcode::agent::build_internal_provider(&config).unwrap();
    let _ = internal
        .chat(&[ChatMessage::user("summarize this")], &[])
        .await
        .expect("internal call succeeds");

    // Capture the request the wiremock server received and assert the
    // thinking key it carried.
    let received = server.received_requests().await.expect("one request recorded");
    let request = received.last().expect("request recorded");
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));

    // With `internal_thinking_disabled = false` the internal provider
    // keeps the user's thinking mode instead.
    config.llm = LlmConfig { internal_thinking_disabled: false, ..llm };
    let internal = lcode::agent::build_internal_provider(&config).unwrap();
    let _ = internal.chat(&[ChatMessage::user("again")], &[]).await.expect("call succeeds");
    let received = server.received_requests().await.expect("second request recorded");
    let request = received.last().expect("request recorded");
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert!(body.get("thinking").is_none());
}

// --- prefix completion (P1-1) ---

#[test]
fn prefix_routes_to_beta_endpoint() {
    assert_eq!(
        completion_url("https://api.deepseek.com", false),
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(
        completion_url("https://api.deepseek.com/", true),
        "https://api.deepseek.com/beta/chat/completions"
    );
}

#[test]
fn prefix_message_serializes_with_prefix_flag() {
    let plain = message_to_json(&ChatMessage::assistant("done"));
    assert!(plain.get("prefix").is_none());
    let locked = message_to_json(&ChatMessage::assistant_prefix("["));
    assert_eq!(locked["prefix"], serde_json::Value::Bool(true));
    assert_eq!(locked["role"], "assistant");
}

#[test]
fn anthropic_provider_rejects_prefix_requests() {
    let provider = AnthropicProvider::new(&dummy_anthropic_config(None)).unwrap();
    let err = provider
        .build_body(&[ChatMessage::user("q"), ChatMessage::assistant_prefix("[")], &[], false)
        .unwrap_err();
    assert!(err.to_string().contains("prefix completion"), "clear error, got: {err}");
}

// --- web_search (P1-2) ---

#[test]
fn web_search_gated_to_deepseek_anthropic_endpoint() {
    // Disabled when enable_web is off.
    let mut config = Config::default();
    config.tools.enable_web = false;
    config.llm = dummy_anthropic_config(None);
    assert!(lcode::agent::web_search_spec(&config).is_none());

    // DeepSeek Anthropic-compatible endpoint: enabled.
    config.tools.enable_web = true;
    let spec = lcode::agent::web_search_spec(&config).expect("deepseek enables web_search");
    assert_eq!(spec.name, "web_search");

    // OpenAI-format endpoint: never enabled (chat completions has no
    // server tools).
    config.llm = dummy_openai_config();
    assert!(lcode::agent::web_search_spec(&config).is_none());

    // Native Anthropic endpoint: not enabled (different tool type).
    config.llm = dummy_anthropic_config(Some("https://api.anthropic.com/v1"));
    assert!(lcode::agent::web_search_spec(&config).is_none());
}

#[test]
fn anthropic_body_serializes_server_tool_and_filters_client_tools() {
    let web_search = ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: "web_search".to_string(),
            description: "search".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        },
        server: Some(ServerToolSpec {
            tool_type: "web_search_20260209".to_string(),
            name: "web_search".to_string(),
            max_queries: Some(3),
        }),
    };
    let client_tool = ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: "read_file".to_string(),
            description: "read".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        },
        server: None,
    };
    let provider = AnthropicProvider::new(&dummy_anthropic_config(None)).unwrap();
    let body = provider
        .build_body(&[ChatMessage::user("q")], &[web_search, client_tool], false)
        .expect("body builds");
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["type"], "web_search_20260209");
    assert_eq!(tools[0]["name"], "web_search");
    assert_eq!(tools[0]["max_queries"], 3);
    assert_eq!(tools[1]["name"], "read_file");
}

#[test]
fn openai_body_drops_server_tools() {
    let web_search = ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: "web_search".to_string(),
            description: "search".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        },
        server: Some(ServerToolSpec {
            tool_type: "web_search_20260209".to_string(),
            name: "web_search".to_string(),
            max_queries: None,
        }),
    };
    let provider = OpenAiProvider::new(&dummy_openai_config()).unwrap();
    let body = provider.build_body(&[ChatMessage::user("q")], &[web_search], false);
    assert!(body.get("tools").is_none(), "server tools must never reach chat completions");
}

#[test]
fn parse_anthropic_server_tool_blocks() {
    let data = serde_json::json!({
        "content": [
            { "type": "text", "text": "Searching...\n" },
            { "type": "server_tool_use", "id": "sr-1", "name": "web_search",
              "input": { "query": "rust tokio select" } },
            { "type": "web_search_tool_result", "tool_use_id": "sr-1",
              "content": [
                  { "type": "text", "text": "Results for rust tokio select" },
                  { "type": "web_page", "title": "Tokio docs", "url": "https://docs.rs/tokio",
                    "snippet": "select! macro" }
              ] }
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 10, "output_tokens": 20 }
    });
    let response = parse_anthropic_response(&data).expect("parses");
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    let calls = response.tool_calls.expect("server call recorded");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "sr-1");
    assert_eq!(calls[0].function.name, "web_search");
    assert_eq!(response.server_results.len(), 1);
    let result = &response.server_results[0];
    assert_eq!(result.id, "sr-1");
    assert!(result.content.contains("Results for rust tokio select"));
    assert!(result.content.contains("[Tokio docs](https://docs.rs/tokio): select! macro"));
}

#[test]
fn orphan_web_search_result_synthesizes_matching_call() {
    let data = serde_json::json!({
        "content": [
            { "type": "web_search_tool_result", "tool_use_id": "sr-9",
              "content": [ { "type": "text", "text": "found" } ] }
        ],
        "stop_reason": "tool_use"
    });
    let response = parse_anthropic_response(&data).expect("parses");
    let calls = response.tool_calls.expect("matching call synthesized");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "sr-9");
    assert_eq!(calls[0].function.name, "web_search");
}

// --- executor: server results are recorded, never executed locally ---

#[tokio::test]
async fn executor_records_server_results_without_local_execution() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(LlmResponse {
                content: String::new(),
                tool_calls: Some(vec![ToolCallRequest {
                    id: "sr-1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "web_search".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                server_results: vec![lcode::llm::ServerToolResult {
                    id: "sr-1".to_string(),
                    name: "web_search".to_string(),
                    content: "[rust](https://rust-lang.org): official site".to_string(),
                }],
                usage: Usage::default(),
                finish_reason: FinishReason::ToolCalls,
            })
        } else {
            Ok(LlmResponse {
                content: "Done.".to_string(),
                tool_calls: None,
                server_results: Vec::new(),
                usage: Usage::default(),
                finish_reason: FinishReason::Stop,
            })
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, mut events_rx, _commands_tx) = lcode::agent::AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        lcode::tools::ToolRegistry::new(&Config::default()).expect("registry"),
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(TodoManager::default())),
            background: Arc::new(BackgroundManager::new(&Config::default()).unwrap()),
            hooks: Arc::new(HookRegistry::default()),
            cron: Arc::new(Mutex::new(CronScheduler::new(&std::path::PathBuf::from(".")))),
            mcp: Arc::new(Mutex::new(McpRegistry::default())),
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: None,
            team_bus: None,
            tuning: None,
            internal_provider: None,
            web_search: None,
        },
    );
    let memory = executor
        .run(
            "search the web",
            &Planner::new(10),
            ConversationMemory::new("sys".to_string()),
            5,
            false,
        )
        .await
        .expect("run completes");

    // The server result landed in the conversation as a tool result —
    // web_search is not in the registry, so a local execution would have
    // failed with "Unknown tool".
    let messages = memory.messages();
    let tool_result = messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("sr-1"))
        .expect("server result recorded");
    assert!(tool_result.content.contains("[rust](https://rust-lang.org)"));

    // An execution event was published for the audit trail.
    let mut seen = false;
    while let Ok(event) = events_rx.try_recv() {
        if let lcode::agent::AgentEvent::ToolCallExecuted { id, output } = event {
            assert_eq!(id, "sr-1");
            assert!(output.contains("rust-lang.org"));
            seen = true;
        }
    }
    assert!(seen, "ToolCallExecuted published for the server result");
}

// --- memory JSON lock ---

#[tokio::test]
async fn json_lock_prefixes_request_and_falls_back() {
    // Provider that rejects prefix messages (Anthropic-style): the lock
    // must retry without the prefix instead of failing extraction.
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(2).returning(|messages: &[ChatMessage], _tools| {
        if messages.iter().any(|m| m.prefix == Some(true)) {
            anyhow::bail!("prefix completion is not supported on the Anthropic-format endpoint");
        }
        Ok(LlmResponse {
            content: r#"[{"name": "prefers-tabs", "description": "tabs", "tags": [], "body": "uses tabs"}]"#.to_string(),
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
    let written = store
        .extract("user: please use tabs everywhere", &mock)
        .await
        .expect("extraction succeeds via fallback");
    assert_eq!(written, 1);
}

#[test]
fn anthropic_injects_thinking_placeholder_when_enabled() {
    use lcode::llm::anthropic::anthropic_message_to_json;

    // Disabled: plain string content, no placeholder.
    let plain = anthropic_message_to_json(&&ChatMessage::assistant("done"), false);
    assert_eq!(plain["content"], "done");

    // Enabled: array form with an empty thinking block leading.
    let injected = anthropic_message_to_json(&&ChatMessage::assistant("done"), true);
    let parts = injected["content"].as_array().expect("array content");
    assert_eq!(parts[0]["type"], "thinking");
    assert_eq!(parts[0]["thinking"], "");
    assert_eq!(parts[1]["type"], "text");

    // Tool-call messages get the placeholder before text/tool_use.
    let with_calls = ChatMessage {
        role: lcode::llm::Role::Assistant,
        content: "calling".to_string(),
        tool_call_id: None,
        tool_calls: Some(vec![ToolCallRequest {
            id: "c1".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall { name: "read_file".to_string(), arguments: "{}".to_string() },
        }]),
        prefix: None,
    };
    let json = anthropic_message_to_json(&&with_calls, true);
    let parts = json["content"].as_array().expect("array content");
    assert_eq!(parts[0]["type"], "thinking");
    assert_eq!(parts[1]["type"], "text");
    assert_eq!(parts[2]["type"], "tool_use");
}

#[test]
fn anthropic_inject_thinking_flag_follows_endpoint() {
    // DeepSeek endpoint + thinking on → placeholder enabled.
    let config = dummy_anthropic_config(Some("https://api.deepseek.com/anthropic"));
    let provider = AnthropicProvider::new(&config).unwrap();
    let body =
        provider.build_body(&[ChatMessage::assistant("done")], &[], false).expect("body builds");
    assert_eq!(body["messages"][0]["content"][0]["type"], "thinking");

    // thinking_disabled → no placeholder needed.
    let config = LlmConfig {
        thinking_disabled: true,
        ..dummy_anthropic_config(Some("https://api.deepseek.com/anthropic"))
    };
    let provider = AnthropicProvider::new(&config).unwrap();
    let body =
        provider.build_body(&[ChatMessage::assistant("done")], &[], false).expect("body builds");
    assert_eq!(body["messages"][0]["content"], "done");
}
