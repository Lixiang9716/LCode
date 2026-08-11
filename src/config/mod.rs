//! Configuration management for LCode.
//!
//! Configuration is loaded from (in order of precedence):
//! 1. Command-line arguments
//! 2. Environment variables (LCODE_ prefix)
//! 3. Project-local `.lcode.toml`
//! 4. User-global `~/.config/lcode/config.toml`
//!
//! Submodules:
//! - [`settings`]: configuration data structures and defaults
//! - [`commands`]: `lcode config` subcommand handlers

mod commands;
mod settings;

pub use commands::handle_command;
#[doc(hidden)]
pub use commands::{get_config_value, mask_key, set_config_value};
pub use settings::{AgentConfig, Config, LlmConfig, ToolsConfig};
#[doc(hidden)]
pub use settings::{
    default_context_size, default_max_tokens, default_max_turns, default_model,
    default_require_approval, default_temperature,
};
use std::path::PathBuf;

/// Load configuration from all sources.
pub fn load() -> anyhow::Result<Config> {
    let mut cfg = Config::default();

    // Load user-global config
    if let Some(global_path) = global_config_path() {
        if global_path.exists() {
            let content = std::fs::read_to_string(&global_path)?;
            let global_cfg: Config = toml::from_str(&content)?;
            merge_config(&mut cfg, global_cfg);
        }
    }

    // Load project-local config
    let local_path = PathBuf::from(".lcode.toml");
    if local_path.exists() {
        let content = std::fs::read_to_string(&local_path)?;
        let local_cfg: Config = toml::from_str(&content)?;
        merge_config(&mut cfg, local_cfg);
    }

    // Override with environment variables
    apply_env_overrides(&mut cfg);

    Ok(cfg)
}

/// Get the global config file path.
pub fn global_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("lcode").join("config.toml"))
}

/// Merge two configs, with `other` overriding fields in `base`.
#[doc(hidden)]
pub fn merge_config(base: &mut Config, other: Config) {
    if !other.llm.provider.is_empty() {
        base.llm.provider = other.llm.provider;
    }
    if !other.llm.api_key.is_empty() {
        base.llm.api_key = other.llm.api_key;
    }
    if other.llm.model != default_model() || !other.llm.model.is_empty() {
        base.llm.model = other.llm.model;
    }
    if other.llm.api_base.is_some() {
        base.llm.api_base = other.llm.api_base;
    }
    if other.llm.max_tokens != default_max_tokens() {
        base.llm.max_tokens = other.llm.max_tokens;
    }
    if other.llm.temperature != default_temperature() {
        base.llm.temperature = other.llm.temperature;
    }
    if !other.agent.system_prompt.is_empty() {
        base.agent.system_prompt = other.agent.system_prompt;
    }
    if other.agent.max_turns != default_max_turns() {
        base.agent.max_turns = other.agent.max_turns;
    }
    if other.agent.require_approval != default_require_approval() {
        base.agent.require_approval = other.agent.require_approval;
    }
    if other.agent.context_size != default_context_size() {
        base.agent.context_size = other.agent.context_size;
    }
    if !other.tools.allowed_dirs.is_empty() {
        base.tools.allowed_dirs = other.tools.allowed_dirs;
    }
    if !other.tools.allowed_commands.is_empty() {
        base.tools.allowed_commands = other.tools.allowed_commands;
    }
    if !other.tools.denied_commands.is_empty() {
        base.tools.denied_commands = other.tools.denied_commands;
    }
    base.tools.enable_web = other.tools.enable_web;
}

/// Apply environment variable overrides (LCODE_ prefix).
#[doc(hidden)]
pub fn apply_env_overrides(cfg: &mut Config) {
    if let Ok(val) = std::env::var("LCODE_LLM_PROVIDER") {
        cfg.llm.provider = val;
    }
    if let Ok(val) = std::env::var("LCODE_LLM_API_KEY") {
        cfg.llm.api_key = val;
    }
    if let Ok(val) = std::env::var("LCODE_LLM_MODEL") {
        cfg.llm.model = val;
    }
    if let Ok(val) = std::env::var("LCODE_LLM_API_BASE") {
        cfg.llm.api_base = Some(val);
    }
    if let Ok(val) = std::env::var("LCODE_LLM_MAX_TOKENS") {
        if let Ok(n) = val.parse() {
            cfg.llm.max_tokens = n;
        }
    }
}
