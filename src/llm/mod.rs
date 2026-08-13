//! LLM provider abstraction layer.
//!
//! Supports multiple LLM backends through a common `LlmProvider` trait:
//! - OpenAI (GPT-4, GPT-4o, etc.)
//! - Anthropic (Claude 3.5, Claude 4, etc.)
//! - OpenAI-compatible providers (Ollama, vLLM, etc.)

use serde::{Deserialize, Serialize};

pub mod anthropic;
pub mod anthropic_parse;
pub mod openai;
pub mod provider;
pub mod sse;
pub mod usage_cost;

pub use provider::LlmProvider;
pub use usage_cost::{estimate_cost, format_cost, pricing_for, usage_summary, Pricing};

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
    /// Prefix-completion marker (DeepSeek beta): an assistant message
    /// carrying `Some(true)` asks the API to continue generating from
    /// exactly this content instead of starting a fresh reply. Only the
    /// OpenAI-format provider honours it; the Anthropic-format provider
    /// rejects such requests with a clear error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<bool>,
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
    /// Server-side tool spec (e.g. DeepSeek's `web_search`). When set,
    /// the Anthropic-format provider serializes this entry as a server
    /// tool (`{"type": ..., "name": ..., "max_queries": ...}`) and the
    /// API executes it itself, returning the result in-band; the
    /// OpenAI-format provider skips it (chat completions has no server
    /// tools).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerToolSpec>,
}

/// Declaration of a server-side tool (executed by the API, not locally).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerToolSpec {
    /// Wire type of the server tool, e.g. `web_search_20260209` on
    /// DeepSeek's Anthropic-compatible endpoint.
    pub tool_type: String,
    /// Local name the model calls it by (e.g. `web_search`).
    pub name: String,
    /// Optional query budget for search-style server tools.
    pub max_queries: Option<u32>,
}

/// A server-side tool result carried back by the API (already executed:
/// there is nothing for the client to run, only to record).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerToolResult {
    /// ID linking the result to the server tool call block.
    pub id: String,
    /// Tool name (e.g. `web_search`).
    pub name: String,
    /// Flattened result text to feed back into the conversation.
    pub content: String,
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
    /// Server-side tool results (web search etc.): already executed by
    /// the API; the caller records them like local tool results.
    pub server_results: Vec<ServerToolResult>,
    /// Token usage stats
    pub usage: Usage,
    /// Finish reason
    pub finish_reason: FinishReason,
}

/// Token usage statistics.
///
/// The cache and reasoning fields are DeepSeek-specific and zero on
/// providers that do not report them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Input tokens served from the context cache (billed at the
    /// discounted cache-hit rate).
    pub cache_hit_tokens: u32,
    /// Input tokens processed fresh (billed at the standard input rate).
    pub cache_miss_tokens: u32,
    /// Completion tokens spent on hidden reasoning (thinking mode).
    pub reasoning_tokens: u32,
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
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            prefix: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            prefix: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            prefix: None,
        }
    }

    /// Assistant message carrying the DeepSeek beta prefix-completion
    /// marker: the API continues generating from `content` instead of
    /// starting a fresh reply.
    pub fn assistant_prefix(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
            prefix: Some(true),
        }
    }

    pub fn tool(content: impl Into<String>, tool_call_id: String) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(tool_call_id),
            tool_calls: None,
            prefix: None,
        }
    }
}

/// Does the request carry a prefix-completion marker message?
pub fn has_prefix(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|m| m.prefix == Some(true))
}
