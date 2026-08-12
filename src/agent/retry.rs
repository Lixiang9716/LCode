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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// Initial max_tokens budget before any truncation upgrade (matches the
/// default `llm.max_tokens` in `LlmConfig`).
const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Hard cap for the upgraded budget, so a doubling chain cannot blow up.
const MAX_TOKENS_CAP: u32 = 65_536;

/// Marker prefix attached to prompt-too-long errors so callers can
/// distinguish them from other API failures.
///
/// Integration point: the executor's compaction channel (`maybe_compact`,
/// batch 1) can detect this prefix on a failed LLM call and trigger a
/// reactive compact + retry (s11 error-recovery path 2). The marker is a
/// string prefix (not a dedicated error type) so it survives any error
/// conversion/formatting in between.
pub const PROMPT_TOO_LONG_MARKER: &str = "[PROMPT_TOO_LONG]";

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
    /// Current max_tokens budget, doubled on truncation (`Length`) and
    /// pushed into the inner provider via [`LlmProvider::set_max_tokens`]
    /// so the upgrade reaches the actual request body.
    max_tokens: AtomicU32,
    /// Number of max_tokens upgrades performed so far.
    upgrade_count: AtomicU32,
    /// Model to fail over to when all `max_attempts` of a call fail
    /// (configured via `llm.fallback_model`).
    fallback_model: Option<String>,
    /// Whether the fallback model has been activated (one switch per
    /// provider lifetime, mirroring s11's 529-failover).
    fallback_used: AtomicBool,
}

impl RetryProvider {
    pub fn new(inner: Box<dyn LlmProvider>, policy: RetryPolicy) -> Self {
        Self::with_fallback(inner, policy, None)
    }

    /// Create a provider with an optional fallback model that is switched
    /// in (once) when a call exhausts its `max_attempts`.
    pub fn with_fallback(
        inner: Box<dyn LlmProvider>,
        policy: RetryPolicy,
        fallback_model: Option<String>,
    ) -> Self {
        Self {
            inner,
            policy,
            max_tokens: AtomicU32::new(DEFAULT_MAX_TOKENS),
            upgrade_count: AtomicU32::new(0),
            fallback_model,
            fallback_used: AtomicBool::new(false),
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
            // Some Anthropic-compatible endpoints intermittently reject a
            // well-formed tool_use/tool_result sequence with a 400
            // (verified on the wire as valid). Retrying the same request
            // succeeds; classify the specific complaint as transient.
            || (msg.contains("400")
                && msg.contains("tool_use ids were found without tool_result"))
    }

    /// Does this error indicate the prompt/context is too long for the
    /// model's window (s11 error-recovery path 2)?
    fn is_prompt_too_long(err: &anyhow::Error) -> bool {
        let msg = err.to_string().to_lowercase();
        msg.contains("prompt_too_long")
            || msg.contains("prompt is too long")
            || msg.contains("maximum context length")
            || msg.contains("context_length_exceeded")
            || msg.contains("context window")
            || msg.contains("413")
    }

    /// Attach the [`PROMPT_TOO_LONG_MARKER`] prefix to a prompt-too-long
    /// error (idempotent) so the executor's compaction channel can
    /// recognise it after any error formatting/translation.
    fn flag_prompt_too_long(err: anyhow::Error) -> anyhow::Error {
        let msg = err.to_string();
        if msg.starts_with(PROMPT_TOO_LONG_MARKER) {
            err
        } else {
            anyhow::anyhow!("{PROMPT_TOO_LONG_MARKER} {msg}")
        }
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

    /// Decide whether to retry a transient failure. Within
    /// `max_attempts` this is always true; afterwards the fallback model
    /// is switched in once (returning true) so the call gets exactly one
    /// more attempt on the fallback model.
    fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.policy.max_attempts || self.try_fallback_model()
    }

    /// Switch to the fallback model once, if one is configured. Returns
    /// whether the switch happened (i.e. the call should be retried).
    fn try_fallback_model(&self) -> bool {
        if self.fallback_used.load(Ordering::Relaxed) {
            return false;
        }
        let Some(model) = self.fallback_model.as_deref() else {
            return false;
        };
        if self.fallback_used.swap(true, Ordering::Relaxed) {
            return false;
        }
        tracing::warn!(model, "retries exhausted; switching to fallback model");
        self.inner.set_model(model.to_string());
        true
    }

    /// Current max_tokens budget (after any truncation upgrades).
    pub fn max_tokens_budget(&self) -> u32 {
        self.max_tokens.load(Ordering::Relaxed)
    }

    /// Number of max_tokens upgrades performed so far.
    pub fn upgrade_count(&self) -> u32 {
        self.upgrade_count.load(Ordering::Relaxed)
    }

    /// Whether the fallback model has been activated.
    pub fn on_fallback_model(&self) -> bool {
        self.fallback_used.load(Ordering::Relaxed)
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
                // Prompt-too-long errors are not transient but need to
                // be flagged so the executor's compaction channel can
                // trigger a reactive compact + retry (s11 path 2).
                Err(err) if Self::is_prompt_too_long(&err) => {
                    return Err(Self::flag_prompt_too_long(err));
                }
                Err(err) if !Self::is_transient(&err) => return Err(err),
                // Transient: retry with exponential backoff + jitter
                // while attempts remain, or fail over to the fallback
                // model once (s11 529-failover).
                Err(err) if self.should_retry(attempt) => {
                    self.backoff_sleep(attempt, &err).await;
                    continue;
                }
                Err(err) => return Err(err),
            };
            if response.finish_reason == FinishReason::Length && attempt < self.policy.max_attempts
            {
                // Truncated: double the max_tokens budget and push it into
                // the inner provider so the retry actually sends the
                // upgraded budget. Bounded by `max_attempts` because
                // `attempt` keeps counting.
                let budget = self.upgrade_budget();
                self.inner.set_max_tokens(budget);
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

    fn set_max_tokens(&self, n: u32) {
        self.max_tokens.store(n, Ordering::Relaxed);
        self.inner.set_max_tokens(n);
    }

    fn set_model(&self, model: String) {
        // An explicit model switch supersedes any future fallback: do not
        // let the failure path override a caller-chosen model afterwards.
        self.fallback_used.store(true, Ordering::Relaxed);
        self.inner.set_model(model);
    }
}
