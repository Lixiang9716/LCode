//! Agent executor — runs the main agent loop.
//!
//! The executor manages the conversation loop between the user,
//! the LLM, and the tool system. It handles:
//! - Sending messages to the LLM
//! - Parsing tool call requests
//! - Executing tools (with user approval if required)
//! - Feeding results back to the LLM

use crate::agent::{ConversationMemory, Planner};
use crate::llm::{FinishReason, LlmProvider, ToolDefinition};
use crate::tools::ToolRegistry;

/// The executor drives the agent loop.
pub struct Executor<'a> {
    provider: &'a dyn LlmProvider,
    registry: &'a ToolRegistry,
    auto_approve: bool,
}

impl<'a> Executor<'a> {
    /// Create a new executor.
    pub fn new(provider: &'a dyn LlmProvider, registry: &'a ToolRegistry, auto_approve: bool) -> Self {
        Self {
            provider,
            registry,
            auto_approve,
        }
    }

    /// Run the agent loop for a given task.
    pub async fn run(
        &mut self,
        task: &str,
        planner: &Planner,
        mut memory: ConversationMemory,
        max_turns: u32,
    ) -> anyhow::Result<()> {
        let plan = planner.create_plan(task);
        memory.add_user(format!("Task: {}", task));

        let tool_defs: Vec<ToolDefinition> = self.registry.definitions();

        let mut turn = 0u32;

        println!("\n🤖 LCode Agent starting...\n");
        println!("Task: {}\n", task);

        loop {
            if turn >= max_turns {
                println!("\n⚠️  Reached maximum turns ({}). Stopping.", max_turns);
                break;
            }

            turn += 1;
            tracing::debug!(turn, "Agent turn");

            // Get the current conversation context
            let context = memory.get_context();

            // Send to LLM
            let response = self.provider.chat(&context, &tool_defs).await?;

            // Handle the response
            match response.finish_reason {
                FinishReason::ToolCalls => {
                    if let Some(ref tool_calls) = response.tool_calls {
                        // Print the assistant's text content if any
                        if !response.content.is_empty() {
                            println!("\n{}", response.content);
                        }

                        // Add the assistant message with tool calls to memory
                        memory.add_assistant_with_tool_calls(response.content, tool_calls.clone());

                        // Execute each tool call
                        for tc in tool_calls {
                            self.handle_tool_call(tc, &mut memory).await?;
                        }
                    }
                }
                FinishReason::Stop | FinishReason::Length => {
                    // Final response — no more tool calls
                    println!("\n{}", response.content);
                    memory.add_assistant(response.content);
                    break;
                }
                FinishReason::ContentFilter => {
                    println!("\n⚠️  Response blocked by content filter.");
                    break;
                }
                FinishReason::Unknown => {
                    // Assume stop — just output the content
                    if !response.content.is_empty() {
                        println!("\n{}", response.content);
                    }
                    break;
                }
            }
        }

        println!("\n✅ Task completed in {} turns.", turn);
        let summary = response_usage_summary(&memory);
        println!(
            "Tokens used: ~{} prompt, ~{} completion, ~{} total",
            summary.0, summary.1, summary.2
        );

        Ok(())
    }

    /// Handle a single tool call: ask for approval, execute, and record result.
    async fn handle_tool_call(
        &self,
        tc: &crate::llm::ToolCallRequest,
        memory: &mut ConversationMemory,
    ) -> anyhow::Result<()> {
        let tool_name = &tc.function.name;
        let args = &tc.function.arguments;

        // Parse arguments
        let parsed_args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();

        // Display what the agent wants to do
        println!("\n🔧 Tool call: {}(", tool_name);
        println!("   args: {}", serde_json::to_string_pretty(&parsed_args)?);
        print!(")");

        // Request approval if needed
        if !self.auto_approve {
            print!("\n   Execute? [y/N] ");
            use std::io::Write;
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let input = input.trim().to_lowercase();

            if input != "y" && input != "yes" {
                println!("   ⏭️  Skipped (user declined).");
                memory.add_tool_result(
                    format!("Tool call declined by user: {}", tool_name),
                    tc.id.clone(),
                );
                return Ok(());
            }
        }

        // Execute the tool
        match self.registry.execute(tool_name, &parsed_args) {
            Ok(result) => {
                let result_str = format!("{}", result);
                println!("   ✅ Result: {}", truncate(&result_str, 500));
                memory.add_tool_result(result_str, tc.id.clone());
            }
            Err(e) => {
                let error_str = format!("Error executing tool: {}", e);
                println!("   ❌ {}", error_str);
                memory.add_tool_result(error_str, tc.id.clone());
            }
        }

        Ok(())
    }
}

/// Truncate a string to max_len characters, adding "..." if truncated.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let boundary = s[..max_len]
        .char_indices()
        .last()
        .map(|(i, _)| i)
        .unwrap_or(max_len);
    &s[..boundary]
}

/// Get a summary of token usage from the conversation memory.
fn response_usage_summary(memory: &ConversationMemory) -> (usize, usize, usize) {
    let prompt_tokens = memory.approximate_tokens();
    let completion_tokens = 0; // Would be tracked per-response in a full implementation
    (prompt_tokens, completion_tokens, prompt_tokens + completion_tokens)
}
