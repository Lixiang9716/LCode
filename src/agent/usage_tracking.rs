//! Per-agent usage accumulation and persistence helpers.
//!
//! Lead usage flows through the event bus (`UsageSummary`); teammate
//! loops run past the lead session and persist their running totals to
//! `.team/usage.jsonl` instead; subagents return their usage through
//! `SubagentCompleted`.

use crate::llm::Usage;
use std::path::Path;

/// Add one response's usage into the running total.
pub fn accumulate_usage(total: &mut Usage, usage: &Usage) {
    total.prompt_tokens += usage.prompt_tokens;
    total.completion_tokens += usage.completion_tokens;
    total.total_tokens += usage.total_tokens;
    total.cache_hit_tokens += usage.cache_hit_tokens;
    total.cache_miss_tokens += usage.cache_miss_tokens;
    total.reasoning_tokens += usage.reasoning_tokens;
}

/// Persist one agent's running usage total to `.team/usage.jsonl`
/// (overwrite-in-place: one line per agent, always the latest total).
pub fn record_agent_usage(team_dir: &Path, agent: &str, usage: &Usage) {
    let path = team_dir.join("usage.jsonl");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let line = serde_json::json!({
        "agent": agent,
        "ts": now,
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "cache_hit_tokens": usage.cache_hit_tokens,
        "cache_miss_tokens": usage.cache_miss_tokens,
        "reasoning_tokens": usage.reasoning_tokens,
    });
    let existing: Vec<String> = std::fs::read_to_string(&path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let mut out: Vec<String> = existing
        .into_iter()
        .filter(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .map(|v| v["agent"] != agent)
                .unwrap_or(true)
        })
        .collect();
    out.push(line.to_string());
    if let Err(e) = std::fs::write(&path, out.join("\n") + "\n") {
        tracing::warn!(agent, error = %e, "failed to persist agent usage");
    }
}
