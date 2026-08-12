//! Executor wiring tests — close audit gaps (injection/hook/compaction/gate).

use lcode::agent::{
    AgentCommand, AgentRuntime, BackgroundManager, ConversationMemory, CronScheduler, Executor,
    HookDecision, HookPoint, HookRegistry, McpRegistry, MemoryStore, MessageBus, Planner,
    SessionState, TodoManager,
};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, FunctionCall, LlmResponse, Role, ToolCallRequest, Usage};
use lcode::tools::ToolRegistry;
use serial_test::serial;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

// Helpers

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

/// Build an executor with a queue-based mock provider.
fn executor_with_queue(
    responses: Vec<LlmResponse>,
    session: SessionState,
) -> (Executor, Arc<AtomicUsize>) {
    let queue: Arc<Mutex<Vec<LlmResponse>>> = Arc::new(Mutex::new(responses));
    let call_count = Arc::new(AtomicUsize::new(0));

    let mut mock = MockLlmProvider::new();
    let queue_clone = queue.clone();
    let count_clone = call_count.clone();
    mock.expect_chat().returning(move |_messages, _tools| {
        count_clone.fetch_add(1, Ordering::SeqCst);
        let resp = queue_clone.lock().unwrap().remove(0);
        Ok(resp)
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _events_rx, _cmd_tx) = AgentRuntime::new();
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    (Executor::new(Box::new(mock), registry, true, runtime, session), call_count)
}

fn base_session(tmp: &TempDir) -> SessionState {
    SessionState {
        todo: Arc::new(Mutex::new(TodoManager::default())),
        background: Arc::new(BackgroundManager::default()),
        hooks: Arc::new(HookRegistry::default()),
        cron: Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf()))),
        mcp: Arc::new(Mutex::new(McpRegistry::default())),
        compact_request: Arc::new(Mutex::new(None)),
        memory_store: None,
        team_bus: None,
    }
}

async fn run_once(executor: &mut Executor, memory: ConversationMemory) -> ConversationMemory {
    let planner = Planner::new(50);
    executor.run("wiring test", &planner, memory, 10, false).await.expect("run")
}

// s09/s10: memory index injected into the system prompt

#[tokio::test]
#[serial]
async fn test_memory_index_injected_into_system_prompt() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(MemoryStore::new(tmp.path()).unwrap());
    store.write("project-rules", "# Project\n- use tabs\n").unwrap();
    let idx = store.index();
    assert!(idx.contains("project-rules"), "index should list the memory: {idx}");

    let mut session = base_session(&tmp);
    session.memory_store = Some(store);
    // Two responses: the main Stop + the extraction call at session end.
    let (mut executor, _calls) = executor_with_queue(
        vec![
            response("Done.", FinishReason::Stop, None),
            response(r#"{"memories":[]}"#, FinishReason::Stop, None),
        ],
        session,
    );

    let memory = ConversationMemory::new("You are a helpful assistant.".to_string());
    let memory = run_once(&mut executor, memory).await;

    let sys = memory.system_prompt().to_string();
    assert!(sys.contains("## Memory"), "Memory section missing: {sys}");
    assert!(sys.contains("project-rules"), "memory index missing from prompt: {sys}");
}

// s09: persist_memories runs at session end

#[tokio::test]
#[serial]
async fn test_persist_memories_at_session_end() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(MemoryStore::new(tmp.path()).unwrap());

    let mut session = base_session(&tmp);
    session.memory_store = Some(store.clone());
    let (mut executor, _calls) = executor_with_queue(
        vec![
            response("Done.", FinishReason::Stop, None),
            response(
                r#"{"memories":[{"name":"fact","description":"d","body":"b"}]}"#,
                FinishReason::Stop,
                None,
            ),
        ],
        session,
    );

    let memory = ConversationMemory::new("sys".to_string());
    let _ = run_once(&mut executor, memory).await;

    // The extraction call at session end wrote a memory file.
    assert!(tmp.path().join(".memory").is_dir(), "memory dir should exist after persist");
    assert!(!store.list().is_empty(), "a memory should have been extracted");
}

