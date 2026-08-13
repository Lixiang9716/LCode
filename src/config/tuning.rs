//! User-tunable runtime parameters (compaction, team, subagent,
//! memory, background, retry, event bus, todo limits).
//!
//! Every value here was previously a module-level hardcoded constant;
//! they now live in `config.toml` sections with `LCODE_*` env overrides.
//! Defaults match the pre-configuration behavior exactly.

use crate::config::settings::Config;
use serde::{Deserialize, Serialize};

/// Context compaction tuning (s06).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Token threshold at which the executor auto-compacts (G1)
    #[serde(default = "default_auto_compact_threshold")]
    pub auto_threshold: usize,

    /// Recent messages always kept verbatim by micro-compaction
    #[serde(default = "default_keep_recent")]
    pub keep_recent: usize,

    /// Conversation tail characters fed to the summarizer
    #[serde(default = "default_summary_tail_chars")]
    pub summary_tail_chars: usize,

    /// Skip compaction when the history is shorter than this
    #[serde(default = "default_compact_min_len")]
    pub min_len: usize,
}
impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            auto_threshold: default_auto_compact_threshold(),
            keep_recent: default_keep_recent(),
            summary_tail_chars: default_summary_tail_chars(),
            min_len: default_compact_min_len(),
        }
    }
}
#[doc(hidden)]
pub fn default_auto_compact_threshold() -> usize {
    50_000
}
#[doc(hidden)]
pub fn default_keep_recent() -> usize {
    3
}
#[doc(hidden)]
pub fn default_summary_tail_chars() -> usize {
    80_000
}
#[doc(hidden)]
pub fn default_compact_min_len() -> usize {
    100
}

/// Teammate loop tuning (s15).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    /// Max LLM turns per WORK phase
    #[serde(default = "default_team_work_turns")]
    pub work_turns: u32,

    /// Seconds between IDLE polls
    #[serde(default = "default_team_idle_interval_secs")]
    pub idle_interval_secs: u64,

    /// Empty IDLE polls before auto-shutdown
    #[serde(default = "default_team_idle_polls")]
    pub idle_polls: u32,
}
impl Default for TeamConfig {
    fn default() -> Self {
        Self {
            work_turns: default_team_work_turns(),
            idle_interval_secs: default_team_idle_interval_secs(),
            idle_polls: default_team_idle_polls(),
        }
    }
}
#[doc(hidden)]
pub fn default_team_work_turns() -> u32 {
    50
}
#[doc(hidden)]
pub fn default_team_idle_interval_secs() -> u64 {
    5
}
#[doc(hidden)]
pub fn default_team_idle_polls() -> u32 {
    12
}

/// Subagent delegation tuning (s04).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentConfig {
    /// Max turns per subagent
    #[serde(default = "default_subagent_max_turns")]
    pub max_turns: u32,

    /// Tool result characters kept per subagent turn
    #[serde(default = "default_subagent_tool_result_chars")]
    pub max_tool_result_chars: usize,
}
impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_subagent_max_turns(),
            max_tool_result_chars: default_subagent_tool_result_chars(),
        }
    }
}
#[doc(hidden)]
pub fn default_subagent_max_turns() -> u32 {
    30
}
#[doc(hidden)]
pub fn default_subagent_tool_result_chars() -> usize {
    50_000
}

/// Cross-session memory tuning (s09).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Files before consolidation kicks in
    #[serde(default = "default_consolidate_threshold")]
    pub consolidate_threshold: usize,

    /// Memories injected into the system prompt
    #[serde(default = "default_max_relevant")]
    pub max_relevant: usize,

    /// Dialogue characters fed to extraction
    #[serde(default = "default_max_extract_chars")]
    pub max_extract_chars: usize,

    /// Lock extraction/consolidation replies to JSON by forcing the
    /// reply to start with `[` (DeepSeek beta prefix completion). The
    /// model cannot then preamble with prose, so the JSON fence parses
    /// reliably. Requires the OpenAI-format DeepSeek endpoint
    /// (`provider = "openai_compatible"` + `api_base =
    /// "https://api.deepseek.com"`); other endpoints reject prefix
    /// requests and the extraction falls back to the plain prompt.
    #[serde(default)]
    pub json_lock: bool,
}
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            consolidate_threshold: default_consolidate_threshold(),
            max_relevant: default_max_relevant(),
            max_extract_chars: default_max_extract_chars(),
            json_lock: false,
        }
    }
}
#[doc(hidden)]
pub fn default_consolidate_threshold() -> usize {
    10
}
#[doc(hidden)]
pub fn default_max_relevant() -> usize {
    5
}
#[doc(hidden)]
pub fn default_max_extract_chars() -> usize {
    4000
}

