//! Configuration management for LCode.
//!
//! Configuration is loaded from (in order of precedence):
//! 1. Command-line arguments
//! 2. Environment variables (LCODE_ prefix)
//! 3. Project-local `.lcode.toml`
//! 4. User-global `~/.config/lcode/config.toml`
//!
//! Submodules:
//! - `settings`: configuration data structures and defaults
//! - `commands`: `lcode config` subcommand handlers

mod commands;
mod settings;
mod tuning;

pub use commands::handle_command;
#[doc(hidden)]
pub use commands::{get_config_value, mask_key, set_config_value};
#[doc(hidden)]
pub use settings::{
    default_context_size, default_max_tokens, default_max_turns, default_model,
    default_require_approval, default_temperature,
};
pub use settings::{AgentConfig, Config, LlmConfig, ToolsConfig};
use std::path::PathBuf;
pub use tuning::{
    BackgroundConfig, CompactionConfig, EventsConfig, MemoryConfig, RetryConfig, RuntimeTuning,
    SubagentConfig, TeamConfig, TodoConfig,
};

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
    if other.llm.fallback_model.is_some() {
        base.llm.fallback_model = other.llm.fallback_model;
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
    if let Ok(val) = std::env::var("LCODE_LLM_FALLBACK_MODEL") {
        cfg.llm.fallback_model = Some(val);
    }

    // Runtime tuning overrides (one env var per tunable).
    set_u32_env("LCODE_AGENT_TODO_NAG_AFTER_TURNS", &mut cfg.agent.todo_nag_after_turns);
    set_usize_env("LCODE_COMPACTION_AUTO_THRESHOLD", &mut cfg.compaction.auto_threshold);
    set_usize_env("LCODE_COMPACTION_KEEP_RECENT", &mut cfg.compaction.keep_recent);
    set_usize_env("LCODE_COMPACTION_SUMMARY_TAIL_CHARS", &mut cfg.compaction.summary_tail_chars);
    set_usize_env("LCODE_COMPACTION_MIN_LEN", &mut cfg.compaction.min_len);
    set_u32_env("LCODE_TEAM_WORK_TURNS", &mut cfg.team.work_turns);
    set_u64_env("LCODE_TEAM_IDLE_INTERVAL_SECS", &mut cfg.team.idle_interval_secs);
    set_u32_env("LCODE_TEAM_IDLE_POLLS", &mut cfg.team.idle_polls);
    set_u32_env("LCODE_SUBAGENT_MAX_TURNS", &mut cfg.subagent.max_turns);
    set_usize_env("LCODE_SUBAGENT_MAX_TOOL_RESULT_CHARS", &mut cfg.subagent.max_tool_result_chars);
    set_usize_env("LCODE_MEMORY_CONSOLIDATE_THRESHOLD", &mut cfg.memory.consolidate_threshold);
    set_usize_env("LCODE_MEMORY_MAX_RELEVANT", &mut cfg.memory.max_relevant);
    set_usize_env("LCODE_MEMORY_MAX_EXTRACT_CHARS", &mut cfg.memory.max_extract_chars);
    set_u64_env("LCODE_BACKGROUND_DEFAULT_TIMEOUT_SECS", &mut cfg.background.default_timeout_secs);
    set_usize_env("LCODE_BACKGROUND_MAX_RESULT_CHARS", &mut cfg.background.max_result_chars);
    set_u32_env("LCODE_RETRY_MAX_ATTEMPTS", &mut cfg.retry.max_attempts);
    set_u64_env("LCODE_RETRY_BASE_DELAY_MS", &mut cfg.retry.base_delay_ms);
    set_u64_env("LCODE_RETRY_MAX_DELAY_MS", &mut cfg.retry.max_delay_ms);
    set_usize_env("LCODE_EVENTS_CHANNEL_CAPACITY", &mut cfg.events.channel_capacity);
    set_usize_env("LCODE_EVENTS_COMMAND_CAPACITY", &mut cfg.events.command_capacity);
    set_usize_env("LCODE_TODO_MAX_ITEMS", &mut cfg.todo.max_items);
}

/// Set a `u32` field from a `LCODE_*` env var when it parses.
fn set_u32_env(key: &str, target: &mut u32) {
    if let Ok(val) = std::env::var(key) {
        if let Ok(n) = val.parse() {
            *target = n;
        }
    }
}

/// Set a `u64` field from a `LCODE_*` env var when it parses.
fn set_u64_env(key: &str, target: &mut u64) {
    if let Ok(val) = std::env::var(key) {
        if let Ok(n) = val.parse() {
            *target = n;
        }
    }
}

/// Set a `usize` field from a `LCODE_*` env var when it parses.
fn set_usize_env(key: &str, target: &mut usize) {
    if let Ok(val) = std::env::var(key) {
        if let Ok(n) = val.parse() {
            *target = n;
        }
    }
}
