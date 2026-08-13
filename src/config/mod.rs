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
pub use settings::{AgentConfig, Config, LlmConfig, ReasoningEffort, ToolsConfig};
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
    merge_llm(&mut base.llm, other.llm);
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
    if other.memory.json_lock {
        base.memory.json_lock = true;
    }
}

/// Apply environment variable overrides (LCODE_ prefix).
#[doc(hidden)]
pub fn apply_env_overrides(cfg: &mut Config) {
    apply_llm_env_overrides(&mut cfg.llm);

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
    if let Ok(val) = std::env::var("LCODE_MEMORY_JSON_LOCK") {
        cfg.memory.json_lock = val == "1" || val.eq_ignore_ascii_case("true");
    }
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

/// Merge `other` LLM settings over `base`, keeping unset fields in
/// `other` (defaults) from stomping explicit values in `base`.
fn merge_llm(base: &mut LlmConfig, other: LlmConfig) {
    if !other.provider.is_empty() {
        base.provider = other.provider;
    }
    if !other.api_key.is_empty() {
        base.api_key = other.api_key;
    }
    if other.model != default_model() || !other.model.is_empty() {
        base.model = other.model;
    }
    if other.api_base.is_some() {
        base.api_base = other.api_base;
    }
    if other.max_tokens != default_max_tokens() {
        base.max_tokens = other.max_tokens;
    }
    if other.temperature != default_temperature() {
        base.temperature = other.temperature;
    }
    if other.fallback_model.is_some() {
        base.fallback_model = other.fallback_model;
    }
    if other.thinking_disabled {
        base.thinking_disabled = true;
    }
    if other.reasoning_effort.is_some() {
        base.reasoning_effort = other.reasoning_effort;
    }
    // One-way merge (false wins): the field defaults to `true`, so a
    // project file that never mentions it must not stomp a global
    // `false`. Consequence, documented in the README: a project layer
    // can relax the internal no-thinking setting but not tighten it
    // back — use the env override or the global file to re-enable.
    if !other.internal_thinking_disabled {
        base.internal_thinking_disabled = false;
    }
}

/// Apply the `LCODE_LLM_*` environment overrides.
fn apply_llm_env_overrides(llm: &mut LlmConfig) {
    if let Ok(val) = std::env::var("LCODE_LLM_PROVIDER") {
        llm.provider = val;
    }
    if let Ok(val) = std::env::var("LCODE_LLM_API_KEY") {
        llm.api_key = val;
    }
    if let Ok(val) = std::env::var("LCODE_LLM_MODEL") {
        llm.model = val;
    }
    if let Ok(val) = std::env::var("LCODE_LLM_API_BASE") {
        llm.api_base = Some(val);
    }
    if let Ok(val) = std::env::var("LCODE_LLM_MAX_TOKENS") {
        if let Ok(n) = val.parse() {
            llm.max_tokens = n;
        }
    }
    if let Ok(val) = std::env::var("LCODE_LLM_FALLBACK_MODEL") {
        llm.fallback_model = Some(val);
    }
    if let Ok(val) = std::env::var("LCODE_LLM_THINKING_DISABLED") {
        llm.thinking_disabled = val == "1" || val.eq_ignore_ascii_case("true");
    }
    if let Ok(val) = std::env::var("LCODE_LLM_REASONING_EFFORT") {
        // Invalid values leave the config-file value untouched (unlike
        // a silent `None` overwrite).
        match val.parse::<settings::ReasoningEffort>() {
            Ok(effort) => llm.reasoning_effort = Some(effort),
            Err(e) => tracing::warn!(env = "LCODE_LLM_REASONING_EFFORT", "{e}"),
        }
    }
    if let Ok(val) = std::env::var("LCODE_LLM_INTERNAL_THINKING_DISABLED") {
        llm.internal_thinking_disabled = val == "1" || val.eq_ignore_ascii_case("true");
    }
}
