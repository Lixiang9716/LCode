//! Agent executor — runs the main agent loop.
//!
//! The executor is event-driven: every observable step is published on the
//! runtime's event bus ([`AgentEvent`]) instead of printing directly, and
//! tool approvals flow back through [`AgentCommand`] messages instead of
//! blocking on stdin. Observers (REPL, logging, tests) subscribe to the
//! event stream.

use crate::agent::event::AgentEvent;
use crate::agent::runtime::{AgentRuntime, ApprovalDecision};
use crate::agent::{
    BackgroundManager, ConversationMemory, CronScheduler, HookContext, HookDecision, HookPoint,
    HookRegistry, McpRegistry, Planner, TodoManager,
};
use crate::llm::{ChatMessage, FinishReason, LlmProvider, StreamEvent, ToolDefinition, Usage};
use crate::tools::{ToolRegistry, ToolResult};
use futures::StreamExt;
use std::sync::{Arc, Mutex};

/// Loop control signal returned by response/tool handlers.
enum LoopControl {
    /// Keep running the agent loop.
    Continue,
    /// Stop the loop (task finished).
    Stop,
    /// Stop the loop because the user aborted.
    Abort,
}

/// Number of turns without a todo update before a nag is injected.
const TODO_NAG_AFTER_TURNS: u32 = 3;

/// The executor drives the agent loop.
///
/// Owns the LLM provider, tool registry, runtime, and the session-scoped
/// state (todo manager for nag reminders, background manager for
/// turn-start notification draining) so it can be constructed with mocks
/// in tests.
pub struct Executor {
    provider: Box<dyn LlmProvider>,
    registry: ToolRegistry,
    auto_approve: bool,
    runtime: AgentRuntime,
    todo: Arc<Mutex<TodoManager>>,
    background: Arc<BackgroundManager>,
    hooks: Arc<HookRegistry>,
    /// Shared cron scheduler: the cron tools manage jobs, the executor
    /// fires due ones by injecting them into the conversation (s14).
    cron: Arc<Mutex<CronScheduler>>,
    mcp: Arc<Mutex<McpRegistry>>,
}

impl Executor {
    /// Create a new executor bound to the given runtime and session state.
    pub fn new(
        provider: Box<dyn LlmProvider>,
        registry: ToolRegistry,
        auto_approve: bool,
        runtime: AgentRuntime,
        todo: Arc<Mutex<TodoManager>>,
        background: Arc<BackgroundManager>,
        hooks: Arc<HookRegistry>,
        cron: Arc<Mutex<CronScheduler>>,
        mcp: Arc<Mutex<McpRegistry>>,
    ) -> Self {
        Self { provider, registry, auto_approve, runtime, todo, background, hooks, cron, mcp }
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
        // The planner output is currently informational; keep the binding
        // explicit for future use.
        let _plan = planner.create_plan(task);
        memory.add_user(format!("Task: {}", task));

        let mut turn = 0u32;

        self.runtime.publish(AgentEvent::SessionStarted { task: task.to_string() });

        let mut aborted = false;
        loop {
            if turn >= max_turns {
                self.runtime.publish(AgentEvent::TaskAborted {
                    reason: format!("Reached maximum turns ({})", max_turns),
                });
                aborted = true;
                break;
            }

            turn += 1;
            // Record the turn so the todo manager can measure how many
            // turns have passed since the model last updated its plan
            // (s03 nag).
            self.todo.lock().unwrap().note_turn(turn);
            self.runtime.publish(AgentEvent::TurnStarted { turn });
            tracing::debug!(turn, "Agent turn");

            // Turn-start injection: drain background-task notifications
            // into the conversation (s08: results arrive before the next
            // LLM call, no polling needed).
            self.inject_background_results(&mut memory);

            // Turn-start injection: fire due cron jobs into the
            // conversation (s14: pull-based, checked when the agent is
            // idle so a one-shot agent still honors schedules).
            self.inject_cron_triggers(&mut memory);

            // Assemble the tool pool per turn: built-ins + connected MCP
            // tools (s19 — dynamic pool, `connect_mcp` takes effect on
            // the next turn).
            let tool_defs = self.tool_pool();

            // Get the current conversation context
            let context = memory.get_context();

            // Send to LLM: stream deltas for a typewriter effect when
            // `stream` is set, otherwise the plain chat call.
            let response = self.call_llm(&context, &tool_defs, stream).await?;

            // In streaming mode the text was already published
            // delta-by-delta (or as a streamed preview before a tool-call
            // fallback), so handle_response must not re-publish it as a
            // single block.
            let finished = match self.handle_response(response, &mut memory, stream).await? {
                LoopControl::Stop => true,
                LoopControl::Abort => {
                    abort_session(&self.runtime, &mut aborted);
                    break;
                }
                LoopControl::Continue => false,
            };

            self.runtime.publish(AgentEvent::TurnFinished { turn });

            // Turn-end nag: remind the model to update its plan when it
            // has not touched the todo list for several turns (s03).
            self.maybe_nag_todo(&mut memory);

            if finished {
                break;
            }
        }

        if !aborted {
            let summary = response_usage_summary(&memory);
            self.runtime.publish(AgentEvent::TaskFinished {
                turns: turn,
                prompt_tokens: summary.0 as u32,
                completion_tokens: summary.1 as u32,
            });
        }

        Ok(memory)
    }

