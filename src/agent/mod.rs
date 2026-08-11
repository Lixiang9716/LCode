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
//!
//! Session capabilities (learn-claude-code parity):
//! - [`todo`] — model-owned plan + nag reminders (s03)
//! - [`skill`] — two-layer skill loading (s05)
//! - [`compaction`] — three-level context compression (s06)
//! - [`subagent`] — context-isolated subtask delegation (s04)
//! - [`background`] — non-blocking background commands (s08)
//! - [`task`] — persistent disk-backed task board (s07)
//! - [`team`] — multi-agent teams, protocols, autonomy (s09-s11)
//! - [`worktree`] — git worktree task isolation (s12)

use crate::config::Config;
use crate::llm::LlmProvider;
use crate::tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

mod background;
mod compaction;
mod event;
mod executor;
mod memory;
mod planner;
mod render;
mod runtime;
mod skill;
mod subagent;
mod task;
mod team;
mod todo;
mod worktree;

pub use background::{
    BackgroundCheckTool, BackgroundManager, BackgroundRunTool, BackgroundStatus, BackgroundTask,
};
pub use compaction::{
    auto_compact, estimate_tokens, micro_compact, CompactTool, AUTO_COMPACT_THRESHOLD, KEEP_RECENT,
    PRESERVE_RESULT_TOOLS,
};
pub use event::{AgentCommand, AgentEvent};
pub use executor::Executor;
pub use memory::ConversationMemory;
pub use planner::{Plan, PlanStatus, PlanStep, Planner, StepStatus};
pub use render::render_event;
pub use runtime::{AgentRuntime, ApprovalDecision};
pub use skill::{LoadSkillTool, Skill, SkillRegistry};
pub use subagent::{run_subagent, TaskTool};
pub use task::{Task, TaskCreateTool, TaskListTool, TaskManager, TaskStatus, TaskUpdateTool};
pub use team::{
    MessageBus, TeamMessage, Teammate, TeammateManager, TeammateState, VALID_MSG_TYPES,
};
pub use todo::{TodoItem, TodoManager, TodoStatus, TodoUpdateTool};
pub use worktree::{EventLog, WorktreeManager};

/// Run a single-shot agent task.
///
/// Creates a fresh agent session bound to a new runtime, spawns the
/// default stdout renderer (which also answers approval prompts via
/// stdin), registers the session tool set (todo/skill/task/background/
/// team/worktree), and executes the task turn by turn until completion
/// or `max_turns` is reached.
pub async fn run_task(
    task: &str,
    max_turns: u32,
    auto_approve: bool,
    config: &Config,
) -> anyhow::Result<()> {
    // Build the LLM provider
    let provider = build_provider(config)?;

    // Build the tool registry with built-in tools
    let mut registry = ToolRegistry::new(config)?;

    // Create the runtime with event bus + command channel
    let (runtime, events_rx, commands_tx) = AgentRuntime::new();

    // Create session-scoped state and register session tools
    let workspace = std::env::current_dir()?;
    let todo = Arc::new(Mutex::new(TodoManager::default()));
    todo::register(&mut registry, todo.clone());
    skill::register(&mut registry, workspace.join("skills"));
    // The synchronous `compact` tool gets its own provider instance (as
    // Arc) built from the same config; the executor owns the other one.
    compaction::register(&mut registry, Arc::from(build_provider(config)?), workspace.clone());
    let background = Arc::new(BackgroundManager::new(config)?.with_events(runtime.events_sender()));
    background::register(&mut registry, background.clone());
    task::register(&mut registry, &workspace);
    team::register(&mut registry, &workspace);
    worktree::register(&mut registry, &workspace);

    // Subagent (s04): children run with a fresh registry holding only the
    // base tools (CHILD_TOOLS parity — no `task` re-delegation, no session
    // state) and their own provider instance.
    let subagent_registry = Arc::new(ToolRegistry::new(config)?);
    subagent::register(&mut registry, Arc::from(build_provider(config)?), subagent_registry);

    // Spawn the default renderer (stdout + stdin approval prompts)
    let renderer = render::spawn_renderer(events_rx, commands_tx);

    // Create agent components
    let memory = ConversationMemory::new(config.agent.system_prompt.clone());
    let planner = Planner::new(config.agent.max_turns);
    let mut executor = Executor::new(provider, registry, auto_approve, runtime, todo, background);

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

/// Resolve the workspace root for session state (skills dir, task board,
/// worktrees). Placeholder until a real workspace resolver exists.
pub fn workspace_root() -> anyhow::Result<PathBuf> {
    std::env::current_dir().map_err(Into::into)
}
