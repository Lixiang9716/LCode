//! Subagents (learn-claude-code s04).
//!
//! The parent can delegate a subtask through the `task` tool: the
//! subagent runs with a fresh `messages = []` context (shared filesystem,
//! same tool handlers, no `task` tool → no recursion), and only a text
//! summary comes back as the tool result.

use crate::tools::{Tool, ToolResult};
use std::sync::Arc;

/// Run a subagent with a fresh context.
///
/// The subagent shares the tool registry but not the conversation;
/// returns the final assistant text (or "(no summary)").
pub async fn run_subagent(
    prompt: &str,
    provider: Arc<dyn crate::llm::LlmProvider>,
    registry: &crate::tools::ToolRegistry,
    max_turns: u32,
) -> anyhow::Result<String> {
    // TODO(s04): fresh messages = [user(prompt)]; loop like the executor
    // (max 30 turns, tools via registry, tool_result backfill); join the
    // final text blocks as the summary; fallback "(no summary)".
    let _ = (prompt, provider, registry, max_turns);
    Ok("(no summary)".to_string())
}

/// Tool: `task` — delegate a subtask to a subagent (parent only).
pub struct TaskTool {
    pub provider: Arc<dyn crate::llm::LlmProvider>,
    pub registry: Arc<crate::tools::ToolRegistry>,
}

impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate a subtask to a subagent with a fresh context. The \
         subagent shares the filesystem and tools but has no conversation \
         history; only its summary is returned. Do not use for trivial \
         file operations."
    }

    fn parameters(&self) -> serde_json::Value {
        // TODO(s04): { prompt: string, max_turns?: int }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        // TODO(s04): run_subagent (needs async — see design note below).
        Ok(ToolResult::err("task not implemented yet"))
    }
}

// NOTE for implementers: the `Tool` trait is synchronous, but subagents
// and background tasks are async. Two options:
//   1. Block on a runtime: `tokio::task::block_in_place` or a dedicated
//      runtime handle passed into the tool.
//   2. Extend the executor's tool dispatch to special-case async tools
//      (e.g. a separate `AsyncTool` trait).
// Prefer option 1 for the tutorial parity: keep the `Tool` trait as-is
// and give the tool a `tokio::runtime::Handle` to block on.

/// Register this module's tools with the registry.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    provider: Arc<dyn crate::llm::LlmProvider>,
    registry_ref: Arc<crate::tools::ToolRegistry>,
) {
    registry.register(Box::new(TaskTool { provider, registry: registry_ref }));
}