    /// Assemble the per-turn tool pool: built-in tools plus connected MCP
    /// tools (namespaced `mcp__{server}__{tool}`, s19).
    fn tool_pool(&self) -> Vec<ToolDefinition> {
        let mut defs = self.registry.definitions();
        if let Ok(mcp) = self.mcp.lock() {
            defs.extend(mcp.tool_definitions());
        }
        defs
    }

    /// Drain completed background-task notifications into the conversation
    /// before the LLM call (s08 turn-start injection).
    fn inject_background_results(&self, memory: &mut ConversationMemory) {
        let notifications = self.background.drain_notifications();
        if notifications.is_empty() {
            return;
        }
        let body = notifications.join("\n");
        memory.add_user(format!("<background-results>\n{}\n</background-results>", body));
        tracing::debug!(count = notifications.len(), "injected background results");
    }

    /// Fire due cron jobs into the conversation (s14 turn-start
    /// injection): lock the shared scheduler, collect the prompts due at
    /// the current minute, and add them as a user message so the model
    /// sees them before the next LLM call. Non-recurring jobs are removed
    /// by `due_prompts` after firing — intended behavior.
    fn inject_cron_triggers(&self, memory: &mut ConversationMemory) {
        let mut scheduler = self.cron.lock().unwrap();
        // `tick()` uses the real clock (`due_prompts(None)`).
        let due = scheduler.tick();
        if due.is_empty() {
            return;
        }
        let body = due.join("\n");
        memory.add_user(format!("<cron-trigger>\n{}\n</cron-trigger>", body));
        tracing::debug!(count = due.len(), "injected cron triggers");
    }

    /// Ask the LLM for a response: streamed when `stream` is set, plain
    /// chat otherwise.
    async fn call_llm(
        &self,
        context: &[ChatMessage],
        tool_defs: &[ToolDefinition],
        stream: bool,
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        if stream {
            self.chat_stream(context, tool_defs).await
        } else {
            self.provider.chat(context, tool_defs).await
        }
    }

