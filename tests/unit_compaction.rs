//! Unit tests for context compaction (s06) — `lcode::agent::compaction`.
//!
//! Exercises `estimate_tokens`, `micro_compact` placeholder replacement,
//! `auto_compact` (transcript persistence + summary via a mock provider),
//! and the synchronous `compact` tool.

use lcode::agent::{
    auto_compact, micro_compact, CompactTool, KEEP_RECENT, PRESERVE_RESULT_TOOLS,
    estimate_tokens,
};
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{
    ChatMessage, FinishReason, FunctionCall, LlmResponse, Role, ToolCallRequest, Usage,
};
use lcode::tools::Tool;
use std::sync::Arc;
use tempfile::TempDir;

// --- Helpers ---

fn tool_call(id: &str, name: &str) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: FunctionCall { name: name.to_string(), arguments: "{}".to_string() },
    }
}

fn assistant_with_calls(calls: Vec<ToolCallRequest>) -> ChatMessage {
    let mut msg = ChatMessage::assistant("thinking...");
    msg.tool_calls = Some(calls);
    msg
}

fn tool_result(id: &str, content: &str) -> ChatMessage {
    ChatMessage::tool(content.to_string(), id.to_string())
}

fn big(len: usize) -> String {
    "x".repeat(len)
}

/// Conversation with five large results: bash(c1) and read_file(c2) are
/// old; bash(c3..c5) are the recent ones that must be kept.
fn conversation() -> Vec<ChatMessage> {
    vec![
        assistant_with_calls(vec![
            tool_call("c1", "bash"),
            tool_call("c2", "read_file"),
            tool_call("c3", "bash"),
            tool_call("c4", "bash"),
            tool_call("c5", "bash"),
        ]),
        tool_result("c1", &big(500)),
        tool_result("c2", &big(500)),
        tool_result("c3", &big(500)),
        tool_result("c4", &big(500)),
        tool_result("c5", &big(500)),
    ]
}

fn summary_response(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls: None,
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }
}

// --- estimate_tokens ---

#[test]
fn test_estimate_tokens_is_chars_over_four() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcdefgh"), 2);
    assert_eq!(estimate_tokens(&big(1000)), 250);
}

// --- micro_compact ---

#[test]
fn test_micro_compact_replaces_old_large_results() {
    assert_eq!(PRESERVE_RESULT_TOOLS, &["read_file"]);

    let mut messages = conversation();
    let compacted = micro_compact(&mut messages, &MockLlmProvider::new());

    assert_eq!(compacted, 1);
    // bash result replaced with a placeholder naming the tool.
    assert_eq!(messages[1].content, "[Previous: used bash]");
    // read_file results are reference material and preserved.
    assert_eq!(messages[2].content.len(), 500);
    // The KEEP_RECENT newest results are untouched.
    assert_eq!(messages[3].content.len(), 500);
    assert_eq!(messages[4].content.len(), 500);
    assert_eq!(messages[5].content.len(), 500);
}

#[test]
fn test_micro_compact_skips_small_results() {
    let mut messages = conversation();
    messages[1].content = "short".to_string();
    messages[2].content = "tiny".to_string();

    let compacted = micro_compact(&mut messages, &MockLlmProvider::new());

    assert_eq!(compacted, 0);
    assert_eq!(messages[1].content, "short");
    assert_eq!(messages[2].content, "tiny");
}

#[test]
fn test_micro_compact_keeps_recent_results() {
    let mut messages: Vec<ChatMessage> = conversation().into_iter().take(KEEP_RECENT + 1).collect();

    assert_eq!(micro_compact(&mut messages, &MockLlmProvider::new()), 0);
    assert_eq!(messages[1].content.len(), 500);
}

#[test]
fn test_micro_compact_unknown_tool_id_uses_unknown() {
    let mut messages = conversation();
    messages[1].tool_call_id = Some("missing-id".to_string());

    let compacted = micro_compact(&mut messages, &MockLlmProvider::new());

    assert_eq!(compacted, 1);
    assert_eq!(messages[1].content, "[Previous: used unknown]");
}

#[test]
fn test_micro_compact_results_without_tool_id_are_compacted() {
    // Tool results with no matching assistant tool_calls (missing id or
    // unknown id) are compacted with the "unknown" name.
    let mut messages = vec![
        ChatMessage::user("plain user text"),
        tool_result("orphan", &big(300)),
        tool_result("c1", &big(300)),
        tool_result("c4", &big(300)),
        tool_result("c5", &big(300)),
        tool_result("c6", &big(300)),
    ];
    messages[2].tool_call_id = None;

    let compacted = micro_compact(&mut messages, &MockLlmProvider::new());

    assert_eq!(compacted, 2);
    assert_eq!(messages[1].content, "[Previous: used unknown]");
    assert_eq!(messages[2].content, "[Previous: used unknown]");
}

