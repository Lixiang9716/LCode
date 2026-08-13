//! Session hook methods for the executor: turn-start injections
//! (background results, cron triggers), context compaction (s06),
//! the dynamic tool pool (s19) and the todo nag (s03).
//!
//! Kept in a separate file so `executor.rs` stays under the 500-line
//! style limit.

use crate::agent::compaction::{auto_compact_with_usage, micro_compact, AUTO_COMPACT_THRESHOLD};
use crate::agent::event::AgentEvent;
use crate::agent::executor::{Executor, LoopControl};
use crate::agent::runtime::AgentRuntime;
use crate::agent::ConversationMemory;
use crate::llm::{FinishReason, ToolDefinition};

impl Executor {
    /// The provider for internal utility calls (compaction summaries,
    /// memory extraction): the dedicated thinking-disabled provider when
    /// the session built one, else the main provider (tests).
    pub(crate) fn internal_provider(&self) -> &dyn crate::llm::LlmProvider {
        self.internal_provider.as_deref().unwrap_or(self.provider.as_ref())
    }

    #[doc(hidden)]
    pub fn tool_pool(&self) -> Vec<ToolDefinition> {
        let mut defs = self.registry.definitions();
        if let Ok(mcp) = self.mcp.lock() {
            defs.extend(mcp.tool_definitions());
        }
        // Server-side web_search (DeepSeek Anthropic-compatible endpoint):
        // declared to the model, executed by the API itself.
        if let Some(spec) = &self.web_search {
            defs.push(web_search_definition(spec));
        }
        defs
    }

    /// Drain completed background-task notifications into the conversation
    /// before the LLM call (s08 turn-start injection).
    pub(crate) fn inject_background_results(&self, memory: &mut ConversationMemory) {
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
    pub(crate) fn inject_cron_triggers(&self, memory: &mut ConversationMemory) {
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
    /// G1: context compaction pipeline — micro pass every turn, plus
    /// auto-compaction when the token budget is exceeded or the model
    /// requested it via the `compact` tool (s06).
    /// `total_usage` receives the summarizer call's usage so internal
    /// calls are visible in the session total.
    pub(crate) async fn maybe_compact(
        &mut self,
        memory: &mut ConversationMemory,
        total_usage: &mut crate::llm::Usage,
    ) -> anyhow::Result<()> {
        // Manual request from the `compact` tool takes priority.
        let requested_focus = self.compact_request.lock().unwrap().take();

        // Micro pass: replace old large tool results with placeholders.
        let compaction_cfg = self.tuning.as_ref().map(|t| t.compaction.clone()).unwrap_or_default();
        micro_compact(memory.messages_mut(), self.provider.as_ref(), &compaction_cfg);

        // Compare the approximate token count directly (estimating the
        // digit-string of the count would never exceed the threshold).
        let threshold = self
            .tuning
            .as_ref()
            .map(|t| t.compaction.auto_threshold)
            .unwrap_or(AUTO_COMPACT_THRESHOLD);
        let over_budget = memory.approximate_tokens() > threshold;
        if requested_focus.is_some() || over_budget {
            let workspace = std::env::current_dir().unwrap_or_default();
            // Internal utility call: the thinking-disabled internal
            // provider keeps the summary cheap and fast (P0-1).
            let provider = self.internal_provider();
            let (summary, usage) = auto_compact_with_usage(
                memory.messages_mut(),
                provider,
                requested_focus.as_deref(),
                &workspace,
                &compaction_cfg,
            )
            .await?;
            let transcript = workspace.join(".transcripts");
            self.runtime.publish(AgentEvent::ContextCompacted {
                summary,
                transcript_path: transcript.display().to_string(),
            });
            crate::agent::usage_tracking::accumulate_usage(total_usage, &usage);
        }
        Ok(())
    }

    /// G3 (s09): at session end, extract durable memories from the
    /// conversation via the LLM and consolidate the store. Failures are
    /// logged, never fatal. Both calls run on the thinking-disabled
    /// internal provider (P0-1), and their usage lands in the session
    /// total so internal calls are not invisible to the UsageSummary.
    pub(crate) async fn persist_memories(&mut self, memory: &ConversationMemory) {
        let Some(store) = &self.memory_store else { return };
        let text = serde_json::to_string(memory.messages()).unwrap_or_default();
        if text.is_empty() {
            return;
        }
        match store.extract_with_usage(&text, self.internal_provider()).await {
            Ok((n, usage)) => {
                accumulate_usage(&mut self.last_usage, &usage);
                tracing::info!(memories = n, "extracted session memories");
            }
            Err(e) => tracing::debug!(error = %e, "memory extraction skipped"),
        }
        match store.consolidate_with_usage(self.internal_provider()).await {
            Ok((_, usage)) => accumulate_usage(&mut self.last_usage, &usage),
            Err(e) => tracing::debug!(error = %e, "memory consolidation skipped"),
        }
    }

    /// Drain the lead's team inbox into the conversation (s09-s17):
    /// teammate replies and protocol responses arrive before the next
    /// LLM call, formatted by [`MessageBus::drain_lead_inbox`].
    pub(crate) fn inject_lead_inbox(&self, memory: &mut ConversationMemory) {
        if let Some(bus) = &self.team_bus {
            let (_msgs, text) = bus.drain_lead_inbox();
            if !text.is_empty() {
                memory.add_user(text);
            }
        }
    }

    /// Publish a nag event when the model has not updated its todos for
    /// several turns; the renderer surfaces it to the user (s03).
    pub(crate) fn maybe_nag_todo(&self, memory: &mut ConversationMemory) {
        let manager = self.todo.lock().unwrap();
        if manager.is_empty() {
            return;
        }
        let turns = manager.turns_since_update();
        let nag_after = self.tuning.as_ref().map(|t| t.todo_nag_after_turns).unwrap_or(3);
        if turns >= nag_after {
            self.runtime.publish(AgentEvent::TodoNag { turns_since_update: turns });
            memory.add_user("<reminder>Update your todos.</reminder>");
        }
    }
}

/// The `web_search` server-tool declaration (P1-2): the schema is only
/// informational — the API executes the search itself and returns the
/// result in-band; the executor records it like a local tool result.
fn web_search_definition(spec: &crate::llm::ServerToolSpec) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".to_string(),
        function: crate::llm::FunctionDefinition {
            name: spec.name.clone(),
            description: "Search the web for current information (library versions, docs, error messages); the search service returns results directly.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" }
                },
                "required": ["query"]
            }),
        },
        server: Some(spec.clone()),
    }
}

