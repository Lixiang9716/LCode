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
mod recorder;
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
pub use recorder::spawn_event_recorder;
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
    register as register_team_tools, MessageBus, TeamMessage, TeamTool, TeamToolKind, Teammate,
    TeammateManager, TeammateState, VALID_MSG_TYPES,
};
pub use teammate::{
    handle_teammate_message, reinject_identity, run_teammate_loop, TeammateEnv, TeammateTools,
};
pub use todo::{TodoItem, TodoManager, TodoStatus, TodoUpdateTool};
pub use worktree::{register as register_worktree_tools, EventLog, WorktreeManager};

/// Run a single-shot agent task.
///
/// Thin wrapper over [`run_task_with_memory`] with a fresh, empty
/// conversation memory.
pub async fn run_task(
    task: &str,
    max_turns: u32,
    auto_approve: bool,
    stream: bool,
    config: &Config,
) -> anyhow::Result<()> {
    run_task_with_memory(task, max_turns, auto_approve, stream, config, None).await
}

/// Build a full agent session: provider, registry with all session
/// tools, hooks, runtime, and the shared session-scoped state.
#[allow(clippy::type_complexity)]
fn build_session(
    config: &Config,
    provider: Arc<dyn LlmProvider>,
) -> anyhow::Result<(
    ToolRegistry,
    Arc<HookRegistry>,
    AgentRuntime,
    tokio::sync::broadcast::Receiver<AgentEvent>,
    tokio::sync::mpsc::Sender<AgentCommand>,
    std::path::PathBuf,
    Arc<Mutex<Option<String>>>,
    Arc<Mutex<CronScheduler>>,
    Arc<Mutex<McpRegistry>>,
    Arc<BackgroundManager>,
    Arc<crate::agent::MemoryStore>,
    Arc<crate::agent::MessageBus>,
)> {
    let mut registry = ToolRegistry::new(config)?;

    let mut hooks_registry = HookRegistry::default();
    register_default_hooks(&mut hooks_registry);
    let hooks = Arc::new(hooks_registry);

    let (runtime, events_rx, commands_tx) = AgentRuntime::new();

    let workspace = std::env::current_dir()?;
    let todo = Arc::new(Mutex::new(TodoManager::default()));
    todo::register(&mut registry, todo.clone(), Some(runtime.events_sender()));

    let skills_dir = config.agent.skills_dir.clone().unwrap_or_else(|| workspace.join("skills"));
    skill::register(&mut registry, skills_dir.clone(), Some(runtime.events_sender()));

    let compact_request = Arc::new(Mutex::new(None));
    compaction::register(&mut registry, compact_request.clone());

    let background = Arc::new(BackgroundManager::new(config)?.with_events(runtime.events_sender()));
    background::register(&mut registry, background.clone());
    task::register(&mut registry, &workspace, Some(runtime.events_sender()));

    // INTEGRATION POINT: teammate replies land in
    // `{workspace}/.team/inbox/lead.jsonl`; the executor's turn-start
    // should drain it via `MessageBus::drain_lead_inbox`.
    team::register(&mut registry, &workspace, provider.clone(), Some(runtime.events_sender()));
    worktree::register(&mut registry, &workspace, Some(runtime.events_sender()));

    let cron = Arc::new(Mutex::new(CronScheduler::new(&workspace)));
    cron::register(&mut registry, cron.clone());
    let mcp_registry = Arc::new(Mutex::new(McpRegistry::default()));
    mcp::register(&mut registry, mcp_registry.clone());
    let _ = memory_store::register(&mut registry, &workspace, provider.clone());
    let memory_store = Arc::new(crate::agent::MemoryStore::new(&workspace)?);
    let team_bus = Arc::new(crate::agent::MessageBus::new(&workspace));

    Ok((
        registry,
        hooks,
        runtime,
        events_rx,
        commands_tx,
        workspace,
        compact_request,
        cron,
        mcp_registry,
        background,
        memory_store,
        team_bus,
    ))
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
    stream: bool,
    config: &Config,
    initial_memory: Option<ConversationMemory>,
) -> anyhow::Result<()> {
    // Build the provider, tool registry, hooks, runtime and all session
    // tools. The provider powers the main loop, the compaction tool and
    // the teammate loops; the event bus keeps every session component
    // observable.
    let provider: Arc<dyn LlmProvider> = Arc::from(build_provider(config)?);
    let (
        mut registry,
        hooks,
        runtime,
        events_rx,
        commands_tx,
        workspace,
        compact_request,
        cron,
        mcp_registry,
        background,
        memory_store,
        team_bus,
    ) = build_session(config, provider.clone())?;

    // G3 (s09): memory tools are registered inside build_session; the
    // executor injects the index into the prompt (initialize_session)
    // and persists memories at session end (persist_memories).

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
        Some(runtime.events_sender()),
    );

    // Run the session: renderer + memory assembly + executor loop.
    execute_session(
        config,
        task,
        max_turns,
        stream,
        auto_approve,
        initial_memory,
        registry,
        hooks,
        runtime,
        events_rx,
        commands_tx,
        workspace,
        compact_request,
        cron,
        mcp_registry,
        background,
        memory_store,
        team_bus,
    )
    .await
}

