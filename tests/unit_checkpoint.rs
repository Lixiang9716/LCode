//! P1 checkpoint tests: store roundtrip, resume-state seeding,
//! budget continuity and the workspace guard.

use lcode::agent::{
    AgentRuntime, BackgroundManager, Checkpoint, CheckpointSink, CheckpointStore,
    ConversationMemory, CronScheduler, HookRegistry, McpRegistry, Planner, RunState, TodoManager,
};
use lcode::config::{Config, RuntimeTuning};
use lcode::llm::{FinishReason, LlmResponse, Usage};
use lcode::tools::ToolRegistry;
use std::sync::{Arc, Mutex};

fn session() -> lcode::agent::SessionState {
    lcode::agent::SessionState {
        todo: Arc::new(Mutex::new(TodoManager::default())),
        background: Arc::new(BackgroundManager::default()),
        hooks: Arc::new(HookRegistry::default()),
        cron: Arc::new(Mutex::new(CronScheduler::new(&std::path::PathBuf::from(".")))),
        mcp: Arc::new(Mutex::new(McpRegistry::default())),
        compact_request: Arc::new(Mutex::new(None)),
        memory_store: None,
        team_bus: None,
        tuning: Some(Arc::new(RuntimeTuning::from_config(&Config::default()))),
        internal_provider: None,
        web_search: None,
    }
}

fn usage(prompt: u32) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: 0,
        total_tokens: prompt,
        cache_miss_tokens: prompt,
        ..Usage::default()
    }
}

// --- store ---

#[test]
fn checkpoint_roundtrip_and_clear() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = CheckpointStore::new(dir.path());
    assert!(store.load().is_none(), "no checkpoint initially");

    let checkpoint = Checkpoint {
        task: "probe".to_string(),
        messages: vec![lcode::llm::ChatMessage::user("hello")],
        turns_used: 7,
        usage: usage(1000),
        budget_warned: true,
        saved_at: 42,
        workspace: dir.path().display().to_string(),
    };
    store.write(&checkpoint).unwrap();

    let loaded = store.load().expect("checkpoint loads");
    assert_eq!(loaded.turns_used, 7);
    assert_eq!(loaded.usage.prompt_tokens, 1000);
    assert!(loaded.budget_warned);
    assert_eq!(loaded.messages.len(), 1);

    // Atomic write leaves no temp file.
    let leftovers = std::fs::read_dir(dir.path().join(".sessions"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .count();
    assert_eq!(leftovers, 0);

    store.clear();
    assert!(store.load().is_none());
}

#[test]
fn sink_due_cadence() {
    let dir = tempfile::TempDir::new().unwrap();
    let sink = CheckpointSink::new(CheckpointStore::new(dir.path()), "t", dir.path(), 5);
    assert!(!sink.due(0));
    assert!(sink.due(5));
    assert!(sink.due(10));
    assert!(!sink.due(7));
    let off = CheckpointSink::new(CheckpointStore::new(dir.path()), "t", dir.path(), 0);
    assert!(!off.due(5), "cadence 0 disables checkpointing");
}

#[test]
fn workspace_mismatch_guard() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = CheckpointStore::new(dir.path());
    let foreign = Checkpoint {
        task: "x".to_string(),
        messages: vec![],
        turns_used: 0,
        usage: Usage::default(),
        budget_warned: false,
        saved_at: 0,
        workspace: "/somewhere/else".to_string(),
    };
    assert!(!store.matches_workspace(&foreign));
}

// --- resume seeding ---

#[tokio::test]
async fn resumed_run_continues_turn_counter_and_writes_checkpoint() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = CheckpointStore::new(dir.path());

    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_, _| {
        Ok(LlmResponse {
            content: "done".to_string(),
            tool_calls: None,
            server_results: Vec::new(),
            usage: usage(100),
            finish_reason: FinishReason::Stop,
        })
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, mut events_rx, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        session(),
    );
    executor.seed(RunState { turns_used: 5, usage: usage(500), budget_warned: false });
    executor.set_checkpoint_sink(CheckpointSink::new(store.clone(), "t", dir.path(), 1));

    executor
        .run("t", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes");

    // The checkpoint written at turn 6 records the seeded + new totals.
    let checkpoint = store.load().expect("checkpoint written at cadence 1");
    assert_eq!(checkpoint.turns_used, 6, "5 seeded + 1 new");
    assert_eq!(checkpoint.usage.prompt_tokens, 600, "500 seeded + 100 new");

    // TaskFinished reports the resumed total.
    let mut turns = 0;
    while let Ok(event) = events_rx.try_recv() {
        if let lcode::agent::AgentEvent::TaskFinished { turns: t, .. } = event {
            turns = t;
        }
    }
    assert_eq!(turns, 6);
}

#[tokio::test]
async fn seeded_usage_counts_toward_budget() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_, _| {
        Ok(LlmResponse {
            content: "done".to_string(),
            tool_calls: None,
            server_results: Vec::new(),
            usage: usage(100_000), // $0.014 on top of the seeded spend
            finish_reason: FinishReason::Stop,
        })
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let mut config = Config::default();
    config.llm.model = "deepseek-v4-flash".to_string();
    config.llm.budget_total_usd = Some(0.15);
    let tuning = Arc::new(RuntimeTuning::from_config(&config));

    let (runtime, mut events_rx, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        lcode::agent::SessionState { tuning: Some(tuning), ..session() },
    );
    // Seeded spend ~$0.14; the first new turn ($0.014) pushes over the $0.15 cap.
    executor.seed(RunState { turns_used: 3, usage: usage(1_000_000), budget_warned: false });

    executor
        .run("t", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes (aborted by budget)");

    let mut saw_budget = false;
    while let Ok(event) = events_rx.try_recv() {
        if matches!(event, lcode::agent::AgentEvent::BudgetExceeded { .. }) {
            saw_budget = true;
        }
    }
    assert!(saw_budget, "seeded spend must count toward the budget cap");
}
