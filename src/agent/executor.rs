//! Agent executor — runs the main agent loop.
//!
//! The executor is event-driven: every observable step is published on the
//! runtime's event bus ([`AgentEvent`]) instead of printing directly, and
//! tool approvals flow back through [`AgentCommand`] messages instead of
//! blocking on stdin. Observers (REPL, logging, tests) subscribe to the
//! event stream.

use crate::agent::event::AgentEvent;
use crate::agent::runtime::{AgentRuntime, ApprovalDecision};
use crate::agent::{BackgroundManager, ConversationMemory, Planner, TodoManager};
use crate::llm::{FinishReason, LlmProvider, ToolDefinition};
use crate::tools::ToolRegistry;
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
    ) -> Self {
        Self { provider, registry, auto_approve, runtime, todo, background }
    }

    /// Run the agent loop for a given task.
    ///
    /// Publishes session/turn/tool events on the runtime event bus and
    /// returns the conversation memory after the run so callers (and
    /// tests) can inspect the final message history.
    pub async fn run(
        &mut self,
        task: &str,
        planner: &Planner,
        mut memory: ConversationMemory,
        max_turns: u32,
    ) -> anyhow::Result<ConversationMemory> {
        // The planner output is currently informational; keep the binding
        // explicit for future use.
        let _plan = planner.create_plan(task);
        memory.add_user(format!("Task: {}", task));

        let tool_defs: Vec<ToolDefinition> = self.registry.definitions();

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

            // Get the current conversation context
            let context = memory.get_context();

            // Send to LLM
            let response = self.provider.chat(&context, &tool_defs).await?;

            // Handle the response
            let finished = match self.handle_response(response, &mut memory).await? {
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
    async fn handle_response(
        &mut self,
        response: crate::llm::LlmResponse,
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<LoopControl> {
        match response.finish_reason {
            FinishReason::ToolCalls => {
                if let Some(ref tool_calls) = response.tool_calls {
                    // Publish the assistant's text content if any
                    publish_text(&self.runtime, &response.content);

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
                publish_text(&self.runtime, &response.content);
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
                publish_text(&self.runtime, &response.content);
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

        // Request approval through the command channel (non-blocking stdin)
        if !self.auto_approve {
            match self.runtime.await_approval(&tc.id).await {
                ApprovalDecision::Approved => {}
                ApprovalDecision::Rejected => {
                    self.runtime.publish(AgentEvent::ToolCallDeclined { id: tc.id.clone() });
                    record_declined(memory, tool_name, &tc.id);
                    return Ok(LoopControl::Continue);
                }
                ApprovalDecision::Aborted => return Ok(LoopControl::Abort),
            }
        }

        // Execute the tool
        match self.registry.execute(tool_name, &parsed_args) {
            Ok(result) => {
                let result_str = format!("{}", result);
                self.runtime.publish(AgentEvent::ToolCallExecuted {
                    id: tc.id.clone(),
                    output: result_str.clone(),
                });
                memory.add_tool_result(result_str, tc.id.clone());
            }
            Err(e) => {
                let error_str = format!("Error executing tool: {}", e);
                self.runtime.publish(AgentEvent::ToolCallFailed {
                    id: tc.id.clone(),
                    error: error_str.clone(),
                });
                memory.add_tool_result(error_str, tc.id.clone());
            }
        }

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
