//! Unit tests for the LLM module (moved from `src/llm/`).
//!
//! Per the project's test/source separation rule, tests live in `tests/` and
//! exercise only the public API of `lcode::llm`.

use lcode::llm::anthropic::{
    anthropic_message_to_json, parse_anthropic_response, split_system_messages,
};
use lcode::llm::openai::{message_to_json, parse_response};
use lcode::llm::{
    ChatMessage, FinishReason, FunctionCall, FunctionDefinition, Role, ToolCallRequest,
    ToolDefinition, Usage,
};

// ---------------------------------------------------------------------------
// llm::mod — ChatMessage / ToolDefinition / ToolCallRequest / Usage
// ---------------------------------------------------------------------------

#[test]
fn test_chat_message_constructors() {
    let sys = ChatMessage::system("system prompt");
    assert_eq!(sys.role, Role::System);
    assert_eq!(sys.content, "system prompt");
    assert!(sys.tool_call_id.is_none());
    assert!(sys.tool_calls.is_none());

    let user = ChatMessage::user("hello");
    assert_eq!(user.role, Role::User);
    assert_eq!(user.content, "hello");
    assert!(user.tool_call_id.is_none());

    let assistant = ChatMessage::assistant("hi");
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(assistant.content, "hi");
    assert!(assistant.tool_calls.is_none());

    let tool = ChatMessage::tool("tool output", "call_1".to_string());
    assert_eq!(tool.role, Role::Tool);
    assert_eq!(tool.content, "tool output");
    assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
    assert!(tool.tool_calls.is_none());
}

#[test]
fn test_tool_definition_serializes_to_expected_json() {
    let def = ToolDefinition {
        tool_type: "function".to_string(),
        function: FunctionDefinition {
            name: "write_file".to_string(),
            description: "Write content to a file".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                },
                "required": ["path", "content"],
            }),
        },
    };

    let value = serde_json::to_value(&def).unwrap();
    assert_eq!(value["type"], "function");
    assert_eq!(value["function"]["name"], "write_file");
    assert_eq!(value["function"]["description"], "Write content to a file");
    assert_eq!(value["function"]["parameters"]["type"], "object");
    assert_eq!(value["function"]["parameters"]["required"][0], "path");
    assert_eq!(value["function"]["parameters"]["required"][1], "content");
}

#[test]
fn test_tool_call_request_serialization() {
    let tc = ToolCallRequest {
        id: "call_abc".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "read_file".to_string(),
            arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
        },
    };

    let value = serde_json::to_value(&tc).unwrap();
    assert_eq!(value["id"], "call_abc");
    assert_eq!(value["type"], "function");
    assert_eq!(value["function"]["name"], "read_file");
    assert_eq!(value["function"]["arguments"], r#"{"path":"Cargo.toml"}"#);
}

#[test]
fn test_usage_defaults_to_zero() {
    let usage = Usage::default();
    assert_eq!(usage.prompt_tokens, 0);
    assert_eq!(usage.completion_tokens, 0);
    assert_eq!(usage.total_tokens, 0);
}

// ---------------------------------------------------------------------------
// llm::openai — parse_response / message_to_json
// ---------------------------------------------------------------------------

