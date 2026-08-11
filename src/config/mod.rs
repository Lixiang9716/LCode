//! Configuration management for LCode.
//!
//! Configuration is loaded from (in order of precedence):
//! 1. Command-line arguments
//! 2. Environment variables (LCODE_ prefix)
//! 3. Project-local `.lcode.toml`
//! 4. User-global `~/.config/lcode/config.toml`

use crate::cli::ConfigAction;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for Config {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            agent: AgentConfig::default(),
            tools: ToolsConfig::default(),
        }
    }
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

// Default value functions
fn default_provider() -> String {
    "anthropic".into()
}
fn default_model() -> String {
    "claude-sonnet-4-20250514".into()
}
fn default_max_tokens() -> u32 {
    8192
}
fn default_temperature() -> f32 {
    0.3
}
fn default_system_prompt() -> String {
    "You are LCode, an expert software engineer and coding agent. \
     You help users write, review, debug, and understand code. \
     Be concise, accurate, and helpful.".into()
}
fn default_max_turns() -> u32 {
    100
}
fn default_require_approval() -> bool {
    true
}
fn default_context_size() -> usize {
    128000
}
fn default_true() -> bool {
    true
}

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
    let config_path = global_config_path()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;

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
        _ => anyhow::bail!("Unknown config key: {}. Use `lcode config list` to see available keys.", key),
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
