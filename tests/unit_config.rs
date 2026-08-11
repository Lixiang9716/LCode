//! Unit tests for the `config` module.
//!
//! These tests were moved verbatim from the inline `#[cfg(test)]` modules in
//! `src/config/{settings,mod,commands}.rs` so that `src/` contains no test
//! code. They exercise the public API of `lcode::config` (including the
//! `#[doc(hidden)] pub` helpers re-exported for testing purposes).

use lcode::cli::ConfigAction;
use lcode::config::*;
use serial_test::serial;

// ---------------------------------------------------------------------------
// Default configuration values (settings)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// merge_config
// ---------------------------------------------------------------------------

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
    other.agent.context_size = default_context_size();
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

// ---------------------------------------------------------------------------
// apply_env_overrides (LCODE_* variables)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// mask_key
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// set_config_value / get_config_value (HOME-isolated)
// ---------------------------------------------------------------------------

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
        toml::from_str(&std::fs::read_to_string(global_config_path().unwrap()).unwrap()).unwrap();
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

// ---------------------------------------------------------------------------
// handle_command (Show / List / Get / Set)
// ---------------------------------------------------------------------------

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
