//! Response parsing for the Anthropic wire format.
//!
//! Kept in a separate file so `anthropic.rs` stays under the 500-line
//! style limit. Covers plain text/tool_use blocks plus the server-side
//! web_search blocks (`server_tool_use` + `web_search_tool_result`) of
//! DeepSeek's Anthropic-compatible endpoint.

use crate::llm::anthropic::anthropic_usage;
use crate::llm::{FinishReason, FunctionCall, LlmResponse, ToolCallRequest};

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
    let mut server_results: Vec<crate::llm::ServerToolResult> = Vec::new();

    if let Some(blocks) = content_blocks {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => parse_text_block(block, &mut text_content),
                Some("tool_use") => parse_tool_use(block, &mut tool_calls)?,
                // Server-side tools (DeepSeek web_search): the call block
                // behaves like a tool_use, and the result arrives in the
                // same message — nothing for the client to execute.
                Some("server_tool_use") => parse_tool_use(block, &mut tool_calls)?,
                Some("web_search_tool_result") => {
                    server_results.push(parse_web_search_result(block))
                }
                _ => {}
            }
        }
    }

    // Anthropic wire pairing requires every tool_result to be answered
    // by a tool_use on the previous assistant message. When the API
    // returns a web-search result without its call block, synthesize the
    // matching call so the next request serializes a valid pair.
    let mut fallback_seq = 0u32;
    for result in &server_results {
        if !tool_calls.iter().any(|tc| tc.id == result.id) {
            fallback_seq += 1;
            let id = if result.id == "web_search" {
                format!("web_search-{fallback_seq}")
            } else {
                result.id.clone()
            };
            tool_calls.push(ToolCallRequest {
                id,
                call_type: "function".to_string(),
                function: FunctionCall { name: result.name.clone(), arguments: "{}".to_string() },
            });
        }
    }

    let finish_reason = match data["stop_reason"].as_str() {
        Some("end_turn") => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        _ => FinishReason::Unknown,
    };

    let usage = data.get("usage").map(anthropic_usage).unwrap_or_default();

    Ok(LlmResponse {
        content: text_content,
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        server_results,
        usage,
        finish_reason,
    })
}

/// Cap for one flattened search result: results are untrusted remote
/// content and would otherwise balloon the context, the event bus and
/// the audit log without limit.
const MAX_SEARCH_RESULT_CHARS: usize = 20_000;
/// Cap per rendered item (title/url/snippet/text).
const MAX_SEARCH_ITEM_CHARS: usize = 2_000;

/// Flatten a `web_search_tool_result` block into readable text: source
/// blocks (title + URL + snippet) become markdown-ish reference lines.
/// Every item and the total are length-capped (untrusted content).
fn parse_web_search_result(block: &serde_json::Value) -> crate::llm::ServerToolResult {
    let id = block["tool_use_id"].as_str().unwrap_or("web_search").to_string();
    let name = block["name"].as_str().unwrap_or("web_search").to_string();
    let parts: Vec<String> = block["content"]
        .as_array()
        .map(|content| content.iter().filter_map(render_search_item).collect())
        .unwrap_or_default();
    let content = truncate_chars(&parts.join("\n"), MAX_SEARCH_RESULT_CHARS);
    crate::llm::ServerToolResult { id, name, content }
}

/// Character-boundary-safe truncation to `max` characters.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Render one `web_search_tool_result` content item as a text line:
/// plain `text` blocks pass through, structured source blocks (web_page
/// and similar) become `[title](url): snippet` references.
fn render_search_item(item: &serde_json::Value) -> Option<String> {
    match item["type"].as_str() {
        Some("text") => item["text"]
            .as_str()
            .map(|t| truncate_chars(t, MAX_SEARCH_ITEM_CHARS))
            .filter(|t| !t.is_empty()),
        _ => {
            let title = item["title"].as_str().unwrap_or("");
            let url = item["url"].as_str().unwrap_or("");
            let snippet = item["snippet"].as_str().unwrap_or("");
            if title.is_empty() && url.is_empty() && snippet.is_empty() {
                return None;
            }
            let mut line = String::new();
            if !title.is_empty() && !url.is_empty() {
                line.push_str(&format!("[{title}]({url})"));
            } else if !url.is_empty() {
                line.push_str(url);
            }
            if !snippet.is_empty() {
                if !line.is_empty() {
                    line.push_str(": ");
                }
                line.push_str(&truncate_chars(snippet, MAX_SEARCH_ITEM_CHARS));
            }
            Some(truncate_chars(&line, MAX_SEARCH_ITEM_CHARS))
        }
    }
}