/// Execute a fully assembled session: spawn the renderer, build the
/// conversation memory (skills layer-1 or restored snapshot), construct
/// the executor with the session state, and run the loop.
#[allow(clippy::too_many_arguments)]
async fn execute_session(
    config: &Config,
    task: &str,
    max_turns: u32,
    stream: bool,
    auto_approve: bool,
    initial_memory: Option<ConversationMemory>,
    registry: ToolRegistry,
    hooks: Arc<HookRegistry>,
    runtime: AgentRuntime,
    events_rx: tokio::sync::broadcast::Receiver<AgentEvent>,
    commands_tx: tokio::sync::mpsc::Sender<AgentCommand>,
    workspace: std::path::PathBuf,
    compact_request: Arc<Mutex<Option<String>>>,
    cron: Arc<Mutex<CronScheduler>>,
    mcp_registry: Arc<Mutex<McpRegistry>>,
    background: Arc<BackgroundManager>,
    memory_store: Arc<crate::agent::MemoryStore>,
    team_bus: Arc<crate::agent::MessageBus>,
) -> anyhow::Result<()> {
    // Audit trail: every event (turns, tool calls, subagents, background
    // tasks, team messages, ...) lands in `.transcripts/events_{ts}.jsonl`
    // with a timestamp, in arrival order. Fire-and-forget; the task ends
    // when the event bus closes. Subscribed before the renderer takes
    // `events_rx`, so no event published after this point is missed.
    let recorder = spawn_event_recorder(events_rx.resubscribe(), &workspace);
    let renderer = render::spawn_renderer(events_rx, commands_tx);

    // G9 (s07): skill layer-1 descriptions join the base system prompt;
    // `resume` restores an existing conversation instead.
    let mut skill_registry = SkillRegistry::default();
    if let Err(e) = skill_registry.load_from(&workspace.join("skills")) {
        tracing::debug!(error = %e, "skills directory unavailable");
    }
    let system_prompt = with_layer1(&config.agent.system_prompt, &skill_registry);
    let memory = match initial_memory {
        Some(memory) => memory,
        None => ConversationMemory::new(system_prompt),
    };

    let planner = Planner::new(config.agent.max_turns);
    let session = crate::agent::executor::SessionState {
        todo: Arc::new(Mutex::new(TodoManager::default())),
        background,
        hooks,
        cron,
        mcp: mcp_registry,
        compact_request,
        memory_store: Some(memory_store),
        team_bus: Some(team_bus),
    };
    let mut executor =
        Executor::new(build_provider(config)?, registry, auto_approve, runtime, session);
    executor.run(task, &planner, memory, max_turns, stream).await?;

    // The executor owns the event-bus sender (via its runtime). Drop it
    // before awaiting the renderer, or the renderer never observes the
    // channel close and the process hangs after the task completes.
    drop(executor);

    let _ = renderer.await;
    let _ = recorder.await;
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
