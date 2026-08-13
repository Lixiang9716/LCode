//! Anthropic (Claude) LLM provider implementation.

use crate::config::LlmConfig;
use crate::llm::sse::{sse_stream, SseData};
use crate::llm::{
    ChatMessage, FinishReason, LlmProvider, LlmResponse, StreamEvent, ToolDefinition,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// Anthropic Claude provider.
///
/// Supports Anthropic-compatible third-party endpoints (DeepSeek, Kimi,
/// MiniMax, GLM, ...) via `LlmConfig::api_base`; when unset, the official
/// `https://api.anthropic.com/v1` endpoint is used.
/// Re-export of [`crate::llm::anthropic_parse::parse_anthropic_response`]
/// (kept in a separate file to respect the style limit).
#[doc(hidden)]
pub use crate::llm::anthropic_parse::parse_anthropic_response;

pub struct AnthropicProvider {
    api_key: String,
    /// Switched at runtime via [`LlmProvider::set_model`] (fallback
    /// failover); interior mutability because `chat` takes `&self`.
    model: Mutex<String>,
    api_base: String,
    /// Current max_tokens budget; raised at runtime via
    /// [`LlmProvider::set_max_tokens`] when a response is truncated.
    max_tokens: AtomicU32,
    temperature: f32,
    client: reqwest::Client,
    /// Provider label used in error messages (e.g. "deepseek" when the
    /// user configured the deepseek alias) instead of a hardcoded
    /// "Anthropic".
    label: String,
    /// Disable the provider's thinking mode when configured.
    thinking_disabled: bool,
    /// Reasoning effort tier for DeepSeek v4 (`low`/`high`/`max`), sent
    /// as `output_config: {effort}` — the only effort knob DeepSeek's
    /// Anthropic-compatible endpoint supports (measured: `output_config
    /// {effort: low}` drops the 88-token thinking template to 9 input
    /// tokens; a top-level `reasoning` field is tolerated but ignored).
    /// Only populated for DeepSeek endpoints.
    reasoning_effort: Option<String>,
    /// DeepSeek's Anthropic-compatible endpoint demands that every
    /// assistant message carry a `thinking` block when thinking mode is
    /// on (400 otherwise), but the real hidden reasoning is dropped on
    /// purpose — replaying it would re-bill it every turn. An empty
    /// placeholder block satisfies the endpoint at zero context cost.
    inject_thinking: bool,
}

/// Default Anthropic API base URL.
const DEFAULT_API_BASE: &str = "https://api.anthropic.com/v1";

impl AnthropicProvider {
    /// Create a new Anthropic provider from configuration.
    pub fn new(config: &LlmConfig) -> anyhow::Result<Self> {
        if secrecy::ExposeSecret::expose_secret(&config.api_key).is_empty() {
            // Also check ANTHROPIC_API_KEY env var
            let env_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
            if env_key.is_empty() {
                anyhow::bail!(
                    "Anthropic API key is required. Set it via `lcode config set llm.api_key <key>` or set the ANTHROPIC_API_KEY environment variable"
                );
            }
            return Self::new_with_key(env_key, config);
        }
        Self::new_with_key(
            secrecy::ExposeSecret::expose_secret(&config.api_key).to_string(),
            config,
        )
    }

    #[doc(hidden)]
    pub fn new_with_key(api_key: String, config: &LlmConfig) -> anyhow::Result<Self> {
        let api_base = config.api_base.clone().unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        let label = if config.provider.is_empty() {
            "anthropic".to_string()
        } else {
            config.provider.clone()
        };
        // DeepSeek-only wire extensions (exact host match, not a
        // substring): native Anthropic validates real thinking blocks
        // and rejects unknown fields; other third parties may not
        // understand them either.
        let deepseek = crate::llm::is_deepseek_endpoint(&api_base);
        let reasoning_effort =
            if deepseek { config.reasoning_effort.map(|e| e.as_str().to_string()) } else { None };
        let inject_thinking = !config.thinking_disabled && deepseek;
        Ok(Self {
            api_key,
            model: Mutex::new(config.model.clone()),
            api_base,
            max_tokens: AtomicU32::new(config.max_tokens),
            temperature: config.temperature,
            // Cap connect at 30s and the whole request at 5 minutes so
            // a stalled connection cannot hang the agent loop forever.
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .expect("reqwest client builds"),
            label,
            thinking_disabled: config.thinking_disabled,
            reasoning_effort,
            inject_thinking,
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

        let body = self.build_body(messages, tools, false)?;

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
            anyhow::bail!("{} API error ({}): {}", self.label, status, text);
        }

        let data: serde_json::Value = response.json().await?;
        parse_anthropic_response(&data)
    }

    /// Real streaming (G11): the same messages body with `stream: true`;
    /// the SSE response maps `content_block_delta` (text_delta) events to
    /// [`StreamEvent::TextDelta`] and the final `message_delta`'s
    /// `stop_reason` to [`StreamEvent::Done`]. End-of-stream sentinels
    /// (`[DONE]`) emit nothing: they must not overwrite the real
    /// `stop_reason` (a `tool_use` stop followed by the sentinel would
    /// otherwise collapse to `Stop` and silently drop the tool call).
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>> {
        let url = format!("{}/messages", self.api_base.trim_end_matches('/'));
        let body = self.build_body(messages, tools, true)?;
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
            anyhow::bail!("{} API error ({}): {}", self.label, status, text);
        }

        let stream = sse_stream(response).filter_map(|item| async move {
            match item {
                Ok(SseData::Json(data)) => anthropic_stream_event(&data).map(Ok),
                Ok(SseData::Done) => None,
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
        if self.model.lock().unwrap().is_empty() {
            anyhow::bail!("Anthropic model is not set");
        }
        Ok(())
    }

    fn set_max_tokens(&self, n: u32) {
        self.max_tokens.store(n, Ordering::Relaxed);
    }

    fn set_model(&self, model: String) {
        *self.model.lock().unwrap() = model;
    }
}

impl AnthropicProvider {
    /// Build the messages request body; `stream` adds `"stream": true` so
    /// the API answers with SSE events instead of a single response.
    ///
    /// Prefix-completion requests are rejected: DeepSeek serves that
    /// feature on the OpenAI-format beta endpoint only.
    #[doc(hidden)]
    pub fn build_body(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        stream: bool,
    ) -> anyhow::Result<serde_json::Value> {
        if crate::llm::has_prefix(messages) {
            anyhow::bail!(
                "prefix completion is not supported on the Anthropic-format endpoint; \
                 use provider = \"openai_compatible\" with api_base = \
                 \"https://api.deepseek.com\" (beta endpoint) instead"
            );
        }
        let (system_prompt, chat_messages) = split_system_messages(messages);

        // Convert tools to Anthropic format. Server-side tools (e.g.
        // DeepSeek `web_search`) use their own wire shape: the API
        // executes them and returns the result in-band.
        let tool_defs: Vec<serde_json::Value> = tools.iter().map(anthropic_tool_json).collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
            "stream": stream,
            "messages": anthropic_messages_to_json(&chat_messages, self.inject_thinking),
        });
        if self.thinking_disabled {
            body["thinking"] = serde_json::json!({ "type": "disabled" });
        } else if let Some(effort) = &self.reasoning_effort {
            body["output_config"] = serde_json::json!({ "effort": effort });
        }

        if !system_prompt.is_empty() {
            body["system"] = serde_json::Value::String(system_prompt);
        }

        if !tool_defs.is_empty() {
            body["tools"] = serde_json::to_value(&tool_defs).unwrap();
        }

        Ok(body)
    }
}

/// Serialize one tool definition into the Anthropic wire format:
/// server-side tools (e.g. DeepSeek `web_search`) use their own shape —
/// the API executes them and returns the result in-band — while client
/// tools keep the function-tool shape.
fn anthropic_tool_json(t: &ToolDefinition) -> serde_json::Value {
    if let Some(server) = &t.server {
        return serde_json::json!({
            "type": server.tool_type,
            "name": server.name,
            "max_queries": server.max_queries,
        });
    }
    serde_json::json!({
        "name": t.function.name,
        "description": t.function.description,
        "input_schema": t.function.parameters,
    })
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
            let usage = data.get("usage").map(anthropic_usage);
            Some(StreamEvent::Done {
                reason: match stop_reason {
                    Some("end_turn") => FinishReason::Stop,
                    Some("max_tokens") => FinishReason::Length,
                    Some("tool_use") => FinishReason::ToolCalls,
                    _ => FinishReason::Unknown,
                },
                usage,
            })
        }
        // End-of-message sentinel: emits nothing. The finish reason
        // comes exclusively from `message_delta.stop_reason`; a fallback
        // `Done(Stop)` here would overwrite a `tool_use` stop and make
        // the executor treat a tool-call stream as a silent empty text.
        Some("message_stop") => None,
        _ => None,
    }
}

