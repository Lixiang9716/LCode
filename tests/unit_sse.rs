//! Unit tests for real SSE streaming (G11): the shared line parser
//! (`llm::sse`), the per-provider chunk mapping (`openai`/`anthropic`),
//! and the `RetryProvider` passthrough that keeps streaming enabled
//! through the provider chain.

use futures::StreamExt;
use lcode::agent::{RetryPolicy, RetryProvider};
use lcode::llm::anthropic::anthropic_stream_event;
use lcode::llm::openai::openai_stream_event;
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::sse::{parse_data_payload, SseData, SseLineParser};
use lcode::llm::{FinishReason, LlmProvider, StreamEvent};

// ---------------------------------------------------------------------------
// llm::sse — parse_data_payload
// ---------------------------------------------------------------------------

#[test]
fn sse_data_payload_classifies_json() {
    let payload = r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#;
    match parse_data_payload(payload) {
        SseData::Json(json) => {
            assert_eq!(json["choices"][0]["delta"]["content"], "Hi");
        }
        other => panic!("expected Json, got {other:?}"),
    }
}

#[test]
fn sse_data_payload_classifies_done_marker() {
    assert_eq!(parse_data_payload("[DONE]"), SseData::Done);
    // Whitespace around the marker is tolerated.
    assert_eq!(parse_data_payload("  [DONE]  "), SseData::Done);
}

#[test]
fn sse_data_payload_classifies_other() {
    assert_eq!(parse_data_payload(""), SseData::Other("".to_string()));
    assert_eq!(parse_data_payload("not json"), SseData::Other("not json".to_string()));
}

// ---------------------------------------------------------------------------
// llm::sse — SseLineParser (chunk-boundary robustness)
// ---------------------------------------------------------------------------