    /// Stream a chat completion, publishing every `TextDelta` as its own
    /// [`AgentEvent::TextGenerated`] so observers (REPL, tests) see the
    /// typewriter effect, then reassemble the full [`LlmResponse`] from
    /// the accumulated text and the `Done` finish reason.
    ///
    /// Streams never carry tool calls, so when the model finishes with
    /// `ToolCalls` we fall back to a single `chat()` call to fetch the
    /// full response including the tool-call arguments (a dual call that
    /// keeps tool calling fully functional). Providers without native
    /// streaming already fall back to `chat()` inside `chat_stream`, so
    /// this path works for every backend.
    async fn chat_stream(
        &self,
        context: &[ChatMessage],
        tool_defs: &[ToolDefinition],
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        let mut stream = self.provider.chat_stream(context, tool_defs).await?;
        let mut content = String::new();
        let mut finish_reason = FinishReason::Unknown;
        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta(delta) => {
                    publish_delta(&self.runtime, &mut content, delta);
                }
                StreamEvent::Done(reason) => finish_reason = reason,
            }
        }
        if finish_reason == FinishReason::ToolCalls {
            // Streams never carry tool calls: fall back to a plain chat
            // call for the full response (tool calls included). The
            // fallback's text is not published again — the streamed
            // preview already showed it — but memory still gets the
            // authoritative content via handle_response.
            return self.provider.chat(context, tool_defs).await;
        }
        Ok(crate::llm::LlmResponse {
            content,
            tool_calls: None,
            usage: Usage::default(),
            finish_reason,
        })
    }

    /// Publish a nag event when the model has not updated its todos for
    /// several turns; the renderer surfaces it to the user (s03).
    fn maybe_nag_todo(&self, memory: &mut ConversationMemory) {
        let manager = self.todo.lock().unwrap();
        if manager.is_empty() {
            return;
        }
        let turns = manager.turns_since_update();
        if turns >= TODO_NAG_AFTER_TURNS {
            self.runtime.publish(AgentEvent::TodoNag { turns_since_update: turns });
            memory.add_user("<reminder>Update your todos.</reminder>");
        }
    }

    /// Handle a single LLM response.
    ///
    /// Executes any requested tool calls (recording results in memory) or
    /// publishes the final answer. Returns the loop control signal.
    ///
    /// `text_already_published` suppresses the one-shot text publish: the
    /// streaming path emits the text as it arrives, so re-publishing the
    /// accumulated block would print it twice in the REPL.
    async fn handle_response(
        &mut self,
        response: crate::llm::LlmResponse,
        memory: &mut ConversationMemory,
        text_already_published: bool,
    ) -> anyhow::Result<LoopControl> {
        match response.finish_reason {
            FinishReason::ToolCalls => {
                if let Some(ref tool_calls) = response.tool_calls {
                    // Publish the assistant's text content if any (in
                    // streaming mode this was already streamed or shown
                    // as a preview before the fallback chat call).
                    publish_text_unless(&self.runtime, &response.content, text_already_published);

                    // Add the assistant message with tool calls to memory
                    memory.add_assistant_with_tool_calls(response.content, tool_calls.clone());

                    // Execute each tool call
                    self.execute_tool_calls(tool_calls, memory).await
                } else {
                    Ok(LoopControl::Continue)
                }
            }
            FinishReason::Stop | FinishReason::Length => {
                // Final response — no more tool calls
                publish_text_unless(&self.runtime, &response.content, text_already_published);
                memory.add_assistant(response.content);
                Ok(LoopControl::Stop)
            }
            FinishReason::ContentFilter => {
                self.runtime.publish(AgentEvent::Error {
                    message: "Response blocked by content filter.".to_string(),
                });
                Ok(LoopControl::Stop)
            }
            FinishReason::Unknown => {
                // Assume stop — just output the content
                publish_text_unless(&self.runtime, &response.content, text_already_published);
                Ok(LoopControl::Stop)
            }
        }
    }

    /// Execute a sequence of tool calls, recording each result in memory.
    ///
    /// Stops at the first abort signal from the user.
    async fn execute_tool_calls(
        &mut self,
        tool_calls: &[crate::llm::ToolCallRequest],
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<LoopControl> {
        for tc in tool_calls {
            match self.handle_tool_call(tc, memory).await? {
                LoopControl::Abort => return Ok(LoopControl::Abort),
                LoopControl::Stop | LoopControl::Continue => {}
            }
        }
        Ok(LoopControl::Continue)
    }

    /// Handle a single tool call: request approval via the event bus,
    /// execute, and publish the result.
    async fn handle_tool_call(
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

/// Publish assistant text as a [`AgentEvent::TextGenerated`] event when
/// non-empty.
fn publish_text(runtime: &AgentRuntime, content: &str) {
    if !content.is_empty() {
        runtime.publish(AgentEvent::TextGenerated { content: content.to_string() });
    }
}

/// [`publish_text`], unless the text was already published (streaming
/// mode emits it delta-by-delta, so a second one-shot publish would print
/// it twice in the REPL).
fn publish_text_unless(runtime: &AgentRuntime, content: &str, already_published: bool) {
    if !already_published {
        publish_text(runtime, content);
    }
}

/// Publish a single streamed text delta as a [`AgentEvent::TextGenerated`]
/// event (typewriter effect) and accumulate it into `content`.
fn publish_delta(runtime: &AgentRuntime, content: &mut String, delta: String) {
    content.push_str(&delta);
    runtime.publish(AgentEvent::TextGenerated { content: delta });
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

/// Get a summary of token usage from the conversation memory.
fn response_usage_summary(memory: &ConversationMemory) -> (usize, usize, usize) {
    let prompt_tokens = memory.approximate_tokens();
    let completion_tokens = 0; // Would be tracked per-response in a full implementation
    (prompt_tokens, completion_tokens, prompt_tokens + completion_tokens)
}
