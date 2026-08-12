//! Anthropic (Claude) LLM provider implementation.

use crate::config::LlmConfig;
use crate::llm::sse::{sse_stream, SseData};
use crate::llm::{
    ChatMessage, FinishReason, FunctionCall, LlmProvider, LlmResponse, StreamEvent,
    ToolCallRequest, ToolDefinition, Usage,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;

/// Anthropic Claude provider.
///
/// Supports Anthropic-compatible third-party endpoints (DeepSeek, Kimi,
/// MiniMax, GLM, ...) via `LlmConfig::api_base`; when unset, the official
/// `https://api.anthropic.com/v1` endpoint is used.
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    api_base: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

/// Default Anthropic API base URL.
const DEFAULT_API_BASE: &str = "https://api.anthropic.com/v1";

impl AnthropicProvider {
    /// Create a new Anthropic provider from configuration.
    pub fn new(config: &LlmConfig) -> anyhow::Result<Self> {
        if config.api_key.is_empty() {
            // Also check ANTHROPIC_API_KEY env var
            let env_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
            if env_key.is_empty() {
                anyhow::bail!(
                    "Anthropic API key is required. Set it via `lcode config set llm.api_key <key>` or set the ANTHROPIC_API_KEY environment variable"
                );
            }
            return Self::new_with_key(env_key, config);
        }
        Self::new_with_key(config.api_key.clone(), config)
    }

    fn new_with_key(api_key: String, config: &LlmConfig) -> anyhow::Result<Self> {
        let api_base = config.api_base.clone().unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        Ok(Self {
            api_key,
            model: config.model.clone(),
            api_base,
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            client: reqwest::Client::new(),
        })
    }

    /// API base URL used for requests (defaults to
    /// `https://api.anthropic.com/v1`).
    pub fn api_base(&self) -> &str {
        &self.api_base
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        // `api_base` may be an Anthropic-compatible third-party endpoint
        // (e.g. `https://api.deepseek.com/anthropic`); the messages route
        // is appended the same way for all of them.
        let url = format!("{}/messages", self.api_base.trim_end_matches('/'));
        let body = stream_body(self, messages, tools, false);
        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await?;
            anyhow::bail!("Anthropic API error ({}): {}", status, text);
        }

        let data: serde_json::Value = response.json().await?;
        parse_anthropic_response(&data)
    }

    /// Real streaming (G11): the same messages body with `stream: true`;
    /// the SSE response maps `content_block_delta` (text_delta) events to
    /// [`StreamEvent::TextDelta`] and the final `message_delta`'s
    /// `stop_reason` to [`StreamEvent::Done`]. `[DONE]` and `message_stop`
    /// are safe fallbacks that end the stream with `Done(Stop)`.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>> {
        let url = format!("{}/messages", self.api_base.trim_end_matches('/'));
        let body = stream_body(self, messages, tools, true);
        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await?;
            anyhow::bail!("Anthropic API error ({}): {}", status, text);
        }

        let stream = sse_stream(response).filter_map(|item| async move {
            match item {
                Ok(SseData::Json(data)) => anthropic_stream_event(&data).map(Ok),
                Ok(SseData::Done) => Some(Ok(StreamEvent::Done(FinishReason::Stop))),
                Ok(SseData::Other(_)) => None,
                Err(e) => Some(Err(e)),
            }
        });
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.api_key.is_empty() {
            anyhow::bail!("Anthropic API key is not set");
        }
        if self.model.is_empty() {
            anyhow::bail!("Anthropic model is not set");
        }
        Ok(())
    }
}

/// Build the messages request body; `stream` adds `"stream": true` so the
/// API answers with SSE events instead of a single response.
fn stream_body(
    provider: &AnthropicProvider,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    stream: bool,
) -> serde_json::Value {
    let (system_prompt, chat_messages) = split_system_messages(messages);

    // Convert tools to Anthropic format
    let tool_defs: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.function.name,
                "description": t.function.description,
                "input_schema": t.function.parameters,
            })
        })
        .collect();

    let mut body = serde_json::json!({
        "model": provider.model,
        "max_tokens": provider.max_tokens,
        "temperature": provider.temperature,
        "stream": stream,
        "messages": chat_messages.iter().map(anthropic_message_to_json).collect::<Vec<_>>(),
    });

    if !system_prompt.is_empty() {
        body["system"] = serde_json::Value::String(system_prompt);
    }

    if !tool_defs.is_empty() {
        body["tools"] = serde_json::to_value(&tool_defs).unwrap();
    }

    body
}

