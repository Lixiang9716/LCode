//! Unit tests for the agent module — executor loop, conversation memory,
//! and task planner.
//!
//! Migrated verbatim from the `#[cfg(test)]` code in `src/agent/`: these
//! tests exercise only the crate's public API from outside the crate.

use lcode::agent::{
    AgentEvent, AgentRuntime, BackgroundManager, ConversationMemory, CronScheduler, Executor,
    PlanStatus, PlanStep, Planner, StepStatus, TodoManager,
};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{
    ChatMessage, FinishReason, FunctionCall, LlmResponse, Role, ToolCallRequest, Usage,
};
use lcode::tools::ToolRegistry;
use serial_test::serial;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// Executor: the agent loop

/// Build a `write_file` tool call with the given id and arguments.
fn write_file_call(id: &str, args: &str) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall { name: "write_file".to_string(), arguments: args.to_string() },
    }
}

fn response(
    content: &str,
    finish_reason: FinishReason,
    tool_calls: Option<Vec<ToolCallRequest>>,
) -> LlmResponse {
    LlmResponse { content: content.to_string(), tool_calls, usage: Usage::default(), finish_reason }
}

/// Build an executor backed by a mock provider that serves responses
/// from a queue. Every received message batch is recorded into `seen`.
/// Returns the executor, the LLM call counter, and the event stream
/// subscription for asserting the published agent events.
fn executor_with_queue(
    responses: Vec<LlmResponse>,
    registry: ToolRegistry,
    seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
) -> (Executor, Arc<AtomicUsize>, tokio::sync::broadcast::Receiver<AgentEvent>) {
    let queue: Arc<Mutex<VecDeque<LlmResponse>>> = Arc::new(Mutex::new(VecDeque::from(responses)));
    let call_count = Arc::new(AtomicUsize::new(0));

    let mut mock = MockLlmProvider::new();
    let queue_clone = queue.clone();
    let seen_clone = seen.clone();
    let count_clone = call_count.clone();
    mock.expect_chat().returning(move |messages, _tools| {
        count_clone.fetch_add(1, Ordering::SeqCst);
        seen_clone.lock().unwrap().push(messages.to_vec());
        let resp =
            queue_clone.lock().unwrap().pop_front().expect("mock provider ran out of responses");
        Ok(resp)
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, events_rx, _commands_tx) = AgentRuntime::new();
    let tmp = tempfile::tempdir().expect("tempdir for cron scheduler");
    let cron = Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf())));
    (
        Executor::new(
            Box::new(mock),
            registry,
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
            },
        ),
        call_count,
        events_rx,
    )
}

fn default_registry_in(dir: &std::path::Path) -> ToolRegistry {
    // WriteFileTool captures the current directory at construction time.
    std::env::set_current_dir(dir).expect("chdir to tempdir");
    ToolRegistry::new(&Config::default()).expect("build tool registry")
}

/// Collect published events until the session ends (`TaskFinished` /
/// `TaskAborted`) or the channel closes, with a timeout guard so a
/// missing terminal event fails the test instead of hanging forever.
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