#[test]
fn sse_line_parser_handles_chunk_boundaries() {
    let mut parser = SseLineParser::new();
    // A line split across three chunks, plus a blank line and a comment.
    parser.push(b"data: {\"cho");
    parser.push(b"ices\":[{\"del");
    parser.push(b"ta\":{\"content\":\"Hi\"}}]}\n\n: comment\ndata: [DONE]\n");
    assert_eq!(parser.pop_ready(), Some(r#"{"choices":[{"delta":{"content":"Hi"}}]}"#.to_string()));
    assert_eq!(parser.pop_ready(), Some("[DONE]".to_string()));
    assert_eq!(parser.pop_ready(), None);
    assert_eq!(parser.finish(), None);
}

#[test]
fn sse_line_parser_tolerates_crlf() {
    let mut parser = SseLineParser::new();
    parser.push(b"data: {\"a\":1}\r\ndata: {\"b\":2}\r\n");
    assert_eq!(parser.pop_ready(), Some(r#"{"a":1}"#.to_string()));
    assert_eq!(parser.pop_ready(), Some(r#"{"b":2}"#.to_string()));
    assert_eq!(parser.pop_ready(), None);
}

#[test]
fn sse_line_parser_skips_non_data_lines() {
    let mut parser = SseLineParser::new();
    parser.push(b"event: message\n: keep-alive\n\nid: 7\ndata: {\"ok\":true}\n");
    assert_eq!(parser.pop_ready(), Some(r#"{"ok":true}"#.to_string()));
    assert_eq!(parser.pop_ready(), None);
}

#[test]
fn sse_line_parser_flushes_trailing_line() {
    let mut parser = SseLineParser::new();
    // The stream ends without a final newline.
    parser.push(b"data: trailing");
    assert_eq!(parser.pop_ready(), None);
    assert_eq!(parser.finish(), Some("trailing".to_string()));
}

#[test]
fn sse_line_parser_reassembles_split_utf8() {
    // A multi-byte char split across chunks must not be corrupted.
    let mut parser = SseLineParser::new();
    let text = "data: {\"delta\":{\"content\":\"你好\"}}\n";
    let (first, rest) = text.as_bytes().split_at(30); // split inside "你" (3 bytes)
    parser.push(first);
    assert_eq!(parser.pop_ready(), None);
    parser.push(rest);
    assert_eq!(parser.pop_ready(), Some(r#"{"delta":{"content":"你好"}}"#.to_string()));
}

// ---------------------------------------------------------------------------
// llm::openai — openai_stream_event
// ---------------------------------------------------------------------------

#[test]
fn openai_chunk_maps_delta_content() {
    let chunk = serde_json::json!({
        "choices": [{ "delta": { "content": "Hello " }, "finish_reason": null }]
    });
    assert_eq!(openai_stream_event(&chunk), Some(StreamEvent::TextDelta("Hello ".to_string())));
}

#[test]
fn openai_chunk_maps_finish_reason() {
    let cases = [
        ("stop", FinishReason::Stop),
        ("length", FinishReason::Length),
        ("tool_calls", FinishReason::ToolCalls),
        ("content_filter", FinishReason::ContentFilter),
        ("weird", FinishReason::Unknown),
    ];
    for (reason, expected) in cases {
        let chunk = serde_json::json!({ "choices": [{ "delta": {}, "finish_reason": reason }] });
        assert_eq!(
            openai_stream_event(&chunk),
            Some(StreamEvent::Done(expected)),
            "finish_reason {reason}"
        );
    }
}

#[test]
fn openai_intermediate_chunks_map_to_none() {
    // Role-only deltas, empty deltas and missing finish_reason must be
    // skipped, not terminate the stream.
    let role_only = serde_json::json!({
        "choices": [{ "delta": { "role": "assistant" }, "finish_reason": null }]
    });
    let empty = serde_json::json!({ "choices": [{ "delta": {}, "finish_reason": null }] });
    assert_eq!(openai_stream_event(&role_only), None);
    assert_eq!(openai_stream_event(&empty), None);
    // Empty content strings are skipped too.
    let blank = serde_json::json!({
        "choices": [{ "delta": { "content": "" }, "finish_reason": null }]
    });
    assert_eq!(openai_stream_event(&blank), None);
}

// ---------------------------------------------------------------------------
// llm::anthropic — anthropic_stream_event
// ---------------------------------------------------------------------------

#[test]
fn anthropic_text_delta_maps_to_text_delta() {
    let event = serde_json::json!({
        "type": "content_block_delta",
        "delta": { "type": "text_delta", "text": "Hello" }
    });
    assert_eq!(anthropic_stream_event(&event), Some(StreamEvent::TextDelta("Hello".to_string())));
}

#[test]
fn anthropic_input_json_delta_is_skipped() {
    // Tool-use argument deltas carry no user-visible text.
    let event = serde_json::json!({
        "type": "content_block_delta",
        "delta": { "type": "input_json_delta", "partial_json": "{\"path\":" }
    });
    assert_eq!(anthropic_stream_event(&event), None);
}

#[test]
fn anthropic_message_delta_maps_stop_reason() {
    let cases = [
        ("end_turn", FinishReason::Stop),
        ("max_tokens", FinishReason::Length),
        ("tool_use", FinishReason::ToolCalls),
        ("pause_turn", FinishReason::Unknown),
    ];
    for (reason, expected) in cases {
        let event = serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": reason }
        });
        assert_eq!(
            anthropic_stream_event(&event),
            Some(StreamEvent::Done(expected)),
            "stop_reason {reason}"
        );
    }
}

#[test]
fn anthropic_message_stop_ends_stream() {
    let event = serde_json::json!({ "type": "message_stop" });
    assert_eq!(anthropic_stream_event(&event), Some(StreamEvent::Done(FinishReason::Stop)));
}

#[test]
fn anthropic_other_event_types_are_skipped() {
    for event in [
        serde_json::json!({ "type": "message_start" }),
        serde_json::json!({ "type": "content_block_start" }),
        serde_json::json!({ "type": "ping" }),
        serde_json::json!({ "type": "content_block_stop" }),
    ] {
        assert_eq!(anthropic_stream_event(&event), None, "event: {event}");
    }
}

// ---------------------------------------------------------------------------
// RetryProvider passthrough (streaming must survive the retry wrapper)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn retry_provider_forwards_chat_stream() {
    let mut mock = MockLlmProvider::new();
    mock.expect_chat_stream().times(1).returning(|_messages, _tools| {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("Hello ".to_string())),
            Ok(StreamEvent::TextDelta("world".to_string())),
            Ok(StreamEvent::Done(FinishReason::Stop)),
        ])))
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let provider = RetryProvider::new(Box::new(mock), RetryPolicy::default());
    let mut stream = provider.chat_stream(&[], &[]).await.expect("stream opens");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta("Hello ".to_string()),
            StreamEvent::TextDelta("world".to_string()),
            StreamEvent::Done(FinishReason::Stop),
        ]
    );
}
