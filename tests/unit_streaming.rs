//! Unit tests for streaming consumption in the executor: when `run` is
//! called with `stream = true`, token deltas are published one by one as
//! `TextDelta` events (typewriter effect), their concatenation equals the
//! full text, and the accumulated text lands in the conversation memory
//! as a single assistant message.

use lcode::agent::{
    AgentEvent, AgentRuntime, BackgroundManager, ConversationMemory, CronScheduler, Executor,
    Planner, TodoManager,
};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, Role, StreamEvent};
use lcode::tools::ToolRegistry;
use std::sync::{Arc, Mutex};

/// Collect published events until the session ends (`TaskFinished` /
/// `TaskAborted`) or the channel closes, with a timeout guard so a
/// missing terminal event fails the test instead of hanging forever.
/// (Copied from tests/unit_agent.rs — each integration test file is its
/// own crate, so the helper must live here too.)
async fn collect_events(mut rx: tokio::sync::broadcast::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(Ok(event)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await
    {
        let done =
            matches!(event, AgentEvent::TaskFinished { .. } | AgentEvent::TaskAborted { .. });
        events.push(event);
        if done {
            break;
        }
    }
    events
}

/// Build an executor whose mock provider streams two text deltas followed
/// by `Done(Stop)` — the exact typewriter sequence the executor must
/// consume.
fn streaming_executor() -> (Executor, tokio::sync::broadcast::Receiver<AgentEvent>) {
    let mut mock = MockLlmProvider::new();
    mock.expect_chat_stream().times(1).returning(|_messages, _tools| {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("Hello ".to_string())),
            Ok(StreamEvent::TextDelta("world".to_string())),
            Ok(StreamEvent::Done(FinishReason::Stop)),
        ])))
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, events_rx, _commands_tx) = AgentRuntime::new();
    let tmp = tempfile::tempdir().expect("tempdir for cron scheduler");
    let cron = Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf())));
    (
        Executor::new(
            Box::new(mock),
            ToolRegistry::new(&Config::default()).expect("build tool registry"),
            true,
            runtime,
            lcode::agent::SessionState {
                todo: Arc::new(Mutex::new(TodoManager::default())),
                background: Arc::new(BackgroundManager::default()),
                hooks: Arc::new(lcode::agent::HookRegistry::default()),
                cron,
                mcp: Arc::new(std::sync::Mutex::new(lcode::agent::McpRegistry::default())),
                compact_request: Arc::new(std::sync::Mutex::new(None)),
                memory_store: None,
                team_bus: None,
                tuning: None,
            },
        ),
        events_rx,
    )
}

#[tokio::test]
async fn test_streaming_publishes_each_delta_and_accumulates_text() {
    let (mut executor, events_rx) = streaming_executor();

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory =
        executor.run("stream task", &planner, memory, 5, true).await.expect("run should succeed");

    // Each delta is published as its own TextDelta event, in order.
    let events = collect_events(events_rx).await;
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Hello ", "world"]);
    // Concatenating the deltas in arrival order gives the full response.
    assert_eq!(deltas.concat(), "Hello world");

    // The accumulated text is recorded as a single assistant message.
    let msgs = memory.messages();
    assert!(
        msgs.iter().any(|m| matches!(m.role, Role::Assistant) && m.content == "Hello world"),
        "expected assistant message 'Hello world', got: {msgs:?}"
    );
}

#[tokio::test]
async fn test_non_streaming_run_does_not_call_chat_stream() {
    // Same mock setup as streaming_executor, but `run(.., false)` must
    // take the plain chat path instead — set expectations for both so
    // only the used one may be called.
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_messages, _tools| {
        Ok(lcode::llm::LlmResponse {
            content: "Plain answer.".to_string(),
            tool_calls: None,
            usage: lcode::llm::Usage::default(),
            finish_reason: FinishReason::Stop,
        })
    });
    mock.expect_chat_stream().times(0);
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, events_rx, _commands_tx) = AgentRuntime::new();
    let tmp = tempfile::tempdir().expect("tempdir for cron scheduler");
    let cron = Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf())));
    let mut executor = Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).expect("build tool registry"),
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(TodoManager::default())),
            background: Arc::new(BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron,
            mcp: Arc::new(std::sync::Mutex::new(lcode::agent::McpRegistry::default())),
            compact_request: Arc::new(std::sync::Mutex::new(None)),
            memory_store: None,
            team_bus: None,
            tuning: None,
        },
    );

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory =
        executor.run("plain task", &planner, memory, 5, false).await.expect("run should succeed");
    assert!(memory.messages().iter().any(|m| m.content == "Plain answer."));

    // Only the plain text is published — one TextGenerated event.
    let events = collect_events(events_rx).await;
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextGenerated { content } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, vec!["Plain answer."]);
}

/// A streamed response that ends in a tool call must fall back to a
/// plain `chat()` call so the tool call is executed instead of being
/// silently dropped (E2E regression: streamed tool-call turns collapsed
/// into empty one-turn "success").
#[tokio::test]
async fn test_streaming_tool_call_falls_back_to_chat() {
    let mut mock = MockLlmProvider::new();
    // Turn 1's stream carries no text, just the tool-call stop reason;
    // turn 2 streams a plain answer and ends the loop.
    let stream_turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let turns = stream_turns.clone();
    mock.expect_chat_stream().times(0..).returning(move |_messages, _tools| {
        let n = turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let events = if n == 0 {
            vec![Ok(StreamEvent::Done(FinishReason::ToolCalls))]
        } else {
            vec![
                Ok(StreamEvent::TextDelta("done".to_string())),
                Ok(StreamEvent::Done(FinishReason::Stop)),
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    });
    // The fallback chat returns the full response with the tool call.
    mock.expect_chat().times(1).returning(|_messages, _tools| {
        Ok(lcode::llm::LlmResponse {
            content: String::new(),
            tool_calls: Some(vec![lcode::llm::ToolCallRequest {
                id: "call-1".to_string(),
                call_type: "function".to_string(),
                function: lcode::llm::FunctionCall {
                    name: "list_dir".to_string(),
                    arguments: serde_json::json!({}).to_string(),
                },
            }]),
            usage: lcode::llm::Usage::default(),
            finish_reason: FinishReason::ToolCalls,
        })
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, events_rx, _commands_tx) = AgentRuntime::new();
    let tmp = tempfile::tempdir().expect("tempdir");
    let cron = Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf())));
    let mut executor = Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).expect("build tool registry"),
        true,
        runtime,
        lcode::agent::SessionState {
            todo: Arc::new(Mutex::new(TodoManager::default())),
            background: Arc::new(BackgroundManager::default()),
            hooks: Arc::new(lcode::agent::HookRegistry::default()),
            cron,
            mcp: Arc::new(std::sync::Mutex::new(lcode::agent::McpRegistry::default())),
            compact_request: Arc::new(std::sync::Mutex::new(None)),
            memory_store: None,
            team_bus: None,
            tuning: None,
        },
    );

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    // The tool call is executed (list_dir) and the loop continues.
    let memory =
        executor.run("tool stream", &planner, memory, 5, true).await.expect("run should succeed");
    let events = collect_events(events_rx).await;
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ToolCallExecuted { .. })),
        "the fallback chat's tool call must execute: {events:?}"
    );
    assert!(
        memory.messages().iter().any(|m| matches!(m.role, Role::Tool)),
        "tool result must land in conversation memory"
    );
}
