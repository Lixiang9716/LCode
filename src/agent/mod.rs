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
//! 3. [`render_event`] renders the stream for stdout by default; other
//!    subscribers can observe the same stream
//!
//! Session capabilities (learn-claude-code parity):
//! - [`TodoManager`] — model-owned plan + nag reminders (s03)
//! - [`SkillRegistry`] — two-layer skill loading (s05)
//! - [`estimate_tokens`] — three-level context compression (s06)
//! - [`run_subagent`] — context-isolated subtask delegation (s04)
//! - [`BackgroundManager`] — non-blocking background commands (s08)
//! - [`TaskManager`] — persistent disk-backed task board (s07)
//! - [`TeammateManager`] — multi-agent teams, protocols, autonomy (s09-s11)
//! - [`WorktreeManager`] — git worktree task isolation (s12)

use crate::config::Config;
use crate::llm::LlmProvider;
use crate::tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

mod background;
mod compaction;
mod cron;
mod event;
mod executor;
mod executor_hooks;
mod hooks;
mod mcp;
mod memory;
mod memory_store;
mod planner;
mod prompt;
mod render;
mod retry;
mod runtime;
mod session;
mod skill;
mod stream;
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
pub use cron::{CancelCronTool, CronJob, CronScheduler, ListCronsTool, ScheduleCronTool};
pub use event::{AgentCommand, AgentEvent};
pub use executor::{Executor, SessionState};
pub use hooks::{
    deny_tool, register_default_hooks, HookContext, HookDecision, HookPoint, HookRegistry,
};
pub use mcp::{ConnectMcpTool, McpRegistry, McpServer};
pub use memory::{exact_tokens, ConversationMemory};
pub use memory_store::{
    ExtractMemoriesTool, ListMemoriesTool, MemoryFile, MemoryStore, ReadMemoryTool,
    WriteMemoryTool, CONSOLIDATE_THRESHOLD,
};
pub use planner::{Plan, PlanStatus, PlanStep, Planner, StepStatus};
pub use prompt::PromptSection;
pub use render::render_event;
pub use retry::{RetryPolicy, RetryProvider};
pub use runtime::{AgentRuntime, ApprovalDecision};
pub use session::{snapshot, SessionSnapshot, SessionStore};
pub use skill::{with_layer1, LoadSkillTool, Skill, SkillRegistry};
pub use subagent::{run_subagent, run_subagents_parallel, TaskParallelTool, TaskTool};
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

    // Session hooks (s20): permission policies run as PreToolUse hooks
    let mut hooks_registry = HookRegistry::default();
    register_default_hooks(&mut hooks_registry);
    let hooks = Arc::new(hooks_registry);

    // Create the runtime with event bus + command channel
    let (runtime, events_rx, commands_tx) = AgentRuntime::new();

    // Create session-scoped state and register session tools
    let workspace = std::env::current_dir()?;
    let todo = Arc::new(Mutex::new(TodoManager::default()));
    todo::register(&mut registry, todo.clone());
    // G9 (s07): the skills directory is configurable, defaulting to
    // `<workspace>/skills`.
    let skills_dir = config.agent.skills_dir.clone().unwrap_or_else(|| workspace.join("skills"));
    skill::register(&mut registry, skills_dir.clone());
    // The `compact` tool requests compaction through a channel; the
    // executor performs it on the live conversation at the next turn.
    let compact_request = Arc::new(Mutex::new(None));
    compaction::register(&mut registry, compact_request.clone());
    let background = Arc::new(BackgroundManager::new(config)?.with_events(runtime.events_sender()));
    background::register(&mut registry, background.clone());
    task::register(&mut registry, &workspace);
    team::register(&mut registry, &workspace);
    worktree::register(&mut registry, &workspace);
    // Cron (s14): one scheduler shared by the three cron tools and the
    // executor, so tools manage jobs while the loop fires due ones.
    let cron = Arc::new(Mutex::new(CronScheduler::new(&workspace)));
    cron::register(&mut registry, cron.clone());
    let mcp_registry = Arc::new(Mutex::new(McpRegistry::default()));
    mcp::register(&mut registry, mcp_registry.clone());

    // G3 (s09): cross-session memory. The store registers four tools;
    // executor injection (stop extraction + per-turn index injection) is
    // wired by the coordinator after the executor refactor lands (see
    // `memory_store` module docs for the two integration points).
    memory_store::register(&mut registry, &workspace, Arc::from(build_provider(config)?))?;

    // Subagent (s04): children run with a fresh registry holding only the
    // base tools (CHILD_TOOLS parity — no `task` re-delegation, no session
    // state) and their own provider instance.
    let subagent_registry = Arc::new(ToolRegistry::new(config)?);
    subagent::register(&mut registry, Arc::from(build_provider(config)?), subagent_registry);

    // Spawn the default renderer (stdout + stdin approval prompts)
    let renderer = render::spawn_renderer(events_rx, commands_tx);

    // Create agent components
    // G9 (s07): skill layer-1 — the catalog of one-line descriptions is
    // part of the base system prompt, so the executor's prompt assembly
    // naturally carries it from the first turn (`load_skill` still pulls
    // full bodies on demand).
    let mut skill_registry = SkillRegistry::default();
    let _ = skill_registry.load_from(&skills_dir);
    let system_prompt = with_layer1(&config.agent.system_prompt, &skill_registry);
    let memory = ConversationMemory::new(system_prompt);
    let planner = Planner::new(config.agent.max_turns);
    let session = crate::agent::executor::SessionState {
        todo,
        background,
        hooks,
        cron,
        mcp: mcp_registry,
        compact_request,
    };
    let mut executor = Executor::new(provider, registry, auto_approve, runtime, session);

    // Start the task
    tracing::info!("Starting task: {}", task);
    // `stream = false` keeps the plain chat call (default behavior);
    // streaming (typewriter) is a REPL enhancement the serve/session
    // layer can opt into via the executor's `run(.., stream)` flag.
    executor.run(task, &planner, memory, max_turns, false).await?;

    // Wait for the renderer to drain the remaining events
    let _ = renderer.await;

    Ok(())
}

