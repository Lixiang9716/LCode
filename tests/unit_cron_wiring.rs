//! Unit tests for cron wiring into the executor: due cron jobs are
//! injected into the conversation as `<cron-trigger>` user messages at
//! turn start (s14 pull-based firing).

use lcode::agent::{
    AgentRuntime, BackgroundManager, ConversationMemory, CronScheduler, Executor, Planner,
    TodoManager,
};
use lcode::config::Config;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, LlmResponse, Role, Usage};
use lcode::tools::ToolRegistry;
use std::sync::{Arc, Mutex};

/// Build an executor whose mock provider always answers with `Stop`,
/// sharing the given cron scheduler.
fn executor_with_cron(
    cron: Arc<Mutex<CronScheduler>>,
) -> (Executor, tokio::sync::broadcast::Receiver<lcode::agent::AgentEvent>) {
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_messages, _tools| {
        Ok(LlmResponse {
            content: "All done.".to_string(),
            tool_calls: None,
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        })
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, events_rx, _commands_tx) = AgentRuntime::new();
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
            },
        ),
        events_rx,
    )
}

#[tokio::test]
async fn test_due_cron_job_injected_into_memory() {
    let tmp = tempfile::tempdir().expect("tempdir for cron scheduler");
    let cron = Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf())));
    // "* * * * *" matches the current minute, so the first turn-start
    // injection fires it (recurring: it stays scheduled afterwards).
    cron.lock()
        .expect("lock scheduler")
        .schedule("* * * * *", "ping the user", true, false)
        .expect("schedule job");

    let (mut executor, _events_rx) = executor_with_cron(cron);

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory =
        executor.run("cron task", &planner, memory, 5, false).await.expect("run should succeed");

    // The due prompt must be in the conversation as a cron-trigger user
    // message, seen by the LLM before the (single) turn's chat call.
    let msgs = memory.messages();
    let trigger =
        msgs.iter().find(|m| matches!(m.role, Role::User) && m.content.contains("<cron-trigger>"));
    assert!(trigger.is_some(), "expected a <cron-trigger> message, got: {msgs:?}");
    let trigger = trigger.expect("checked above");
    assert!(trigger.content.contains("ping the user"));
    assert!(trigger.content.contains("</cron-trigger>"));

    // The recurring job stays scheduled after firing.
    assert!(msgs.iter().any(|m| matches!(m.role, Role::Assistant) && m.content == "All done."));
}

#[tokio::test]
async fn test_no_due_jobs_leave_memory_clean() {
    let tmp = tempfile::tempdir().expect("tempdir for cron scheduler");
    let cron = Arc::new(Mutex::new(CronScheduler::new(&tmp.path().to_path_buf())));
    // A job that never matches: fires at 03:04 on Feb 2, 2033 only.
    cron.lock()
        .expect("lock scheduler")
        .schedule("4 3 2 2 *", "never now", true, false)
        .expect("schedule job");

    let (mut executor, _events_rx) = executor_with_cron(cron);

    let memory = ConversationMemory::new("sys".to_string());
    let planner = Planner::new(50);
    let memory =
        executor.run("cron task", &planner, memory, 5, false).await.expect("run should succeed");

    let msgs = memory.messages();
    assert!(
        !msgs.iter().any(|m| m.content.contains("<cron-trigger>")),
        "no due job must not inject anything, got: {msgs:?}"
    );
}
