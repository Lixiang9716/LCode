//! P0 quality-loop tests: the test-until-green reminder and the
//! self-review pass (opt-in).

use lcode::agent::{
    AgentRuntime, BackgroundManager, ConversationMemory, CronScheduler, HookRegistry, McpRegistry,
    Planner, TodoManager,
};
use lcode::config::{Config, RuntimeTuning};
use lcode::llm::{FinishReason, FunctionCall, LlmResponse, ToolCallRequest, Usage};
use lcode::tools::ToolRegistry;
use std::sync::{Arc, Mutex};

fn tuning_with(mutate: impl Fn(&mut Config)) -> Arc<RuntimeTuning> {
    let mut config = Config::default();
    mutate(&mut config);
    Arc::new(RuntimeTuning::from_config(&config))
}

fn session(tuning: Arc<RuntimeTuning>) -> lcode::agent::SessionState {
    lcode::agent::SessionState {
        todo: Arc::new(Mutex::new(TodoManager::default())),
        background: Arc::new(BackgroundManager::default()),
        hooks: Arc::new(HookRegistry::default()),
        cron: Arc::new(Mutex::new(CronScheduler::new(&std::path::PathBuf::from(".")))),
        mcp: Arc::new(Mutex::new(McpRegistry::default())),
        compact_request: Arc::new(Mutex::new(None)),
        memory_store: None,
        team_bus: None,
        tuning: Some(tuning),
        internal_provider: None,
        web_search: None,
    }
}

fn shell_call(command: &str) -> ToolCallRequest {
    ToolCallRequest {
        id: "shell-1".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "shell".to_string(),
            arguments: serde_json::json!({ "command": command }).to_string(),
        },
    }
}

fn stop_response() -> LlmResponse {
    LlmResponse {
        content: "done".to_string(),
        tool_calls: None,
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }
}

fn tool_response(call: ToolCallRequest) -> LlmResponse {
    LlmResponse {
        content: String::new(),
        tool_calls: Some(vec![call]),
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason: FinishReason::ToolCalls,
    }
}

fn text_response(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls: None,
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }
}

// --- test-until-green ---

#[tokio::test]
async fn failed_test_run_injects_fix_reminder() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(tool_response(shell_call("cargo test --help > /dev/null; exit 1")))
        } else {
            Ok(stop_response())
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _events, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        session(tuning_with(|_| {})),
    );

    // Root the shell tool in the tempdir so the command runs there.
    let _guard = std::env::set_current_dir(tmp.path());
    let memory = executor
        .run(
            "run the tests",
            &Planner::new(10),
            ConversationMemory::new("sys".to_string()),
            10,
            false,
        )
        .await
        .expect("run completes");
    drop(_guard);

    assert!(
        memory.messages().iter().any(|m| m.content.contains("The last test run failed")),
        "fix reminder injected: {:?}",
        memory.messages().iter().map(|m| &m.content[..m.content.len().min(60)]).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn passing_test_run_injects_no_reminder() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(tool_response(shell_call("echo all good; exit 0")))
        } else {
            Ok(stop_response())
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _events, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        session(tuning_with(|_| {})),
    );
    let memory = executor
        .run("no tests", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes");
    assert!(!memory.messages().iter().any(|m| m.content.contains("test run failed")));
}

// --- self-review ---

#[tokio::test]
async fn self_review_issues_restart_the_loop() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(3).returning(move |_, _| {
        turns += 1;
        match turns {
            1 => Ok(stop_response()),
            2 => Ok(text_response("ISSUES: the answer is wrong, fix it")),
            _ => Ok(stop_response()),
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, mut events_rx, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        session(tuning_with(|c| c.agent.self_review = true)),
    );
    let memory = executor
        .run("task", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes");

    assert!(
        memory.messages().iter().any(|m| m.content.contains("Self-review found issues")),
        "issues injected into the conversation"
    );
    let mut turns_total = 0;
    while let Ok(event) = events_rx.try_recv() {
        if let lcode::agent::AgentEvent::TaskFinished { turns, .. } = event {
            turns_total = turns;
        }
    }
    assert_eq!(turns_total, 2, "loop restarted once (1 + 1)");
}

#[tokio::test]
async fn self_review_approve_finishes_without_restart() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(2).returning(move |_, _| {
        turns += 1;
        if turns == 1 {
            Ok(stop_response())
        } else {
            Ok(text_response("APPROVE"))
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _events, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        session(tuning_with(|c| c.agent.self_review = true)),
    );
    let memory = executor
        .run("task", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes");
    assert!(!memory.messages().iter().any(|m| m.content.contains("Self-review found issues")));
}

#[tokio::test]
async fn self_review_off_by_default_makes_no_review_call() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_, _| Ok(stop_response()));
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, _events, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        session(tuning_with(|_| {})),
    );
    executor
        .run("task", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes with a single chat call");
}
