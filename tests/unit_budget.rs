//! P0 budget-gate tests: hard cap abort, one-shot warning injection,
//! and the config wiring.

use lcode::agent::{
    AgentRuntime, BackgroundManager, ConversationMemory, CronScheduler, HookRegistry, McpRegistry,
    Planner, TodoManager,
};
use lcode::config::{Config, RuntimeTuning};
use lcode::llm::{FinishReason, LlmResponse, Usage};
use lcode::tools::ToolRegistry;
use std::sync::{Arc, Mutex};

fn session_state(tuning: Arc<RuntimeTuning>) -> lcode::agent::SessionState {
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

fn budget_config(total_usd: f64) -> Arc<RuntimeTuning> {
    let mut config = Config::default();
    config.llm.model = "deepseek-v4-flash".to_string();
    config.llm.budget_total_usd = Some(total_usd);
    config.llm.budget_warning_ratio = 0.5;
    Arc::new(RuntimeTuning::from_config(&config))
}

fn response(prompt_tokens: u32, tool_calls: bool) -> LlmResponse {
    let calls = if tool_calls {
        Some(vec![lcode::llm::ToolCallRequest {
            id: format!("c{prompt_tokens}"),
            call_type: "function".to_string(),
            function: lcode::llm::FunctionCall {
                name: "read_file".to_string(),
                arguments: serde_json::json!({ "path": "missing.txt" }).to_string(),
            },
        }])
    } else {
        None
    };
    LlmResponse {
        content: "working".to_string(),
        tool_calls: calls,
        server_results: Vec::new(),
        usage: Usage {
            prompt_tokens,
            completion_tokens: 0,
            total_tokens: prompt_tokens,
            cache_miss_tokens: prompt_tokens,
            ..Usage::default()
        },
        finish_reason: if tool_calls { FinishReason::ToolCalls } else { FinishReason::Stop },
    }
}

#[tokio::test]
async fn budget_cap_aborts_and_publishes_event() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_, _| Ok(response(1_000_000, false))); // $0.14
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let (runtime, mut events_rx, _commands) = AgentRuntime::new();
    let mut executor = lcode::agent::Executor::new(
        Box::new(mock),
        ToolRegistry::new(&Config::default()).unwrap(),
        true,
        runtime,
        session_state(budget_config(0.10)), // cap below the first turn's cost
    );

    let memory = executor
        .run("task", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes (aborted by budget)");

    let mut saw_budget = false;
    let mut saw_abort = false;
    while let Ok(event) = events_rx.try_recv() {
        match event {
            lcode::agent::AgentEvent::BudgetExceeded { spent_usd, budget_usd } => {
                assert!(spent_usd >= budget_usd);
                saw_budget = true;
            }
            lcode::agent::AgentEvent::TaskAborted { reason } => {
                assert!(reason.contains("budget"), "{reason}");
                saw_abort = true;
            }
            _ => {}
        }
    }
    assert!(saw_budget, "BudgetExceeded event published");
    assert!(saw_abort, "abort event with budget reason published");
    assert_eq!(memory.messages().len(), 1, "task message only — no assistant text consumed");
}

#[tokio::test]
async fn budget_warning_injected_once_before_cap() {
    let mut mock = lcode::llm::provider::MockLlmProvider::new();
    let mut turns = 0;
    mock.expect_chat().times(3).returning(move |_, _| {
        turns += 1;
        // $0.14, $0.014, $0.28 → warn at $0.15, cap at $0.30.
        // Tool calls keep the loop going until the cap aborts on turn 3.
        match turns {
            1 => Ok(response(1_000_000, true)), // $0.14
            2 => Ok(response(100_000, true)),   // $0.014 → warn at $0.15
            _ => Ok(response(2_000_000, true)), // $0.28 → cap at $0.30
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
        session_state(budget_config(0.30)),
    );

    let memory = executor
        .run("task", &Planner::new(10), ConversationMemory::new("sys".to_string()), 10, false)
        .await
        .expect("run completes");

    let reminders: Vec<&str> = memory
        .messages()
        .iter()
        .map(|m| m.content.as_str())
        .filter(|c| c.contains("Budget warning"))
        .collect();
    assert_eq!(reminders.len(), 1, "one-shot warning: {reminders:?}");

    let mut saw_budget = false;
    while let Ok(event) = events_rx.try_recv() {
        if matches!(event, lcode::agent::AgentEvent::BudgetExceeded { .. }) {
            saw_budget = true;
        }
    }
    assert!(saw_budget, "cap hit on the third turn");
}

#[test]
fn budget_config_defaults_and_merge() {
    let config = Config::default();
    assert_eq!(config.llm.budget_total_usd, None);
    assert_eq!(config.llm.budget_warning_ratio, 0.8);

    let mut base = Config::default();
    let mut other = Config::default();
    other.llm.budget_total_usd = Some(5.0);
    other.llm.budget_warning_ratio = 0.5;
    lcode::config::merge_config(&mut base, other);
    assert_eq!(base.llm.budget_total_usd, Some(5.0));
    assert_eq!(base.llm.budget_warning_ratio, 0.5);
}
