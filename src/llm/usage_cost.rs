//! Token cost estimation for DeepSeek models (per-1M-token pricing).
//!
//! Costs are estimates: they ignore free tiers, discounts and the exact
//! rounding the provider applies, but let sessions show a real-dollar
//! figure and, more importantly, the savings from context-cache hits.

use crate::llm::Usage;

/// Per-1M-token pricing for one model family.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    /// Input tokens served from the context cache.
    pub cache_hit_per_1m: f64,
    /// Input tokens processed fresh.
    pub input_per_1m: f64,
    /// Output tokens generated.
    pub output_per_1m: f64,
}

/// Official DeepSeek pricing (2026-08): flash and pro tiers.
impl Pricing {
    pub const FLASH: Pricing =
        Pricing { cache_hit_per_1m: 0.0028, input_per_1m: 0.14, output_per_1m: 0.28 };
    pub const PRO: Pricing =
        Pricing { cache_hit_per_1m: 0.003625, input_per_1m: 0.435, output_per_1m: 0.87 };
}

/// Pick the pricing tier for a model name. Legacy names (`deepseek-chat`,
/// `deepseek-reasoner`) resolve to the flash tier, matching the provider's
/// alias mapping; anything unrecognized falls back to flash as well.
pub fn pricing_for(model: &str) -> Pricing {
    if model.contains("pro") {
        Pricing::PRO
    } else {
        Pricing::FLASH
    }
}

/// Estimated USD cost of one usage block.
pub fn estimate_cost(model: &str, usage: &Usage) -> f64 {
    let p = pricing_for(model);
    let hit = usage.cache_hit_tokens as f64 * p.cache_hit_per_1m;
    let miss = usage.cache_miss_tokens as f64 * p.input_per_1m;
    let output = usage.completion_tokens as f64 * p.output_per_1m;
    (hit + miss + output) / 1_000_000.0
}

/// Format a cost: plain dollars when meaningful, scientific notation
/// for sub-micro-dollar amounts.
pub fn format_cost(cost: f64) -> String {
    if cost < 0.000_001 {
        format!("{:.2e}", cost)
    } else {
        format!("${:.6}", cost)
    }
}

/// Human-readable single-line usage summary, e.g.
/// `📊 4 prompt (0 cache-hit) + 1 output tokens ≈ $0.00028`.
pub fn usage_summary(model: &str, usage: &Usage) -> String {
    let cost = estimate_cost(model, usage);
    format!(
        "📊 Tokens: {} prompt ({} cache-hit, {} reasoning) + {} output ≈ {}",
        usage.prompt_tokens,
        usage.cache_hit_tokens,
        usage.reasoning_tokens,
        usage.completion_tokens,
        format_cost(cost)
    )
}