#[test]
fn test_parse_response_full() {
    let data = serde_json::json!({
        "choices": [{
            "message": {
                "content": "Let me check that.",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"Cargo.toml\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
    });

    let resp = parse_response(&data).unwrap();
    assert_eq!(resp.content, "Let me check that.");
    assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
    assert_eq!(resp.usage.prompt_tokens, 10);
    assert_eq!(resp.usage.completion_tokens, 5);
    assert_eq!(resp.usage.total_tokens, 15);

    let tool_calls = resp.tool_calls.expect("tool calls present");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].call_type, "function");
    assert_eq!(tool_calls[0].function.name, "read_file");
    assert_eq!(tool_calls[0].function.arguments, r#"{"path":"Cargo.toml"}"#);
}

#[test]
fn test_parse_response_missing_fields_use_defaults() {
    let data = serde_json::json!({});

    let resp = parse_response(&data).unwrap();
    assert_eq!(resp.content, "");
    assert!(resp.tool_calls.is_none());
    assert_eq!(resp.finish_reason, FinishReason::Unknown);
    assert_eq!(resp.usage.prompt_tokens, 0);
    assert_eq!(resp.usage.completion_tokens, 0);
    assert_eq!(resp.usage.total_tokens, 0);
}

#[test]
fn test_parse_response_finish_reason_mapping() {
    let cases = [
        ("stop", FinishReason::Stop),
        ("length", FinishReason::Length),
        ("tool_calls", FinishReason::ToolCalls),
        ("content_filter", FinishReason::ContentFilter),
        ("something_weird", FinishReason::Unknown),
    ];
    for (reason, expected) in cases {
        let data = serde_json::json!({
            "choices": [{ "message": { "content": "x" }, "finish_reason": reason }]
        });
        assert_eq!(
            parse_response(&data).unwrap().finish_reason,
            expected,
            "finish_reason {:?}",
            reason
        );
    }
}

#[test]
fn test_parse_response_usage_partial_defaults() {
    let data = serde_json::json!({
        "choices": [{ "message": { "content": "x" }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 3 }
    });
    let resp = parse_response(&data).unwrap();
    assert_eq!(resp.usage.prompt_tokens, 3);
    assert_eq!(resp.usage.completion_tokens, 0);
    assert_eq!(resp.usage.total_tokens, 0);
}

#[test]
fn test_message_to_json_all_roles() {
    let system = message_to_json(&ChatMessage::system("be helpful"));
    assert_eq!(system["role"], "system");
    assert_eq!(system["content"], "be helpful");
    assert!(system.get("tool_call_id").is_none());

    let user = message_to_json(&ChatMessage::user("hi"));
    assert_eq!(user["role"], "user");
    assert_eq!(user["content"], "hi");

    let assistant = message_to_json(&ChatMessage::assistant("yo"));
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"], "yo");

    let tool = message_to_json(&ChatMessage::tool("tool out", "call_9".to_string()));
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["content"], "tool out");
    assert_eq!(tool["tool_call_id"], "call_9");
}

#[test]
fn test_message_to_json_assistant_with_tool_calls() {
    let mut msg = ChatMessage::assistant("thinking...");
    msg.tool_calls = Some(vec![ToolCallRequest {
        id: "call_1".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "write_file".to_string(),
            arguments: r#"{"path":"a.txt"}"#.to_string(),
        },
    }]);

    let json = message_to_json(&msg);
    assert_eq!(json["role"], "assistant");
    assert_eq!(json["content"], "thinking...");
    assert_eq!(json["tool_calls"][0]["id"], "call_1");
    assert_eq!(json["tool_calls"][0]["type"], "function");
    assert_eq!(json["tool_calls"][0]["function"]["name"], "write_file");
    assert_eq!(json["tool_calls"][0]["function"]["arguments"], r#"{"path":"a.txt"}"#);
}

#[test]
fn test_role_enum_serializes_lowercase() {
    assert_eq!(serde_json::to_string(&Role::System).unwrap(), r#""system""#);
    assert_eq!(serde_json::to_string(&Role::User).unwrap(), r#""user""#);
    assert_eq!(serde_json::to_string(&Role::Assistant).unwrap(), r#""assistant""#);
    assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), r#""tool""#);
}

// ---------------------------------------------------------------------------
// llm::anthropic — parse_anthropic_response / split_system_messages /
//                  anthropic_message_to_json
// ---------------------------------------------------------------------------

#[test]
fn test_parse_anthropic_text_response() {
    let data = serde_json::json!({
        "content": [{"type": "text", "text": "Hello there"}],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 5, "output_tokens": 3 }
    });

    let resp = parse_anthropic_response(&data).unwrap();
    assert_eq!(resp.content, "Hello there");
    assert!(resp.tool_calls.is_none());
    assert_eq!(resp.finish_reason, FinishReason::Stop);
    assert_eq!(resp.usage.prompt_tokens, 5);
    assert_eq!(resp.usage.completion_tokens, 3);
    assert_eq!(resp.usage.total_tokens, 8);
}

#[test]
fn test_parse_anthropic_tool_use_response() {
    let data = serde_json::json!({
        "content": [
            {"type": "text", "text": "Let me write the file."},
            {
                "type": "tool_use",
                "id": "toolu_01",
                "name": "write_file",
                "input": {"path": "x.txt", "content": "hi"}
            }
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": 9, "output_tokens": 4 }
    });

    let resp = parse_anthropic_response(&data).unwrap();
    // Text and tool_use blocks are combined; text content preserved.
    assert_eq!(resp.content, "Let me write the file.");
    assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
    assert_eq!(resp.usage.total_tokens, 13);

    let tool_calls = resp.tool_calls.expect("tool calls present");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "toolu_01");
    assert_eq!(tool_calls[0].call_type, "function");
    assert_eq!(tool_calls[0].function.name, "write_file");
    let args: serde_json::Value = serde_json::from_str(&tool_calls[0].function.arguments).unwrap();
    assert_eq!(args["path"], "x.txt");
    assert_eq!(args["content"], "hi");
}

#[test]
fn test_parse_anthropic_stop_reason_mapping() {
    let cases = [
        ("end_turn", FinishReason::Stop),
        ("max_tokens", FinishReason::Length),
        ("tool_use", FinishReason::ToolCalls),
        ("stop_sequence", FinishReason::Unknown),
    ];
    for (reason, expected) in cases {
        let data = serde_json::json!({
            "content": [{"type": "text", "text": "x"}],
            "stop_reason": reason
        });
        assert_eq!(
            parse_anthropic_response(&data).unwrap().finish_reason,
            expected,
            "stop_reason {:?}",
            reason
        );
    }
}

#[test]
fn test_parse_anthropic_empty_and_defaults() {
    let data = serde_json::json!({});
    let resp = parse_anthropic_response(&data).unwrap();
    assert_eq!(resp.content, "");
    assert!(resp.tool_calls.is_none());
    assert_eq!(resp.finish_reason, FinishReason::Unknown);
    assert_eq!(resp.usage.prompt_tokens, 0);
    assert_eq!(resp.usage.completion_tokens, 0);
    assert_eq!(resp.usage.total_tokens, 0);
}

#[test]
fn test_split_system_messages_extracts_system_prompt() {
    let messages = vec![
        ChatMessage::system("You are Claude."),
        ChatMessage::user("hello"),
        ChatMessage::system("Be concise."),
        ChatMessage::assistant("hi"),
        ChatMessage::tool("result", "call_1".to_string()),
    ];

    let (system_prompt, chat) = split_system_messages(&messages);
    assert_eq!(system_prompt, "You are Claude.\n\nBe concise.");
    assert_eq!(chat.len(), 3);
    assert!(
        chat.iter().all(|m| m.role != Role::System),
        "system messages removed from chat history"
    );
    assert_eq!(chat[0].role, Role::User);
    assert_eq!(chat[1].role, Role::Assistant);
    assert_eq!(chat[2].role, Role::Tool);
}

#[test]
fn test_split_system_messages_no_system() {
    let messages = vec![ChatMessage::user("hi"), ChatMessage::assistant("yo")];
    let (system_prompt, chat) = split_system_messages(&messages);
    assert_eq!(system_prompt, "");
    assert_eq!(chat.len(), 2);
}

#[test]
fn test_anthropic_message_to_json_plain_roles() {
    let user = anthropic_message_to_json(&&ChatMessage::user("hi"));
    assert_eq!(user["role"], "user");
    assert_eq!(user["content"], "hi");

    let assistant = anthropic_message_to_json(&&ChatMessage::assistant("yo"));
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"], "yo");
}

#[test]
fn test_anthropic_message_to_json_tool_result() {
    let msg = ChatMessage::tool("wrote 5 bytes", "toolu_01".to_string());
    let json = anthropic_message_to_json(&&msg);
    assert_eq!(json["role"], "user");
    assert_eq!(json["content"][0]["type"], "tool_result");
    assert_eq!(json["content"][0]["tool_use_id"], "toolu_01");
    assert_eq!(json["content"][0]["content"], "wrote 5 bytes");
}

#[test]
fn test_anthropic_message_to_json_assistant_tool_calls() {
    let mut msg = ChatMessage::assistant("Let me check");
    msg.tool_calls = Some(vec![ToolCallRequest {
        id: "toolu_02".to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: "read_file".to_string(),
            arguments: r#"{"path":"a.rs"}"#.to_string(),
        },
    }]);

    let json = anthropic_message_to_json(&&msg);
    assert_eq!(json["role"], "assistant");
    assert_eq!(json["content"][0]["type"], "text");
    assert_eq!(json["content"][0]["text"], "Let me check");
    assert_eq!(json["content"][1]["type"], "tool_use");
    assert_eq!(json["content"][1]["id"], "toolu_02");
    assert_eq!(json["content"][1]["name"], "read_file");
    assert_eq!(json["content"][1]["input"]["path"], "a.rs");
}