/// Kind of LLM backend a provider alias resolves to.
enum ProviderKind {
    Anthropic,
    OpenAi,
}

/// Build the appropriate LLM provider from configuration.
///
/// Provider aliases (all map to the existing Anthropic/OpenAI-compatible
/// implementations, only the default endpoint differs):
/// - `openai` / `openai_compatible` — OpenAI API or any OpenAI-compatible endpoint
/// - `anthropic` / `claude` — Anthropic native endpoint
/// - `deepseek`, `kimi` — Anthropic-compatible third-party endpoints
/// - `minimax`, `glm` — OpenAI-compatible third-party endpoints
///
/// An explicit `llm.api_base` always wins over the alias's default
/// endpoint. The result is wrapped in a [`RetryProvider`] so every LLM
/// call gets retry/backoff and max_tokens-upgrade semantics (#4).
pub fn build_provider(config: &Config) -> anyhow::Result<Box<dyn LlmProvider>> {
    let provider = config.llm.provider.to_lowercase();
    let (kind, default_base) = match provider.as_str() {
        "openai" | "openai_compatible" => (ProviderKind::OpenAi, None),
        "anthropic" | "claude" => (ProviderKind::Anthropic, None),
        "deepseek" => (ProviderKind::Anthropic, Some("https://api.deepseek.com/anthropic")),
        "kimi" => (ProviderKind::Anthropic, Some("https://api.moonshot.cn/anthropic")),
        "minimax" => (ProviderKind::OpenAi, Some("https://api.minimaxi.com/v1")),
        "glm" => (ProviderKind::OpenAi, Some("https://open.bigmodel.cn/api/paas/v4")),
        other => anyhow::bail!(
            "Unknown LLM provider: {other}. Supported: openai, openai_compatible, \
             anthropic, claude, deepseek, kimi, minimax, glm"
        ),
    };

    // Explicit `llm.api_base` wins; otherwise fall back to the alias's
    // default endpoint.
    let api_base = config.llm.api_base.clone().or_else(|| default_base.map(str::to_string));
    let llm = crate::config::LlmConfig { api_base, ..config.llm.clone() };

    let inner: Box<dyn LlmProvider> = match kind {
        ProviderKind::Anthropic => Box::new(crate::llm::anthropic::AnthropicProvider::new(&llm)?),
        ProviderKind::OpenAi => Box::new(crate::llm::openai::OpenAiProvider::new(&llm)?),
    };
    Ok(Box::new(RetryProvider::new(inner, RetryPolicy::default())))
}

/// Resolve the workspace root for session state (skills dir, task board,
/// worktrees). Placeholder until a real workspace resolver exists.
pub fn workspace_root() -> anyhow::Result<PathBuf> {
    std::env::current_dir().map_err(Into::into)
}