/// Run the turn-start injections: background results (s08), cron
/// triggers (s14) and the lead's team inbox (s15).
pub(crate) fn inject_turn_start(executor: &mut Executor, memory: &mut ConversationMemory) {
    executor.inject_background_results(memory);
    executor.inject_cron_triggers(memory);
    executor.inject_lead_inbox(memory);
}

/// Add one response's usage into the running session total (all fields,
/// so reasoning and cache split survive to the UsageSummary).
pub(crate) fn accumulate_usage(total: &mut crate::llm::Usage, usage: &crate::llm::Usage) {
    crate::agent::usage_tracking::accumulate_usage(total, usage);
}

// --- Response handling (moved from executor.rs to keep it under the
// 500-line style limit; see executor_hooks' file docs) ---

impl Executor {
    /// Handle a single LLM response.
    ///
    /// Executes any requested tool calls (recording results in memory) or
    /// publishes the final answer. Returns the loop control signal.
    ///
    /// `text_already_published` suppresses the one-shot text publish: the
    /// streaming path emits the text as it arrives, so re-publishing the
    /// accumulated block would print it twice in the REPL.
    pub(crate) async fn handle_response(
        &mut self,
        response: crate::llm::LlmResponse,
        memory: &mut ConversationMemory,
        text_already_published: bool,
    ) -> anyhow::Result<LoopControl> {
        match response.finish_reason {
            FinishReason::ToolCalls => {
                // Server-side results (web_search) arrive with matching
                // synthesized tool calls, so `tool_calls` is non-empty
                // whenever `server_results` is. Both empty means the API
                // asked for a tool turn but sent nothing actionable.
                let tool_calls = response.tool_calls.clone().unwrap_or_default();
                if tool_calls.is_empty() && response.server_results.is_empty() {
                    tracing::warn!("tool_calls turn with no calls and no server results");
                    return Ok(LoopControl::Continue);
                }

                // Publish the assistant's text content if any (in
                // streaming mode this was already streamed or shown
                // as a preview before the fallback chat call).
                publish_text_unless(&self.runtime, &response.content, text_already_published);

                // Add the assistant message with tool calls to memory
                memory.add_assistant_with_tool_calls(response.content.clone(), tool_calls.clone());

                // Execute each tool call; server-side results are already
                // executed by the API and only need to be recorded.
                self.execute_tool_calls(&tool_calls, &response.server_results, memory).await
            }
            FinishReason::Stop | FinishReason::Length => {
                // Final response. DeepSeek may inline server-side results
                // in the same message as the final answer (stop_reason
                // end_turn): record them so the grounding survives in the
                // conversation, pairing the assistant tool calls with the
                // recorded results.
                publish_text_unless(&self.runtime, &response.content, text_already_published);
                // The assistant message must carry a tool_use for every
                // recorded result, or the wire pairing breaks on resume.
                let tool_calls = ensure_paired_calls(
                    response.tool_calls.clone().unwrap_or_default(),
                    &response.server_results,
                );
                if tool_calls.is_empty() && response.server_results.is_empty() {
                    memory.add_assistant(response.content);
                } else {
                    memory.add_assistant_with_tool_calls(response.content.clone(), tool_calls);
                    self.record_all_server_results(&response.server_results, memory);
                }
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
    /// Server-side results (web search etc.) are already executed by the
    /// API — their content is recorded in place of a local execution.
    /// Stops at the first abort signal from the user.
    async fn execute_tool_calls(
        &mut self,
        tool_calls: &[crate::llm::ToolCallRequest],
        server_results: &[crate::llm::ServerToolResult],
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<LoopControl> {
        for tc in tool_calls {
            // A result matching by id always belongs to this call, even
            // when the server tool was not declared in this session's
            // pool (defensive: results only arrive when it was).
            if let Some(server) = server_results.iter().find(|r| r.id == tc.id) {
                self.record_server_result(server, tc, memory);
                continue;
            }
            if self.is_server_tool(tc) {
                // Server-side tools never execute locally: the API ran
                // them before this response arrived. A missing result
                // becomes an explicit error result instead of leaking
                // into local execution (which would block on approval
                // for a tool that has no local body).
                self.record_server_error(tc, memory);
                continue;
            }
            match self.handle_tool_call(tc, memory).await? {
                LoopControl::Abort => return Ok(LoopControl::Abort),
                LoopControl::Stop | LoopControl::Continue => {}
            }
        }
        Ok(LoopControl::Continue)
    }

    /// Is this tool call a declared server-side tool (web_search)?
    fn is_server_tool(&self, tc: &crate::llm::ToolCallRequest) -> bool {
        self.web_search.as_ref().is_some_and(|spec| tc.function.name == spec.name)
    }

    /// Record every API-executed server result (used by the Stop path,
    /// where no further loop turn will consume them).
    fn record_all_server_results(
        &self,
        server_results: &[crate::llm::ServerToolResult],
        memory: &mut ConversationMemory,
    ) {
        for server in server_results {
            let call = synthesized_call(server);
            self.record_server_result(server, &call, memory);
        }
    }

    /// Record an API-executed server-tool result (web search): publish
    /// the call + execution events and append the result text to the
    /// conversation, exactly like a locally executed tool. No approval:
    /// the API already executed the search before the response arrived.
    fn record_server_result(
        &self,
        server: &crate::llm::ServerToolResult,
        call: &crate::llm::ToolCallRequest,
        memory: &mut ConversationMemory,
    ) {
        let output = if server.content.is_empty() {
            "No results.".to_string()
        } else {
            server.content.clone()
        };
        self.runtime.publish(AgentEvent::ToolCallRequested {
            id: call.id.clone(),
            name: call.function.name.clone(),
            arguments: serde_json::from_str(&call.function.arguments).unwrap_or_default(),
            requires_approval: false,
        });
        self.runtime.publish(AgentEvent::ToolCallExecuted {
            id: server.id.clone(),
            output: output.clone(),
        });
        memory.add_tool_result(output, server.id.clone());
    }

    /// A server tool call without a matching result: record an explicit
    /// error result instead of attempting local execution.
    fn record_server_error(
        &self,
        tc: &crate::llm::ToolCallRequest,
        memory: &mut ConversationMemory,
    ) {
        let message = format!("Server-side tool '{}' returned no result", tc.function.name);
        tracing::warn!(tool = tc.function.name, "server tool call without result");
        self.runtime
            .publish(AgentEvent::ToolCallExecuted { id: tc.id.clone(), output: message.clone() });
        memory.add_tool_result(message, tc.id.clone());
    }
}

/// A tool-call placeholder matching a server-side result: guarantees
/// the assistant message pairs the recorded result on the wire.
fn synthesized_call(server: &crate::llm::ServerToolResult) -> crate::llm::ToolCallRequest {
    crate::llm::ToolCallRequest {
        id: server.id.clone(),
        call_type: "function".to_string(),
        function: crate::llm::FunctionCall {
            name: server.name.clone(),
            arguments: "{}".to_string(),
        },
    }
}

/// Extend the assistant message's tool calls so every recorded server
/// result has a matching tool_use (wire pairing on resume).
fn ensure_paired_calls(
    mut tool_calls: Vec<crate::llm::ToolCallRequest>,
    server_results: &[crate::llm::ServerToolResult],
) -> Vec<crate::llm::ToolCallRequest> {
    for server in server_results {
        if !tool_calls.iter().any(|tc| tc.id == server.id) {
            tool_calls.push(synthesized_call(server));
        }
    }
    tool_calls
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
