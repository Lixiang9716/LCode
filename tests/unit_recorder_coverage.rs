//! End-to-end event-coverage test: drive a real executor session whose
//! mock LLM walks through every session tool, then assert the event
//! recorder's `.transcripts/events_*.jsonl` contains each event type.
//!
//! This is the runtime counterpart of the static publish audit: if a
//! future refactor drops a publish call, the recorder log misses the
//! event and this test fails.

use lcode::agent::{
    spawn_event_recorder, BackgroundManager, CronScheduler, Executor, HookRegistry, McpRegistry,
    MemoryStore, MessageBus, Planner, SessionState, TodoManager,
};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, FunctionCall, LlmResponse, ToolCallRequest, Usage};
use lcode::tools::ToolRegistry;
use serial_test::serial;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

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

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall { name: name.to_string(), arguments: args.to_string() },
    }
}

/// The scripted session: one tool call per turn, then a final answer.
/// Indices 4/12/13 serve the subagent, the compaction summary and the
/// session-end memory extraction respectively.
fn scripted_responses() -> VecDeque<LlmResponse> {
    VecDeque::from([
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call(
                "c1",
                "todo_update",
                serde_json::json!({ "items": [{ "text": "t", "status": "pending" }] }),
            )]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call("c2", "task_create", serde_json::json!({ "title": "t" }))]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call("c3", "load_skill", serde_json::json!({ "name": "demo" }))]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call("c4", "task", serde_json::json!({ "prompt": "hi" }))]),
        ),
        response("sub summary", FinishReason::Stop, None),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call(
                "c5",
                "background_run",
                serde_json::json!({ "command": "echo hello" }),
            )]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call(
                "c6",
                "worktree_create",
                serde_json::json!({ "name": "wt1", "task_id": 1 }),
            )]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call("c7", "worktree_remove", serde_json::json!({ "name": "wt1" }))]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call(
                "c8",
                "send_message",
                serde_json::json!({ "to": "lead", "content": "hi" }),
            )]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call(
                "c9",
                "task_update",
                serde_json::json!({ "id": 1, "status": "completed" }),
            )]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call("c10", "does_not_exist", serde_json::json!({}))]),
        ),
        response(
            "",
            FinishReason::ToolCalls,
            Some(vec![tool_call("c11", "compact", serde_json::json!({ "focus": "keep it" }))]),
        ),
        response("Summary.", FinishReason::Stop, None),
        response("All done.", FinishReason::Stop, None),
        response("[]", FinishReason::Stop, None),
    ])
}

/// Event variants the scripted session must produce in the audit log.
fn expected_events() -> Vec<&'static str> {
    vec![
        "SessionStarted",
        "TurnStarted",
        "TextGenerated",
        "ToolCallRequested",
        "ToolCallExecuted",
        "ToolCallFailed",
        "TurnFinished",
        "TaskFinished",
        "TodoUpdated",
        "TaskCreated",
        "TaskUpdated",
        "SkillLoaded",
        "SubagentSpawned",
        "SubagentCompleted",
        "BackgroundTaskStarted",
        "BackgroundTaskCompleted",
        "TeamMessageSent",
        "WorktreeCreated",
        "WorktreeRemoved",
        "ContextCompacted",
        "TodoNag",
    ]
}