/// Parse an Anthropic-style usage block: `cache_read_input_tokens` maps
/// to cache hits, everything else in the input is a cache miss, and
/// `output_tokens` cover both reasoning and final text on DeepSeek's
/// Anthropic-compatible endpoint.
pub(crate) fn anthropic_usage(u: &serde_json::Value) -> crate::llm::Usage {
    let input: u32 = u["input_tokens"].as_u64().unwrap_or(0) as u32;
    let output: u32 = u["output_tokens"].as_u64().unwrap_or(0) as u32;
    let hit: u32 = u["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32;
    crate::llm::Usage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: input + output,
        cache_hit_tokens: hit,
        cache_miss_tokens: input.saturating_sub(hit),
        reasoning_tokens: 0,
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

/// Serialize a conversation for the Anthropic wire format.
///
/// Anthropic requires every `tool_use` of an assistant message to be
/// paired by a `tool_result` in the immediately following user message,
/// so consecutive tool-result messages are merged into a single user
/// message carrying all result blocks in order (E2E regression: parallel
/// tool calls in one assistant message were serialized as separate user
/// messages and rejected with 400 by strict Anthropic-compatible APIs).
#[doc(hidden)]
pub fn anthropic_messages_to_json(
    messages: &[&ChatMessage],
    inject_thinking: bool,
) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role == crate::llm::Role::Tool {
            let mut results = Vec::new();
            while i < messages.len() && messages[i].role == crate::llm::Role::Tool {
                let tool = &messages[i];
                if let Some(ref tool_id) = tool.tool_call_id {
                    results.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_id,
                        "content": tool.content,
                    }));
                }
                i += 1;
            }
            out.push(serde_json::json!({ "role": "user", "content": results }));
        } else {
            out.push(anthropic_message_to_json(&messages[i], inject_thinking));
            i += 1;
        }
    }
    out
}

