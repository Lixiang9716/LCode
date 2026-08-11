//! OpenAI LLM provider implementation.
//!
//! Supports both OpenAI's native API and OpenAI-compatible APIs
//! (Ollama, vLLM, local models, etc.)

use async_trait::async_trait;
use crate::config::LlmConfig;
use crate::llm::{
    ChatMessage, FinishReason, LlmResponse, LlmProvider, ToolCallRequest, ToolDefinition, Usage,
};

/// OpenAI / OpenAI-compatible provider.
pub struct OpenAiProvider {
    api_key: String,
    model: String,
    api_base: String,
    max_tokens: u32,
    temperature: f32,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Create a new OpenAI provider from configuration.
    pub fn new(config: &LlmConfig) -> anyhow::Result<Self> {
        if config.api_key.is_empty() {
            anyhow::bail!("OpenAI API key is required. Set it via: lcode config set llm.api_key <key>");
        }

        let api_base = config
            .api_base
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        Ok(Self {
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            api_base,
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        let url = format!("{}/chat/completions", self.api_base.trim_end_matches('/'));

        // Build the request body
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages.iter().map(|m| message_to_json(m)).collect::<Vec<_>>(),
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
            body["tool_choice"] = serde_json::json!("auto");
        }

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await?;
            anyhow::bail!("OpenAI API error ({}): {}", status, text);
        }

        let data: serde_json::Value = response.json().await?;
        parse_response(&data)
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.api_key.is_empty() {
            anyhow::bail!("OpenAI API key is not set");
        }
        if self.model.is_empty() {
            anyhow::bail!("OpenAI model is not set");
        }
        Ok(())
    }
}

/// Convert an internal ChatMessage to OpenAI-compatible JSON.
fn message_to_json(msg: &ChatMessage) -> serde_json::Value {
    let role_str = match msg.role {
        crate::llm::Role::System => "system",
        crate::llm::Role::User => "user",
        crate::llm::Role::Assistant => "assistant",
        crate::llm::Role::Tool => "tool",
    };

    let mut json = serde_json::json!({
        "role": role_str,
        "content": msg.content,
    });

    if let Some(ref tool_call_id) = msg.tool_call_id {
        json["tool_call_id"] = serde_json::Value::String(tool_call_id.clone());
    }

    if let Some(ref tool_calls) = msg.tool_calls {
        json["tool_calls"] = serde_json::to_value(tool_calls).unwrap();
    }

    json
}

/// Parse OpenAI response JSON into an LlmResponse.
fn parse_response(data: &serde_json::Value) -> anyhow::Result<LlmResponse> {
    let choice = &data["choices"][0];
    let message = &choice["message"];

    let content = message["content"].as_str().unwrap_or("").to_string();

    let tool_calls = if let Some(tc) = message.get("tool_calls") {
        Some(
            serde_json::from_value::<Vec<ToolCallRequest>>(tc.clone())
                .unwrap_or_default(),
        )
    } else {
        None
    };

    let finish_reason = match choice["finish_reason"].as_str() {
        Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    };

    let usage = data.get("usage").map_or(Usage::default(), |u| Usage {
        prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
        total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
    });

    Ok(LlmResponse {
        content,
        tool_calls,
        usage,
        finish_reason,
    })
}