// --- auto_compact ---

#[tokio::test]
async fn test_auto_compact_writes_transcript_and_replaces_history() {
    let tmp = TempDir::new().unwrap();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().returning(|messages, _tools| {
        // A single user message with the summarization prompt + tail.
        assert_eq!(messages.len(), 1);
        let prompt = &messages[0].content;
        assert!(prompt.contains("What was accomplished"));
        assert!(prompt.contains("Current state"));
        assert!(prompt.contains("Key decisions made"));
        assert!(prompt.contains("hello there"));
        Ok(summary_response("mock summary"))
    });

    let mut messages = vec![ChatMessage::user("hello there"), ChatMessage::assistant("hi")];
    let summary = auto_compact(&mut messages, &mock, None, tmp.path()).await.unwrap();

    assert_eq!(summary, "mock summary");
    // History replaced by a single marker user message.
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, Role::User);
    let marker = &messages[0].content;
    assert!(marker.starts_with("[Conversation compressed. Transcript:"));
    assert!(marker.contains(".transcripts"));
    assert!(marker.ends_with(".jsonl]"));

    // Full transcript persisted as JSONL, one line per message.
    let transcripts = tmp.path().join(".transcripts");
    assert!(transcripts.is_dir());
    let files: Vec<_> =
        std::fs::read_dir(&transcripts).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(files.len(), 1);
    let jsonl = std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(jsonl.lines().count(), 2);
    for line in jsonl.lines() {
        let msg: ChatMessage = serde_json::from_str(line).unwrap();
        assert!(matches!(msg.role, Role::User | Role::Assistant));
    }
}

#[tokio::test]
async fn test_auto_compact_focus_is_preserved_in_prompt() {
    let tmp = TempDir::new().unwrap();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().returning(|messages, _tools| {
        assert!(messages[0].content.contains("preserving details about: auth flow"));
        Ok(summary_response("s"))
    });

    let mut messages = vec![ChatMessage::user("hi")];
    let _ = auto_compact(&mut messages, &mock, Some("auth flow"), tmp.path()).await.unwrap();
}

#[tokio::test]
async fn test_auto_compact_falls_back_when_summary_empty() {
    let tmp = TempDir::new().unwrap();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat()
        .returning(|_, _| Ok(summary_response("   \n ")));

    let mut messages = vec![ChatMessage::user("hi")];
    let summary = auto_compact(&mut messages, &mock, None, tmp.path()).await.unwrap();

    assert_eq!(summary, "No summary generated.");
}

#[tokio::test]
async fn test_auto_compact_truncates_long_conversations() {
    let tmp = TempDir::new().unwrap();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().returning(|messages, _tools| {
        // Only the last ~80_000 characters of the conversation fit.
        assert!(messages[0].content.len() < 81_500);
        Ok(summary_response("s"))
    });

    let mut messages = vec![ChatMessage::user(big(200_000))];
    let _ = auto_compact(&mut messages, &mock, None, tmp.path()).await.unwrap();
}

// --- The compact tool ---

fn compact_tool(mock: MockLlmProvider, workspace: &std::path::Path) -> CompactTool {
    CompactTool { provider: Arc::new(mock), workspace: workspace.to_path_buf() }
}

#[test]
fn test_compact_tool_metadata_and_parameters() {
    let tmp = TempDir::new().unwrap();
    let tool = compact_tool(MockLlmProvider::new(), tmp.path());

    assert_eq!(tool.name(), "compact");
    assert!(tool.description().contains("focus"));

    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    assert_eq!(params["required"], serde_json::json!([]));
    assert_eq!(params["properties"]["focus"]["type"], "string");
}

#[test]
fn test_compact_tool_executes_auto_compact() {
    let tmp = TempDir::new().unwrap();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().returning(|messages, _tools| {
        assert!(messages[0].content.contains("Summarize this conversation"));
        Ok(summary_response("manual summary"))
    });
    let tool = compact_tool(mock, tmp.path());

    let result = tool.execute(&serde_json::json!({ "focus": "keep auth" })).unwrap();

    assert!(result.success);
    assert_eq!(result.output, "manual summary");
    assert!(tmp.path().join(".transcripts").is_dir());
}

#[test]
fn test_compact_tool_reports_failure() {
    let tmp = TempDir::new().unwrap();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().returning(|_, _| Err(anyhow::anyhow!("boom")));
    let tool = compact_tool(mock, tmp.path());

    let result = tool.execute(&serde_json::json!({})).unwrap();

    assert!(!result.success);
    assert!(result.output.contains("boom"));
}
