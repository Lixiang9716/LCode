//! Regression tests for the review-batch fixes: endpoint gating,
//! streaming prefix rejection, thinking-placeholder shapes, server-tool
//! edge cases, internal-usage aggregation, host matching, and length caps.

use lcode::config::{Config, LlmConfig, ReasoningEffort};
use lcode::llm::anthropic::{anthropic_message_to_json, AnthropicProvider};
use lcode::llm::openai::OpenAiProvider;
use lcode::llm::{
    ChatMessage, FinishReason, FunctionCall, LlmProvider, LlmResponse, ServerToolSpec,
    ToolCallRequest, Usage,
};
use lcode::tools::ToolRegistry;
use std::sync::{Arc, Mutex};

fn deepseek_openai_config() -> LlmConfig {
    LlmConfig {
        provider: "openai_compatible".to_string(),
        api_key: "test-key".to_string(),
        model: "deepseek-v4-flash".to_string(),
        api_base: Some("https://api.deepseek.com".to_string()),
        ..LlmConfig::default()
    }
}

fn deepseek_anthropic_config() -> LlmConfig {
    LlmConfig {
        provider: "deepseek".to_string(),
        api_key: "test-key".to_string(),
        model: "deepseek-v4-flash".to_string(),
        api_base: Some("https://api.deepseek.com/anthropic".to_string()),
        ..LlmConfig::default()
    }
}

// --- endpoint gating (review C1/M1) ---

#[test]
fn reasoning_effort_gated_to_deepseek_on_both_formats() {
    // OpenAI format: sent on deepseek, withheld elsewhere.
    let config =
        LlmConfig { reasoning_effort: Some(ReasoningEffort::Max), ..deepseek_openai_config() };
    let provider = OpenAiProvider::new(&config).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false);
    assert_eq!(body["reasoning_effort"], "max");

    let minimax = LlmConfig {
        reasoning_effort: Some(ReasoningEffort::Max),
        api_base: Some("https://api.minimaxi.com/v1".to_string()),
        ..deepseek_openai_config()
    };
    let provider = OpenAiProvider::new(&minimax).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false);
    assert!(body.get("reasoning_effort").is_none(), "minimax must not receive reasoning_effort");

    // Anthropic format: kimi (third-party but not deepseek) gets nothing.
    let kimi = LlmConfig {
        reasoning_effort: Some(ReasoningEffort::Max),
        api_base: Some("https://api.moonshot.cn/anthropic".to_string()),
        ..deepseek_anthropic_config()
    };
    let provider = AnthropicProvider::new(&kimi).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false).expect("body builds");
    assert!(body.get("output_config").is_none(), "kimi must not receive output_config");
}

#[test]
fn anthropic_effort_max_and_thinking_disabled_priority() {
    let config =
        LlmConfig { reasoning_effort: Some(ReasoningEffort::Max), ..deepseek_anthropic_config() };
    let provider = AnthropicProvider::new(&config).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false).expect("body builds");
    assert_eq!(body["output_config"], serde_json::json!({ "effort": "max" }));

    // thinking_disabled wins: no output_config alongside it.
    let config = LlmConfig {
        thinking_disabled: true,
        reasoning_effort: Some(ReasoningEffort::Max),
        ..deepseek_anthropic_config()
    };
    let provider = AnthropicProvider::new(&config).unwrap();
    let body = provider.build_body(&[ChatMessage::user("hi")], &[], false).expect("body builds");
    assert!(body.get("output_config").is_none());
    assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
}

// --- streaming prefix rejection (review M1) ---

#[tokio::test]
async fn openai_stream_rejects_prefix_requests() {
    let provider = OpenAiProvider::new(&deepseek_openai_config()).unwrap();
    let messages = [ChatMessage::user("q"), ChatMessage::assistant_prefix("[")];
    let result = provider.chat_stream(&messages, &[]).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("prefix streaming must be rejected before any request"),
    };
    assert!(err.to_string().contains("streaming"), "clear error, got: {err}");
}

// --- thinking placeholder shapes (review M3) ---

