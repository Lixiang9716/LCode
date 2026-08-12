//! Binary-level end-to-end tests for LCode.
//!
//! These tests compile the real `lcode` binary (`CARGO_BIN_EXE_lcode`)
//! and drive it as a subprocess against a wiremock-backed OpenAI-compatible
//! LLM server — the full path from CLI parsing → config/env overrides →
//! provider HTTP → agent loop → tool execution → stdout rendering.
//!
//! Two scenarios cover the loop end to end:
//!
//! 1. **Plain-text turn** — the mock answers with a single assistant
//!    message; the binary must exit 0 and print the text.
//! 2. **Tool-call turn** — turn 1 asks for `read_file`, the executor
//!    actually reads the file, and turn 2 answers with the content;
//!    the binary must print both the tool call and the final answer.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

/// Matches chat requests whose `messages` array has exactly `count`
/// entries.
///
/// Turn 1 carries a single user message; turn 2 carries user + assistant
/// tool-call + tool result. An exact match lets the mock answer per turn
/// of the agent loop (a `>=` match would let the first mock swallow every
/// later turn and loop forever).
struct TurnMatcher {
    count: usize,
}

impl Match for TurnMatcher {
    fn matches(&self, request: &Request) -> bool {
        let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
            return false;
        };
        body.get("messages")
            .and_then(Value::as_array)
            .map(|msgs| msgs.len() == self.count)
            .unwrap_or(false)
    }
}

/// A chat-completions response with a plain assistant message.
fn text_response(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "choices": [{
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
    }))
}

/// A chat-completions response requesting one `read_file` tool call.
///
/// Mirrors the real OpenAI wire format: `arguments` is a JSON-encoded
/// *string* (an inline object is silently dropped by the provider parse).
fn tool_call_response(file: &str) -> ResponseTemplate {
    let arguments = serde_json::json!({ "path": file }).to_string();
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_e2e_1",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": arguments
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
    }))
}

/// Run the real `lcode` binary as a subprocess with `args`, pointing the
/// LLM connection at `server` and the working directory at `cwd`.
///
/// The user-global config is isolated via `XDG_CONFIG_HOME` and every
/// `LCODE_LLM_*` override is pinned, so the test never touches the
/// developer's machine configuration.
async fn run_lcode(server: &MockServer, cwd: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_lcode"));
    cmd.args(args)
        .current_dir(cwd)
        .env("LCODE_LLM_PROVIDER", "openai")
        .env("LCODE_LLM_API_KEY", "e2e-test-key")
        .env("LCODE_LLM_MODEL", "e2e-mock")
        .env("LCODE_LLM_API_BASE", format!("{}/v1", server.uri()))
        .env("XDG_CONFIG_HOME", cwd.join(".xdg-config"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Fail the test instead of hanging the CI runner on a livelock.
    tokio::time::timeout(Duration::from_secs(90), cmd.output())
        .await
        .expect("lcode subprocess timed out (possible agent livelock)")
        .expect("failed to spawn lcode subprocess")
}

/// stdout + stderr as lossy strings for assertions.
fn output_text(output: &std::process::Output) -> String {
    format!(
        "{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Plain-text turn: mock answers "Hello from the mock LLM" and the
/// binary must print it and exit cleanly.
#[tokio::test]
async fn e2e_run_single_turn_prints_assistant_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(text_response("Hello from the mock LLM"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().expect("tempdir for e2e workspace");
    let output = run_lcode(&server, tmp.path(), &["run", "say hello"]).await;

    let text = output_text(&output);
    assert!(output.status.success(), "lcode run should exit 0\n{text}");
    assert!(
        text.contains("Hello from the mock LLM"),
        "assistant text should be rendered to stdout\n{text}"
    );

    // One chat turn plus the session-end memory extraction call — and
    // nothing more: the loop must stop after the text turn.
    let requests = server.received_requests().await.expect("received requests");
    assert_eq!(requests.len(), 2, "loop should stop after the text turn\n{text}");
}

/// Tool-call turn: the mock asks for `read_file`, the binary executes it
/// against the real filesystem, and the second turn answers with the
/// file content.
#[tokio::test]
async fn e2e_run_tool_call_executes_and_loops() {
    let server = MockServer::start().await;
    let tmp = tempfile::tempdir().expect("tempdir for e2e workspace");
    let notes = tmp.path().join("notes.txt");
    std::fs::write(&notes, "E2E magic line").expect("write notes file");

    let file_arg = notes.to_string_lossy().to_string();
    // Turn 1: system prompt + user task (2 messages) -> request read_file.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(TurnMatcher { count: 2 })
        .respond_with(tool_call_response(&file_arg))
        .mount(&server)
        .await;
    // Turn 2: + assistant tool-call + tool result (4 messages) -> answer.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(TurnMatcher { count: 4 })
        .respond_with(text_response("The file says: E2E magic line"))
        .mount(&server)
        .await;
    // Catch-all: the session-end memory extraction call carries a
    // different message layout; answer with plain text so it degrades to
    // "no memories extracted" instead of a 404.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(text_response(""))
        .mount(&server)
        .await;

    let output = run_lcode(&server, tmp.path(), &["run", "read the notes", "-y"]).await;

    let text = output_text(&output);
    let requests = server.received_requests().await.expect("received requests");
    let mut bodies = String::new();
    for (i, req) in requests.iter().enumerate() {
        bodies.push_str(&format!(
            "\n--- req {i}: {} ---\n{}",
            req.url.path(),
            String::from_utf8_lossy(&req.body)
        ));
    }
    assert!(output.status.success(), "lcode run with a tool call should exit 0\n{text}\n{bodies}");
    assert!(
        text.contains("Tool call: read_file"),
        "the tool call should be rendered\n{text}\n{bodies}"
    );
    assert!(
        text.contains("The file says: E2E magic line"),
        "the final answer should be rendered\n{text}\n{bodies}"
    );

    // Turn 1 (tool call) + turn 2 (final answer) + memory extraction —
    // and nothing more.
    assert_eq!(requests.len(), 3, "agent loop should take exactly two turns\n{text}\n{bodies}");
}