/// Map one SSE event of an Anthropic streaming response to a
/// [`StreamEvent`]: `content_block_delta` (text_delta) → `TextDelta`,
/// `message_delta` → `Done` with the mapped stop reason. Other event
/// types (`message_start`, `content_block_start`, `ping`, ...) map to
/// `None` and are skipped by the stream consumer.
#[doc(hidden)]
pub fn anthropic_stream_event(data: &serde_json::Value) -> Option<StreamEvent> {
    match data["type"].as_str() {
        Some("content_block_delta") => {
            let delta = &data["delta"];
            if delta["type"].as_str() == Some("text_delta") {
                let text = delta["text"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    return Some(StreamEvent::TextDelta(text.to_string()));
                }
            }
            // input_json_delta (tool-use arguments) and other delta types
            // carry no user-visible text.
            None
        }
        Some("message_delta") => {
            let stop_reason = data["delta"]["stop_reason"].as_str();
            Some(StreamEvent::Done(match stop_reason {
                Some("end_turn") => FinishReason::Stop,
                Some("max_tokens") => FinishReason::Length,
                Some("tool_use") => FinishReason::ToolCalls,
                _ => FinishReason::Unknown,
            }))
        }
        // Final event of a message; a fallback in case `message_delta`
        // was missing (some compatible endpoints omit it).
        Some("message_stop") => Some(StreamEvent::Done(FinishReason::Stop)),
        _ => None,
    }
}

/// Extract system prompt from messages and return remaining conversation.
#[doc(hidden)]
pub fn split_system_messages(messages: &[ChatMessage]) -> (String, Vec<&ChatMessage>) {
    let system_parts: Vec<&str> = messages
        .iter()
        .filter(|m| matches!(m.role, crate::llm::Role::System))
        .map(|m| m.content.as_str())
        .collect();

    let system_prompt = system_parts.join("\n\n");

    let chat_messages: Vec<&ChatMessage> =
        messages.iter().filter(|m| !matches!(m.role, crate::llm::Role::System)).collect();

    (system_prompt, chat_messages)
}

/// Convert a ChatMessage to Anthropic-compatible JSON.
#[doc(hidden)]
pub fn anthropic_message_to_json(msg: &&ChatMessage) -> serde_json::Value {
    let role = match msg.role {
        crate::llm::Role::User => "user",
        crate::llm::Role::Assistant => "assistant",
        // Tool results are sent as user messages in Anthropic format
        crate::llm::Role::Tool => "user",
        // System messages are handled separately
        crate::llm::Role::System => "user",
    };

    let mut json = serde_json::json!({
        "role": role,
    });

    // Handle tool results
    if msg.role == crate::llm::Role::Tool {
        if let Some(ref tool_id) = msg.tool_call_id {
            json["content"] = serde_json::json!([{
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": msg.content,
            }]);
        }
    }
    // Handle assistant messages with tool calls
    else if let Some(ref tool_calls) = msg.tool_calls {
        let mut content_parts: Vec<serde_json::Value> = Vec::new();

        if !msg.content.is_empty() {
            content_parts.push(serde_json::json!({
                "type": "text",
                "text": msg.content,
            }));
        }

        for tc in tool_calls {
            content_parts.push(serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.function.name,
                "input": serde_json::from_str::<serde_json::Value>(&tc.function.arguments).unwrap_or_default(),
            }));
        }

        json["content"] = serde_json::to_value(content_parts).unwrap();
    } else {
        json["content"] = serde_json::json!(msg.content);
    }

    json
}

/// Append a `text` content block's contents to the accumulated text.
fn parse_text_block(block: &serde_json::Value, text_content: &mut String) {
    if let Some(text) = block["text"].as_str() {
        text_content.push_str(text);
    }
}

/// Extract a `tool_use` content block into a [`ToolCallRequest`].
#[doc(hidden)]
pub fn parse_tool_use(
    block: &serde_json::Value,
    tool_calls: &mut Vec<ToolCallRequest>,
) -> anyhow::Result<()> {
    tool_calls.push(ToolCallRequest {
        id: block["id"].as_str().unwrap_or("").to_string(),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: block["name"].as_str().unwrap_or("").to_string(),
            arguments: serde_json::to_string(&block["input"])?,
        },
    });
    Ok(())
}

/// Parse Anthropic response into an LlmResponse.
#[doc(hidden)]
pub fn parse_anthropic_response(data: &serde_json::Value) -> anyhow::Result<LlmResponse> {
    let content_blocks = data["content"].as_array();
    let mut text_content = String::new();
    let mut tool_calls: Vec<ToolCallRequest> = Vec::new();

    if let Some(blocks) = content_blocks {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => parse_text_block(block, &mut text_content),
                Some("tool_use") => parse_tool_use(block, &mut tool_calls)?,
                _ => {}
            }
        }
    }

    let finish_reason = match data["stop_reason"].as_str() {
        Some("end_turn") => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        _ => FinishReason::Unknown,
    };

    let usage = data.get("usage").map_or(Usage::default(), |u| Usage {
        prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
        total_tokens: (u["input_tokens"].as_u64().unwrap_or(0)
            + u["output_tokens"].as_u64().unwrap_or(0)) as u32,
    });

    Ok(LlmResponse {
        content: text_content,
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        usage,
        finish_reason,
    })
}