#[tokio::test]
async fn test_run_completes_on_stop_and_records_assistant_message() {
    let seen: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
    let (mut executor, call_count, events_rx) = executor_with_queue(
        vec![response("Final answer.", FinishReason::Stop, None)],
        ToolRegistry::new(&Config::default()).unwrap(),
        seen.clone(),
    );

    let memory = ConversationMemory::new("You are a helpful assistant.".to_string());
    let planner = Planner::new(50);
    let memory = executor
        .run("Write a test", &planner, memory, 10, false)
        .await
        .expect("run should succeed");

    // Exactly one LLM call, receiving system + user context.
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
    {
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].len(), 2);
        assert!(matches!(recorded[0][0].role, Role::System));
        assert!(
            recorded[0][0].content.contains("You are a helpful assistant."),
            "Role section missing from assembled prompt"
        );
        assert!(matches!(recorded[0][1].role, Role::User));
        assert!(recorded[0][1].content.contains("Write a test"));
    } // guard dropped before any await

    // The final assistant message must have been added to memory.
    let msgs = memory.messages();
    assert!(msgs.iter().any(|m| matches!(m.role, Role::Assistant)));
    assert!(msgs.iter().any(|m| matches!(m.role, Role::Assistant) && m.content == "Final answer."));

    // Event-driven: the session must publish the expected event sequence.
    let events = collect_events(events_rx).await;
    assert!(matches!(&events[0], AgentEvent::SessionStarted { .. }));
    assert!(matches!(&events[1], AgentEvent::TurnStarted { turn: 1 }));
    assert!(matches!(
        &events[2],
        AgentEvent::TextGenerated { content } if content == "Final answer."
    ));
    assert!(matches!(&events[3], AgentEvent::TurnFinished { turn: 1 }));
    assert!(matches!(&events[4], AgentEvent::TaskFinished { turns: 1, .. }));
}

#[tokio::test]
#[serial]
async fn test_tool_call_executes_write_file_in_tempdir() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let original_cwd = std::env::current_dir().expect("get cwd");
    let registry = default_registry_in(tmp.path());

    let seen: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
    let (mut executor, _call_count, _events_rx) = executor_with_queue(
        vec![
            response(
                "I will write the file.",
                FinishReason::ToolCalls,
                Some(vec![write_file_call("call_1", r#"{"path":"test.txt","content":"hello"}"#)]),
            ),
            response("File written.", FinishReason::Stop, None),
        ],
        registry,
        seen.clone(),
    );

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let result = executor.run("Write a file", &planner, memory, 10, false).await;
    // Restore cwd before any assertion/panic so other tests are unaffected.
    std::env::set_current_dir(&original_cwd).expect("restore cwd");
    let memory = result.expect("run should succeed");

    // The write_file tool actually created the file in the tempdir.
    let content = std::fs::read_to_string(tmp.path().join("test.txt"))
        .expect("test.txt should exist after tool call");
    assert_eq!(content, "hello");

    // Turn 1 sent [system, user]; turn 2 sent [system, user, assistant
    // (with tool calls), tool (result)] — proving the assistant message
    // with tool calls and the tool result were recorded in memory.
    let recorded = seen.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].len(), 2);
    assert_eq!(recorded[1].len(), 4);
    assert!(matches!(recorded[1][2].role, Role::Assistant));
    let sent_calls = recorded[1][2].tool_calls.as_ref().expect("tool calls sent");
    assert_eq!(sent_calls[0].function.name, "write_file");
    assert!(matches!(recorded[1][3].role, Role::Tool));
    assert!(recorded[1][3].content.contains("Wrote"));
    assert!(recorded[1][3].tool_call_id.as_deref().is_some_and(|id| id == "call_1"));
    drop(recorded);

    // Final memory: assistant stop message present, tool result present.
    let msgs = memory.messages();
    assert!(msgs.iter().any(|m| matches!(m.role, Role::Assistant) && m.content == "File written."));
    assert!(msgs.iter().any(|m| matches!(m.role, Role::Tool)));
}

