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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, Role};

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
}
