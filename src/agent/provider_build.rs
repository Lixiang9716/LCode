//! Provider construction (P0/P1): backend-kind resolution, the main /
//! internal providers, and the server-side web_search declaration.
//!
//! Kept in a separate file so `mod.rs` stays under the 500-line style
//! limit.

use crate::agent::retry::{RetryPolicy, RetryProvider};
use crate::config::Config;
use crate::llm::LlmProvider;

/// Kind of LLM backend a provider alias resolves to.
enum ProviderKind {
    Anthropic,
    OpenAi,
}

/// Resolve a provider alias to its backend kind and default endpoint.
fn provider_kind(provider: &str) -> anyhow::Result<(ProviderKind, Option<&'static str>)> {
    match provider {
        "openai" | "openai_compatible" => Ok((ProviderKind::OpenAi, None)),
        "anthropic" | "claude" => Ok((ProviderKind::Anthropic, None)),
        "deepseek" => Ok((ProviderKind::Anthropic, Some("https://api.deepseek.com/anthropic"))),
        "kimi" => Ok((ProviderKind::Anthropic, Some("https://api.moonshot.cn/anthropic"))),
        "minimax" => Ok((ProviderKind::OpenAi, Some("https://api.minimaxi.com/v1"))),
        "glm" => Ok((ProviderKind::OpenAi, Some("https://open.bigmodel.cn/api/paas/v4"))),
        other => anyhow::bail!(
            "Unknown LLM provider: {other}. Supported: openai, openai_compatible, \
             anthropic, claude, deepseek, kimi, minimax, glm"
        ),
    }
}

/// The `web_search` server-tool declaration, when the configured
/// endpoint supports it: DeepSeek's Anthropic-compatible endpoint
/// executes searches server-side (`web_search_20260209`) and returns the
/// results in-band. Gated on `tools.enable_web`; native Anthropic and
/// OpenAI-format endpoints are excluded (chat completions has no server
/// tools, and the native Anthropic tool type differs).
pub fn web_search_spec(config: &Config) -> Option<crate::llm::ServerToolSpec> {
    if !config.tools.enable_web {
        return None;
    }
    let (kind, _) = provider_kind(&config.llm.provider.to_lowercase()).ok()?;
    if !matches!(kind, ProviderKind::Anthropic) {
        return None;
    }
    let api_base = config
        .llm
        .api_base
        .clone()
        .unwrap_or_else(|| "https://api.deepseek.com/anthropic".to_string());
    if !api_base.contains("api.deepseek.com") {
        return None;
    }
    Some(crate::llm::ServerToolSpec {
        tool_type: "web_search_20260209".to_string(),
        name: "web_search".to_string(),
        max_queries: Some(5),
    })
}

/// Build the appropriate LLM provider from configuration.
///
/// Provider aliases (all map to the existing Anthropic/OpenAI-compatible
/// implementations, only the default endpoint differs):
/// - `openai` / `openai_compatible` — OpenAI API or any OpenAI-compatible endpoint
/// - `anthropic` / `claude` — Anthropic native endpoint
/// - `deepseek`, `kimi` — Anthropic-compatible third-party endpoints
/// - `minimax`, `glm` — OpenAI-compatible third-party endpoints
///
/// An explicit `llm.api_base` always wins over the alias's default
/// endpoint. The result is wrapped in a [`RetryProvider`] so every LLM
/// call gets retry/backoff and max_tokens-upgrade semantics (#4).
pub fn build_provider(config: &Config) -> anyhow::Result<Box<dyn LlmProvider>> {
    let (kind, default_base) = provider_kind(&config.llm.provider.to_lowercase())?;

    // Explicit `llm.api_base` wins; otherwise fall back to the alias's
    // default endpoint.
    let api_base = config.llm.api_base.clone().or_else(|| default_base.map(str::to_string));
    let llm = crate::config::LlmConfig { api_base, ..config.llm.clone() };
    build_provider_from_llm(kind, &llm, config)
}

/// Build a provider for internal utility calls (context-compaction
/// summaries, memory extraction/consolidation). These summarize or
/// classify existing text, so hidden reasoning is pure waste: by default
/// thinking mode is forced off regardless of `llm.thinking_disabled`
/// (measured ~10x token difference on the same task). Set
/// `llm.internal_thinking_disabled = false` to keep thinking on for
/// internal calls.
pub fn build_internal_provider(config: &Config) -> anyhow::Result<Box<dyn LlmProvider>> {
    let (kind, default_base) = provider_kind(&config.llm.provider.to_lowercase())?;
    let api_base = config.llm.api_base.clone().or_else(|| default_base.map(str::to_string));
    let mut llm = crate::config::LlmConfig { api_base, ..config.llm.clone() };
    if config.llm.internal_thinking_disabled {
        llm.thinking_disabled = true;
    }
    build_provider_from_llm(kind, &llm, config)
}

/// Shared provider assembly: inner provider from the resolved kind plus
/// the retry decorator seeded with the configured max_tokens budget.
fn build_provider_from_llm(
    kind: ProviderKind,
    llm: &crate::config::LlmConfig,
    config: &Config,
) -> anyhow::Result<Box<dyn LlmProvider>> {
    let inner: Box<dyn LlmProvider> = match kind {
        ProviderKind::Anthropic => Box::new(crate::llm::anthropic::AnthropicProvider::new(llm)?),
        ProviderKind::OpenAi => Box::new(crate::llm::openai::OpenAiProvider::new(llm)?),
    };
    let policy = RetryPolicy {
        max_attempts: config.retry.max_attempts,
        base_delay_ms: config.retry.base_delay_ms,
        max_delay_ms: config.retry.max_delay_ms,
    };
    let retry = RetryProvider::with_fallback(inner, policy, config.llm.fallback_model.clone());
    // Seed the retry budget (and thus the inner provider's request body)
    // with the configured max_tokens instead of the hardcoded default.
    retry.set_max_tokens(config.llm.max_tokens);
    Ok(Box::new(retry))
}
