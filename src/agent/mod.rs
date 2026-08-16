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

mod assets_skill;
mod background;
mod checkpoint;
mod compaction;
mod cron;
mod event;
mod executor;
mod executor_hooks;
pub mod guardrails;
mod hooks;
mod mcp;
mod mcp_stdio;
mod memory;
mod memory_store;
mod memory_store_llm;
mod message_bus;
mod planner;
mod prompt;
mod protocol;
mod provider_build;
mod quality;
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
mod usage_tracking;
mod workspace;
mod worktree;

pub use assets_skill::{ensure_assets_skill, ASSETS_SKILL};
pub use background::{
    register as register_background_tools, BackgroundCheckTool, BackgroundManager,
    BackgroundRunTool, BackgroundStatus, BackgroundTask,
};
pub use checkpoint::{Checkpoint, CheckpointSink, CheckpointStore, RunState};
pub use compaction::{
    auto_compact, estimate_tokens, micro_compact, register as register_compact_tool, CompactTool,
    AUTO_COMPACT_THRESHOLD, KEEP_RECENT, PRESERVE_RESULT_TOOLS,
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
pub use provider_build::{build_internal_provider, build_provider, web_search_spec};
pub use recorder::spawn_event_recorder;
pub use render::render_event;
pub use retry::{RetryPolicy, RetryProvider, PROMPT_TOO_LONG_MARKER};
pub use runtime::{AgentRuntime, ApprovalDecision};
pub use session::{snapshot, SessionSnapshot, SessionStore};
pub use skill::{
    register as register_skill_tools, with_layer1, LoadSkillTool, Skill, SkillRegistry,
};
pub use subagent::{
    register as register_subagent_tools, run_subagent, run_subagents_parallel, TaskParallelTool,
    TaskTool,
};
pub use task::{
    register as register_task_tools, Task, TaskClaimTool, TaskCreateTool, TaskListTool,
    TaskManager, TaskStatus, TaskUpdateTool,
};
pub use team::{
    register as register_team_tools, MessageBus, TeamMessage, TeamTool, TeamToolKind, Teammate,
    TeammateManager, TeammateState, VALID_MSG_TYPES,
};
pub use teammate::{
    handle_teammate_message, reinject_identity, run_teammate_loop, TeammateEnv, TeammateTools,
};
pub use todo::{
    register as register_todo_tools, TodoItem, TodoManager, TodoStatus, TodoUpdateTool,
};
pub use worktree::{register as register_worktree_tools, EventLog, WorktreeManager};

/// Run a single-shot agent task.
///
/// Thin wrapper over [`run_task_with_memory`] with a fresh, empty
/// conversation memory.
/// Outcome of a session: did it finish normally or abort?
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TaskOutcome {
    /// The session ended because the model stopped (true) rather than
    /// being aborted by max turns / an interrupt (false).
    pub completed: bool,
    /// Turns consumed before the session ended.
    pub turns: u32,
}

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
) -> anyhow::Result<TaskOutcome> {
    run_task_with_memory(task, max_turns, auto_approve, stream, config, None, None).await
}

