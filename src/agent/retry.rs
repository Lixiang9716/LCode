//! LLM call resilience (#4 — retry with exponential backoff).
//!
//! A decorator provider that retries transient failures (rate limits,
//! server errors) with exponential backoff + jitter, upgrades max_tokens
//! on truncation, and triggers reactive compaction on prompt-too-long.

use crate::llm::{ChatMessage, LlmProvider, ToolDefinition};
use async_trait::async_trait;
use std::sync::atomic::{AtomicU32, Ordering};

/// Retry policy.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum attempts per call.
    pub max_attempts: u32,
    /// Base backoff in milliseconds.
    pub base_delay_ms: u64,
    /// Max backoff in milliseconds.
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 5, base_delay_ms: 500, max_delay_ms: 30_000 }
    }
}

/// Wraps a provider and adds retry/backoff semantics.
pub struct RetryProvider {
    inner: Box<dyn LlmProvider>,
    policy: RetryPolicy,
    /// Current max_tokens budget (upgraded on truncation).
    max_tokens: AtomicU32,
}

impl RetryProvider {
    pub fn new(inner: Box<dyn LlmProvider>, policy: RetryPolicy) -> Self {
        Self { inner, policy, max_tokens: AtomicU32::new(0) }
    }

    /// Is this error transient (rate limit / 5xx)?
    fn is_transient(err: &anyhow::Error) -> bool {
        // TODO(#4): match on message: "429", "529", "5", "timeout",
        // "overloaded", "rate limit".
        let msg = err.to_string().to_lowercase();
        msg.contains("429") || msg.contains("529") || msg.contains("timeout")
            || msg.contains("rate limit") || msg.contains("overloaded")
            || msg.contains("500") || msg.contains("502") || msg.contains("503")
    }
}

#[async_trait]
impl LlmProvider for RetryProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        // TODO(#4): loop attempts with exponential backoff on transient
        // errors; on FinishReason::Length upgrade max_tokens (via a
        // clone of the request or a provider knob) and retry once.
        self.inner.chat(messages, tools).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn validate(&self) -> anyhow::Result<()> {
        self.inner.validate()
    }
}
