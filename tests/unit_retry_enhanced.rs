//! Tests for the enhanced retry layer (G10 — real max_tokens upgrades,
//! prompt-too-long marking, and fallback-model failover).

use lcode::agent::{RetryPolicy, RetryProvider, PROMPT_TOO_LONG_MARKER};
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, LlmProvider, LlmResponse, Usage};
use mockall::predicate;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

fn response(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls: None,
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }
}

fn truncated_response() -> LlmResponse {
    LlmResponse {
        content: "partial".to_string(),
        tool_calls: None,
        usage: Usage::default(),
        finish_reason: FinishReason::Length,
    }
}

fn fast_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy { max_attempts, base_delay_ms: 1, max_delay_ms: 5 }
}

fn counter() -> (Arc<AtomicU32>, Arc<AtomicU32>) {
    let calls = Arc::new(AtomicU32::new(0));
    (calls.clone(), calls)
}

// ---------------------------------------------------------------------------
// max_tokens upgrades are real: the inner provider receives them
// ---------------------------------------------------------------------------

#[tokio::test]
async fn length_retry_pushes_doubled_budget_to_inner_provider() {
    let mut mock = MockLlmProvider::new();
    let (clicks, calls) = counter();
    mock.expect_chat().times(2).returning(move |_messages, _tools| {
        let n = clicks.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 1 {
            Ok(truncated_response())
        } else {
            Ok(response("full output"))
        }
    });
    // The truncation retry must carry the doubled budget (8192 → 16384)
    // into the actual provider, not just the wrapper's bookkeeping.
    mock.expect_set_max_tokens().with(predicate::eq(16_384u32)).times(1).return_const(());

    let provider = RetryProvider::new(Box::new(mock), fast_policy(3));

    let result = provider.chat(&[], &[]).await.expect("truncation retry succeeds");
    assert_eq!(result.content, "full output");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.upgrade_count(), 1);
    assert_eq!(provider.max_tokens_budget(), 16_384);
}

#[tokio::test]
async fn set_max_tokens_propagates_to_inner_provider() {
    let mut mock = MockLlmProvider::new();
    mock.expect_set_max_tokens().with(predicate::eq(4096u32)).times(1).return_const(());

    let provider = RetryProvider::new(Box::new(mock), fast_policy(3));
    provider.set_max_tokens(4096);

    assert_eq!(provider.max_tokens_budget(), 4096, "wrapper budget tracks the value");
}

// ---------------------------------------------------------------------------
// prompt_too_long detection and marking
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prompt_too_long_errors_are_marked_without_retry() {
    let mut mock = MockLlmProvider::new();
    let (clicks, calls) = counter();
    mock.expect_chat().times(1).returning(move |_messages, _tools| {
        clicks.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("API error 400: prompt_too_long"))
    });

    let provider = RetryProvider::new(Box::new(mock), fast_policy(3));

    let err = provider.chat(&[], &[]).await.expect_err("must fail");
    assert!(
        err.to_string().starts_with(PROMPT_TOO_LONG_MARKER),
        "error must carry the marker: {err}"
    );
    assert!(err.to_string().contains("prompt_too_long"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no retry for prompt-too-long");
}

#[tokio::test]
async fn context_length_errors_are_marked() {
    // OpenAI-compatible wording ("maximum context length") and 413 must
    // both be recognised.
    for message in ["400 maximum context length is 128000 tokens", "413 Request Entity Too Large"] {
        let mut mock = MockLlmProvider::new();
        mock.expect_chat().times(1).returning(move |_m, _t| Err(anyhow::anyhow!("{message}")));

        let provider = RetryProvider::new(Box::new(mock), fast_policy(3));
        let err = provider.chat(&[], &[]).await.expect_err("must fail");
        assert!(
            err.to_string().starts_with(PROMPT_TOO_LONG_MARKER),
            "`{message}` must be marked, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// Fallback model failover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fallback_model_used_after_retries_exhausted() {
    let mut mock = MockLlmProvider::new();
    let (clicks, calls) = counter();
    mock.expect_chat().times(3).returning(move |_messages, _tools| {
        clicks.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("529 overloaded"))
    });
    // After max_attempts (2) consecutive failures, the provider switches
    // to the configured fallback model for one more attempt.
    mock.expect_set_model()
        .with(predicate::eq("claude-haiku".to_string()))
        .times(1)
        .return_const(());

    let provider = RetryProvider::with_fallback(
        Box::new(mock),
        fast_policy(2),
        Some("claude-haiku".to_string()),
    );

    let err = provider.chat(&[], &[]).await.expect_err("still fails on the fallback model");
    assert!(err.to_string().contains("529"));
    assert_eq!(calls.load(Ordering::SeqCst), 3, "2 attempts + 1 fallback attempt");
    assert!(provider.on_fallback_model(), "fallback model must be active");
}

#[tokio::test]
async fn fallback_model_succeeds_after_switch() {
    let mut mock = MockLlmProvider::new();
    let (clicks, _calls) = counter();
    mock.expect_chat().times(3).returning(move |_messages, _tools| {
        let n = clicks.fetch_add(1, Ordering::SeqCst) + 1;
        if n < 3 {
            Err(anyhow::anyhow!("529 overloaded"))
        } else {
            Ok(response("recovered on fallback"))
        }
    });
    mock.expect_set_model()
        .with(predicate::eq("backup-model".to_string()))
        .times(1)
        .return_const(());

    let provider = RetryProvider::with_fallback(
        Box::new(mock),
        fast_policy(2),
        Some("backup-model".to_string()),
    );

    let result = provider.chat(&[], &[]).await.expect("fallback attempt succeeds");
    assert_eq!(result.content, "recovered on fallback");
    assert!(provider.on_fallback_model());
}

#[tokio::test]
async fn no_fallback_configured_returns_after_max_attempts() {
    let mut mock = MockLlmProvider::new();
    let (clicks, calls) = counter();
    mock.expect_chat().times(2).returning(move |_messages, _tools| {
        clicks.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("529 overloaded"))
    });
    // No set_model expectation: the mock panics if the wrapper ever tries
    // to switch without a configured fallback.

    let provider = RetryProvider::with_fallback(Box::new(mock), fast_policy(2), None);

    let err = provider.chat(&[], &[]).await.expect_err("must fail");
    assert!(err.to_string().contains("529"));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "no extra attempt without a fallback");
    assert!(!provider.on_fallback_model());
}

#[tokio::test]
async fn explicit_set_model_supersedes_fallback() {
    let mut mock = MockLlmProvider::new();
    mock.expect_set_model().with(predicate::eq("user-model".to_string())).times(1).return_const(());

    let provider = RetryProvider::with_fallback(
        Box::new(mock),
        fast_policy(2),
        Some("fallback-model".to_string()),
    );
    provider.set_model("user-model".to_string());

    assert!(provider.on_fallback_model(), "explicit switch disables later failover");
}