#[tokio::test]
#[serial]
async fn test_max_turns_truncates_never_finishing_loop() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let original_cwd = std::env::current_dir().expect("get cwd");
    let registry = default_registry_in(tmp.path());

    let seen: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
    let (mut executor, call_count, events_rx) = executor_with_queue(
        vec![
            response(
                "",
                FinishReason::ToolCalls,
                Some(vec![write_file_call("c1", r#"{"path":"a.txt","content":"1"}"#)]),
            ),
            response(
                "",
                FinishReason::ToolCalls,
                Some(vec![write_file_call("c2", r#"{"path":"b.txt","content":"2"}"#)]),
            ),
            response(
                "",
                FinishReason::ToolCalls,
                Some(vec![write_file_call("c3", r#"{"path":"c.txt","content":"3"}"#)]),
            ),
        ],
        registry,
        seen.clone(),
    );

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let result = executor.run("loop", &planner, memory, 3, false).await;
    std::env::set_current_dir(&original_cwd).expect("restore cwd");
    result.expect("run should stop gracefully at max_turns");

    // The loop must stop after exactly max_turns LLM calls.
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
    assert_eq!(seen.lock().unwrap().len(), 3);

    // Max-turns aborts the session via the event bus.
    let events = collect_events(events_rx).await;
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::TaskAborted { .. })),
        "expected TaskAborted event at max turns, got: {:?}",
        events
    );
}

// ConversationMemory: message management and compaction

#[test]
fn test_memory_add_messages() {
    let mut mem = ConversationMemory::new("You are a helpful assistant.".into());
    mem.add_user("Hello");
    mem.add_assistant("Hi there!");

    let ctx = mem.get_context();
    assert_eq!(ctx.len(), 3); // system + user + assistant
    assert_eq!(ctx[0].content, "You are a helpful assistant.");
    assert_eq!(ctx[1].content, "Hello");
    assert_eq!(ctx[2].content, "Hi there!");
}

#[test]
fn test_token_approximation() {
    let mut mem = ConversationMemory::new("Hello world".into());
    mem.add_user("This is a test message");
    // ~3 tokens for system + ~5 tokens for message = ~8 tokens
    assert!(mem.approximate_tokens() > 0);
}

#[test]
fn test_add_assistant_with_tool_calls() {
    let mut mem = ConversationMemory::new("sys".into());
    let tc = ToolCallRequest {
        id: "call_1".into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        },
    };
    mem.add_assistant_with_tool_calls("Writing...", vec![tc]);

    let last = mem.messages().last().unwrap();
    assert_eq!(last.role, Role::Assistant);
    assert_eq!(last.content, "Writing...");
    assert!(last.tool_call_id.is_none());
    let calls = last.tool_calls.as_ref().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].function.name, "write_file");

    // The message must be part of the context sent to the LLM.
    let ctx = mem.get_context();
    assert_eq!(ctx.len(), 2);
    assert_eq!(ctx[1].tool_calls.as_ref().unwrap().len(), 1);
}

#[test]
fn test_compact_if_needed_trims_oldest_messages() {
    let mut mem = ConversationMemory::new("system prompt".into());
    for i in 0..20 {
        mem.add_user(format!("user message number {}", i));
    }
    assert_eq!(mem.messages().len(), 20);

    // Force compaction regardless of the token budget.
    mem.compact_if_needed(0);

    // 20 messages -> trimmed down to half (10), oldest removed first.
    assert_eq!(mem.messages().len(), 10);
    assert!(
        mem.messages()[0].content.contains("10"),
        "oldest remaining message is the 11th user message"
    );
    assert!(mem.messages()[9].content.contains("19"));
    assert!(mem.messages().iter().all(|m| m.role == Role::User));
}

#[test]
fn test_compact_if_needed_noop_within_budget() {
    let mut mem = ConversationMemory::new("sys".into());
    mem.add_user("hi");
    mem.compact_if_needed(10_000);
    assert_eq!(mem.messages().len(), 1);
}

#[test]
fn test_compact_if_needed_keeps_small_conversations() {
    let mut mem = ConversationMemory::new("sys".into());
    for i in 0..4 {
        mem.add_user(format!("m{}", i));
    }
    // The compaction loop requires len > 4, so small conversations survive.
    mem.compact_if_needed(0);
    assert_eq!(mem.messages().len(), 4);
}

// Planner: plan creation, progress, and dependency ordering

#[test]
fn test_simple_plan() {
    let planner = Planner::new(50);
    let plan = planner.create_plan("Fix the login bug");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].number, 1);
    assert_eq!(plan.status, PlanStatus::Draft);
}

