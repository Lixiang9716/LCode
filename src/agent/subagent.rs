//! Subagents (learn-claude-code s04).
//!
//! The parent can delegate a subtask through the `task` tool: the
//! subagent runs with a fresh `messages = [user(prompt)]` context (shared
//! filesystem, same tool handlers, no `task` tool → no recursion), and
//! only a text summary comes back as the tool result.

use crate::llm::{ChatMessage, FinishReason};
use crate::tools::{Tool, ToolResult};
use std::sync::Arc;

/// Maximum number of subagent turns (tutorial parity).
const MAX_SUBAGENT_TURNS: u32 = 30;
/// Tool results are truncated so the subagent context stays bounded.
const MAX_TOOL_RESULT_CHARS: usize = 50_000;

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
    // Fresh context: only the prompt (s04: context isolation).
    let mut messages = vec![ChatMessage::user(prompt.to_string())];
    let tool_defs = registry.definitions();
    let turns = max_turns.clamp(1, MAX_SUBAGENT_TURNS);

    for _ in 0..turns {
        let response = provider.chat(&messages, &tool_defs).await?;

        if response.finish_reason != FinishReason::ToolCalls {
            // Stop / Length / filter: the final text is the summary.
            let text = response.content.trim();
            return Ok(if text.is_empty() { "(no summary)".to_string() } else { text.to_string() });
        }

        // Record the assistant message (with its tool calls) so the model
        // can see its own reasoning in the next turn.
        let tool_calls = response.tool_calls.clone().unwrap_or_default();
        let mut assistant_msg = ChatMessage::assistant(response.content.clone());
        assistant_msg.tool_calls = Some(tool_calls.clone());
        messages.push(assistant_msg);

        // Execute every requested tool call and backfill the results.
        for tc in &tool_calls {
            let args = serde_json::from_str(&tc.function.arguments).unwrap_or_default();
            let output = match registry.execute(&tc.function.name, &args) {
                Ok(result) => format!("{result}"),
                Err(e) => format!("Error: {e}"),
            };
            let output = crate::agent::background::truncate_chars(&output, MAX_TOOL_RESULT_CHARS);
            messages.push(ChatMessage::tool(output, tc.id.clone()));
        }
    }

    // No final answer within the turn budget.
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
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The subtask to delegate to the subagent"
                },
                "max_turns": {
                    "type": "integer",
                    "description": "Maximum subagent turns (default: 30)"
                }
            },
            "required": ["prompt"]
        })
    }

    fn execute(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt =
            args["prompt"].as_str().ok_or_else(|| anyhow::anyhow!("Missing 'prompt' argument"))?;
        let max_turns = args["max_turns"].as_u64().unwrap_or(MAX_SUBAGENT_TURNS as u64) as u32;

        // The `Tool` trait is synchronous but subagents are async; the
        // executor calls tools from inside the async loop, so block on
        // the current runtime handle. `block_in_place` temporarily moves
        // this worker out of the scheduler, which makes `block_on` legal
        // (see the design note in the scaffold: keep `Tool` as-is, block
        // on a runtime handle).
        tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("task tool requires a tokio runtime context"))?;
        let summary = tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(run_subagent(prompt, self.provider.clone(), &self.registry, max_turns))
        })
        .map_err(|e| anyhow::anyhow!("subagent failed: {e}"))?;
        Ok(ToolResult::ok(summary))
    }
}

/// Register this module's tools with the registry.
pub fn register(
    registry: &mut crate::tools::ToolRegistry,
    provider: Arc<dyn crate::llm::LlmProvider>,
    registry_ref: Arc<crate::tools::ToolRegistry>,
) {
    registry.register(Box::new(TaskTool { provider, registry: registry_ref }));
}