// `TaskTool` uses `block_in_place` (sync Tool over async engine), which
// only exists on the multi-threaded runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn recorder_log_covers_every_session_event() {
    let tmp = TempDir::new().expect("tempdir");
    let workspace = tmp.path().to_path_buf();

    // Worktree tools need a git repository with at least one commit.
    let git = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&workspace)
        .status()
        .expect("git runs");
    assert!(git.success(), "git init must succeed (worktree tools need it)");
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(&workspace)
        .status()
        .expect("git runs");
    assert!(commit.success(), "empty commit must succeed (worktree tools need it)");

    // Skills live under `skills/` relative to the workspace.
    let skills_dir = workspace.join("skills");
    std::fs::create_dir_all(skills_dir.join("demo")).unwrap();
    std::fs::write(skills_dir.join("demo").join("SKILL.md"), "# demo\n\nDemo skill.\n").unwrap();

    // Two mock instances share one script queue: the executor needs a
    // `Box<dyn LlmProvider>` while team/subagent registrations need an
    // `Arc`; both drain the same turn sequence.
    let queue = Arc::new(Mutex::new(scripted_responses()));
    let q = queue.clone();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat()
        .times(0..)
        .returning(move |_, _| Ok(q.lock().unwrap().pop_front().expect("script ran out")));
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));
    let provider: Arc<dyn lcode::llm::LlmProvider> = Arc::new(mock);

    let q = queue.clone();
    let mut executor_mock = MockLlmProvider::new();
    executor_mock
        .expect_chat()
        .times(0..)
        .returning(move |_, _| Ok(q.lock().unwrap().pop_front().expect("script ran out")));
    executor_mock.expect_name().times(0..).return_const("mock".to_string());
    executor_mock.expect_validate().times(0..).returning(|| Ok(()));

    // Assemble the session with the same wiring as `build_session`.
    let (runtime, events_rx, _commands_tx) = lcode::agent::AgentRuntime::new();
    let mut registry = ToolRegistry::new(&Config::default()).expect("builtin tools");

    let todo = Arc::new(Mutex::new(TodoManager::default()));
    lcode::agent::register_todo_tools(&mut registry, todo.clone(), Some(runtime.events_sender()));
    lcode::agent::register_skill_tools(&mut registry, skills_dir, Some(runtime.events_sender()));
    lcode::agent::register_task_tools(&mut registry, &workspace, Some(runtime.events_sender()));

    let background = Arc::new(BackgroundManager::default().with_events(runtime.events_sender()));
    lcode::agent::register_background_tools(&mut registry, background.clone());

    lcode::agent::register_team_tools(
        &mut registry,
        &workspace,
        provider.clone(),
        Some(runtime.events_sender()),
        &Config::default().team,
    );
    lcode::agent::register_worktree_tools(&mut registry, &workspace, Some(runtime.events_sender()));

    let compact_request = Arc::new(Mutex::new(None));
    lcode::agent::register_compact_tool(&mut registry, compact_request.clone());

    let sub_registry = Arc::new(ToolRegistry::new(&Config::default()).expect("sub registry"));
    lcode::agent::register_subagent_tools(
        &mut registry,
        provider.clone(),
        sub_registry,
        None,
        Some(runtime.events_sender()),
        lcode::config::SubagentConfig::default(),
    );

    let session = SessionState {
        todo,
        background,
        hooks: Arc::new(HookRegistry::default()),
        cron: Arc::new(Mutex::new(CronScheduler::new(&workspace))),
        mcp: Arc::new(Mutex::new(McpRegistry::default())),
        compact_request,
        memory_store: Some(Arc::new(MemoryStore::new(&workspace).expect("memory store"))),
        team_bus: Some(Arc::new(MessageBus::new(&workspace))),
        tuning: None,
    };

    let recorder = spawn_event_recorder(events_rx.resubscribe(), &workspace);
    let mut executor = Executor::new(Box::new(executor_mock), registry, true, runtime, session);

    let memory = lcode::agent::ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let _memory = executor.run("cover all events", &planner, memory, 50, false).await.expect("run");

    // Give the detached background command a moment to finish inside the
    // session (its completion event must land while the recorder lives).
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    drop(executor);
    recorder.await.expect("recorder ends when the bus closes");

    // Collect every event type the recorder captured.
    let transcripts = workspace.join(".transcripts");
    let entries: Vec<PathBuf> = std::fs::read_dir(&transcripts)
        .expect("transcripts dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("events_"))
        .map(|e| e.path())
        .collect();
    assert!(!entries.is_empty(), "the session wrote an event log");

    let mut seen = Vec::new();
    for path in entries {
        let text = std::fs::read_to_string(&path).expect("read events");
        for line in text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).expect("JSON line");
            if let Some(event) = value["event"].as_object() {
                seen.extend(event.keys().cloned());
            }
        }
    }

    let expected = expected_events();
    let missing: Vec<&&str> = expected.iter().filter(|e| !seen.iter().any(|s| s == *e)).collect();
    assert!(missing.is_empty(), "audit log missed {missing:?}; recorded: {seen:?}");
}
