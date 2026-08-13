//! Configuration data structures and their default values.

use crate::config::tuning::{
    BackgroundConfig, CompactionConfig, EventsConfig, MemoryConfig, RetryConfig, SubagentConfig,
    TeamConfig, TodoConfig,
};
use serde::{Deserialize, Serialize};

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// LLM provider settings
    #[serde(default)]
    pub llm: LlmConfig,

    /// Agent behavior settings
    #[serde(default)]
    pub agent: AgentConfig,

    /// Tool-specific settings
    #[serde(default)]
    pub tools: ToolsConfig,

    /// Context compaction tuning
    #[serde(default)]
    pub compaction: CompactionConfig,

    /// Teammate loop tuning
    #[serde(default)]
    pub team: TeamConfig,

    /// Subagent delegation tuning
    #[serde(default)]
    pub subagent: SubagentConfig,

    /// Cross-session memory tuning
    #[serde(default)]
    pub memory: MemoryConfig,

    /// Background command tuning
    #[serde(default)]
    pub background: BackgroundConfig,

    /// Retry/backoff tuning
    #[serde(default)]
    pub retry: RetryConfig,

    /// Event bus / command channel capacities
    #[serde(default)]
    pub events: EventsConfig,

    /// Todo list limits
    #[serde(default)]
    pub todo: TodoConfig,
}

/// LLM provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Provider name: "openai", "anthropic", "openai_compatible"
    #[serde(default = "default_provider")]
    pub provider: String,

    /// API key for the provider
    #[serde(default)]
    pub api_key: String,

    /// Model name to use
    #[serde(default = "default_model")]
    pub model: String,

    /// API base URL (for OpenAI-compatible providers)
    #[serde(default)]
    pub api_base: Option<String>,

    /// Maximum tokens in a single response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,

    /// Temperature for generation
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// Model to fail over to when the primary model keeps failing (used
    /// by the retry layer after `max_attempts` consecutive failures)
    #[serde(default)]
    pub fallback_model: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            api_key: String::new(),
            model: default_model(),
            api_base: None,
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            fallback_model: None,
        }
    }
}

/// Agent behavior configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// System prompt or path to system prompt template
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,

    /// Maximum conversation turns in a session
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Require user approval before executing tools
    #[serde(default = "default_require_approval")]
    pub require_approval: bool,

    /// Maximum context window size (in tokens)
    #[serde(default = "default_context_size")]
    pub context_size: usize,

    /// Directory containing skills (`SKILL.md` files). Defaults to
    /// `<workspace>/skills` when unset (G9 / s07).
    #[serde(default)]
    pub skills_dir: Option<std::path::PathBuf>,

    /// Turns without a todo update before the nag reminder fires (s03)
    #[serde(default = "default_todo_nag_after_turns")]
    pub todo_nag_after_turns: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: default_system_prompt(),
            max_turns: default_max_turns(),
            require_approval: default_require_approval(),
            context_size: default_context_size(),
            skills_dir: None,
            todo_nag_after_turns: default_todo_nag_after_turns(),
        }
    }
}

/// Tool permission settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// Allowed directories for file operations (empty = project root only)
    #[serde(default)]
    pub allowed_dirs: Vec<String>,

    /// Shell commands that are always allowed
    #[serde(default)]
    pub allowed_commands: Vec<String>,

    /// Shell commands that are always denied
    #[serde(default)]
    pub denied_commands: Vec<String>,

    /// Enable web fetch/search tools
    #[serde(default = "default_true")]
    pub enable_web: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            allowed_dirs: Vec::new(),
            allowed_commands: Vec::new(),
            denied_commands: vec![
                "rm -rf /".into(),
                "sudo".into(),
                "chmod 777".into(),
                "mkfs".into(),
            ],
            enable_web: true,
        }
    }
}

// Default value functions. `pub(super)` so `config` module can use them in
// merge/override logic; the ones re-exported for the test suite are
// `#[doc(hidden)] pub`. serde `default =` paths resolve within this module.
pub(super) fn default_provider() -> String {
    "anthropic".into()
}
#[doc(hidden)]
pub fn default_model() -> String {
    "claude-sonnet-4-20250514".into()
}
#[doc(hidden)]
pub fn default_max_tokens() -> u32 {
    8192
}
#[doc(hidden)]
pub fn default_temperature() -> f32 {
    0.3
}
pub(super) fn default_system_prompt() -> String {
    "You are LCode, an expert software engineer and coding agent. \
     You help users write, review, debug, and understand code. \
     Be concise, accurate, and helpful."
        .into()
}
#[doc(hidden)]
pub fn default_max_turns() -> u32 {
    100
}
#[doc(hidden)]
pub fn default_require_approval() -> bool {
    true
}
#[doc(hidden)]
pub fn default_context_size() -> usize {
    128000
}
#[doc(hidden)]
pub fn default_todo_nag_after_turns() -> u32 {
    3
}
pub(super) fn default_true() -> bool {
    true
}
