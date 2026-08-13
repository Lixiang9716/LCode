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
pub mod sse;

pub use provider::LlmProvider;

/// A single event in a streamed chat response.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A text token delta.
    TextDelta(String),
    /// The stream finished with this reason; `usage` carries the final
    /// usage block when the endpoint emits one (e.g. Anthropic-style
    /// `message_delta.usage`).
    Done { reason: FinishReason, usage: Option<Usage> },
}

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
#[derive(Debug, Clone, Default, PartialEq)]
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
