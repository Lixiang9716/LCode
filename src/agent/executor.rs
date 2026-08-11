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
///
/// Owns the LLM provider and tool registry so it can be constructed
/// with mocks in tests.
pub struct Executor {
    provider: Box<dyn LlmProvider>,
    registry: ToolRegistry,
    auto_approve: bool,
}

impl Executor {
    /// Create a new executor.
    pub fn new(provider: Box<dyn LlmProvider>, registry: ToolRegistry, auto_approve: bool) -> Self {
        Self { provider, registry, auto_approve }
    }

    /// Run the agent loop for a given task.
    ///
    /// Returns the conversation memory after the run so callers (and
    /// tests) can inspect the final message history.
    pub async fn run(
        &mut self,
        task: &str,
        planner: &Planner,
        mut memory: ConversationMemory,
        max_turns: u32,
    ) -> anyhow::Result<ConversationMemory> {
        // The planner output is currently informational; keep the binding
        // explicit for future use.
        let _plan = planner.create_plan(task);
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

        Ok(memory)
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
    let boundary = s[..max_len].char_indices().last().map(|(i, _)| i).unwrap_or(max_len);
    &s[..boundary]
}

/// Get a summary of token usage from the conversation memory.
fn response_usage_summary(memory: &ConversationMemory) -> (usize, usize, usize) {
    let prompt_tokens = memory.approximate_tokens();
    let completion_tokens = 0; // Would be tracked per-response in a full implementation
    (prompt_tokens, completion_tokens, prompt_tokens + completion_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::llm::provider::MockLlmProvider;
    use crate::llm::{ChatMessage, FunctionCall, LlmResponse, Role, ToolCallRequest, Usage};
    use crate::tools::ToolRegistry;
    use serial_test::serial;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Build a `write_file` tool call with the given id and arguments.
    fn write_file_call(id: &str, args: &str) -> ToolCallRequest {
        ToolCallRequest {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: FunctionCall { name: "write_file".to_string(), arguments: args.to_string() },
        }
    }

    fn response(
        content: &str,
        finish_reason: FinishReason,
        tool_calls: Option<Vec<ToolCallRequest>>,
    ) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            tool_calls,
            usage: Usage::default(),
            finish_reason,
        }
    }

    /// Build an executor backed by a mock provider that serves responses
    /// from a queue. Every received message batch is recorded into `seen`.
    fn executor_with_queue(
        responses: Vec<LlmResponse>,
        registry: ToolRegistry,
        seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    ) -> (Executor, Arc<AtomicUsize>) {
        let queue: Arc<Mutex<VecDeque<LlmResponse>>> =
            Arc::new(Mutex::new(VecDeque::from(responses)));
        let call_count = Arc::new(AtomicUsize::new(0));

        let mut mock = MockLlmProvider::new();
        let queue_clone = queue.clone();
        let seen_clone = seen.clone();
        let count_clone = call_count.clone();
        mock.expect_chat().returning(move |messages, _tools| {
            count_clone.fetch_add(1, Ordering::SeqCst);
            seen_clone.lock().unwrap().push(messages.to_vec());
            let resp = queue_clone
                .lock()
                .unwrap()
                .pop_front()
                .expect("mock provider ran out of responses");
            Ok(resp)
        });
        mock.expect_name().times(0..).return_const("mock".to_string());
        mock.expect_validate().times(0..).returning(|| Ok(()));

        (Executor::new(Box::new(mock), registry, true), call_count)
    }

    fn default_registry_in(dir: &std::path::Path) -> ToolRegistry {
        // WriteFileTool captures the current directory at construction time.
        std::env::set_current_dir(dir).expect("chdir to tempdir");
        ToolRegistry::new(&Config::default()).expect("build tool registry")
    }

