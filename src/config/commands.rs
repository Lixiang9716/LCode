//! `lcode config` subcommand handlers: show / list / get / set.

use super::settings::Config;
use super::{global_config_path, load};
use crate::cli::ConfigAction;

/// Handle config subcommands.
pub fn handle_command(action: ConfigAction) -> anyhow::Result<()> {
    match action {
        ConfigAction::Show => {
            let cfg = load()?;
            println!("{}", toml::to_string_pretty(&cfg)?);
        }
        ConfigAction::List => {
            println!("Available configuration keys:");
            println!("  llm.provider        - LLM provider (openai, anthropic, openai_compatible)");
            println!("  llm.api_key         - API key for the provider");
            println!("  llm.model           - Model name");
            println!("  llm.api_base        - Custom API base URL");
            println!("  llm.max_tokens      - Max tokens per response");
            println!("  llm.temperature     - Generation temperature (0.0-2.0)");
            println!("  llm.fallback_model  - Fallback model when retries are exhausted");
            println!("  llm.thinking_disabled - Disable the provider's thinking mode (true/false)");
            println!("  llm.reasoning_effort  - Thinking effort: low / high / max");
            println!("  llm.internal_thinking_disabled - Force thinking off for internal calls (true/false)");
            println!("  memory.json_lock    - Lock memory extraction replies to JSON via prefix (true/false)");
            println!("  agent.system_prompt - Custom system prompt");
            println!("  agent.max_turns     - Max conversation turns");
            println!("  agent.require_approval - Require approval for tool calls");
            println!("  tools.allowed_dirs  - Allowed directories (comma-separated)");
            println!("  tools.allowed_commands - Always-allowed shell commands");
            println!("  tools.enable_web    - Enable web tools (true/false)");
            println!("  tools.max_fetch_bytes - Max bytes for one URL fetch");
            println!("  tools.fetch_timeout_secs - URL fetch timeout (seconds)");
            println!("  tools.network_requires_approval - Always approve URL fetches (true/false)");
        }
        ConfigAction::Get { key } => {
            let cfg = load()?;
            let value = get_config_value(&cfg, &key)?;
            println!("{} = {}", key, value);
        }
        ConfigAction::Set { key, value } => {
            set_config_value(&key, &value)?;
            println!("Set {} = {}", key, value);
        }
    }
    Ok(())
}

/// Get a specific config value by key path.
#[doc(hidden)]
pub fn get_config_value(cfg: &Config, key: &str) -> anyhow::Result<String> {
    match key {
        "llm.provider" => Ok(cfg.llm.provider.clone()),
        "llm.api_key" => Ok(mask_key(&cfg.llm.api_key)),
        "llm.model" => Ok(cfg.llm.model.clone()),
        "llm.api_base" => Ok(cfg.llm.api_base.clone().unwrap_or_default()),
        "llm.max_tokens" => Ok(cfg.llm.max_tokens.to_string()),
        "llm.temperature" => Ok(cfg.llm.temperature.to_string()),
        "llm.fallback_model" => Ok(cfg.llm.fallback_model.clone().unwrap_or_default()),
        "llm.thinking_disabled" => Ok(cfg.llm.thinking_disabled.to_string()),
        "llm.reasoning_effort" => {
            Ok(cfg.llm.reasoning_effort.map(|e| e.as_str().to_string()).unwrap_or_default())
        }
        "llm.internal_thinking_disabled" => Ok(cfg.llm.internal_thinking_disabled.to_string()),
        "memory.json_lock" => Ok(cfg.memory.json_lock.to_string()),
        "agent.system_prompt" => Ok(cfg.agent.system_prompt.clone()),
        "agent.max_turns" => Ok(cfg.agent.max_turns.to_string()),
        "agent.require_approval" => Ok(cfg.agent.require_approval.to_string()),
        "tools.enable_web" => Ok(cfg.tools.enable_web.to_string()),
        "tools.max_fetch_bytes" => Ok(cfg.tools.max_fetch_bytes.to_string()),
        "tools.fetch_timeout_secs" => Ok(cfg.tools.fetch_timeout_secs.to_string()),
        "tools.network_requires_approval" => Ok(cfg.tools.network_requires_approval.to_string()),
        _ => anyhow::bail!("Unknown config key: {}", key),
    }
}

