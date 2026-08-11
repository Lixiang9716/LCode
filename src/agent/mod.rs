//! Agent module — the core of LCode.
//!
//! The agent orchestrates the conversation loop:
//! 1. Send the user's task + conversation history to the LLM
//! 2. Parse the response (text or tool calls)
//! 3. Execute tool calls (with optional user approval)
//! 4. Feed tool results back to the LLM
//! 5. Repeat until the task is complete or max turns reached

use crate::config::Config;
use crate::llm::LlmProvider;
use crate::tools::ToolRegistry;

mod executor;
mod memory;
mod planner;

pub use executor::Executor;
pub use memory::ConversationMemory;
pub use planner::{Plan, PlanStatus, PlanStep, Planner, StepStatus};

/// Run a single-shot agent task.
///
/// This is the main entry point for non-interactive task execution.
/// It creates a fresh agent session, plans the task, and executes it
/// turn by turn until completion or max_turns is reached.
pub async fn run_task(
    task: &str,
    max_turns: u32,
    auto_approve: bool,
    config: &Config,
) -> anyhow::Result<()> {
    // Build the LLM provider
    let provider = build_provider(config)?;

    // Build the tool registry
    let registry = ToolRegistry::new(config)?;

    // Create agent components
    let memory = ConversationMemory::new(config.agent.system_prompt.clone());
    let planner = Planner::new(config.agent.max_turns);
    let mut executor = Executor::new(provider, registry, auto_approve);

    // Start the task
    tracing::info!("Starting task: {}", task);
    executor.run(task, &planner, memory, max_turns).await?;

    Ok(())
}

/// Build the appropriate LLM provider from configuration.
pub fn build_provider(config: &Config) -> anyhow::Result<Box<dyn LlmProvider>> {
    match config.llm.provider.to_lowercase().as_str() {
        "openai" | "openai_compatible" => {
            let provider = crate::llm::openai::OpenAiProvider::new(&config.llm)?;
            Ok(Box::new(provider))
        }
        "anthropic" | "claude" => {
            let provider = crate::llm::anthropic::AnthropicProvider::new(&config.llm)?;
            Ok(Box::new(provider))
        }
        other => anyhow::bail!(
            "Unknown LLM provider: {}. Supported: openai, anthropic, openai_compatible",
            other
        ),
    }
}
