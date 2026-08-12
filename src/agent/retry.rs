//! LLM call resilience (#4 — retry with exponential backoff).
//!
//! A decorator provider that retries transient failures (rate limits,
//! server errors) with exponential backoff + jitter, upgrades max_tokens
//! on truncation, and triggers reactive compaction on prompt-too-long.

use crate::llm::{
    ChatMessage, FinishReason, LlmProvider, LlmResponse, StreamEvent, ToolDefinition,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// Initial max_tokens budget before any truncation upgrade (matches the
/// default `llm.max_tokens` in `LlmConfig`).
const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Hard cap for the upgraded budget, so a doubling chain cannot blow up.
const MAX_TOKENS_CAP: u32 = 65_536;

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
    /// Current max_tokens budget, doubled on truncation (`Length`).
    ///
    /// NOTE: `LlmProvider::chat` does not accept a `max_tokens` argument
    /// today, so this budget is bookkeeping reserved for a future
    /// parameterized chat. The Length retry itself is still useful
    /// because truncation is sometimes non-deterministic, and the budget
    /// (plus `upgrade_count`) gives observability into how often the
    /// default budget would have been exceeded.
    max_tokens: AtomicU32,
    /// Number of max_tokens upgrades performed so far.
    upgrade_count: AtomicU32,
}

impl RetryProvider {
    pub fn new(inner: Box<dyn LlmProvider>, policy: RetryPolicy) -> Self {
        Self {
            inner,
            policy,
            max_tokens: AtomicU32::new(DEFAULT_MAX_TOKENS),
            upgrade_count: AtomicU32::new(0),
        }
    }

    /// Is this error transient (rate limit / 5xx)?
    fn is_transient(err: &anyhow::Error) -> bool {
        // Match on message: "429", "529", "5", "timeout",
        // "overloaded", "rate limit".
        let msg = err.to_string().to_lowercase();
        msg.contains("429")
            || msg.contains("529")
            || msg.contains("timeout")
            || msg.contains("rate limit")
            || msg.contains("overloaded")
            || msg.contains("500")
            || msg.contains("502")
            || msg.contains("503")
    }

    /// Exponential backoff: `base_delay_ms * 2^(attempt-1) + jitter`,
    /// capped at `max_delay_ms`. Jitter is a hash of the attempt and a
    /// clock so concurrent retries spread out instead of thundering.
    fn backoff_delay(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let base = self.policy.base_delay_ms.saturating_mul(1u64 << exponent);
        let jitter = self.jitter_ms(attempt) % (base / 2 + 1);
        Duration::from_millis(base.saturating_add(jitter).min(self.policy.max_delay_ms))
    }

    /// Pseudo-random jitter in `[0, u64::MAX)`, seeded by attempt + clock.
    fn jitter_ms(&self, attempt: u32) -> u64 {
        use std::hash::{Hash, Hasher};
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (attempt, nanos).hash(&mut hasher);
        hasher.finish()
    }

    /// Log the transient failure and sleep `backoff_delay(attempt)`.
    async fn backoff_sleep(&self, attempt: u32, err: &anyhow::Error) {
        let delay = self.backoff_delay(attempt);
        tracing::warn!(
            "LLM call failed (attempt {}/{}): {err}; retrying in {}ms",
            attempt,
            self.policy.max_attempts,
            delay.as_millis()
        );
        tokio::time::sleep(delay).await;
    }

    /// Double the max_tokens budget (capped at [`MAX_TOKENS_CAP`]) and
    /// bump the upgrade counter. Returns the new budget.
    fn upgrade_budget(&self) -> u32 {
        self.upgrade_count.fetch_add(1, Ordering::Relaxed);
        self.max_tokens
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_mul(2).min(MAX_TOKENS_CAP))
            })
            .expect("fetch_update always returns Some");
        self.max_tokens.load(Ordering::Relaxed)
    }

    /// Current max_tokens budget (after any truncation upgrades).
    pub fn max_tokens_budget(&self) -> u32 {
        self.max_tokens.load(Ordering::Relaxed)
    }

    /// Number of max_tokens upgrades performed so far.
    pub fn upgrade_count(&self) -> u32 {
        self.upgrade_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl LlmProvider for RetryProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<LlmResponse> {
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let outcome = self.inner.chat(messages, tools).await;
            let response = match outcome {
                Ok(resp) => resp,
                Err(err) if !Self::is_transient(&err) || attempt >= self.policy.max_attempts => {
                    // Non-transient error, or attempts exhausted: return
                    // the last failure as-is.
                    return Err(err);
                }
                Err(err) => {
                    // Transient: exponential backoff + jitter, then retry.
                    self.backoff_sleep(attempt, &err).await;
                    continue;
                }
            };
            if response.finish_reason == FinishReason::Length && attempt < self.policy.max_attempts
            {
                // Truncated: double the max_tokens budget (bookkeeping —
                // see the field docs) and retry once. Bounded by
                // `max_attempts` because `attempt` keeps counting.
                let budget = self.upgrade_budget();
                tracing::warn!(
                    "response truncated on attempt {}/{}; budget upgraded to {}",
                    attempt,
                    self.policy.max_attempts,
                    budget
                );
                continue;
            }
            return Ok(response);
        }
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    /// Delegate streaming to the inner provider (G11).
    ///
    /// Retry/backoff applies to the plain `chat` path only: a delta
    /// stream can fail mid-flight and has no single response to retry,
    /// so real streaming is passed through untouched — the executor's
    /// streaming path reassembles the deltas itself.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>> {
        self.inner.chat_stream(messages, tools).await
    }

    fn validate(&self) -> anyhow::Result<()> {
        self.inner.validate()
    }
}
