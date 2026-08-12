//! Event recorder: persist every agent event as JSONL.
//!
//! A session's event stream is broadcast in memory and rendered to
//! stdout, but nothing survives the process. This module subscribes to
//! the event bus and appends each event — with a timestamp, in arrival
//! order — to `.transcripts/events_{ts}.jsonl`, giving a full audit
//! trail of what happened during a session (turns, tool calls, todo
//! updates, subagents, background tasks, team messages, ...).
//!
//! The recorder is fire-and-forget: it never blocks the agent loop, and
//! a write failure only logs a warning.

use crate::agent::event::AgentEvent;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Timestamp (unix seconds) shared with the compaction transcripts.
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// `events_{ts}.jsonl`, with an increasing numeric suffix when a file
/// for the same second already exists (two sessions starting in the same
/// tick must not share a log).
fn event_log_path(dir: &Path) -> std::path::PathBuf {
    let base = now_secs();
    let mut n = 0u64;
    loop {
        let name = if n == 0 {
            format!("events_{}.jsonl", base)
        } else {
            format!("events_{}_{}.jsonl", base, n)
        };
        let path = dir.join(&name);
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

/// Append one event to the log file as `{"ts": <secs>, "event": {...}}`.
fn append_event(file: &mut impl Write, event: &AgentEvent) {
    let line = serde_json::json!({
        "ts": now_secs(),
        "event": event,
    });
    if let Err(e) = serde_json::to_writer(&mut *file, &line) {
        tracing::warn!(error = %e, "event recorder: serialize failed");
        return;
    }
    if let Err(e) = file.write_all(b"\n") {
        tracing::warn!(error = %e, "event recorder: write failed");
    }
}

/// Spawn the event recorder for a session rooted at `workspace`.
///
/// Events are written to `.transcripts/events_{ts}.jsonl`; the file is
/// created lazily on the first event. The task ends on its own once the
/// event bus closes (all senders dropped, i.e. the session is over).
pub fn spawn_event_recorder(
    mut events: tokio::sync::broadcast::Receiver<AgentEvent>,
    workspace: &Path,
) -> tokio::task::JoinHandle<()> {
    let transcripts_dir = workspace.join(".transcripts");
    tokio::spawn(async move {
        if let Err(e) = std::fs::create_dir_all(&transcripts_dir) {
            tracing::warn!(error = %e, "event recorder: cannot create .transcripts/");
            return;
        }
        let file = match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(event_log_path(&transcripts_dir))
        {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(error = %e, "event recorder: cannot open transcript file");
                return;
            }
        };
        let mut file = std::io::BufWriter::new(file);
        while let Ok(event) = events.recv().await {
            append_event(&mut file, &event);
        }
        if let Err(e) = file.flush() {
            tracing::warn!(error = %e, "event recorder: flush failed");
        }
    })
}