/// Background command tuning (s08).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundConfig {
    /// Default timeout for background commands (seconds)
    #[serde(default = "default_background_timeout")]
    pub default_timeout_secs: u64,

    /// Result characters kept per completed task
    #[serde(default = "default_background_result_chars")]
    pub max_result_chars: usize,
}
impl Default for BackgroundConfig {
    fn default() -> Self {
        Self {
            default_timeout_secs: default_background_timeout(),
            max_result_chars: default_background_result_chars(),
        }
    }
}
#[doc(hidden)]
pub fn default_background_timeout() -> u64 {
    300
}
#[doc(hidden)]
pub fn default_background_result_chars() -> usize {
    50_000
}

/// Retry/backoff tuning (error recovery #4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Attempts per LLM call
    #[serde(default = "default_retry_attempts")]
    pub max_attempts: u32,

    /// Base backoff (milliseconds)
    #[serde(default = "default_retry_base_delay_ms")]
    pub base_delay_ms: u64,

    /// Backoff cap (milliseconds)
    #[serde(default = "default_retry_max_delay_ms")]
    pub max_delay_ms: u64,
}
impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_retry_attempts(),
            base_delay_ms: default_retry_base_delay_ms(),
            max_delay_ms: default_retry_max_delay_ms(),
        }
    }
}
#[doc(hidden)]
pub fn default_retry_attempts() -> u32 {
    5
}
#[doc(hidden)]
pub fn default_retry_base_delay_ms() -> u64 {
    500
}
#[doc(hidden)]
pub fn default_retry_max_delay_ms() -> u64 {
    30_000
}

/// Event bus / command channel capacities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsConfig {
    /// Broadcast event buffer size
    #[serde(default = "default_event_capacity")]
    pub channel_capacity: usize,

    /// Command channel buffer size
    #[serde(default = "default_command_capacity")]
    pub command_capacity: usize,
}
impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            channel_capacity: default_event_capacity(),
            command_capacity: default_command_capacity(),
        }
    }
}
#[doc(hidden)]
pub fn default_event_capacity() -> usize {
    256
}
#[doc(hidden)]
pub fn default_command_capacity() -> usize {
    64
}

/// Todo list limits (s03).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoConfig {
    /// Maximum tracked items
    #[serde(default = "default_max_todos")]
    pub max_items: usize,
}
impl Default for TodoConfig {
    fn default() -> Self {
        Self { max_items: default_max_todos() }
    }
}
#[doc(hidden)]
pub fn default_max_todos() -> usize {
    20
}

/// Bundle of runtime tuning parameters, built once from [`Config`] and
/// shared through the session state so the executor and its tools read
/// user-tunable values instead of module-level constants.
#[derive(Debug, Clone, Default)]
pub struct RuntimeTuning {
    pub compaction: CompactionConfig,
    pub team: TeamConfig,
    pub subagent: SubagentConfig,
    pub memory: MemoryConfig,
    pub background: BackgroundConfig,
    pub retry: RetryConfig,
    pub events: EventsConfig,
    pub todo: TodoConfig,
    pub todo_nag_after_turns: u32,
    /// URL fetches (read_file/write_file) ignore auto_approve and go
    /// through the approval channel when true.
    pub network_requires_approval: bool,
    /// Hard cost cap for the session (None = no cap).
    pub budget_total_usd: Option<f64>,
    /// Spend ratio that triggers the one-shot budget warning.
    pub budget_warning_ratio: f64,
    /// Model used for cost estimation (pricing tier lookup).
    pub cost_model: String,
    /// Fix-and-rerun reminder after failing test commands.
    pub test_until_green: bool,
    /// Self-review pass before finishing.
    pub self_review: bool,
    /// Restart rounds allowed for self-review fixes.
    pub self_review_max_rounds: u32,
}

impl RuntimeTuning {
    /// Build the tuning bundle from the full configuration.
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            compaction: cfg.compaction.clone(),
            team: cfg.team.clone(),
            subagent: cfg.subagent.clone(),
            memory: cfg.memory.clone(),
            background: cfg.background.clone(),
            retry: cfg.retry.clone(),
            events: cfg.events.clone(),
            todo: cfg.todo.clone(),
            todo_nag_after_turns: cfg.agent.todo_nag_after_turns,
            network_requires_approval: cfg.tools.network_requires_approval,
            budget_total_usd: cfg.llm.budget_total_usd,
            budget_warning_ratio: cfg.llm.budget_warning_ratio,
            cost_model: cfg.llm.model.clone(),
            test_until_green: cfg.agent.test_until_green,
            self_review: cfg.agent.self_review,
            self_review_max_rounds: cfg.agent.self_review_max_rounds,
        }
    }
}
