//! Context compaction (learn-claude-code s06).
//!
//! Three escalation levels:
//! 1. `micro_compact` — replace old large tool_results with placeholders
//!    (keeps `read_file` results), runs silently every turn.
//! 2. `auto_compact` — when estimated tokens exceed the threshold, write
//!    the full transcript to disk, ask the LLM for a summary, and replace
//!    the history with it.
//! 3. Manual `compact` tool / command — same summary, optional focus.

use crate::llm::LlmProvider;
use crate::tools::{Tool, ToolResult};
use std::path::Path as _Path; // used by Phase 2 implementer

/// Token threshold that triggers automatic compaction.
pub const AUTO_COMPACT_THRESHOLD: usize = 50_000;
/// How many recent tool results to keep in micro compaction.
pub const KEEP_RECENT: usize = 3;
/// Tools whose results are never compacted (reference material).
pub const PRESERVE_RESULT_TOOLS: &[&str] = &["read_file"];

/// Rough token estimate: characters / 4 (zero-dependency heuristic).
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Replace old large `tool_result` content with placeholders in place.
pub fn micro_compact(
    messages: &mut [crate::llm::ChatMessage],
    _provider: &dyn crate::llm::LlmProvider,
) -> usize {
    // TODO(s06): collect (idx, part) of tool_results older than KEEP_RECENT;
    // skip results <= 100 chars and PRESERVE_RESULT_TOOLS; replace the
    // rest with "[Previous: used {tool_name}]".
    // Return the number of compacted results.
    let _ = messages;
    let _ = _provider;
    0
}

/// Write the transcript to `.transcripts/` and ask the LLM to summarize
/// the conversation (1. what was done, 2. current state, 3. key decisions).
pub async fn auto_compact(
    messages: &mut Vec<crate::llm::ChatMessage>,
    provider: &dyn crate::llm::LlmProvider,
    focus: Option<&str>,
    workspace: &std::path::Path,
) -> anyhow::Result<String> {
    // TODO(s06): mkdir transcripts, dump messages as JSONL, call provider
    // with the tail of the conversation, replace history with a single
    // "[Conversation compressed. Transcript: ...]" user message.
    // Return the summary text (also published as ContextCompacted).
    let _ = (messages, provider, focus, workspace);
    Ok(String::new())
}

/// Tool: `compact` — the model explicitly triggers compaction.
pub struct CompactTool;

impl Tool for CompactTool {
    fn name(&self) -> &str {
        "compact"
    }

    fn description(&self) -> &str {
        "Compress the conversation into a summary. Optionally pass a \
         focus to preserve specific details."
    }

    fn parameters(&self) -> serde_json::Value {
        // TODO(s06): { focus?: string }
        serde_json::json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn execute(&self, _args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        // TODO(s06): signal the executor to compact after this turn.
        Ok(ToolResult::err("compact not implemented yet"))
    }
}

/// Register this module's tools with the registry.
pub fn register(registry: &mut crate::tools::ToolRegistry) {
    registry.register(Box::new(CompactTool));
}
