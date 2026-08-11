//! Anthropic (Claude) LLM provider implementation.

use crate::config::LlmConfig;
use crate::llm::{
    ChatMessage, FinishReason, FunctionCall, LlmProvider, LlmResponse, ToolCallRequest,
    ToolDefinition, Usage,
};
use async_trait::async_trait;

/// Anthropic Claude provider.
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

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
        Ok(Self {
            api_key,
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        let url = "https://api.anthropic.com/v1/messages";

        // Build system prompt from messages
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
            "model": self.model,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
            "messages": chat_messages.iter().map(anthropic_message_to_json).collect::<Vec<_>>(),
        });

        if !system_prompt.is_empty() {
            body["system"] = serde_json::Value::String(system_prompt);
        }

        if !tool_defs.is_empty() {
            body["tools"] = serde_json::to_value(&tool_defs)?;
        }

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
