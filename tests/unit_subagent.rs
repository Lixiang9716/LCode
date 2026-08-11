//! Unit tests for subagents (learn-claude-code s04).
//!
//! Drives `run_subagent` with a mock LLM provider: a Stop response yields
//! the summary, tool-call responses execute registry tools and backfill
//! results, and the turn budget caps runaway loops.

use lcode::agent::{run_subagent, TaskTool};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{
    ChatMessage, FinishReason, FunctionCall, LlmResponse, Role, ToolCallRequest, Usage,
};
use lcode::tools::{Tool, ToolRegistry};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

fn response(
    content: &str,
    finish_reason: FinishReason,
    tool_calls: Option<Vec<ToolCallRequest>>,
) -> LlmResponse {
    LlmResponse { content: content.to_string(), tool_calls, usage: Usage::default(), finish_reason }
}

type SharedProvider = Arc<dyn lcode::llm::LlmProvider>;
type SeenMessages = Arc<Mutex<Vec<Vec<ChatMessage>>>>;

/// Build a mock provider serving `responses` from a queue; every received
/// message batch is recorded into `seen`.
fn mock_with_queue(responses: Vec<LlmResponse>) -> (SharedProvider, SeenMessages) {
    let queue: Arc<Mutex<VecDeque<LlmResponse>>> = Arc::new(Mutex::new(VecDeque::from(responses)));
    let seen: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));

    let mut mock = MockLlmProvider::new();
    let queue_clone = queue.clone();
    let seen_clone = seen.clone();
    mock.expect_chat().times(0..).returning(move |messages, _tools| {
        seen_clone.lock().unwrap().push(messages.to_vec());
        let resp =
            queue_clone.lock().unwrap().pop_front().expect("mock provider ran out of responses");
        Ok(resp)
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));
    (Arc::new(mock), seen)
}

#[tokio::test]
async fn test_subagent_stop_returns_summary_with_fresh_context() {
    let (provider, seen) =
        mock_with_queue(vec![response("Found the bug in login.rs.", FinishReason::Stop, None)]);
    let registry = ToolRegistry::new(&Config::default()).unwrap();

    let summary = run_subagent("Find the bug", provider, &registry, 30).await.unwrap();
    assert_eq!(summary, "Found the bug in login.rs.");

    // The subagent starts from a fresh context: exactly one user message
    // carrying the prompt.
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), 1);
    assert!(matches!(calls[0][0].role, Role::User));
    assert_eq!(calls[0][0].content, "Find the bug");
}

#[tokio::test]
async fn test_subagent_empty_final_text_falls_back() {
    let (provider, _seen) =
        mock_with_queue(vec![response("   ", FinishReason::Stop, None)]);
    let registry = ToolRegistry::new(&Config::default()).unwrap();

    let summary = run_subagent("Do nothing", provider, &registry, 30).await.unwrap();
    assert_eq!(summary, "(no summary)");
}

#[tokio::test]
async fn test_subagent_executes_registry_tools() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let original_cwd = std::env::current_dir().expect("get cwd");
    // WriteFileTool captures the cwd at construction time.
    std::env::set_current_dir(tmp.path()).expect("chdir to tempdir");
    let registry = ToolRegistry::new(&Config::default()).expect("build tool registry");

    let call = ToolCallRequest {
        id: "call_1".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "write_file".to_string(),
            arguments: r#"{"path":"sub.txt","content":"hello from subagent"}"#.to_string(),
        },
    };
    let (provider, seen) = mock_with_queue(vec![
        response("Writing the file.", FinishReason::ToolCalls, Some(vec![call])),
        response("File written; task complete.", FinishReason::Stop, None),
    ]);

    let summary =
        run_subagent("Write sub.txt", provider, &registry, 30).await.expect("subagent runs");
    std::env::set_current_dir(&original_cwd).expect("restore cwd");

    // The registry tool actually executed inside the subagent.
    assert_eq!(summary, "File written; task complete.");
    let content = std::fs::read_to_string(tmp.path().join("sub.txt")).expect("file written");
    assert_eq!(content, "hello from subagent");

    // Turn 1 sent the prompt; turn 2 sent prompt + assistant (with tool
    // calls) + tool result — proving the backfill.
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].len(), 1);
    assert_eq!(calls[1].len(), 3);
    assert!(matches!(calls[1][1].role, Role::Assistant));
    let sent = calls[1][1].tool_calls.as_ref().expect("tool calls sent");
    assert_eq!(sent[0].function.name, "write_file");
    assert!(matches!(calls[1][2].role, Role::Tool));
    assert!(calls[1][2].content.contains("Wrote"), "tool result: {}", calls[1][2].content);
    assert_eq!(calls[1][2].tool_call_id.as_deref(), Some("call_1"));
}

#[tokio::test]
async fn test_subagent_hits_turn_budget_without_summary() {
    // The model keeps requesting (empty) tool calls; the loop must stop at
    // the turn budget and fall back to "(no summary)".
    let (provider, seen) = mock_with_queue(vec![
        response("", FinishReason::ToolCalls, Some(vec![])),
        response("", FinishReason::ToolCalls, Some(vec![])),
    ]);
    let registry = ToolRegistry::new(&Config::default()).unwrap();

    let summary = run_subagent("Loop forever", provider, &registry, 2).await.unwrap();
    assert_eq!(summary, "(no summary)");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_task_tool_executes_subagent_via_block_on() {
    // TaskTool::execute is synchronous; it must block on the async
    // subagent loop through the current runtime handle.
    let (provider, _seen) =
        mock_with_queue(vec![response("Summary from subagent.", FinishReason::Stop, None)]);
    let registry = Arc::new(ToolRegistry::new(&Config::default()).unwrap());
    let tool = TaskTool { provider, registry };

    let result =
        tool.execute(&serde_json::json!({ "prompt": "investigate", "max_turns": 5 })).unwrap();
    assert!(result.success);
    assert_eq!(result.output, "Summary from subagent.");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_task_tool_requires_prompt_argument() {
    let (provider, _seen) = mock_with_queue(vec![]);
    let registry = Arc::new(ToolRegistry::new(&Config::default()).unwrap());
    let tool = TaskTool { provider, registry };

    assert!(tool.execute(&serde_json::json!({})).is_err());
}

#[test]
fn test_task_tool_requires_runtime_context() {
    // No tokio runtime on this thread: the synchronous execute must fail
    // cleanly instead of panicking.
    let (provider, _seen) = mock_with_queue(vec![]);
    let registry = Arc::new(ToolRegistry::new(&Config::default()).unwrap());
    let tool = TaskTool { provider, registry };

    let err = tool.execute(&serde_json::json!({ "prompt": "x" })).unwrap_err();
    assert!(err.to_string().contains("runtime"), "error: {err}");
}

#[test]
fn test_task_tool_parameters_schema() {
    let (provider, _seen) = mock_with_queue(vec![]);
    let registry = Arc::new(ToolRegistry::new(&Config::default()).unwrap());
    let tool = TaskTool { provider, registry };

    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert_eq!(params["required"][0], "prompt");
    assert!(params["properties"]["prompt"]["type"].is_string());
    assert!(params["properties"]["max_turns"]["type"].is_string());
}
