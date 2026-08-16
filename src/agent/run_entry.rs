//! Session entry points: resume-from-checkpoint (P1) plus the
//! checkpoint sink attachment.
//!
//! Kept in a separate file so `mod.rs` stays under the 500-line style
//! limit.

use crate::agent::{
    run_task_with_memory, Checkpoint, CheckpointStore, ConversationMemory, RunState, TaskOutcome,
};
use crate::config::Config;

/// Resume an interrupted session from a checkpoint (P1): the
/// conversation, turn counter, usage total and budget-warning state
/// continue exactly where the run stopped.
pub async fn run_task_resume(
    checkpoint: Checkpoint,
    max_turns: u32,
    auto_approve: bool,
    stream: bool,
    config: &Config,
) -> anyhow::Result<TaskOutcome> {
    let store = CheckpointStore::new(&std::env::current_dir()?);
    if !store.matches_workspace(&checkpoint) {
        anyhow::bail!(
            "checkpoint belongs to a different workspace ({}); refusing to resume",
            checkpoint.workspace
        );
    }
    let memory =
        ConversationMemory::from_messages(config.agent.system_prompt.clone(), checkpoint.messages);
    let state = RunState {
        turns_used: checkpoint.turns_used,
        usage: checkpoint.usage,
        budget_warned: checkpoint.budget_warned,
    };
    // The CLI's --max-turns wins over the config file (standard
    // precedence); the remaining budget is computed inside the loop.
    run_task_with_memory(
        &checkpoint.task,
        max_turns,
        auto_approve,
        stream,
        config,
        Some(memory),
        Some(state),
    )
    .await
}
/// Publish the session-level UsageSummary event (lead agent).
pub(crate) fn publish_usage_summary(executor: &crate::agent::Executor, config: &Config) {
    let usage = executor.last_usage.clone();
    executor.runtime.publish(crate::agent::AgentEvent::UsageSummary {
        agent: "lead".to_string(),
        model: config.llm.model.clone(),
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        cache_hit_tokens: usage.cache_hit_tokens,
        cache_miss_tokens: usage.cache_miss_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cost_usd: crate::llm::estimate_cost(&config.llm.model, &usage),
    });
}

/// P0 budget status line: spent vs cap when a cap is configured.
pub(crate) fn print_budget_status(config: &Config, usage: &crate::llm::Usage) {
    let Some(budget) = config.llm.budget_total_usd else {
        return;
    };
    let spent = crate::llm::estimate_cost(&config.llm.model, usage);
    let remaining = (budget - spent).max(0.0);
    println!(
        "💰 Budget: {} / {} spent ({} remaining)",
        crate::llm::format_cost(spent),
        crate::llm::format_cost(budget),
        crate::llm::format_cost(remaining)
    );
}
