//! Unit tests for the retry/backoff provider wrapper
//! (`src/agent/retry.rs`).

use lcode::agent::{RetryPolicy, RetryProvider};
use lcode::llm::provider::MockLlmProvider;
use lcode::llm::{FinishReason, LlmProvider, LlmResponse, Usage};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// A successful stop response.
fn response(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        tool_calls: None,
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason: FinishReason::Stop,
    }
}

/// A truncated (max_tokens exhausted) response.
fn truncated_response() -> LlmResponse {
    LlmResponse {
        content: "partial output".to_string(),
        tool_calls: None,
        server_results: Vec::new(),
        usage: Usage::default(),
        finish_reason: FinishReason::Length,
    }
}

/// Fast retry policy for tests (real sleeps stay in the millisecond range).
fn fast_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy { max_attempts, base_delay_ms: 1, max_delay_ms: 5 }
}

/// Shared call counter for asserting how many times the mock was invoked.
fn counter() -> (Arc<AtomicU32>, Arc<AtomicU32>) {
    let calls = Arc::new(AtomicU32::new(0));
    (calls.clone(), calls)
}

#[tokio::test]
async fn retries_transient_rate_limit_then_succeeds() {
    // ① First two calls fail with a 429 rate limit; the third succeeds.
    let mut mock = MockLlmProvider::new();
    let (clicks, calls) = counter();
    mock.expect_chat().times(3).returning(move |_messages, _tools| {
        let n = clicks.fetch_add(1, Ordering::SeqCst) + 1;
        if n < 3 {
            Err(anyhow::anyhow!("429 rate limit exceeded"))
        } else {
            Ok(response("done"))
        }
    });

    let provider = RetryProvider::new(Box::new(mock), fast_policy(3));

    let result = provider.chat(&[], &[]).await.expect("should recover after retries");
    assert_eq!(result.content, "done");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "expected exactly 3 LLM calls");
}

#[tokio::test]
async fn does_not_retry_non_transient_errors() {
    // ② Non-transient error ("invalid api key") → single call, immediate failure.
    let mut mock = MockLlmProvider::new();
    let (clicks, calls) = counter();
    mock.expect_chat().times(1).returning(move |_messages, _tools| {
        clicks.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("invalid api key"))
    });

    let provider = RetryProvider::new(Box::new(mock), fast_policy(3));

    let err = provider.chat(&[], &[]).await.expect_err("non-transient error must fail");
    assert!(err.to_string().contains("invalid api key"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "must not retry non-transient errors");
}

#[tokio::test]
async fn returns_error_after_max_attempts_exhausted() {
    // ③ Always-transient failure → exactly max_attempts calls, last error returned.
    let mut mock = MockLlmProvider::new();
    let (clicks, calls) = counter();
    mock.expect_chat().times(3).returning(move |_messages, _tools| {
        clicks.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("529 overloaded"))
    });

    let provider = RetryProvider::new(Box::new(mock), fast_policy(3));

    let err = provider.chat(&[], &[]).await.expect_err("must fail after max_attempts");
    assert!(err.to_string().contains("529"), "last error should be returned as-is");
    assert_eq!(calls.load(Ordering::SeqCst), 3, "expected max_attempts LLM calls");
}

#[tokio::test]
async fn retries_truncated_response_with_upgraded_budget() {
    // FinishReason::Length → max_tokens budget doubles (8192 → 16384)
    // and the call is retried once; the second attempt succeeds.
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
    mock.expect_set_max_tokens().with(mockall::predicate::eq(16_384u32)).times(1).return_const(());

    let provider = RetryProvider::new(Box::new(mock), fast_policy(3));

    let result = provider.chat(&[], &[]).await.expect("truncation retry should succeed");
    assert_eq!(result.content, "full output");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.upgrade_count(), 1, "budget upgraded exactly once");
    assert_eq!(provider.max_tokens_budget(), 16_384, "budget doubled from 8192");
}

#[tokio::test]
async fn truncated_response_returned_as_is_when_attempts_exhausted() {
    // With max_attempts = 1 there is no retry slot: the truncated
    // response is returned unchanged.
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().times(1).returning(|_messages, _tools| Ok(truncated_response()));

    let provider = RetryProvider::new(Box::new(mock), fast_policy(1));

    let result = provider.chat(&[], &[]).await.expect("single attempt returns the response");
    assert_eq!(result.finish_reason, FinishReason::Length);
    assert_eq!(provider.upgrade_count(), 0);
}

/// The specific "tool_use without tool_result" 400 some Anthropic-
/// compatible endpoints emit intermittently against valid wire messages
/// is transient: retried until it succeeds (E2E regression — one such
/// hiccup used to kill an otherwise healthy session).
#[tokio::test]
async fn retries_tool_result_400_hickup_then_succeeds() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_clone = calls.clone();
    let mut mock = MockLlmProvider::new();
    mock.expect_chat().times(1..).returning(move |_, _| {
        let n = calls_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            Err(anyhow::anyhow!(
                "Anthropic API error (400): tool_use ids were found without tool_result blocks"
            ))
        } else {
            Ok(LlmResponse {
                content: "ok".to_string(),
                tool_calls: None,
                server_results: Vec::new(),
                usage: Usage::default(),
                finish_reason: FinishReason::Stop,
            })
        }
    });
    mock.expect_name().times(0..).return_const("mock".to_string());
    mock.expect_validate().times(0..).returning(|| Ok(()));

    let provider = RetryProvider::new(Box::new(mock), RetryPolicy::default());
    let response = provider.chat(&[], &[]).await.expect("hickup retried to success");
    assert_eq!(response.content, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "exactly one retry");
}
