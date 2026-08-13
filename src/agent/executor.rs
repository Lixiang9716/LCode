//! Agent executor — runs the main agent loop.
//!
//! The executor is event-driven: every observable step is published on the
//! runtime's event bus ([`AgentEvent`]) instead of printing directly, and
//! tool approvals flow back through [`AgentCommand`] messages instead of
//! blocking on stdin. Observers (REPL, logging, tests) subscribe to the
//! event stream.

use crate::agent::event::AgentEvent;
use crate::agent::prompt;
use crate::agent::runtime::{AgentRuntime, ApprovalDecision};
use crate::agent::{
    BackgroundManager, ConversationMemory, CronScheduler, HookContext, HookDecision, HookPoint,
    HookRegistry, McpRegistry, Planner, TodoManager,
};
use crate::llm::LlmProvider;
use crate::tools::{ToolRegistry, ToolResult};
use std::sync::atomic::Ordering;
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
            aborted: false,
            last_turn: 0,
            last_usage: crate::llm::Usage::default(),
        }
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

        let (aborted, turn, usage) = self.run_loop(&mut memory, max_turns, stream).await?;
        self.aborted = aborted;
        self.last_turn = turn;

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
            self.last_usage = usage;
            self.persist_memories(&memory).await;

            self.runtime.publish(AgentEvent::TaskFinished {
                turns: turn,
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
    async fn run_loop(
        &mut self,
        memory: &mut ConversationMemory,
        max_turns: u32,
        stream: bool,
    ) -> anyhow::Result<(bool, u32, crate::llm::Usage)> {
        let mut turn = 0u32;
        let mut aborted = false;
        let mut total_usage = crate::llm::Usage::default();
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

            let context = memory.get_context();
            let response = match self.call_llm(&context, &tool_defs, stream).await {
                Ok(resp) => resp,
                Err(e) if e.to_string().contains(crate::agent::retry::PROMPT_TOO_LONG_MARKER) => {
                    // Reactive compaction (s11 path 2): mark and continue;
                    // the next turn's maybe_compact compacts then retries.
                    self.prompt_too_long.store(true, Ordering::SeqCst);
                    let msg = format!("Context too long — compacting and retrying: {}", e);
                    self.runtime.publish(AgentEvent::Error { message: msg });
                    continue;
                }
                Err(e) => {
                    // Unrecoverable call failure: publish the error so
                    // observers (audit log) see why the session died,
                    // then propagate.
                    self.runtime.publish(AgentEvent::Error { message: e.to_string() });
                    return Err(e);
                }
            };

            crate::agent::executor_hooks::accumulate_usage(&mut total_usage, &response.usage);

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
        Ok((aborted, turn, total_usage))
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

        // Publish the tool call request with its approval requirement
        self.runtime.publish(AgentEvent::ToolCallRequested {
            id: tc.id.clone(),
            name: tool_name.clone(),
            arguments: parsed_args.clone(),
            requires_approval: !self.auto_approve,
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
        // Request approval through the command channel (non-blocking stdin)
        if !self.auto_approve {
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
        match result {
            Ok(result) => {
                let result_str = format!("{}", result);
                self.runtime.publish(AgentEvent::ToolCallExecuted {
                    id: tool_call_id.to_string(),
                    output: result_str.clone(),
                });
                memory.add_tool_result(result_str, tool_call_id.to_string());
            }
            Err(e) => {
                let error_str = format!("Error executing tool: {}", e);
                self.runtime.publish(AgentEvent::ToolCallFailed {
                    id: tool_call_id.to_string(),
                    error: error_str.clone(),
                });
                memory.add_tool_result(error_str, tool_call_id.to_string());
            }
        }

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

/// Record a user-declined tool call in the conversation memory.
fn record_declined(memory: &mut ConversationMemory, tool_name: &str, tool_call_id: &str) {
    memory.add_tool_result(
        format!("Tool call declined by user: {}", tool_name),
        tool_call_id.to_string(),
    );
}
