//! P0 doctor/events tests: the event analyzer over a synthetic log, and
//! the authenticated balance fetch against wiremock.

use lcode::events::{analyze, render};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_event(dir: &std::path::Path, ts: u64, event: serde_json::Value) {
    let path = dir.join("events_1.jsonl");
    if !path.exists() {
        std::fs::write(&path, "").unwrap();
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    let line = serde_json::json!({ "ts": ts, "event": event });
    let _ = writeln!(file, "{}", line);
}

fn event(kind: &str, payload: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ kind: payload })
}

#[test]
fn analyzer_counts_and_correlates() {
    let dir = tempfile::TempDir::new().unwrap();
    write_event(dir.path(), 1000, event("SessionStarted", serde_json::json!({"task": "t"})));
    write_event(
        dir.path(),
        1100,
        event(
            "ToolCallRequested",
            serde_json::json!({"id": "c1", "name": "shell", "arguments": {}}),
        ),
    );
    write_event(
        dir.path(),
        1350,
        event("ToolCallExecuted", serde_json::json!({"id": "c1", "output": "ok"})),
    );
    write_event(
        dir.path(),
        2000,
        event("ToolCallFailed", serde_json::json!({"id": "c2", "name": "grep", "error": "boom"})),
    );
    write_event(dir.path(), 2100, event("TaskAborted", serde_json::json!({"reason": "max turns"})));
    write_event(dir.path(), 3000, event("UsageSummary", serde_json::json!({"cost_usd": 0.42})));

    let report = analyze(dir.path(), None).unwrap();
    assert_eq!(report.events, 6);
    assert_eq!(report.sessions, 1);
    assert_eq!(report.aborted, 1);
    assert_eq!(report.tool_failures.len(), 1);
    assert_eq!(report.total_cost_usd, 0.42);
    assert_eq!(report.slowest_tools.len(), 1);
    assert_eq!(report.slowest_tools[0].1, 250, "1100 -> 1350");
    assert_eq!(report.span_ms, Some(2000));

    let text = render(&report);
    assert!(text.contains("6 event(s)"), "{text}");
    assert!(text.contains("ToolCallRequested"), "{text}");
    assert!(text.contains("slowest tool calls"), "{text}");
}

#[test]
fn analyzer_skips_missing_dir() {
    let dir = tempfile::TempDir::new().unwrap();
    let report = analyze(&dir.path().join("nope"), None).unwrap();
    assert_eq!(report.events, 0);
}

#[tokio::test]
async fn balance_fetch_sends_auth_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user/balance"))
        .and(header("Authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "is_available": true,
            "balance_infos": [{ "currency": "usd", "total_balance": "12.34" }]
        })))
        .mount(&server)
        .await;

    let (bytes, _) = lcode::tools::fetch::fetch_json_with_auth(
        &format!("{}/user/balance", server.uri()),
        10,
        4096,
        Some("Bearer test-key".to_string()),
    )
    .expect("fetch succeeds");
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["balance_infos"][0]["total_balance"], "12.34");
}

#[test]
fn cli_parses_doctor_and_events() {
    use clap::Parser;
    assert!(lcode::cli::Cli::try_parse_from(["lcode", "doctor"]).is_ok());
    assert!(lcode::cli::Cli::try_parse_from(["lcode", "events", "--last", "3"]).is_ok());
}
