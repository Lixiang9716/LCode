//! Agent executor — runs the main agent loop.
//!
//! The executor is event-driven: every observable step is published on the
//! runtime's event bus ([`AgentEvent`]) instead of printing directly, and
//! tool approvals flow back through [`AgentCommand`] messages instead of
//! blocking on stdin. Observers (REPL, logging, tests) subscribe to the
//! event stream.

use crate::agent::event::AgentEvent;
use crate::agent::executor_hooks::check_budget;
use crate::agent::prompt;
use crate::agent::runtime::{AgentRuntime, ApprovalDecision};
use crate::agent::{
    BackgroundManager, ConversationMemory, CronScheduler, HookContext, HookDecision, HookPoint,
    HookRegistry, McpRegistry, Planner, TodoManager,
};
use crate::llm::LlmProvider;
use crate::tools::{ToolRegistry, ToolResult};
use std::sync::{Arc, Mutex};

/// Loop control signal returned by response/tool handlers.
pub(crate) enum LoopControl {
    /// Keep running the agent loop.
    Continue,
    /// Stop the loop (task finished).
    Stop,
    /// Stop the loop because the user aborted.
    Abort,
}

/// Session-scoped state shared between the executor and the session
/// tools (todo/skill/task/background/team/worktree/cron/mcp).
pub struct SessionState {
    pub todo: Arc<Mutex<TodoManager>>,
    pub background: Arc<BackgroundManager>,
    pub hooks: Arc<HookRegistry>,
    pub cron: Arc<Mutex<CronScheduler>>,
    pub mcp: Arc<Mutex<McpRegistry>>,
    /// Compact-request channel written by the `compact` tool, read by
    /// the executor at the next turn boundary (s06 manual layer).
    pub compact_request: Arc<Mutex<Option<String>>>,
    /// Cross-session memory store (s09): index injected into the prompt,
    /// extract/consolidate run at session end.
    pub memory_store: Option<Arc<crate::agent::MemoryStore>>,
    /// Team message bus (s09-s17): the lead's inbox is drained at
    /// turn-start so teammate replies reach the main conversation.
    pub team_bus: Option<Arc<crate::agent::MessageBus>>,
    /// User-tunable runtime parameters (compaction/team/subagent/...).
    /// `None` keeps the built-in defaults (tests).
    pub tuning: Option<Arc<crate::config::RuntimeTuning>>,
    /// Provider for internal utility calls (compaction summaries, memory
    /// extraction): thinking mode is forced off on it by default (P0-1).
    /// `None` (tests) falls back to the main provider.
    pub internal_provider: Option<Box<dyn LlmProvider>>,
    /// Server-side `web_search` declaration for DeepSeek's
    /// Anthropic-compatible endpoint (P1-2); `None` disables it.
    pub web_search: Option<crate::llm::ServerToolSpec>,
}

/// The executor drives the agent loop.
///
/// Owns the LLM provider, tool registry, runtime, and the session-scoped
/// state (todo manager for nag reminders, background manager for
/// turn-start notification draining) so it can be constructed with mocks
/// in tests.
pub struct Executor {
    pub(crate) provider: Box<dyn LlmProvider>,
    pub(crate) registry: ToolRegistry,
    auto_approve: bool,
    pub(crate) runtime: AgentRuntime,
    pub(crate) todo: Arc<Mutex<TodoManager>>,
    pub(crate) background: Arc<BackgroundManager>,
    pub(crate) hooks: Arc<HookRegistry>,
    /// Shared cron scheduler: the cron tools manage jobs, the executor
    /// fires due ones by injecting them into the conversation (s14).
    pub(crate) cron: Arc<Mutex<CronScheduler>>,
    pub(crate) mcp: Arc<Mutex<McpRegistry>>,
    pub(crate) compact_request: Arc<Mutex<Option<String>>>,
    pub(crate) prompt_too_long: std::sync::atomic::AtomicBool,
    pub(crate) memory_store: Option<Arc<crate::agent::MemoryStore>>,
    pub(crate) team_bus: Option<Arc<crate::agent::MessageBus>>,
    pub(crate) tuning: Option<Arc<crate::config::RuntimeTuning>>,
    /// Provider for internal utility calls; `None` falls back to
    /// [`Self::provider`] (tests).
    pub(crate) internal_provider: Option<Box<dyn LlmProvider>>,
    /// Server-side web_search declaration (appended to the tool pool).
    pub(crate) web_search: Option<crate::llm::ServerToolSpec>,
    /// A test command failed in the previous turn (test-until-green
    /// reminder pending).
    pub(crate) test_failed: bool,
    /// Budget-warning state (P0 gate), carried across resumed runs.
    pub(crate) budget_warned: bool,
    /// Turn/cost state to continue from on a resumed run (P1).
    pub(crate) seeded: Option<crate::agent::checkpoint::RunState>,
    /// Periodic checkpoint writer (P1), `None` when disabled.
    pub(crate) checkpoint_sink: Option<crate::agent::checkpoint::CheckpointSink>,
    /// Whether the session ended via abort (e.g. max turns) instead
    /// of finishing normally; surfaced to the CLI for the exit code.
    pub(crate) aborted: bool,
    /// Last turn counter reached before the session ended.
    pub(crate) last_turn: u32,
    /// Aggregated usage of the last finished session (cache/理 breakdown
    /// included), consumed by the session layer for the UsageSummary.
    pub(crate) last_usage: crate::llm::Usage,
}

