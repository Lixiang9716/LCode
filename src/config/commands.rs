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
fn get_config_value(cfg: &Config, key: &str) -> anyhow::Result<String> {
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
fn set_config_value(key: &str, value: &str) -> anyhow::Result<()> {
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
fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "***".to_string();
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ConfigAction;
    use serial_test::serial;

    // ------------------------------------------------------------------
    // mask_key
    // ------------------------------------------------------------------

    #[test]
    fn mask_key_short_keys_return_stars() {
        assert_eq!(mask_key(""), "***");
        assert_eq!(mask_key("abc"), "***");
        assert_eq!(mask_key("12345678"), "***");
    }

    #[test]
    fn mask_key_long_keys_show_first_and_last_four() {
        let key = "sk-ant-1234567890abcdef";
        let expected = format!("{}...{}", &key[..4], &key[key.len() - 4..]);
        assert_eq!(mask_key(key), expected);
        assert_eq!(mask_key("abcdefghijklmnopqrstuvwxyz"), "abcd...wxyz");
    }

    // ------------------------------------------------------------------
    // set_config_value / get_config_value (HOME-isolated)
    // ------------------------------------------------------------------

    /// Point $HOME at a fresh temp dir so `dirs::config_dir()` resolves to
    /// an isolated, empty location. Must be called from a `#[serial]` test.
    fn isolate_home(temp_dir: &tempfile::TempDir) {
        std::env::set_var("HOME", temp_dir.path());
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    #[serial]
    fn set_config_value_writes_global_config_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);

        set_config_value("llm.provider", "openai").unwrap();
        set_config_value("llm.api_key", "sk-secret-1234").unwrap();
        set_config_value("llm.model", "gpt-4o").unwrap();
        set_config_value("llm.api_base", "https://api.example.com").unwrap();
        set_config_value("llm.max_tokens", "2048").unwrap();

        // The config file must be written under the isolated HOME.
        let config_path = global_config_path().expect("config path resolves");
        assert!(config_path.starts_with(temp_dir.path()));
        assert!(config_path.exists());

        // Verify the raw file content.
        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("provider = \"openai\""), "content: {content}");
        assert!(content.contains("api_key = \"sk-secret-1234\""), "content: {content}");
        assert!(content.contains("model = \"gpt-4o\""), "content: {content}");
        assert!(content.contains("api_base = \"https://api.example.com\""), "content: {content}");
        assert!(content.contains("max_tokens = 2048"), "content: {content}");

        // Round-trip: parse the written file back into a Config.
        let parsed: Config = toml::from_str(&content).unwrap();
        assert_eq!(parsed.llm.provider, "openai");
        assert_eq!(parsed.llm.api_key, "sk-secret-1234");
        assert_eq!(parsed.llm.model, "gpt-4o");
        assert_eq!(parsed.llm.api_base.as_deref(), Some("https://api.example.com"));
        assert_eq!(parsed.llm.max_tokens, 2048);

        // get_config_value reads the same values back (api_key is masked).
        assert_eq!(get_config_value(&parsed, "llm.provider").unwrap(), "openai");
        assert_eq!(get_config_value(&parsed, "llm.api_key").unwrap(), mask_key("sk-secret-1234"));
        assert_eq!(get_config_value(&parsed, "llm.model").unwrap(), "gpt-4o");
        assert_eq!(get_config_value(&parsed, "llm.api_base").unwrap(), "https://api.example.com");
        assert_eq!(get_config_value(&parsed, "llm.max_tokens").unwrap(), "2048");
    }

    #[test]
    #[serial]
    fn set_config_value_updates_existing_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);

        set_config_value("llm.provider", "openai").unwrap();
        set_config_value("llm.provider", "anthropic").unwrap();

        let parsed: Config =
            toml::from_str(&std::fs::read_to_string(global_config_path().unwrap()).unwrap())
                .unwrap();
        assert_eq!(parsed.llm.provider, "anthropic");
    }

    #[test]
    #[serial]
    fn set_config_value_rejects_unknown_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);

        let err = set_config_value("llm.bogus", "x").unwrap_err();
        assert!(err.to_string().contains("Unknown config key: llm.bogus"));
    }

    #[test]
    #[serial]
    fn set_config_value_rejects_invalid_numbers() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);

        assert!(set_config_value("llm.max_tokens", "abc").is_err());
        assert!(set_config_value("llm.temperature", "hot").is_err());
        assert!(set_config_value("agent.require_approval", "maybe").is_err());
        assert!(set_config_value("tools.enable_web", "y").is_err());
    }

    #[test]
    fn get_config_value_unknown_key_errors() {
        let cfg = Config::default();
        let err = get_config_value(&cfg, "llm.bogus").unwrap_err();
        assert!(err.to_string().contains("Unknown config key: llm.bogus"));
    }

    // ------------------------------------------------------------------
    // handle_command (Show / List / Get / Set)
    // ------------------------------------------------------------------

    #[test]
    #[serial]
    fn handle_command_show_succeeds() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);
        assert!(handle_command(ConfigAction::Show).is_ok());
    }

    #[test]
    fn handle_command_list_succeeds() {
        assert!(handle_command(ConfigAction::List).is_ok());
    }

    #[test]
    #[serial]
    fn handle_command_get_succeeds_and_fails_on_unknown_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);

        set_config_value("llm.provider", "openai").unwrap();
        assert!(handle_command(ConfigAction::Get { key: "llm.provider".into() }).is_ok());
        assert!(handle_command(ConfigAction::Get { key: "llm.api_key".into() }).is_ok());
        assert!(handle_command(ConfigAction::Get { key: "llm.model".into() }).is_ok());

        let err = handle_command(ConfigAction::Get { key: "unknown.key".into() }).unwrap_err();
        assert!(err.to_string().contains("Unknown config key: unknown.key"));
    }

    #[test]
    #[serial]
    fn handle_command_set_persists_to_disk() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);

        assert!(handle_command(ConfigAction::Set {
            key: "llm.provider".into(),
            value: "openai".into(),
        })
        .is_ok());

        let content = std::fs::read_to_string(global_config_path().unwrap()).unwrap();
        assert!(content.contains("provider = \"openai\""), "content: {content}");
    }
}