#[test]
fn test_progress() {
    let planner = Planner::new(50);
    let mut plan = planner.create_plan("Test task");
    assert_eq!(planner.progress(&plan), 0.0);

    plan.steps[0].status = StepStatus::Completed;
    assert_eq!(planner.progress(&plan), 100.0);
}

#[test]
fn test_next_step_respects_dependency_order() {
    let planner = Planner::new(50);
    let mut plan = planner.create_plan("task");
    plan.steps = vec![
        PlanStep {
            number: 1,
            description: "step 1".into(),
            depends_on: vec![],
            status: StepStatus::Pending,
        },
        PlanStep {
            number: 2,
            description: "step 2".into(),
            depends_on: vec![1],
            status: StepStatus::Pending,
        },
        PlanStep {
            number: 3,
            description: "step 3".into(),
            depends_on: vec![2],
            status: StepStatus::Pending,
        },
    ];

    // Step 1 has no dependencies, so it comes first.
    assert_eq!(planner.next_step(&plan).unwrap().number, 1);

    plan.steps[0].status = StepStatus::Completed;
    // Step 2's dependency is now satisfied.
    assert_eq!(planner.next_step(&plan).unwrap().number, 2);

    plan.steps[1].status = StepStatus::Completed;
    assert_eq!(planner.next_step(&plan).unwrap().number, 3);

    plan.steps[2].status = StepStatus::Completed;
    assert!(planner.next_step(&plan).is_none());
}

#[test]
fn test_next_step_skips_pending_dependencies() {
    let planner = Planner::new(50);
    let mut plan = planner.create_plan("task");
    plan.steps = vec![
        PlanStep {
            number: 1,
            description: "s1".into(),
            depends_on: vec![],
            status: StepStatus::Completed,
        },
        PlanStep {
            number: 2,
            description: "s2".into(),
            depends_on: vec![1],
            status: StepStatus::Pending,
        },
        PlanStep {
            number: 3,
            description: "s3".into(),
            depends_on: vec![2],
            status: StepStatus::Pending,
        },
    ];

    // Step 3 depends on step 2 which is still pending, so step 2 is next.
    assert_eq!(planner.next_step(&plan).unwrap().number, 2);

    plan.steps[1].status = StepStatus::Completed;
    assert_eq!(planner.next_step(&plan).unwrap().number, 3);
}

#[test]
fn test_next_step_none_when_all_blocked() {
    let planner = Planner::new(50);
    let mut plan = planner.create_plan("task");
    plan.steps = vec![
        PlanStep {
            number: 1,
            description: "s1".into(),
            depends_on: vec![],
            status: StepStatus::InProgress,
        },
        PlanStep {
            number: 2,
            description: "s2".into(),
            depends_on: vec![1],
            status: StepStatus::Pending,
        },
    ];

    // Step 1 is in progress (not pending) and step 2 is blocked on it.
    assert!(planner.next_step(&plan).is_none());
}

#[test]
fn test_next_step_missing_dependency_treated_as_met() {
    let planner = Planner::new(50);
    let mut plan = planner.create_plan("task");
    plan.steps = vec![PlanStep {
        number: 1,
        description: "s1".into(),
        depends_on: vec![99], // no such step
        status: StepStatus::Pending,
    }];

    assert_eq!(planner.next_step(&plan).unwrap().number, 1);
}

#[test]
fn test_progress_with_failures_and_empty_plan() {
    let planner = Planner::new(50);
    // Empty plans report 100% to avoid a divide-by-zero.
    let mut plan = planner.create_plan("x");
    plan.steps.clear();
    assert_eq!(planner.progress(&plan), 100.0);

    let mut plan = planner.create_plan("x");
    plan.steps[0].status = StepStatus::Failed("boom".into());
    assert_eq!(planner.progress(&plan), 0.0);

    plan.steps[0].status = StepStatus::Skipped;
    assert_eq!(planner.progress(&plan), 100.0);
}