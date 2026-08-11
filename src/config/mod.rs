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
pub use settings::{AgentConfig, Config, LlmConfig, ToolsConfig};

use settings::{
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
fn merge_config(base: &mut Config, other: Config) {
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
fn apply_env_overrides(cfg: &mut Config) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ------------------------------------------------------------------
    // merge_config
    // ------------------------------------------------------------------

    #[test]
    fn merge_config_non_empty_values_override_base() {
        let mut base = Config::default();
        let mut other = Config::default();
        other.llm.provider = "openai".into();
        other.llm.api_key = "sk-merge-test".into();
        other.llm.model = "gpt-4o".into();
        other.llm.api_base = Some("https://api.example.com".into());
        other.llm.max_tokens = 4096;
        other.llm.temperature = 0.7;
        other.agent.system_prompt = "custom system prompt".into();
        other.agent.max_turns = 5;
        other.agent.require_approval = false;
        other.agent.context_size = 64_000;
        other.tools.allowed_commands = vec!["git".into()];
        other.tools.denied_commands = vec!["shutdown".into()];
        other.tools.enable_web = false;

        merge_config(&mut base, other);

        assert_eq!(base.llm.provider, "openai");
        assert_eq!(base.llm.api_key, "sk-merge-test");
        assert_eq!(base.llm.model, "gpt-4o");
        assert_eq!(base.llm.api_base.as_deref(), Some("https://api.example.com"));
        assert_eq!(base.llm.max_tokens, 4096);
        assert_eq!(base.llm.temperature, 0.7);
        assert_eq!(base.agent.system_prompt, "custom system prompt");
        assert_eq!(base.agent.max_turns, 5);
        assert!(!base.agent.require_approval);
        assert_eq!(base.agent.context_size, 64_000);
        assert_eq!(base.tools.allowed_commands, vec!["git"]);
        assert_eq!(base.tools.denied_commands, vec!["shutdown"]);
        assert!(!base.tools.enable_web);
    }

    #[test]
    fn merge_config_empty_values_keep_base() {
        let mut base = Config::default();
        base.llm.provider = "openai".into();
        base.llm.api_key = "sk-base-key".into();
        base.llm.model = "gpt-4o".into();
        base.llm.api_base = Some("https://base.example.com".into());
        base.llm.max_tokens = 4096;
        base.llm.temperature = 1.5;
        base.agent.system_prompt = "base prompt".into();
        base.agent.max_turns = 42;
        base.agent.require_approval = false;
        base.agent.context_size = 64_000;
        base.tools.allowed_commands = vec!["git".into()];
        base.tools.denied_commands = vec!["danger".into()];

        // `other` holds every field at its empty / default sentinel value,
        // which must NOT clobber the values already present in `base`.
        // (llm.model is excluded: merge_config always overrides it — see
        // `merge_config_model_is_always_overridden`.)
        let mut other = Config::default();
        other.llm.provider.clear();
        other.llm.api_key.clear();
        other.llm.api_base = None;
        other.llm.max_tokens = default_max_tokens();
        other.llm.temperature = default_temperature();
        other.agent.system_prompt.clear();
        other.agent.max_turns = default_max_turns();
        other.agent.require_approval = default_require_approval();
        other.agent.context_size = settings::default_context_size();
        other.tools.allowed_commands.clear();
        other.tools.denied_commands.clear();

        merge_config(&mut base, other);

        assert_eq!(base.llm.provider, "openai");
        assert_eq!(base.llm.api_key, "sk-base-key");
        assert_eq!(base.llm.api_base.as_deref(), Some("https://base.example.com"));
        assert_eq!(base.llm.max_tokens, 4096);
        assert_eq!(base.llm.temperature, 1.5);
        assert_eq!(base.agent.system_prompt, "base prompt");
        assert_eq!(base.agent.max_turns, 42);
        assert!(!base.agent.require_approval);
        assert_eq!(base.agent.context_size, 64_000);
        assert_eq!(base.tools.allowed_commands, vec!["git"]);
        assert_eq!(base.tools.denied_commands, vec!["danger"]);
    }

    #[test]
    fn merge_config_model_is_always_overridden() {
        // Quirk of the current implementation: the guard for `llm.model` is
        // `other != default || !other.is_empty()`, which is true for every
        // possible value (including the default and the empty string), so the
        // model from `other` always replaces the one in `base`.
        let mut base = Config::default();
        base.llm.model = "gpt-4o".into();

        let mut other = Config::default();
        other.llm.model.clear();
        merge_config(&mut base, other);
        assert_eq!(base.llm.model, "");

        let mut base = Config::default();
        base.llm.model = "gpt-4o".into();
        let other = Config::default(); // model == default model
        merge_config(&mut base, other);
        assert_eq!(base.llm.model, default_model());
    }

    #[test]
    fn merge_config_enable_web_is_overwritten_unconditionally() {
        // Documented behavior of merge_config: tools.enable_web is assigned
        // from `other` without checking for a default sentinel value.
        let mut base = Config::default();
        base.tools.enable_web = false;
        let other = Config::default(); // enable_web = true
        merge_config(&mut base, other);
        assert!(base.tools.enable_web);
    }

    // ------------------------------------------------------------------
    // apply_env_overrides (LCODE_* variables)
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn apply_env_overrides_sets_all_llm_fields() {
        std::env::set_var("LCODE_LLM_PROVIDER", "openai");
        std::env::set_var("LCODE_LLM_API_KEY", "sk-env-key");
        std::env::set_var("LCODE_LLM_MODEL", "gpt-4o");
        std::env::set_var("LCODE_LLM_API_BASE", "https://env.example.com");
        std::env::set_var("LCODE_LLM_MAX_TOKENS", "2048");

        let mut cfg = Config::default();
        apply_env_overrides(&mut cfg);

        assert_eq!(cfg.llm.provider, "openai");
        assert_eq!(cfg.llm.api_key, "sk-env-key");
        assert_eq!(cfg.llm.model, "gpt-4o");
        assert_eq!(cfg.llm.api_base.as_deref(), Some("https://env.example.com"));
        assert_eq!(cfg.llm.max_tokens, 2048);

        std::env::remove_var("LCODE_LLM_PROVIDER");
        std::env::remove_var("LCODE_LLM_API_KEY");
        std::env::remove_var("LCODE_LLM_MODEL");
        std::env::remove_var("LCODE_LLM_API_BASE");
        std::env::remove_var("LCODE_LLM_MAX_TOKENS");
    }

    #[test]
    #[serial]
    fn apply_env_overrides_ignores_invalid_max_tokens() {
        std::env::set_var("LCODE_LLM_MAX_TOKENS", "not-a-number");
        let mut cfg = Config::default();
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.llm.max_tokens, default_max_tokens());
        std::env::remove_var("LCODE_LLM_MAX_TOKENS");
    }

    #[test]
    #[serial]
    fn apply_env_overrides_without_vars_changes_nothing() {
        std::env::remove_var("LCODE_LLM_PROVIDER");
        std::env::remove_var("LCODE_LLM_API_KEY");
        std::env::remove_var("LCODE_LLM_MODEL");
        std::env::remove_var("LCODE_LLM_API_BASE");
        std::env::remove_var("LCODE_LLM_MAX_TOKENS");

        let mut cfg = Config::default();
        cfg.llm.provider = "openai".into();
        cfg.llm.max_tokens = 1234;
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.llm.provider, "openai");
        assert_eq!(cfg.llm.max_tokens, 1234);
    }
}