/// Persist a config value to the global config file.
#[doc(hidden)]
pub fn set_config_value(key: &str, value: &str) -> anyhow::Result<()> {
    let config_path =
        global_config_path().ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;

    // Ensure parent directory exists
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Load or create config. A hand-edited file that fails to parse
    // must abort loudly instead of silently wiping every stored value
    // (api_key, model, ...) back to defaults.
    let mut cfg: Config = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content).map_err(|e| {
            anyhow::anyhow!(
                "{} is invalid TOML ({}). Fix it before changing config via the CLI.",
                config_path.display(),
                e
            )
        })?
    } else {
        Config::default()
    };

    // Dispatch to the section helper, then persist exactly once. The
    // helpers mutate `cfg` in place and only fail on unknown keys, so
    // the file is never half-written.
    set_field(&mut cfg, key, value)?;

    let content = toml::to_string_pretty(&cfg)?;
    std::fs::write(&config_path, content)?;
    Ok(())
}

/// Apply one `section.key` value to the config in memory.
fn set_field(cfg: &mut Config, key: &str, value: &str) -> anyhow::Result<()> {
    if let Some(tool_key) = key.strip_prefix("tools.") {
        return set_tools_value(&mut cfg.tools, tool_key, value);
    }
    if let Some(agent_key) = key.strip_prefix("agent.") {
        return set_agent_value(&mut cfg.agent, agent_key, value);
    }
    if let Some(memory_key) = key.strip_prefix("memory.") {
        return set_memory_value(&mut cfg.memory, memory_key, value);
    }
    if let Some(llm_key) = key.strip_prefix("llm.") {
        return set_llm_value(&mut cfg.llm, llm_key, value);
    }
    anyhow::bail!("Unknown config key: {key}. Use `lcode config list` to see available keys.");
}

/// Set an `llm.*` scalar config key.
fn set_llm_value(llm: &mut crate::config::LlmConfig, key: &str, value: &str) -> anyhow::Result<()> {
    match key {
        "provider" => llm.provider = value.to_string(),
        "api_key" => llm.api_key = value.to_string(),
        "model" => llm.model = value.to_string(),
        "api_base" => llm.api_base = Some(value.to_string()),
        "max_tokens" => llm.max_tokens = value.parse()?,
        "temperature" => llm.temperature = value.parse()?,
        "fallback_model" => {
            llm.fallback_model = if value.is_empty() { None } else { Some(value.to_string()) }
        }
        "thinking_disabled" => llm.thinking_disabled = value.parse()?,
        "reasoning_effort" => {
            // Empty / "none" clears the setting (back to the model
            // default tier, high).
            llm.reasoning_effort = if value.is_empty() || value.eq_ignore_ascii_case("none") {
                None
            } else {
                Some(value.parse().map_err(|e: String| anyhow::anyhow!(e))?)
            };
        }
        "internal_thinking_disabled" => llm.internal_thinking_disabled = value.parse()?,
        other => anyhow::bail!("Unknown config key: llm.{other}"),
    }
    Ok(())
}

/// Set an `agent.*` scalar config key.
fn set_agent_value(
    agent: &mut crate::config::AgentConfig,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    match key {
        "system_prompt" => agent.system_prompt = value.to_string(),
        "max_turns" => agent.max_turns = value.parse()?,
        "require_approval" => agent.require_approval = value.parse()?,
        other => anyhow::bail!("Unknown config key: agent.{other}"),
    }
    Ok(())
}

/// Set a `memory.*` scalar config key.
fn set_memory_value(
    memory: &mut crate::config::MemoryConfig,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    match key {
        "json_lock" => memory.json_lock = value.parse()?,
        other => anyhow::bail!("Unknown config key: memory.{other}"),
    }
    Ok(())
}

/// Set a `tools.*` scalar config key.
fn set_tools_value(
    tools: &mut crate::config::ToolsConfig,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    match key {
        "enable_web" => tools.enable_web = value.parse()?,
        "max_fetch_bytes" => tools.max_fetch_bytes = value.parse()?,
        "fetch_timeout_secs" => tools.fetch_timeout_secs = value.parse()?,
        "network_requires_approval" => tools.network_requires_approval = value.parse()?,
        other => anyhow::bail!("Unknown config key: tools.{other}"),
    }
    Ok(())
}

/// Mask an API key for display, showing only last 4 characters.
#[doc(hidden)]
pub fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".to_string();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}