    #[tokio::test]
    async fn test_run_completes_on_stop_and_records_assistant_message() {
        let seen: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let (mut executor, call_count) = executor_with_queue(
            vec![response("Final answer.", FinishReason::Stop, None)],
            ToolRegistry::new(&Config::default()).unwrap(),
            seen.clone(),
        );

        let memory = ConversationMemory::new("You are a helpful assistant.".to_string());
        let planner = Planner::new(50);
        let memory =
            executor.run("Write a test", &planner, memory, 10).await.expect("run should succeed");

        // Exactly one LLM call, receiving system + user context.
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].len(), 2);
        assert!(matches!(recorded[0][0].role, Role::System));
        assert_eq!(recorded[0][0].content, "You are a helpful assistant.");
        assert!(matches!(recorded[0][1].role, Role::User));
        assert!(recorded[0][1].content.contains("Write a test"));

        // The final assistant message must have been added to memory.
        let msgs = memory.messages();
        assert!(msgs.iter().any(|m| matches!(m.role, Role::Assistant)));
        assert!(msgs
            .iter()
            .any(|m| matches!(m.role, Role::Assistant) && m.content == "Final answer."));
    }

    #[tokio::test]
    #[serial]
    async fn test_tool_call_executes_write_file_in_tempdir() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let original_cwd = std::env::current_dir().expect("get cwd");
        let registry = default_registry_in(tmp.path());

        let seen: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let (mut executor, _call_count) = executor_with_queue(
            vec![
                response(
                    "I will write the file.",
                    FinishReason::ToolCalls,
                    Some(vec![write_file_call(
                        "call_1",
                        r#"{"path":"test.txt","content":"hello"}"#,
                    )]),
                ),
                response("File written.", FinishReason::Stop, None),
            ],
            registry,
            seen.clone(),
        );

        let memory = ConversationMemory::new("sys".to_string());
        let planner = Planner::new(50);
        let result = executor.run("Write a file", &planner, memory, 10).await;
        // Restore cwd before any assertion/panic so other tests are unaffected.
        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        let memory = result.expect("run should succeed");

        // The write_file tool actually created the file in the tempdir.
        let content = std::fs::read_to_string(tmp.path().join("test.txt"))
            .expect("test.txt should exist after tool call");
        assert_eq!(content, "hello");

        // Turn 1 sent [system, user]; turn 2 sent [system, user, assistant
        // (with tool calls), tool (result)] — proving the assistant message
        // with tool calls and the tool result were recorded in memory.
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].len(), 2);
        assert_eq!(recorded[1].len(), 4);
        assert!(matches!(recorded[1][2].role, Role::Assistant));
        let sent_calls = recorded[1][2].tool_calls.as_ref().expect("tool calls sent");
        assert_eq!(sent_calls[0].function.name, "write_file");
        assert!(matches!(recorded[1][3].role, Role::Tool));
        assert!(recorded[1][3].content.contains("Wrote"));
        assert!(recorded[1][3].tool_call_id.as_deref().is_some_and(|id| id == "call_1"));
        drop(recorded);

        // Final memory: assistant stop message present, tool result present.
        let msgs = memory.messages();
        assert!(msgs
            .iter()
            .any(|m| matches!(m.role, Role::Assistant) && m.content == "File written."));
        assert!(msgs.iter().any(|m| matches!(m.role, Role::Tool)));
    }

    #[tokio::test]
    #[serial]
    async fn test_max_turns_truncates_never_finishing_loop() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let original_cwd = std::env::current_dir().expect("get cwd");
        let registry = default_registry_in(tmp.path());

        let seen: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let (mut executor, call_count) = executor_with_queue(
            vec![
                response(
                    "",
                    FinishReason::ToolCalls,
                    Some(vec![write_file_call("c1", r#"{"path":"a.txt","content":"1"}"#)]),
                ),
                response(
                    "",
                    FinishReason::ToolCalls,
                    Some(vec![write_file_call("c2", r#"{"path":"b.txt","content":"2"}"#)]),
                ),
                response(
                    "",
                    FinishReason::ToolCalls,
                    Some(vec![write_file_call("c3", r#"{"path":"c.txt","content":"3"}"#)]),
                ),
            ],
            registry,
            seen.clone(),
        );

        let memory = ConversationMemory::new("sys".to_string());
        let planner = Planner::new(50);
        let result = executor.run("loop", &planner, memory, 3).await;
        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        result.expect("run should stop gracefully at max_turns");

        // The loop must stop after exactly max_turns LLM calls.
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
        assert_eq!(seen.lock().unwrap().len(), 3);
    }
}
