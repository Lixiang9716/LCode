//! Agent module — the core of LCode.
//!
//! The agent is **event-driven**: a session publishes every observable
//! step ([`AgentEvent`]) on the runtime's event bus, and control flows
//! back through [`AgentCommand`] messages (tool approvals, abort).
//! Observers (REPL, logging, tests, UIs) subscribe without coupling to
//! the loop's internals.
//!
//! Architecture:
//! 1. [`AgentRuntime`] owns the event bus (broadcast) and command channel
//! 2. [`Executor`] runs the loop, publishing events and awaiting approvals
//! 3. [`render`] provides the default stdout renderer used by single-shot
//!    tasks; other subscribers can observe the same stream

use crate::config::Config;
use crate::llm::LlmProvider;
use crate::tools::ToolRegistry;

mod event;
mod executor;
mod memory;
mod planner;
mod render;
mod runtime;

pub use event::{AgentCommand, AgentEvent};
pub use executor::Executor;
pub use memory::ConversationMemory;
pub use planner::{Plan, PlanStatus, PlanStep, Planner, StepStatus};
pub use render::render_event;
pub use runtime::{AgentRuntime, ApprovalDecision};

/// Run a single-shot agent task.
///
/// Creates a fresh agent session bound to a new runtime, spawns the
/// default stdout renderer (which also answers approval prompts via
/// stdin), and executes the task turn by turn until completion or
/// `max_turns` is reached.
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

    // Create the runtime with event bus + command channel
    let (runtime, events_rx, commands_tx) = AgentRuntime::new();

    // Spawn the default renderer (stdout + stdin approval prompts)
    let renderer = render::spawn_renderer(events_rx, commands_tx);

    // Create agent components
    let memory = ConversationMemory::new(config.agent.system_prompt.clone());
    let planner = Planner::new(config.agent.max_turns);
    let mut executor = Executor::new(provider, registry, auto_approve, runtime);

    // Start the task
    tracing::info!("Starting task: {}", task);
    executor.run(task, &planner, memory, max_turns).await?;

    // Wait for the renderer to drain the remaining events
    let _ = renderer.await;

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
