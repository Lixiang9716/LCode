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
                    "Anthropic API key is required. Set it via:\n  \
                     lcode config set llm.api_key <key>\n  \
                     or set ANTHROPIC_API_KEY environment variable"
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
fn split_system_messages(messages: &[ChatMessage]) -> (String, Vec<&ChatMessage>) {
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
fn anthropic_message_to_json(msg: &&ChatMessage) -> serde_json::Value {
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

/// Extract a `tool_use` content block into a [`ToolCallRequest`].
fn parse_tool_use(
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
fn parse_anthropic_response(data: &serde_json::Value) -> anyhow::Result<LlmResponse> {
    let content_blocks = data["content"].as_array();
    let mut text_content = String::new();
    let mut tool_calls: Vec<ToolCallRequest> = Vec::new();

    if let Some(blocks) = content_blocks {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(text) = block["text"].as_str() {
                        text_content.push_str(text);
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, Role, ToolCallRequest};

    #[test]
    fn test_parse_anthropic_text_response() {
        let data = serde_json::json!({
            "content": [{"type": "text", "text": "Hello there"}],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 5, "output_tokens": 3 }
        });

        let resp = parse_anthropic_response(&data).unwrap();
        assert_eq!(resp.content, "Hello there");
        assert!(resp.tool_calls.is_none());
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.prompt_tokens, 5);
        assert_eq!(resp.usage.completion_tokens, 3);
        assert_eq!(resp.usage.total_tokens, 8);
    }

    #[test]
    fn test_parse_anthropic_tool_use_response() {
        let data = serde_json::json!({
            "content": [
                {"type": "text", "text": "Let me write the file."},
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "write_file",
                    "input": {"path": "x.txt", "content": "hi"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 9, "output_tokens": 4 }
        });

        let resp = parse_anthropic_response(&data).unwrap();
        // Text and tool_use blocks are combined; text content preserved.
        assert_eq!(resp.content, "Let me write the file.");
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.usage.total_tokens, 13);

        let tool_calls = resp.tool_calls.expect("tool calls present");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "toolu_01");
        assert_eq!(tool_calls[0].call_type, "function");
        assert_eq!(tool_calls[0].function.name, "write_file");
        let args: serde_json::Value =
            serde_json::from_str(&tool_calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "x.txt");
        assert_eq!(args["content"], "hi");
    }

    #[test]
    fn test_parse_anthropic_stop_reason_mapping() {
        let cases = [
            ("end_turn", FinishReason::Stop),
            ("max_tokens", FinishReason::Length),
            ("tool_use", FinishReason::ToolCalls),
            ("stop_sequence", FinishReason::Unknown),
        ];
        for (reason, expected) in cases {
            let data = serde_json::json!({
                "content": [{"type": "text", "text": "x"}],
                "stop_reason": reason
            });
            assert_eq!(
                parse_anthropic_response(&data).unwrap().finish_reason,
                expected,
                "stop_reason {:?}",
                reason
            );
        }
    }

    #[test]
    fn test_parse_anthropic_empty_and_defaults() {
        let data = serde_json::json!({});
        let resp = parse_anthropic_response(&data).unwrap();
        assert_eq!(resp.content, "");
        assert!(resp.tool_calls.is_none());
        assert_eq!(resp.finish_reason, FinishReason::Unknown);
        assert_eq!(resp.usage.prompt_tokens, 0);
        assert_eq!(resp.usage.completion_tokens, 0);
        assert_eq!(resp.usage.total_tokens, 0);
    }

    #[test]
    fn test_split_system_messages_extracts_system_prompt() {
        let messages = vec![
            ChatMessage::system("You are Claude."),
            ChatMessage::user("hello"),
            ChatMessage::system("Be concise."),
            ChatMessage::assistant("hi"),
            ChatMessage::tool("result", "call_1".to_string()),
        ];

        let (system_prompt, chat) = split_system_messages(&messages);
        assert_eq!(system_prompt, "You are Claude.\n\nBe concise.");
        assert_eq!(chat.len(), 3);
        assert!(
            chat.iter().all(|m| m.role != Role::System),
            "system messages removed from chat history"
        );
        assert_eq!(chat[0].role, Role::User);
        assert_eq!(chat[1].role, Role::Assistant);
        assert_eq!(chat[2].role, Role::Tool);
    }

    #[test]
    fn test_split_system_messages_no_system() {
        let messages = vec![ChatMessage::user("hi"), ChatMessage::assistant("yo")];
        let (system_prompt, chat) = split_system_messages(&messages);
        assert_eq!(system_prompt, "");
        assert_eq!(chat.len(), 2);
    }

    #[test]
    fn test_anthropic_message_to_json_plain_roles() {
        let user = anthropic_message_to_json(&&ChatMessage::user("hi"));
        assert_eq!(user["role"], "user");
        assert_eq!(user["content"], "hi");

        let assistant = anthropic_message_to_json(&&ChatMessage::assistant("yo"));
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "yo");
    }

    #[test]
    fn test_anthropic_message_to_json_tool_result() {
        let msg = ChatMessage::tool("wrote 5 bytes", "toolu_01".to_string());
        let json = anthropic_message_to_json(&&msg);
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"][0]["type"], "tool_result");
        assert_eq!(json["content"][0]["tool_use_id"], "toolu_01");
        assert_eq!(json["content"][0]["content"], "wrote 5 bytes");
    }

    #[test]
    fn test_anthropic_message_to_json_assistant_tool_calls() {
        let mut msg = ChatMessage::assistant("Let me check");
        msg.tool_calls = Some(vec![ToolCallRequest {
            id: "toolu_02".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: r#"{"path":"a.rs"}"#.to_string(),
            },
        }]);

        let json = anthropic_message_to_json(&&msg);
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "Let me check");
        assert_eq!(json["content"][1]["type"], "tool_use");
        assert_eq!(json["content"][1]["id"], "toolu_02");
        assert_eq!(json["content"][1]["name"], "read_file");
        assert_eq!(json["content"][1]["input"]["path"], "a.rs");
    }
}
