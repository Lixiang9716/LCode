//! Conversation memory management.
//!
//! Manages the conversation history, including:
//! - System prompt
//! - User/assistant message history
//! - Tool call/result pairs
//! - Memory compaction (summarization) when context gets too long

use crate::llm::ChatMessage;

/// Manages the conversation history for an agent session.
#[derive(Debug, Clone)]
pub struct ConversationMemory {
    system_prompt: String,
    messages: Vec<ChatMessage>,
    /// Maximum number of messages to keep before compaction
    max_messages: usize,
}

impl ConversationMemory {
    /// Create a new conversation memory with the given system prompt.
    pub fn new(system_prompt: String) -> Self {
        Self { system_prompt, messages: Vec::new(), max_messages: 200 }
    }

    /// Build a memory pre-seeded with an existing message history (e.g. a
    /// restored session snapshot). Mirrors [`Self::new`]: the system
    /// prompt is kept separately and prepended by [`Self::get_context`],
    /// so `messages` should not contain system-role entries.
    pub fn from_messages(system_prompt: String, messages: Vec<ChatMessage>) -> Self {
        Self { system_prompt, messages, max_messages: 200 }
    }

    /// Add a system message (typically only one at the start).
    pub fn add_system(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage::system(content));
    }

    /// Add a user message.
    pub fn add_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage::user(content));
    }

    /// Add an assistant message (with optional tool calls).
    pub fn add_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage::assistant(content));
    }

    /// Add an assistant message with tool calls.
    pub fn add_assistant_with_tool_calls(
        &mut self,
        content: impl Into<String>,
        tool_calls: Vec<crate::llm::ToolCallRequest>,
    ) {
        let mut msg = ChatMessage::assistant(content);
        msg.tool_calls = Some(tool_calls);
        self.messages.push(msg);
    }

    /// Add a tool result message.
    pub fn add_tool_result(&mut self, content: impl Into<String>, tool_call_id: String) {
        self.messages.push(ChatMessage::tool(content, tool_call_id));
    }

    /// Get the full message history including the system prompt for sending to the LLM.
    pub fn get_context(&self) -> Vec<ChatMessage> {
        let mut context = Vec::with_capacity(self.messages.len() + 1);

        // System prompt goes first
        context.push(ChatMessage::system(&self.system_prompt));

        // Then conversation history
        context.extend(self.messages.clone());

        context
    }

    /// Get only the conversation messages (no system prompt).
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Approximate token count of the conversation (fast heuristic:
    /// `chars / 4`; use [`Self::token_count`] for a real count).
    pub fn approximate_tokens(&self) -> usize {
        let system_tokens = self.system_prompt.len() / 4;
        let msg_tokens: usize = self.messages.iter().map(|m| m.content.len() / 4).sum();
        system_tokens + msg_tokens
    }

    /// Exact token count of the full context — system prompt plus all
    /// message contents — using the cl100k_base BPE tokenizer (tiktoken).
    ///
    /// Slower than [`Self::approximate_tokens`] (requires locking the
    /// tokenizer), so use the approximate path when only a threshold
    /// comparison is needed.
    pub fn token_count(&self) -> usize {
        let mut total = exact_tokens(&self.system_prompt);
        for msg in &self.messages {
            total += exact_tokens(&msg.content);
        }
        total
    }

    /// Compact old messages if the context is getting too large.
    /// This replaces older messages with a summary to save tokens.
    pub fn compact_if_needed(&mut self, max_tokens: usize) {
        if self.approximate_tokens() <= max_tokens && self.messages.len() <= self.max_messages {
            return;
        }

        // Simple compaction strategy: remove oldest non-system messages
        // In a full implementation, this would summarize them instead
        let target_messages = self.messages.len() / 2;
        while self.messages.len() > target_messages && self.messages.len() > 4 {
            self.messages.remove(0);
        }

        tracing::info!(
            "Compacted conversation: {} messages remaining, ~{} tokens",
            self.messages.len(),
            self.approximate_tokens()
        );
    }

    /// Clear all messages except system prompt.
    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

/// Exact token count of a text using the cl100k_base BPE tokenizer
/// (the tokenizer GPT-4-class models use for counting).
///
/// `cl100k_base_singleton` lazily initializes the tokenizer once and
/// caches it, so subsequent calls only pay for the encode itself.
pub fn exact_tokens(text: &str) -> usize {
    let bpe = tiktoken_rs::cl100k_base_singleton();
    let tokens = bpe.lock().encode_ordinary(text).len();
    tokens
}
