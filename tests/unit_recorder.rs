//! Unit tests for the event recorder (`.transcripts/events_{ts}.jsonl`):
//! every event is persisted in arrival order with a unix timestamp, the
//! file is named like the compaction transcripts, and the task ends on
//! its own when the event bus closes.

use lcode::agent::spawn_event_recorder;
use lcode::agent::AgentEvent;
use serde_json::Value;
use tempfile::tempdir;
use tokio::sync::broadcast;

/// Events are appended as one JSON line each, in order, with a `ts`.
#[tokio::test]
async fn recorder_persists_events_in_order() {
    let tmp = tempdir().unwrap();
    let (tx, rx) = broadcast::channel(16);
    let handle = spawn_event_recorder(rx, tmp.path());

    tx.send(AgentEvent::SessionStarted { task: "hello".to_string() }).unwrap();
    tx.send(AgentEvent::TextGenerated { content: "hi".to_string() }).unwrap();
    tx.send(AgentEvent::TurnFinished { turn: 1 }).unwrap();
    drop(tx);
    handle.await.expect("recorder task ends when the bus closes");

    let dir = tmp.path().join(".transcripts");
    let entries: Vec<_> =
        std::fs::read_dir(&dir).expect("transcripts dir exists").filter_map(|e| e.ok()).collect();
    assert_eq!(entries.len(), 1, "one event file per session");
    let path = entries[0].path();
    assert!(path.to_string_lossy().contains("events_"), "named events_{{ts}}.jsonl");

    let contents = std::fs::read_to_string(&path).expect("read event file");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 3, "one JSON line per event");

    let first: Value = serde_json::from_str(lines[0]).expect("line is JSON");
    assert!(first["ts"].as_u64().is_some(), "line carries a unix timestamp");
    assert_eq!(first["event"]["SessionStarted"]["task"], "hello");
    assert!(lines[1].contains("TextGenerated"), "second line is TextGenerated");
    assert!(lines[2].contains("TurnFinished"), "third line is TurnFinished");
}

/// Tool-call events serialize their arguments verbatim (audit value).
#[tokio::test]
async fn recorder_serializes_tool_call_arguments() {
    let tmp = tempdir().unwrap();
    let (tx, rx) = broadcast::channel(16);
    let handle = spawn_event_recorder(rx, tmp.path());

    tx.send(AgentEvent::ToolCallRequested {
        id: "call-1".to_string(),
        name: "shell".to_string(),
        arguments: serde_json::json!({ "command": "ls -la" }),
        requires_approval: true,
    })
    .unwrap();
    drop(tx);
    handle.await.expect("recorder task ends when the bus closes");

    let dir = tmp.path().join(".transcripts");
    let path = std::fs::read_dir(&dir).expect("dir").next().unwrap().unwrap().path();
    let line = std::fs::read_to_string(&path).expect("read");
    let value: Value = serde_json::from_str(line.trim()).expect("JSON");
    let event = &value["event"]["ToolCallRequested"];
    assert_eq!(event["name"], "shell");
    assert_eq!(event["arguments"]["command"], "ls -la");
    assert_eq!(event["requires_approval"], true);
}

/// A new session appends to a fresh file (per-session audit trail).
#[tokio::test]
async fn recorder_creates_one_file_per_session() {
    let tmp = tempdir().unwrap();

    let (tx1, rx1) = broadcast::channel(16);
    let h1 = spawn_event_recorder(rx1, tmp.path());
    tx1.send(AgentEvent::TurnFinished { turn: 1 }).unwrap();
    drop(tx1);
    h1.await.unwrap();

    let (tx2, rx2) = broadcast::channel(16);
    let h2 = spawn_event_recorder(rx2, tmp.path());
    tx2.send(AgentEvent::TurnFinished { turn: 2 }).unwrap();
    drop(tx2);
    h2.await.unwrap();

    let entries: Vec<_> = std::fs::read_dir(tmp.path().join(".transcripts"))
        .expect("dir")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 2, "two sessions -> two event files");
}
