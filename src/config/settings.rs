//! Configuration data structures and their default values.

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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: default_system_prompt(),
            max_turns: default_max_turns(),
            require_approval: default_require_approval(),
            context_size: default_context_size(),
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

// Default value functions (pub(super) so `config` module can use them in
// merge/override logic; serde `default =` paths resolve within this module).
pub(super) fn default_provider() -> String {
    "anthropic".into()
}
pub(super) fn default_model() -> String {
    "claude-sonnet-4-20250514".into()
}
pub(super) fn default_max_tokens() -> u32 {
    8192
}
pub(super) fn default_temperature() -> f32 {
    0.3
}
pub(super) fn default_system_prompt() -> String {
    "You are LCode, an expert software engineer and coding agent. \
     You help users write, review, debug, and understand code. \
     Be concise, accurate, and helpful."
        .into()
}
pub(super) fn default_max_turns() -> u32 {
    100
}
pub(super) fn default_require_approval() -> bool {
    true
}
pub(super) fn default_context_size() -> usize {
    128000
}
pub(super) fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_values() {
        let cfg = Config::default();

        // LLM settings
        assert_eq!(cfg.llm.provider, "anthropic");
        assert_eq!(cfg.llm.model, "claude-sonnet-4-20250514");
        assert_eq!(cfg.llm.max_tokens, 8192);
        assert_eq!(cfg.llm.temperature, 0.3);
        assert!(cfg.llm.api_key.is_empty());
        assert!(cfg.llm.api_base.is_none());

        // Agent settings
        assert_eq!(cfg.agent.max_turns, 100);
        assert!(cfg.agent.require_approval);
        assert_eq!(cfg.agent.context_size, 128_000);
        assert!(cfg.agent.system_prompt.contains("LCode"));

        // Tool settings
        assert!(cfg.tools.allowed_dirs.is_empty());
        assert!(cfg.tools.allowed_commands.is_empty());
        assert!(cfg.tools.denied_commands.iter().any(|c| c == "sudo"));
        assert!(cfg.tools.denied_commands.iter().any(|c| c == "rm -rf /"));
        assert!(cfg.tools.denied_commands.iter().any(|c| c == "chmod 777"));
        assert!(cfg.tools.denied_commands.iter().any(|c| c == "mkfs"));
        assert!(cfg.tools.enable_web);
    }
}