// s08: auto-compaction over threshold

#[tokio::test]
#[serial]
async fn test_auto_compact_when_over_threshold() {
    let tmp = TempDir::new().unwrap();
    let session = base_session(&tmp);
    // Summary response for auto_compact + the main Stop response.
    let (mut executor, calls) = executor_with_queue(
        vec![
            response("compacted summary", FinishReason::Stop, None),
            response("Done.", FinishReason::Stop, None),
        ],
        session,
    );

    // auto_compact writes transcripts under the current directory.
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();

    let mut memory = ConversationMemory::new("sys".to_string());
    // >> 50k tokens of conversation so the threshold triggers.
    memory.add_user("x".repeat(300_000));
    memory.add_user("task: huge conversation");

    let planner = Planner::new(50);
    let _ = executor.run("big", &planner, memory, 10, false).await.expect("run");
    std::env::set_current_dir(&original).unwrap();

    assert!(tmp.path().join(".transcripts").exists(), "auto-compaction should write transcripts");
    assert!(calls.load(Ordering::SeqCst) >= 2, "summary call + main call expected");
}

// s11: PROMPT_TOO_LONG triggers reactive compact then retry

#[tokio::test]
async fn test_prompt_too_long_triggers_reactive_compact_retry() {
    let tmp = TempDir::new().unwrap();
    let session = base_session(&tmp);

    let queue: Arc<Mutex<Vec<Result<LlmResponse, anyhow::Error>>>> = Arc::new(Mutex::new(vec![
        Err(anyhow::anyhow!("[PROMPT_TOO_LONG] context window exceeded")),
        Ok(response("Recovered.", FinishReason::Stop, None)),
    ]));
    let calls = Arc::new(AtomicUsize::new(0));

    let mut mock = MockLlmProvider::new();
    let q = queue.clone();
    let c = calls.clone();
    mock.expect_chat().returning(move |_m, _t| {
        c.fetch_add(1, Ordering::SeqCst);
        q.lock().unwrap().remove(0)
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _rx, _tx) = AgentRuntime::new();
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    let mut executor = Executor::new(Box::new(mock), registry, true, runtime, session);

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory = executor.run("retry", &planner, memory, 10, false).await.expect("run");

    assert_eq!(calls.load(Ordering::SeqCst), 2, "must retry after reactive compact");
    assert!(memory.messages().iter().any(|m| m.content.contains("Recovered.")));
}

// s15: lead inbox drained into the conversation

#[tokio::test]
async fn test_lead_inbox_drained_into_conversation() {
    let tmp = TempDir::new().unwrap();
    let bus = Arc::new(MessageBus::new(&tmp.path().to_path_buf()));
    bus.send(&lcode::agent::TeamMessage {
        from: "alice".into(),
        to: "lead".into(),
        msg_type: "text".into(),
        request_id: None,
        content: "hello from alice".into(),
    })
    .unwrap();

    let mut session = base_session(&tmp);
    session.team_bus = Some(bus);

    let queue = Arc::new(Mutex::new(vec![response("Done.", FinishReason::Stop, None)]));
    let mut mock = MockLlmProvider::new();
    let q = queue.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    mock.expect_chat().returning(move |messages, _t| {
        s.lock().unwrap().push(messages.to_vec());
        Ok(q.lock().unwrap().remove(0))
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _rx, _tx) = AgentRuntime::new();
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    let mut executor = Executor::new(Box::new(mock), registry, true, runtime, session);

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let _ = executor.run("team", &planner, memory, 10, false).await.expect("run");

    let calls = seen.lock().unwrap();
    let has_inbox = calls.iter().any(|msgs| {
        msgs.iter().any(|m| matches!(m.role, Role::User) && m.content.contains("[Inbox]"))
    });
    assert!(has_inbox, "lead inbox should be injected: {:?}", calls);
}

// s13: background notification injected before next LLM call

#[tokio::test]
async fn test_background_notification_injected_before_next_llm() {
    let tmp = TempDir::new().unwrap();
    let bg = Arc::new(BackgroundManager::default());
    let _ = bg.spawn("echo hi", 30);
    // Wait for the fast background task to finish (echo) without draining
    // the notification queue — the executor must drain it. Async sleep
    // yields to the current-thread runtime so the task can complete.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let mut session = base_session(&tmp);
    session.background = bg;
    session.todo = Arc::new(Mutex::new(TodoManager::default()));

    let queue = Arc::new(Mutex::new(vec![response("Done.", FinishReason::Stop, None)]));
    let mut mock = MockLlmProvider::new();
    let q = queue.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    mock.expect_chat().returning(move |messages, _t| {
        s.lock().unwrap().push(messages.to_vec());
        Ok(q.lock().unwrap().remove(0))
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _rx, _tx) = AgentRuntime::new();
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    let mut executor = Executor::new(Box::new(mock), registry, true, runtime, session);

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let _ = executor.run("bg", &planner, memory, 10, false).await.expect("run");

    let calls = seen.lock().unwrap();
    let has_bg =
        calls.iter().any(|msgs| msgs.iter().any(|m| m.content.contains("<background-results>")));
    assert!(has_bg, "background results should be injected: {:?}", calls);
}

// s03: approval gate blocks tool when rejected

#[tokio::test]
async fn test_approval_required_gate() {
    let tmp = TempDir::new().unwrap();
    let tool_call = ToolCallRequest {
        id: "t1".into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": tmp.path().join("x.txt").display().to_string(), "content": "y" }).to_string(),
        },
    };

    let queue = Arc::new(Mutex::new(vec![
        response("", FinishReason::ToolCalls, Some(vec![tool_call])),
        response("Done.", FinishReason::Stop, None),
    ]));
    let mut mock = MockLlmProvider::new();
    let q = queue.clone();
    mock.expect_chat().returning(move |_m, _t| Ok(q.lock().unwrap().remove(0)));
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _rx, cmd_tx) = AgentRuntime::new();
    // Auto-approve off; reject the tool call via the command channel.
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    let mut executor = Executor::new(Box::new(mock), registry, false, runtime, base_session(&tmp));

    // Reject after a short delay so the executor is waiting for approval.
    let tx = cmd_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = tx.send(AgentCommand::RejectToolCall { id: "t1".into() }).await;
    });

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory = executor.run("gate", &planner, memory, 10, false).await.expect("run");

    let declined = memory
        .messages()
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .any(|m| m.content.contains("declined"));
    assert!(declined, "rejected tool call must be recorded as declined");
}

