//! Common LLM provider trait.
//!
//! All LLM backends implement this trait to provide a uniform interface
//! for chat completion with tool calling support.

use async_trait::async_trait;
use crate::llm::{ChatMessage, LlmResponse, ToolDefinition};

/// Trait that all LLM providers must implement.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a chat completion request to the LLM.
    ///
    /// # Arguments
    /// - `messages`: The conversation history
    /// - `tools`: Available tool definitions for the model to call
    /// - `stream`: Whether to stream the response token by token
    ///
    /// # Returns
    /// The model's response, potentially including tool calls.
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse>;

    /// Get the provider name (e.g., "openai", "anthropic").
    fn name(&self) -> &str;

    /// Validate that the provider is properly configured.
    fn validate(&self) -> anyhow::Result<()>;
}
