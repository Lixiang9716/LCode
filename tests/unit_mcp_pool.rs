//! Integration tests for the MCP dynamic tool pool (s19):
//! connecting a server must expose `mcp__{server}__{tool}` tools to the
//! model on the next turn, and calling them must route through the MCP
//! registry.

use lcode::agent::{AgentRuntime, ConversationMemory, Executor, McpRegistry, Planner};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, FunctionCall, LlmResponse, ToolCallRequest, ToolDefinition, Usage};
use lcode::tools::ToolRegistry;
use std::sync::{Arc, Mutex};

/// Build a mock provider; records the tool definitions of every chat call
/// into `seen_tools`.
fn mock_with_tool_recorder(
    responses: Vec<LlmResponse>,
    seen_tools: Arc<Mutex<Vec<Vec<ToolDefinition>>>>,
) -> MockLlmProvider {
    let mut mock = MockLlmProvider::new();
    let responses = Arc::new(Mutex::new(responses.into_iter()));
    mock.expect_chat().returning(move |_messages, tools| {
        seen_tools.lock().unwrap().push(tools.to_vec());
        let resp = responses.lock().unwrap().next().expect("mock ran out of responses");
        Ok(resp)
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));
    mock
}

fn response(
    content: &str,
    finish: FinishReason,
    tool_calls: Option<Vec<ToolCallRequest>>,
) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls,
        usage: Usage::default(),
        finish_reason: finish,
    }
}

#[tokio::test]
async fn mcp_connected_server_appears_in_next_turn_tool_pool() {
    // Registry with a connected mock server (docs → read/search tools).
    let mcp = Arc::new(Mutex::new(McpRegistry::default()));
    mcp.lock().unwrap().connect("docs", "mock://docs").expect("connect mock server");

    let seen_tools: Arc<Mutex<Vec<Vec<ToolDefinition>>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = mock_with_tool_recorder(
        vec![response("Done.", FinishReason::Stop, None)],
        seen_tools.clone(),
    );

    let (runtime, _events_rx, _cmd_tx) = AgentRuntime::new();
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    let mut executor = Executor::new(
        Box::new(mock),
        registry,
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(lcode::agent::TodoManager::default())),
            background: Arc::new(lcode::agent::BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron: Arc::new(std::sync::Mutex::new(lcode::agent::CronScheduler::new(
                &std::path::PathBuf::from("."),
            ))),
            mcp,
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: None,
            team_bus: None,
        },
    );

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    executor.run("inspect", &planner, memory, 5, false).await.expect("run");

    let tools = seen_tools.lock().unwrap();
    assert!(!tools.is_empty(), "provider should have been called");
    let names: Vec<&str> = tools[0].iter().map(|t| t.function.name.as_str()).collect();
    assert!(
        names.contains(&"mcp__docs__get_version"),
        "mcp__docs__get_version missing from pool: {:?}",
        names
    );
    assert!(
        names.contains(&"mcp__docs__search"),
        "mcp__docs__search missing from pool: {:?}",
        names
    );
    // Built-ins must still be present.
    assert!(names.contains(&"read_file"));
}

#[tokio::test]
async fn mcp_tool_call_routes_through_mcp_registry() {
    let mcp = Arc::new(Mutex::new(McpRegistry::default()));
    mcp.lock().unwrap().connect("docs", "mock://docs").expect("connect mock server");

    // First turn: model calls mcp__docs__search; second turn: stop.
    let tool_call = ToolCallRequest {
        id: "mcp_call_1".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "mcp__docs__search".to_string(),
            arguments: r#"{"query": "setup"}"#.to_string(),
        },
    };

    let seen_tools: Arc<Mutex<Vec<Vec<ToolDefinition>>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = mock_with_tool_recorder(
        vec![
            response("", FinishReason::ToolCalls, Some(vec![tool_call])),
            response("Finished.", FinishReason::Stop, None),
        ],
        seen_tools.clone(),
    );

    let (runtime, _events_rx, _cmd_tx) = AgentRuntime::new();
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    let mut executor = Executor::new(
        Box::new(mock),
        registry,
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(lcode::agent::TodoManager::default())),
            background: Arc::new(lcode::agent::BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron: Arc::new(std::sync::Mutex::new(lcode::agent::CronScheduler::new(
                &std::path::PathBuf::from("."),
            ))),
            mcp,
            compact_request: Arc::new(Mutex::new(None)),
            memory_store: None,
            team_bus: None,
        },
    );

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory = executor.run("use mcp", &planner, memory, 5, false).await.expect("run");

    // The MCP call result must be recorded in the conversation.
    let msgs = memory.messages();
    let tool_results: Vec<&str> = msgs
        .iter()
        .filter(|m| matches!(m.role, lcode::llm::Role::Tool))
        .map(|m| m.content.as_str())
        .collect();
    assert!(
        tool_results.iter().any(|r| r.contains("docs.search called with")),
        "MCP call result missing, got: {:?}",
        tool_results
    );
    assert_eq!(seen_tools.lock().unwrap().len(), 2, "two LLM calls expected");
}
