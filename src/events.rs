//! `lcode events` — session log analyzer (P0).
//!
//! Read-only aggregation over the recorder's `.transcripts/events_*.jsonl`
//! files: event-type distribution, session outcomes, tool failures,
//! usage/cost summaries and the slowest tool calls.

use std::path::Path;

/// One analyzed batch of event files.
#[derive(Debug, Default)]
pub struct EventReport {
    pub files: usize,
    pub events: usize,
    pub type_counts: Vec<(String, usize)>,
    pub sessions: usize,
    pub aborted: usize,
    pub errors: usize,
    pub tool_failures: Vec<(String, String)>,
    pub total_cost_usd: f64,
    pub slowest_tools: Vec<(String, u64)>,
    pub span_ms: Option<u64>,
}

/// Analyze the most recent `last` event files under `dir` (default:
/// all of them).
pub fn analyze(dir: &Path, last: Option<usize>) -> anyhow::Result<EventReport> {
    let files = event_files(dir, last);
    let mut report = EventReport { files: files.len(), ..Default::default() };
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut requested_ts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut first_ts: Option<u64> = None;
    let mut last_ts: Option<u64> = None;

    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        consume_file(
            &text,
            &mut report,
            &mut counts,
            &mut requested_ts,
            &mut first_ts,
            &mut last_ts,
        );
    }

    report.type_counts = counts.into_iter().collect();
    report.type_counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    report.slowest_tools.sort_by_key(|(_, took)| std::cmp::Reverse(*took));
    report.slowest_tools.truncate(10);
    report.span_ms = match (first_ts, last_ts) {
        (Some(f), Some(l)) => Some(l.saturating_sub(f)),
        _ => None,
    };
    Ok(report)
}

/// The `last` most recently modified `events_*.jsonl` files under `dir`.
fn event_files(dir: &Path, last: Option<usize>) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "jsonl")
                && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("events_"))
        })
        .collect();
    files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let take = last.unwrap_or(files.len()).min(files.len());
    files.truncate(take);
    files
}

/// Aggregate one event file into the report.
fn consume_file(
    text: &str,
    report: &mut EventReport,
    counts: &mut std::collections::HashMap<String, usize>,
    requested_ts: &mut std::collections::HashMap<String, u64>,
    first_ts: &mut Option<u64>,
    last_ts: &mut Option<u64>,
) {
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(ts) = value["ts"].as_u64() else { continue };
        *first_ts = Some(first_ts.map_or(ts, |t| t.min(ts)));
        *last_ts = Some(last_ts.map_or(ts, |t| t.max(ts)));
        let Some(event) = value["event"].as_object() else { continue };
        let Some((kind, payload)) = event.iter().next() else { continue };
        *counts.entry(kind.clone()).or_default() += 1;
        report.events += 1;
        match kind.as_str() {
            "SessionStarted" => report.sessions += 1,
            "TaskAborted" => report.aborted += 1,
            "Error" => report.errors += 1,
            "ToolCallRequested" => {
                if let Some(id) = payload["id"].as_str() {
                    requested_ts.insert(id.to_string(), ts);
                }
            }
            "ToolCallExecuted" => {
                let Some(id) = payload["id"].as_str() else { continue };
                let Some(start) = requested_ts.remove(id) else { continue };
                let name = payload["name"].as_str().unwrap_or(id).to_string();
                report.slowest_tools.push((name, ts.saturating_sub(start)));
            }
            "ToolCallFailed" => {
                let name = payload["name"].as_str().unwrap_or("?").to_string();
                let error = payload["error"].as_str().unwrap_or("?").to_string();
                report.tool_failures.push((name, error));
            }
            "UsageSummary" => {
                if let Some(cost) = payload["cost_usd"].as_f64() {
                    report.total_cost_usd += cost;
                }
            }
            _ => {}
        }
    }
}

/// Human-readable report.
pub fn render(report: &EventReport) -> String {
    let mut lines = vec![format!(
        "events: {} event(s) across {} file(s), {} session(s)",
        report.events, report.files, report.sessions
    )];
    if let Some(span) = report.span_ms {
        lines.push(format!("span: {:.1}s ({} ms)", span as f64 / 1000.0, span));
    }
    if report.aborted > 0 || report.errors > 0 {
        lines.push(format!(
            "outcomes: {} aborted session(s), {} error event(s)",
            report.aborted, report.errors
        ));
    }
    if report.total_cost_usd > 0.0 {
        lines.push(format!(
            "cost: {} total (UsageSummary events)",
            crate::llm::format_cost(report.total_cost_usd)
        ));
    }
    lines.push("event types:".to_string());
    for (kind, count) in &report.type_counts {
        lines.push(format!("  {count:>6}  {kind}"));
    }
    if !report.tool_failures.is_empty() {
        lines.push("tool failures:".to_string());
        for (name, error) in report.tool_failures.iter().take(10) {
            lines.push(format!("  - {name}: {}", error.chars().take(80).collect::<String>()));
        }
    }
    if !report.slowest_tools.is_empty() {
        lines.push("slowest tool calls:".to_string());
        for (name, took) in &report.slowest_tools {
            lines.push(format!("  - {name}: {} ms", took));
        }
    }
    lines.join("\n")
}
