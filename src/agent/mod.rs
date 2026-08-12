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
//! - [`TeammateManager`] — multi-agent teams with real LLM loops, team
//!   protocols, and autonomy (s09-s17)
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
mod mcp_stdio;
mod memory;
mod memory_store;
mod planner;
mod prompt;
mod protocol;
mod render;
mod retry;
mod runtime;
mod session;
mod skill;
mod stream;
mod subagent;
mod task;
mod team;
mod teammate;
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
// MCP stdio helpers (G13), exported for integration tests.
pub use mcp_stdio::{parse_frame, split_command};
pub use memory::{exact_tokens, ConversationMemory};
pub use memory_store::{
    ExtractMemoriesTool, ListMemoriesTool, MemoryFile, MemoryStore, ReadMemoryTool,
    WriteMemoryTool, CONSOLIDATE_THRESHOLD,
};
pub use planner::{Plan, PlanStatus, PlanStep, Planner, StepStatus};
pub use prompt::PromptSection;
pub use protocol::{
    dispatch_message, parse_plan_verdict, plan_verdict_content, DispatchAction, ProtocolManager,
    ProtocolState, ProtocolStatus, RequestPlanTool, RequestShutdownTool, ResponseMatch,
    ReviewPlanTool, SubmitPlanTool,
};
pub use render::render_event;
pub use retry::{RetryPolicy, RetryProvider, PROMPT_TOO_LONG_MARKER};
pub use runtime::{AgentRuntime, ApprovalDecision};
pub use session::{snapshot, SessionSnapshot, SessionStore};
pub use skill::{with_layer1, LoadSkillTool, Skill, SkillRegistry};
pub use subagent::{run_subagent, run_subagents_parallel, TaskParallelTool, TaskTool};
pub use task::{
    Task, TaskClaimTool, TaskCreateTool, TaskListTool, TaskManager, TaskStatus, TaskUpdateTool,
};
pub use team::{
<<<<<<< HEAD
    MessageBus, TeamMessage, TeamTool, TeamToolKind, Teammate, TeammateManager, TeammateState,
    VALID_MSG_TYPES,
};
pub use teammate::{
    handle_teammate_message, reinject_identity, run_teammate_loop, TeammateEnv, TeammateTools,
=======
    register as register_team_tools, MessageBus, TeamMessage, Teammate, TeammateManager,
    TeammateState, VALID_MSG_TYPES,
>>>>>>> origin/task/cons-stream-mcp
};
pub use todo::{TodoItem, TodoManager, TodoStatus, TodoUpdateTool};
pub use worktree::{register as register_worktree_tools, EventLog, WorktreeManager};

/// Run a single-shot agent task.
///
<<<<<<< HEAD
/// Thin wrapper over [`run_task_with_memory`] with a fresh, empty
/// conversation memory.
=======
/// Creates a fresh agent session bound to a new runtime, spawns the
/// default stdout renderer (which also answers approval prompts via
/// stdin), registers the session tool set (todo/skill/task/background/
/// team/worktree), and executes the task turn by turn until completion
/// or `max_turns` is reached.
///
/// `stream` toggles real SSE streaming (G11): the provider's
/// `chat_stream` publishes token deltas as they arrive instead of one
/// plain `chat` response per turn. NOTE: this mirrors the executor's
/// `run(.., stream)` flag; the executor refactor (batch 1) keeps both
/// signatures in sync.
>>>>>>> origin/task/cons-stream-mcp
pub async fn run_task(
    task: &str,
    max_turns: u32,
    auto_approve: bool,
    stream: bool,
    config: &Config,
) -> anyhow::Result<()> {
    run_task_with_memory(task, max_turns, auto_approve, config, None).await
}

/// Run an agent task, optionally seeded with a restored conversation
/// memory (session resume).
///
/// Creates a fresh agent session bound to a new runtime, spawns the
/// default stdout renderer (which also answers approval prompts via
/// stdin), registers the session tool set (todo/skill/task/background/
/// team/worktree), and executes the task turn by turn until completion
/// or `max_turns` is reached. `initial_memory` (e.g. loaded from a
/// session snapshot) is used as-is; the executor injects the task
/// description into the conversation like any other run.
pub async fn run_task_with_memory(
    task: &str,
    max_turns: u32,
    auto_approve: bool,
    config: &Config,
    initial_memory: Option<ConversationMemory>,
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
<<<<<<< HEAD
    // Team (s09-s17): teammates run real LLM loops (s15) with team
    // protocols (s16) and autonomous task claiming (s17). The provider is
    // built from the same config so teammates share the retry/backoff
    // semantics of the main loop; the event bus keeps teammates observable.
    //
    // INTEGRATION POINT for the main agent / executor: teammate replies and
    // protocol responses land in `{workspace}/.team/inbox/lead.jsonl`. At
    // turn-start the executor should drain the lead's inbox and inject the
    // text into the conversation (`MessageBus::read_inbox("lead")` or the
    // ready-to-inject `MessageBus::drain_lead_inbox`). Executor wiring is
    // owned by the executor batch; the read + format side lives in team.rs.
    team::register(
        &mut registry,
        &workspace,
        Arc::from(build_provider(config)?),
        Some(runtime.events_sender()),
    );
    worktree::register(&mut registry, &workspace);
=======
    // Team / worktree tools attach the runtime event bus (G14) so
    // message sends, teammate state changes and worktree lifecycle
    // events are published to observers alongside the loop events.
    team::register(&mut registry, &workspace, Some(runtime.events_sender()));
    worktree::register(&mut registry, &workspace, Some(runtime.events_sender()));
>>>>>>> origin/task/cons-stream-mcp
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
    // state) and their own provider instance. The session hook registry
    // is shared so PreToolUse policies (s20/G12) also gate subagent tools.
    let subagent_registry = Arc::new(ToolRegistry::new(config)?);
    subagent::register(
        &mut registry,
        Arc::from(build_provider(config)?),
        subagent_registry,
        Some(hooks.clone()),
    );

    // Spawn the default renderer (stdout + stdin approval prompts)
    let renderer = render::spawn_renderer(events_rx, commands_tx);

    // Create agent components
    // G9 (s07): skill layer-1 descriptions join the base system prompt
    // (the executor's prompt assembly carries them from turn one);
    // `resume` restores an existing conversation instead.
    let mut skill_registry = SkillRegistry::default();
    let _ = skill_registry.load_from(&skills_dir);
    let system_prompt = with_layer1(&config.agent.system_prompt, &skill_registry);
    let memory = match initial_memory {
        Some(memory) => memory,
        None => ConversationMemory::new(system_prompt),
    };
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
    // `stream` flows from `lcode run --stream` (G11): the provider's
    // `chat_stream` (real SSE for openai/anthropic) publishes token
    // deltas as they arrive; `false` keeps the plain chat call (the
    // REPL default).
    executor.run(task, &planner, memory, max_turns, stream).await?;

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
    let retry = RetryProvider::with_fallback(
        inner,
        RetryPolicy::default(),
        config.llm.fallback_model.clone(),
    );
    // Seed the retry budget (and thus the inner provider's request body)
    // with the configured max_tokens instead of the hardcoded default.
    retry.set_max_tokens(config.llm.max_tokens);
    Ok(Box::new(retry))
}

/// Resolve the workspace root for session state (skills dir, task board,
/// worktrees). Placeholder until a real workspace resolver exists.
pub fn workspace_root() -> anyhow::Result<PathBuf> {
    std::env::current_dir().map_err(Into::into)
}