#[test]
fn thinking_placeholder_multi_message_shape() {
    let tool_calls = vec![ToolCallRequest {
        id: "c1".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall { name: "read_file".to_string(), arguments: "{}".to_string() },
    }];
    let mut assistant = ChatMessage::assistant("reading");
    assistant.tool_calls = Some(tool_calls);
    let messages = vec![
        ChatMessage::user("read it"),
        assistant,
        ChatMessage::tool("content", "c1".to_string()),
        ChatMessage::assistant("done"),
    ];
    let provider = AnthropicProvider::new(&deepseek_anthropic_config()).unwrap();
    let body = provider.build_body(&messages, &[], false).expect("body builds");
    let wire = body["messages"].as_array().unwrap();
    assert_eq!(wire.len(), 4);
    // Assistant with tool calls: thinking placeholder leads.
    assert_eq!(wire[1]["content"][0]["type"], "thinking");
    assert_eq!(wire[1]["content"][1]["type"], "text");
    assert_eq!(wire[1]["content"][2]["type"], "tool_use");
    // Tool-result message: user role, no placeholder.
    assert_eq!(wire[2]["role"], "user");
    assert_eq!(wire[2]["content"][0]["type"], "tool_result");
    // Plain assistant: placeholder + text.
    assert_eq!(wire[3]["content"][0]["type"], "thinking");
    assert_eq!(wire[3]["content"][1]["type"], "text");

    // Kimi endpoint: no placeholder, plain string content.
    let kimi = LlmConfig {
        api_base: Some("https://api.moonshot.cn/anthropic".to_string()),
        ..deepseek_anthropic_config()
    };
    let provider = AnthropicProvider::new(&kimi).unwrap();
    let body =
        provider.build_body(&[ChatMessage::assistant("done")], &[], false).expect("body builds");
    assert_eq!(body["messages"][0]["content"], "done");
}

#[test]
fn thinking_placeholder_omits_empty_text_block() {
    let empty = anthropic_message_to_json(&&ChatMessage::assistant(""), true);
    let parts = empty["content"].as_array().expect("array content");
    assert_eq!(parts.len(), 1, "empty text block must be omitted");
    assert_eq!(parts[0]["type"], "thinking");
}

// --- tool pool assembly (review m5) ---

#[test]
fn tool_pool_includes_web_search_only_when_configured() {
    let session = |web_search| {
        let (_runtime, _events, _commands) = lcode::agent::AgentRuntime::new();
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(lcode::agent::TodoManager::default())),
            background: Arc::new(lcode::agent::BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron: Arc::new(Mutex::new(lcode::agent::CronScheduler::new(
                &std::path::PathBuf::from("."),
            ))),
            mcp: Arc::new(Mutex::new(lcode::agent::McpRegistry::default())),
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: None,
            team_bus: None,
            tuning: None,
            internal_provider: None,
            web_search,
        }
    };

    let mock = || lcode::llm::provider::MockLlmProvider::new();
    let with_spec = Some(ServerToolSpec {
        tool_type: "web_search_20260209".to_string(),
        name: "web_search".to_string(),
        max_queries: Some(5),
    });

    let executor = lcode::agent::Executor::new(
        Box::new(mock()),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        lcode::agent::AgentRuntime::new().0,
        session(with_spec),
    );
    let pool = executor.tool_pool();
    assert!(pool.iter().any(|t| t.server.is_some()), "web_search declared in the tool pool");

    let executor = lcode::agent::Executor::new(
        Box::new(mock()),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        lcode::agent::AgentRuntime::new().0,
        session(None),
    );
    let pool = executor.tool_pool();
    assert!(pool.iter().all(|t| t.server.is_none()), "no server tools without the spec");
}

// --- server tool edge cases (review M1/M4) ---

