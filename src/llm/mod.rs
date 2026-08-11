//! LLM provider abstraction layer.
//!
//! Supports multiple LLM backends through a common `LlmProvider` trait:
//! - OpenAI (GPT-4, GPT-4o, etc.)
//! - Anthropic (Claude 3.5, Claude 4, etc.)
//! - OpenAI-compatible providers (Ollama, vLLM, etc.)

use serde::{Deserialize, Serialize};

pub mod anthropic;
pub mod openai;
pub mod provider;

pub use provider::LlmProvider;

/// Role of a chat message participant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single chat message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Tool call ID (for tool result messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool calls requested by the assistant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRequest>>,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// Function call details within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// A tool definition exposed to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

/// Function definition schema in a tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Response from a chat completion request.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// The assistant's message content
    pub content: String,
    /// Any tool calls requested
    pub tool_calls: Option<Vec<ToolCallRequest>>,
    /// Token usage stats
    pub usage: Usage,
    /// Finish reason
    pub finish_reason: FinishReason,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Reason the model stopped generating.
#[derive(Debug, Clone, PartialEq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Unknown,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into(), tool_call_id: None, tool_calls: None }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), tool_call_id: None, tool_calls: None }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn tool(content: impl Into<String>, tool_call_id: String) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id),
            tool_calls: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
