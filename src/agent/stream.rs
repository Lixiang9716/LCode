//! Streaming LLM consumption (typewriter effect).
//!
//! [`Executor::call_llm`] switches between the plain `chat` call and the
//! streaming path; [`Executor::chat_stream`] publishes every delta as its
//! own [`AgentEvent::TextDelta`] so observers (REPL, audit log) see
//! tokens arrive incrementally, then reassembles the full response.

use crate::agent::event::AgentEvent;
use crate::agent::executor::Executor;
use crate::agent::runtime::AgentRuntime;
use crate::llm::{ChatMessage, FinishReason, LlmResponse, StreamEvent, ToolDefinition, Usage};
use futures::StreamExt;

impl Executor {
    /// Pick the chat path: streamed (deltas published per token) or the
    /// plain single-response call.
    pub(crate) async fn call_llm(
        &self,
        context: &[ChatMessage],
        tool_defs: &[ToolDefinition],
        stream: bool,
    ) -> anyhow::Result<LlmResponse> {
        if stream {
            self.chat_stream(context, tool_defs).await
        } else {
            self.provider.chat(context, tool_defs).await
        }
    }

    /// Stream a chat completion, publishing every delta as its own
    /// [`AgentEvent::TextDelta`] so observers (REPL, audit log) see the
    /// typewriter effect, then reassemble the full [`LlmResponse`] from
    /// the accumulated text and the `Done` finish reason.
    ///
    /// Streams never carry tool calls, so when the model finishes with
    /// `ToolCalls` we fall back to a single `chat()` call to fetch the
    /// full response including the tool-call arguments (a dual call that
    /// keeps tool calling fully functional). Providers without native
    /// streaming already fall back to `chat()` inside `chat_stream`, so
    /// this path works for every backend.
    pub(crate) async fn chat_stream(
        &self,
        context: &[ChatMessage],
        tool_defs: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        let mut stream = self.provider.chat_stream(context, tool_defs).await?;
        let mut content = String::new();
        let mut finish_reason = FinishReason::Unknown;
        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta(delta) => {
                    publish_delta(&self.runtime, &mut content, delta);
                }
                StreamEvent::Done(reason) => finish_reason = reason,
            }
        }
        if finish_reason == FinishReason::ToolCalls {
            // Streams never carry tool calls: fall back to a plain chat
            // call for the full response (tool calls included). The
            // fallback's text is not published again — the streamed
            // preview already showed it — but memory still gets the
            // authoritative content via handle_response.
            return self.provider.chat(context, tool_defs).await;
        }
        Ok(LlmResponse { content, tool_calls: None, usage: Usage::default(), finish_reason })
    }
}

/// Append a text delta to the accumulated content and publish it as a
/// [`AgentEvent::TextDelta`] event. Concatenating the deltas in arrival
/// order reproduces the full response text.
fn publish_delta(runtime: &AgentRuntime, content: &mut String, delta: String) {
    content.push_str(&delta);
    runtime.publish(AgentEvent::TextDelta { content: delta });
}