/// Convert a ChatMessage to Anthropic-compatible JSON. When
/// `inject_thinking` is set (DeepSeek endpoint, thinking mode on), every
/// assistant message leads with an empty `thinking` block — the endpoint
/// demands one, and an empty placeholder avoids re-billing the hidden
/// reasoning on every turn.
#[doc(hidden)]
pub fn anthropic_message_to_json(msg: &&ChatMessage, inject_thinking: bool) -> serde_json::Value {
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
        push_thinking_placeholder(&mut content_parts, inject_thinking);

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
    } else if inject_thinking && msg.role == crate::llm::Role::Assistant {
        // Plain assistant message: the placeholder forces the array
        // content form (thinking block first, then the text). An empty
        // text block is skipped — strict APIs reject empty text blocks.
        let mut parts = vec![serde_json::json!({ "type": "thinking", "thinking": "" })];
        if !msg.content.is_empty() {
            parts.push(serde_json::json!({ "type": "text", "text": msg.content }));
        }
        json["content"] = serde_json::to_value(parts).unwrap();
    } else {
        json["content"] = serde_json::json!(msg.content);
    }

    json
}

/// Push the empty thinking placeholder that DeepSeek's Anthropic-compatible
/// endpoint demands on every assistant message when thinking mode is on.
fn push_thinking_placeholder(parts: &mut Vec<serde_json::Value>, inject: bool) {
    if inject {
        parts.push(serde_json::json!({ "type": "thinking", "thinking": "" }));
    }
}
