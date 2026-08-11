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
            println!("  agent.system_prompt - Custom system prompt");
            println!("  agent.max_turns     - Max conversation turns");
            println!("  agent.require_approval - Require approval for tool calls");
            println!("  tools.allowed_dirs  - Allowed directories (comma-separated)");
            println!("  tools.allowed_commands - Always-allowed shell commands");
            println!("  tools.enable_web    - Enable web tools (true/false)");
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
        "agent.system_prompt" => Ok(cfg.agent.system_prompt.clone()),
        "agent.max_turns" => Ok(cfg.agent.max_turns.to_string()),
        "agent.require_approval" => Ok(cfg.agent.require_approval.to_string()),
        "tools.enable_web" => Ok(cfg.tools.enable_web.to_string()),
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

    // Load or create config
    let mut cfg: Config = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content).unwrap_or_default()
    } else {
        Config::default()
    };

    // Update the specific field
    match key {
        "llm.provider" => cfg.llm.provider = value.to_string(),
        "llm.api_key" => cfg.llm.api_key = value.to_string(),
        "llm.model" => cfg.llm.model = value.to_string(),
        "llm.api_base" => cfg.llm.api_base = Some(value.to_string()),
        "llm.max_tokens" => cfg.llm.max_tokens = value.parse()?,
        "llm.temperature" => cfg.llm.temperature = value.parse()?,
        "agent.system_prompt" => cfg.agent.system_prompt = value.to_string(),
        "agent.max_turns" => cfg.agent.max_turns = value.parse()?,
        "agent.require_approval" => cfg.agent.require_approval = value.parse()?,
        "tools.enable_web" => cfg.tools.enable_web = value.parse()?,
        _ => anyhow::bail!(
            "Unknown config key: {}. Use `lcode config list` to see available keys.",
            key
        ),
    }

    // Write config
    let content = toml::to_string_pretty(&cfg)?;
    std::fs::write(&config_path, content)?;

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