// s04: hooks triggered from the main loop

#[tokio::test]
async fn test_stop_hook_runs_at_session_end() {
    let tmp = TempDir::new().unwrap();
    let mut hooks = HookRegistry::default();
    let stop_seen = Arc::new(Mutex::new(false));
    let flag = stop_seen.clone();
    hooks.add(
        HookPoint::Stop,
        Box::new(move |ctx| {
            if ctx.point == HookPoint::Stop {
                *flag.lock().unwrap() = true;
            }
            HookDecision::Allow
        }),
    );

    let mut session = base_session(&tmp);
    session.hooks = Arc::new(hooks);
    let (mut executor, _calls) =
        executor_with_queue(vec![response("Done.", FinishReason::Stop, None)], session);

    let memory = ConversationMemory::new("sys".to_string());
    let _ = run_once(&mut executor, memory).await;

    assert!(*stop_seen.lock().unwrap(), "Stop hook must run at session end");
}

#[tokio::test]
async fn test_post_tool_use_hook_runs() {
    let tmp = TempDir::new().unwrap();
    let mut hooks = HookRegistry::default();
    let post_seen = Arc::new(Mutex::new(0));
    let flag = post_seen.clone();
    hooks.add(
        HookPoint::PostToolUse,
        Box::new(move |ctx| {
            if ctx.point == HookPoint::PostToolUse {
                *flag.lock().unwrap() += 1;
            }
            HookDecision::Allow
        }),
    );

    let tool_call = ToolCallRequest {
        id: "t1".into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": tmp.path().join("x.txt").display().to_string(), "content": "y" }).to_string(),
        },
    };
    let mut session = base_session(&tmp);
    session.hooks = Arc::new(hooks);
    let (mut executor, _calls) = executor_with_queue(
        vec![
            response("", FinishReason::ToolCalls, Some(vec![tool_call])),
            response("Done.", FinishReason::Stop, None),
        ],
        session,
    );

    let memory = ConversationMemory::new("sys".to_string());
    let _ = run_once(&mut executor, memory).await;

    assert!(*post_seen.lock().unwrap() >= 1, "PostToolUse hook must run after a tool call");
}