impl Executor {
    /// Create a new executor bound to the given runtime and session state.
    pub fn new(
        provider: Box<dyn LlmProvider>,
        registry: ToolRegistry,
        auto_approve: bool,
        runtime: AgentRuntime,
        session: SessionState,
    ) -> Self {
        Self {
            provider,
            registry,
            auto_approve,
            runtime,
            todo: session.todo,
            background: session.background,
            hooks: session.hooks,
            cron: session.cron,
            mcp: session.mcp,
            compact_request: session.compact_request,
            prompt_too_long: std::sync::atomic::AtomicBool::new(false),
            memory_store: session.memory_store,
            team_bus: session.team_bus,
            tuning: session.tuning,
            internal_provider: session.internal_provider,
            web_search: session.web_search,
            test_failed: false,
            budget_warned: false,
            seeded: None,
            checkpoint_sink: None,
            aborted: false,
            last_turn: 0,
            last_usage: crate::llm::Usage::default(),
        }
    }

    /// Seed the turn/cost state for a resumed run (P1 checkpoint).
    #[doc(hidden)]
    pub fn seed(&mut self, state: crate::agent::checkpoint::RunState) {
        self.budget_warned = state.budget_warned;
        self.seeded = Some(state);
    }

    /// Attach the periodic checkpoint writer (P1).
    #[doc(hidden)]
    pub fn set_checkpoint_sink(&mut self, sink: crate::agent::checkpoint::CheckpointSink) {
        self.checkpoint_sink = Some(sink);
    }

    /// Run the agent loop for a given task.
    ///
    /// Publishes session/turn/tool events on the runtime event bus and
    /// returns the conversation memory after the run so callers (and
    /// tests) can inspect the final message history.
    ///
    /// `stream` toggles the LLM call style: `false` (the default) uses the
    /// plain `chat` call; `true` streams token deltas through
    /// [`LlmProvider::chat_stream`], publishing each delta as a
    /// [`AgentEvent::TextGenerated`] so observers (e.g. the REPL) get a
    /// typewriter effect.
    pub async fn run(
        &mut self,
        task: &str,
        planner: &Planner,
        mut memory: ConversationMemory,
        max_turns: u32,
        stream: bool,
    ) -> anyhow::Result<ConversationMemory> {
        // Session initialization: plan echo (G6), UserPromptSubmit hook
        // (G8) and system prompt assembly (G7).
        if self.initialize_session(task, planner, &mut memory)? {
            return Ok(memory);
        }

        self.runtime.publish(AgentEvent::SessionStarted { task: task.to_string() });

        let (aborted, total_turns, total_usage) =
            self.run_with_review(task, planner, &mut memory, max_turns, stream).await?;
        self.aborted = aborted;
        self.aborted = aborted;
        self.last_turn = total_turns;

        if !aborted {
            // G8: Stop hook (policy/observation at session end)
            let stop_ctx = HookContext {
                point: HookPoint::Stop,
                tool_name: None,
                tool_args: None,
                prompt: None,
            };
            self.hooks.run(&stop_ctx);

            // G3 (s09): at session end, extract durable memories from the
            // conversation and consolidate the memory store. Their usage
            // accumulates into the session total (`last_usage`).
            self.last_usage = total_usage;
            self.persist_memories(&memory).await;

            self.runtime.publish(AgentEvent::TaskFinished {
                turns: total_turns,
                prompt_tokens: self.last_usage.prompt_tokens,
                completion_tokens: self.last_usage.completion_tokens,
            });
        }

        Ok(memory)
    }

