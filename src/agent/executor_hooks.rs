//! Session hook methods for the executor: turn-start injections
//! (background results, cron triggers), context compaction (s06),
//! the dynamic tool pool (s19) and the todo nag (s03).
//!
//! Kept in a separate file so `executor.rs` stays under the 500-line
//! style limit.

use crate::agent::compaction::{
    auto_compact, estimate_tokens, micro_compact, AUTO_COMPACT_THRESHOLD,
};
use crate::agent::event::AgentEvent;
use crate::agent::executor::{Executor, TODO_NAG_AFTER_TURNS};
use crate::agent::ConversationMemory;
use crate::llm::ToolDefinition;

impl Executor {
    pub(crate) fn tool_pool(&self) -> Vec<ToolDefinition> {
        let mut defs = self.registry.definitions();
        if let Ok(mcp) = self.mcp.lock() {
            defs.extend(mcp.tool_definitions());
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
    pub(crate) async fn maybe_compact(
        &mut self,
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<()> {
        // Manual request from the `compact` tool takes priority.
        let requested_focus = self.compact_request.lock().unwrap().take();

        // Micro pass: replace old large tool results with placeholders.
        micro_compact(memory.messages_mut(), self.provider.as_ref());

        let over_budget =
            estimate_tokens(&memory.approximate_tokens().to_string()) > AUTO_COMPACT_THRESHOLD;
        if requested_focus.is_some() || over_budget {
            let workspace = std::env::current_dir().unwrap_or_default();
            let summary = auto_compact(
                memory.messages_mut(),
                self.provider.as_ref(),
                requested_focus.as_deref(),
                &workspace,
            )
            .await?;
            let transcript = workspace.join(".transcripts");
            self.runtime.publish(AgentEvent::ContextCompacted {
                summary,
                transcript_path: transcript.display().to_string(),
            });
        }
        Ok(())
    }

    /// G3 (s09): at session end, extract durable memories from the
    /// conversation via the LLM and consolidate the store. Failures are
    /// logged, never fatal.
    pub(crate) async fn persist_memories(&self, memory: &ConversationMemory) {
        let Some(store) = &self.memory_store else { return };
        let text = serde_json::to_string(memory.messages()).unwrap_or_default();
        if text.is_empty() {
            return;
        }
        match store.extract(&text, self.provider.as_ref()).await {
            Ok(n) => tracing::info!(memories = n, "extracted session memories"),
            Err(e) => tracing::debug!(error = %e, "memory extraction skipped"),
        }
        if let Err(e) = store.consolidate(self.provider.as_ref()).await {
            tracing::debug!(error = %e, "memory consolidation skipped");
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
        if turns >= TODO_NAG_AFTER_TURNS {
            self.runtime.publish(AgentEvent::TodoNag { turns_since_update: turns });
            memory.add_user("<reminder>Update your todos.</reminder>");
        }
    }
}