/// Resume an interrupted session from a checkpoint (P1): the
/// conversation, turn counter, usage total and budget-warning state
/// continue exactly where the run stopped.
pub async fn run_task_resume(
    checkpoint: Checkpoint,
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
    run_task_with_memory(
        &checkpoint.task,
        config.agent.max_turns,
        auto_approve,
        stream,
        config,
        Some(memory),
        Some(state),
    )
    .await
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
    // Shell guardrails: sensitive paths and denied hosts also gate the
    // shell tool, so the context guardrails are not bypassable.
    guardrails::register(&mut hooks_registry, config.tools.clone());
    let hooks = Arc::new(hooks_registry);

    let (runtime, events_rx, commands_tx) =
        AgentRuntime::with_capacity(config.events.channel_capacity, config.events.command_capacity);

    let workspace = std::env::current_dir()?;
    let todo = Arc::new(Mutex::new(TodoManager::with_max_items(config.todo.max_items)));
    todo::register(&mut registry, todo.clone(), Some(runtime.events_sender()));

    let skills_dir = config.agent.skills_dir.clone().unwrap_or_else(|| workspace.join("skills"));
    // Built-in skills ship as files too: materialize the assets skill
    // when missing (never overwriting user edits), then load the dir.
    assets_skill::ensure_assets_skill(&skills_dir);
    skill::register(&mut registry, skills_dir.clone(), Some(runtime.events_sender()));

    let compact_request = Arc::new(Mutex::new(None));
    compaction::register(&mut registry, compact_request.clone());

    let background = Arc::new(BackgroundManager::new(config)?.with_events(runtime.events_sender()));
    background::register(&mut registry, background.clone());
    task::register(&mut registry, &workspace, Some(runtime.events_sender()));

    // INTEGRATION POINT: teammate replies land in
    // `{workspace}/.team/inbox/lead.jsonl`; the executor's turn-start
    // should drain it via `MessageBus::drain_lead_inbox`.
    team::register(
        &mut registry,
        &workspace,
        provider.clone(),
        Some(runtime.events_sender()),
        &config.team,
    );
    worktree::register(&mut registry, &workspace, Some(runtime.events_sender()));

    let cron = Arc::new(Mutex::new(CronScheduler::new(&workspace)));
    cron::register(&mut registry, cron.clone());
    let mcp_registry = Arc::new(Mutex::new(McpRegistry::default()));
    mcp::register(&mut registry, mcp_registry.clone());
    let _ = memory_store::register(&mut registry, &workspace, provider.clone());
    let memory_store =
        Arc::new(crate::agent::MemoryStore::with_config(&workspace, &config.memory)?);
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
    resume_state: Option<RunState>,
) -> anyhow::Result<TaskOutcome> {
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
        config.subagent.clone(),
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
        resume_state,
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
    resume_state: Option<RunState>,
) -> anyhow::Result<TaskOutcome> {
    // Audit trail: every event lands in `.transcripts/events_{ts}.jsonl`
    // in arrival order. Subscribed before the renderer, so nothing is missed.
    let recorder = spawn_event_recorder(events_rx.resubscribe(), &workspace);
    let renderer = render::spawn_renderer(events_rx, commands_tx);

    // G9 (s07): skill layer-1 descriptions join the system prompt.
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
        team_bus: Some(team_bus.clone()),
        tuning: Some(Arc::new(crate::config::RuntimeTuning::from_config(config))),
        // Internal utility calls (compaction summaries, memory
        // extraction) run on a dedicated provider with thinking mode
        // forced off — see `build_internal_provider`.
        internal_provider: Some(build_internal_provider(config)?),
        web_search: web_search_spec(config),
    };
    let mut executor =
        Executor::new(build_provider(config)?, registry, auto_approve, runtime, session);
    if let Some(state) = resume_state {
        executor.seed(state);
    }
    // P1 checkpoint: attach the periodic writer when enabled; a
    // completed session clears the checkpoint (nothing left to resume).
    let checkpoint_store =
        (config.agent.checkpoint_every_turns > 0).then(|| CheckpointStore::new(&workspace));
    if let Some(store) = &checkpoint_store {
        executor.set_checkpoint_sink(CheckpointSink::new(
            store.clone(),
            task,
            &workspace,
            config.agent.checkpoint_every_turns,
        ));
    }
    executor.run(task, &planner, memory, max_turns, stream).await?;
    if !executor.aborted {
        if let Some(store) = &checkpoint_store {
            store.clear();
        }
    }

    // The executor owns the event-bus sender (via its runtime). Drop it
    // before awaiting the renderer, or the renderer never observes the
    // channel close and the process hangs after the task completes.
    let outcome = TaskOutcome { completed: !executor.aborted, turns: executor.last_turn };
    // Session-level usage summary: token/cache counts and cost.
    if !executor.aborted {
        publish_usage_summary(&executor, config);
    }
    print_budget_status(config, &executor.last_usage);
    drop(executor);

    // Teammate loops hold the team event bus; without a shutdown signal
    // they keep working and the renderer never sees the channel close.
    team_bus.shutdown();

    let _ = renderer.await;

    // Per-agent usage: teammates persist to `.team/usage.jsonl`; the
    // renderer await above guarantees they exited, so totals are final.
    print_team_usage(&workspace, &config.llm.model);
    let _ = recorder.await;
    Ok(outcome)
}

/// Publish the session-level UsageSummary event (lead agent).
fn publish_usage_summary(executor: &crate::agent::Executor, config: &Config) {
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
fn print_budget_status(config: &Config, usage: &crate::llm::Usage) {
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

/// Print one usage line per teammate from `.team/usage.jsonl` (best
/// effort: a teammate may still be working its final turn).
fn print_team_usage(workspace: &std::path::Path, model: &str) {
    let path = workspace.join(".team").join("usage.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let usage = crate::llm::Usage {
            prompt_tokens: value["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: value["completion_tokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: value["prompt_tokens"].as_u64().unwrap_or(0) as u32
                + value["completion_tokens"].as_u64().unwrap_or(0) as u32,
            cache_hit_tokens: value["cache_hit_tokens"].as_u64().unwrap_or(0) as u32,
            cache_miss_tokens: value["cache_miss_tokens"].as_u64().unwrap_or(0) as u32,
            reasoning_tokens: value["reasoning_tokens"].as_u64().unwrap_or(0) as u32,
        };
        let agent = value["agent"].as_str().unwrap_or("teammate");
        println!(
            "👥 {}: {} tokens ≈ {}",
            agent,
            usage.prompt_tokens + usage.completion_tokens,
            crate::llm::format_cost(crate::llm::estimate_cost(model, &usage))
        );
    }
}

/// Resolve the workspace root for session state (skills dir, task board,
/// worktrees). Placeholder until a real workspace resolver exists.
pub fn workspace_root() -> anyhow::Result<PathBuf> {
    std::env::current_dir().map_err(Into::into)
}