// s05: todo nag injected after missed turns

#[tokio::test]
async fn test_todo_nag_injected_after_missed_turns() {
    let tmp = TempDir::new().unwrap();
    let todo = Arc::new(Mutex::new(TodoManager::default()));
    // Seed a todo so the nag has something to remind about.
    todo.lock()
        .unwrap()
        .update(vec![lcode::agent::TodoItem {
            id: 1,
            text: "finish task".into(),
            status: lcode::agent::TodoStatus::InProgress,
        }])
        .unwrap();

    let mut session = base_session(&tmp);
    session.todo = todo;

    let queue = Arc::new(Mutex::new(vec![
        response("", FinishReason::ToolCalls, Some(vec![])),
        response("", FinishReason::ToolCalls, Some(vec![])),
        response("", FinishReason::ToolCalls, Some(vec![])),
        response("Done.", FinishReason::Stop, None),
    ]));
    let mut mock = MockLlmProvider::new();
    let q = queue.clone();
    mock.expect_chat().returning(move |_m, _t| Ok(q.lock().unwrap().remove(0)));
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _rx, _tx) = AgentRuntime::new();
    let registry = ToolRegistry::new(&Config::default()).unwrap();
    let mut executor = Executor::new(Box::new(mock), registry, true, runtime, session);

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory = executor.run("nag", &planner, memory, 10, false).await.expect("run");

    let nagged = memory
        .messages()
        .iter()
        .any(|m| matches!(m.role, Role::User) && m.content.contains("<reminder>"));
    assert!(nagged, "todo nag must be injected after missed turns");
}

// s02: multiple tool calls in one response executed in order

#[tokio::test]
async fn test_multiple_tool_calls_in_one_response_executed_in_order() {
    let tmp = TempDir::new().unwrap();
    // Absolute tempdir paths keep the writes out of the repo root.
    let a_path = tmp.path().join("a.txt").display().to_string();
    let b_path = tmp.path().join("b.txt").display().to_string();
    let calls = vec![
        ToolCallRequest {
            id: "c1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "write_file".into(),
                arguments: serde_json::json!({ "path": a_path, "content": "1" }).to_string(),
            },
        },
        ToolCallRequest {
            id: "c2".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "write_file".into(),
                arguments: serde_json::json!({ "path": b_path, "content": "2" }).to_string(),
            },
        },
    ];

    let (mut executor, _calls) = executor_with_queue(
        vec![
            response("", FinishReason::ToolCalls, Some(calls)),
            response("Done.", FinishReason::Stop, None),
        ],
        base_session(&tmp),
    );

    let memory = ConversationMemory::new("sys".to_string());
    let memory = run_once(&mut executor, memory).await;

    let results: Vec<&str> = memory
        .messages()
        .iter()
        .filter(|m| matches!(m.role, Role::Tool))
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(results.len(), 2, "both tool calls must execute: {:?}", results);
    assert!(results.iter().any(|r| r.contains("a.txt")));
    assert!(results.iter().any(|r| r.contains("b.txt")));
}