#[tokio::test]
async fn server_tool_call_without_result_records_error_not_local_execution() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(LlmResponse {
                content: String::new(),
                tool_calls: Some(vec![ToolCallRequest {
                    id: "ws-1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "web_search".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                server_results: Vec::new(),
                usage: Usage::default(),
                finish_reason: FinishReason::ToolCalls,
            })
        } else {
            Ok(stop_response("Done."))
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let mut executor = executor_with(mock, web_search_spec());
    let memory = executor
        .run(
            "search",
            &lcode::agent::Planner::new(10),
            lcode::agent::ConversationMemory::new("sys".to_string()),
            5,
            false,
        )
        .await
        .expect("run completes");

    let result = memory
        .messages()
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("ws-1"))
        .expect("result recorded");
    assert!(
        result.content.contains("returned no result"),
        "explicit server error, not local 'Unknown tool': {}",
        result.content
    );
}

#[tokio::test]
async fn stop_turn_with_server_results_records_them() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_, _| {
        Ok(LlmResponse {
            content: "The answer is 1.93".to_string(),
            tool_calls: None,
            server_results: vec![lcode::llm::ServerToolResult {
                id: "sr-1".to_string(),
                name: "web_search".to_string(),
                content: "[rust](https://rust-lang.org)".to_string(),
            }],
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        })
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let mut executor = executor_with(mock, web_search_spec());
    let memory = executor
        .run(
            "search",
            &lcode::agent::Planner::new(10),
            lcode::agent::ConversationMemory::new("sys".to_string()),
            5,
            false,
        )
        .await
        .expect("run completes");

    let result = memory
        .messages()
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("sr-1"))
        .expect("server result recorded on Stop");
    assert!(result.content.contains("rust-lang.org"));
    // The assistant message must carry the matching tool_use for pairing.
    let assistant = memory
        .messages()
        .iter()
        .find(|m| m.role == lcode::llm::Role::Assistant)
        .expect("assistant message present");
    assert!(
        assistant.tool_calls.as_ref().is_some_and(|calls| calls.iter().any(|c| c.id == "sr-1")),
        "assistant message pairs the recorded result"
    );
}

// --- internal usage aggregation (review M2) ---

#[tokio::test]
async fn internal_call_usage_lands_in_session_total() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(LlmResponse {
                content: "Done.".to_string(),
                tool_calls: None,
                server_results: Vec::new(),
                usage: Usage {
                    prompt_tokens: 10,
                    completion_tokens: 3,
                    total_tokens: 13,
                    ..Usage::default()
                },
                finish_reason: FinishReason::Stop,
            })
        } else {
            // The session-end memory extraction call.
            Ok(LlmResponse {
                content: "[]".to_string(),
                tool_calls: None,
                server_results: Vec::new(),
                usage: Usage {
                    prompt_tokens: 20,
                    completion_tokens: 7,
                    total_tokens: 27,
                    reasoning_tokens: 42,
                    ..Usage::default()
                },
                finish_reason: FinishReason::Stop,
            })
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let tmp = tempfile::TempDir::new().unwrap();
    let (runtime, mut events_rx, _commands) = lcode::agent::AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(lcode::agent::TodoManager::default())),
            background: Arc::new(lcode::agent::BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron: Arc::new(Mutex::new(lcode::agent::CronScheduler::new(
                &std::path::PathBuf::from("."),
            ))),
            mcp: Arc::new(Mutex::new(lcode::agent::McpRegistry::default())),
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: Some(Arc::new(lcode::agent::MemoryStore::new(tmp.path()).unwrap())),
            team_bus: None,
            tuning: None,
            internal_provider: None,
            web_search: None,
        },
    );
    executor
        .run(
            "task",
            &lcode::agent::Planner::new(10),
            lcode::agent::ConversationMemory::new("sys".to_string()),
            5,
            false,
        )
        .await
        .expect("run completes");

    // TaskFinished aggregates main + internal usage.
    let mut total = Usage::default();
    while let Ok(event) = events_rx.try_recv() {
        if let lcode::agent::AgentEvent::TaskFinished { prompt_tokens, completion_tokens, .. } =
            event
        {
            total.prompt_tokens = prompt_tokens;
            total.completion_tokens = completion_tokens;
        }
    }
    assert_eq!(total.prompt_tokens, 30, "main + extraction prompt tokens");
    assert_eq!(total.completion_tokens, 10, "main + extraction completion tokens");
}

// --- helpers ---

fn stop_response(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls: None,
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }
}

fn web_search_spec() -> Option<ServerToolSpec> {
    Some(ServerToolSpec {
        tool_type: "web_search_20260209".to_string(),
        name: "web_search".to_string(),
        max_queries: Some(5),
    })
}

fn executor_with(
    mock: lcode::llm::provider::MockLlmProvider,
    web_search: Option<ServerToolSpec>,
) -> lcode::agent::Executor {
    let (runtime, _events, _commands) = lcode::agent::AgentRuntime::new();
    lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(lcode::agent::TodoManager::default())),
            background: Arc::new(lcode::agent::BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron: Arc::new(Mutex::new(lcode::agent::CronScheduler::new(
                &std::path::PathBuf::from("."),
            ))),
            mcp: Arc::new(Mutex::new(lcode::agent::McpRegistry::default())),
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: None,
            team_bus: None,
            tuning: None,
            internal_provider: None,
            web_search,
        },
    )
}