    /// Initialize a session: run the UserPromptSubmit hook, inject the
    /// plan echo, and assemble the system prompt from sections.
    ///
    /// Returns `true` when the session was blocked by the hook.
    fn initialize_session(
        &mut self,
        task: &str,
        planner: &Planner,
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<bool> {
        // G6: plan echo — the model sees the (model-owned) plan.
        let plan = planner.create_plan(task);
        let plan_text = plan.render();

        // G8: UserPromptSubmit hook gates the user's prompt.
        let prompt_ctx = HookContext {
            point: HookPoint::UserPromptSubmit,
            tool_name: None,
            tool_args: None,
            prompt: Some(task.to_string()),
        };
        if let HookDecision::Block { reason } = self.hooks.run(&prompt_ctx) {
            self.runtime
                .publish(AgentEvent::Error { message: format!("Prompt blocked: {}", reason) });
            return Ok(true);
        }

        memory.add_user(format!("Task: {}\n\n<plan>\n{}\n</plan>", task, plan_text));

        // G7: assemble the system prompt from sections (s10) — base
        // identity + workspace + tool names; skills/memory sections are
        // appended by their owners (s05/s09).
        let tool_names: Vec<String> =
            self.registry.definitions().iter().map(|t| t.function.name.clone()).collect();
        let workspace = std::env::current_dir().unwrap_or_default();
        // G3 (s09): the memory index (`.memory/MEMORY.md`) becomes the
        // system prompt's Memory section so prior sessions' knowledge is
        // visible from the first turn.
        let memory_index = self.memory_store.as_ref().map(|s| s.index());
        let sections = prompt::session_sections(
            memory.system_prompt(),
            &workspace,
            &tool_names,
            "",
            memory_index.as_deref(),
        );
        memory.set_system_prompt(prompt::assemble(&sections));

        Ok(false)
    }

    /// The main agent loop: turns of inject → compact → chat → handle
    /// until stop, abort or max turns. Returns (aborted, final turn).
    pub(crate) async fn run_loop(
        &mut self,
        memory: &mut ConversationMemory,
        max_turns: u32,
        stream: bool,
        initial_usage: &crate::llm::Usage,
    ) -> anyhow::Result<(bool, u32, crate::llm::Usage)> {
        let mut turn = 0u32;
        let mut aborted = false;
        // Seeded usage (checkpoint resume) counts toward the budget gate.
        let mut total_usage = initial_usage.clone();
        loop {
            if turn >= max_turns {
                self.runtime.publish(AgentEvent::TaskAborted {
                    reason: format!("Reached maximum turns ({})", max_turns),
                });
                aborted = true;
                break;
            }

            turn += 1;
            self.todo.lock().unwrap().note_turn(turn);
            self.runtime.publish(AgentEvent::TurnStarted { turn });
            tracing::debug!(turn, "Agent turn");

            // Turn-start injections (s08 background, s14 cron, s15 inbox).
            crate::agent::executor_hooks::inject_turn_start(self, memory);

            // Tool pool (s19) + compaction (s06); summarizer usage joins the total.
            let tool_defs = self.tool_pool();
            self.maybe_compact(memory, &mut total_usage).await?;

            let Some(response) = crate::agent::executor_hooks::call_llm_with_recovery(
                self, memory, &tool_defs, stream,
            )
            .await?
            else {
                continue;
            };

            crate::agent::executor_hooks::accumulate_usage(&mut total_usage, &response.usage);

            // Budget gate (P0): warn at the ratio, abort at the cap.
            let mut warned = self.budget_warned;
            let over_budget = check_budget(self, &mut warned, &total_usage, memory);
            self.budget_warned = warned;
            if over_budget {
                aborted = true;
                break;
            }

            let finished = match self.handle_response(response, memory, stream).await? {
                LoopControl::Stop => true,
                LoopControl::Abort => {
                    abort_session(&self.runtime, &mut aborted);
                    break;
                }
                LoopControl::Continue => false,
            };

            self.runtime.publish(AgentEvent::TurnFinished { turn });
            self.maybe_nag_todo(memory);

            if finished {
                break;
            }
        }
        // Return only the freshly accumulated usage: the caller already
        // carries the seeded total (checkpoint resume).
        let fresh = crate::llm::Usage {
            prompt_tokens: total_usage.prompt_tokens.saturating_sub(initial_usage.prompt_tokens),
            completion_tokens: total_usage
                .completion_tokens
                .saturating_sub(initial_usage.completion_tokens),
            total_tokens: total_usage.total_tokens.saturating_sub(initial_usage.total_tokens),
            cache_hit_tokens: total_usage
                .cache_hit_tokens
                .saturating_sub(initial_usage.cache_hit_tokens),
            cache_miss_tokens: total_usage
                .cache_miss_tokens
                .saturating_sub(initial_usage.cache_miss_tokens),
            reasoning_tokens: total_usage
                .reasoning_tokens
                .saturating_sub(initial_usage.reasoning_tokens),
        };
        Ok((aborted, turn, fresh))
    }

    /// Handle a single tool call: request approval via the event bus,
    /// execute, and publish the result.
    pub(crate) async fn handle_tool_call(
        &mut self,
        tc: &crate::llm::ToolCallRequest,
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<LoopControl> {
        let tool_name = &tc.function.name;
        let args = &tc.function.arguments;

        // Parse arguments
        let parsed_args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();

        // Publish the tool call request with its approval requirement.
        self.runtime.publish(AgentEvent::ToolCallRequested {
            id: tc.id.clone(),
            name: tool_name.clone(),
            arguments: parsed_args.clone(),
            requires_approval: requires_approval_for(self, tool_name, &parsed_args),
        });

        // PreToolUse hooks: a Block decision cancels the call (s20)
        let hook_ctx = HookContext {
            point: HookPoint::PreToolUse,
            tool_name: Some(tool_name.clone()),
            tool_args: Some(parsed_args.clone()),
            prompt: None,
        };
        if let HookDecision::Block { reason } = self.hooks.run(&hook_ctx) {
            self.runtime.publish(AgentEvent::ToolCallDeclined { id: tc.id.clone() });
            memory.add_tool_result(format!("Tool call blocked by hook: {}", reason), tc.id.clone());
            return Ok(LoopControl::Continue);
        }

        self.execute_tool(tool_name, parsed_args, &tc.id, memory).await
    }

    /// Await approval (when required), execute the tool, and publish the
    /// outcome; runs the PostToolUse hook afterwards.
    async fn execute_tool(
        &mut self,
        tool_name: &str,
        parsed_args: serde_json::Value,
        tool_call_id: &str,
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<LoopControl> {
        // Request approval through the command channel (non-blocking
        // stdin); URL fetches may force approval (see helper below).
        if requires_approval_for(self, tool_name, &parsed_args) {
            match self.runtime.await_approval(tool_call_id).await {
                ApprovalDecision::Approved => {}
                ApprovalDecision::Rejected => {
                    let declined = AgentEvent::ToolCallDeclined { id: tool_call_id.to_string() };
                    self.runtime.publish(declined);
                    record_declined(memory, tool_name, tool_call_id);
                    return Ok(LoopControl::Continue);
                }
                ApprovalDecision::Aborted => return Ok(LoopControl::Abort),
            }
        }

        // Execute the tool (MCP namespaced tools go to the MCP registry;
        // McpRegistry::call expects the full `mcp__{server}__{tool}` name)
        let mcp_result = if tool_name.starts_with("mcp__") {
            Some(self.mcp.lock().unwrap().call(tool_name, &parsed_args))
        } else {
            None
        };
        let result = match mcp_result {
            Some(Ok(output)) => Ok(ToolResult::ok(output)),
            Some(Err(e)) => Err(e),
            None => self.registry.execute(tool_name, &parsed_args),
        };
        // One output string for the event and the conversation; P0
        // test-until-green notes failing test commands below.
        let (output, ok) = match result {
            Ok(result) => (format!("{}", result), true),
            Err(e) => (format!("Error executing tool: {}", e), false),
        };
        if ok {
            self.runtime.publish(AgentEvent::ToolCallExecuted {
                id: tool_call_id.to_string(),
                output: output.clone(),
            });
        } else {
            self.runtime.publish(AgentEvent::ToolCallFailed {
                id: tool_call_id.to_string(),
                error: output.clone(),
            });
        }
        memory.add_tool_result(output.clone(), tool_call_id.to_string());
        self.note_shell_outcome(tool_name, &parsed_args, &output, ok);

        // PostToolUse hook (observability / policy follow-up)
        let post_ctx = HookContext {
            point: HookPoint::PostToolUse,
            tool_name: Some(tool_name.to_string()),
            tool_args: Some(parsed_args),
            prompt: None,
        };
        self.hooks.run(&post_ctx);

        Ok(LoopControl::Continue)
    }
}

/// Publish a task-abort event and mark the session as aborted.
fn abort_session(runtime: &AgentRuntime, aborted: &mut bool) {
    runtime.publish(AgentEvent::TaskAborted { reason: "Aborted by user".to_string() });
    *aborted = true;
}

/// Does this tool invocation require approval? Always when
/// auto-approve is off; URL fetches also force it while
/// `tools.network_requires_approval` is on (default).
fn requires_approval_for(executor: &Executor, tool_name: &str, args: &serde_json::Value) -> bool {
    !executor.auto_approve
        || (is_network_call(tool_name, args)
            && executor.tuning.as_ref().is_some_and(|t| t.network_requires_approval))
}

/// Is this invocation a network fetch (write_file `url` or read_file
/// with an http(s) path)?
fn is_network_call(tool_name: &str, args: &serde_json::Value) -> bool {
    args.get("url").is_some()
        || (tool_name == "read_file"
            && args["path"].as_str().is_some_and(crate::tools::fetch::is_http_url))
}

/// Record a user-declined tool call in the conversation memory.
fn record_declined(memory: &mut ConversationMemory, tool_name: &str, tool_call_id: &str) {
    memory.add_tool_result(
        format!("Tool call declined by user: {}", tool_name),
        tool_call_id.to_string(),
    );
}
