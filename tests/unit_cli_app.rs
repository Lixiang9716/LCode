//! Unit tests migrated from `src/` for the CLI parsing, app routing, and
//! error-type modules.
//!
//! These were previously inline `#[cfg(test)]` modules in `src/cli.rs`,
//! `src/app.rs`, and `src/utils/mod.rs`; they exercise only the public API:
//!
//! 1. **CLI parsing** — clap-derived `Cli`/`Command`/`ConfigAction` parsing
//!    (default REPL mode, `run`/`config` subcommands, flag handling, and
//!    rejection of unknown subcommands).
//! 2. **App routing** — `app::run` dispatch for `Command::Config` and empty
//!    task validation. Environment state (`HOME`/`XDG_CONFIG_HOME`) is
//!    isolated with `serial_test` and a fresh temp dir.
//! 3. **Error types** — `LCodeError` Display formatting and the `Result`
//!    alias.

use clap::Parser;
use serial_test::serial;

use lcode::app::run;
use lcode::cli::{Cli, Command, ConfigAction};
use lcode::config::{global_config_path, Config};
use lcode::utils::{LCodeError, Result};

// ---------------------------------------------------------------------------
// CLI parsing (migrated from src/cli.rs)
// ---------------------------------------------------------------------------

#[test]
fn no_arguments_means_default_repl() {
    let cli = Cli::try_parse_from(["lcode"]).unwrap();
    assert!(cli.command.is_none());
    assert!(!cli.verbose);
    assert_eq!(cli.project, ".");
    assert!(cli.config_file.is_none());
}

#[test]
fn run_command_single_quoted_task() {
    let cli = Cli::try_parse_from(["lcode", "run", "add tests"]).unwrap();
    match cli.command {
        Some(Command::Run { task, .. }) => assert_eq!(task, vec!["add tests"]),
        other => panic!("expected Run command, got {other:?}"),
    }
}

#[test]
fn run_command_joins_multiple_task_words() {
    let cli = Cli::try_parse_from(["lcode", "run", "add", "tests", "now"]).unwrap();
    match cli.command {
        Some(Command::Run { task, .. }) => assert_eq!(task, vec!["add", "tests", "now"]),
        other => panic!("expected Run command, got {other:?}"),
    }
}

#[test]
fn run_y_flag_sets_auto_approve() {
    let cli = Cli::try_parse_from(["lcode", "run", "-y", "task"]).unwrap();
    match cli.command {
        Some(Command::Run { auto_approve, .. }) => assert!(auto_approve),
        other => panic!("expected Run command, got {other:?}"),
    }
}

#[test]
fn run_n_flag_sets_max_turns() {
    let cli = Cli::try_parse_from(["lcode", "run", "-n", "10", "task"]).unwrap();
    match cli.command {
        Some(Command::Run { max_turns, .. }) => assert_eq!(max_turns, 10),
        other => panic!("expected Run command, got {other:?}"),
    }
}

#[test]
fn run_default_max_turns_and_auto_approve() {
    let cli = Cli::try_parse_from(["lcode", "run", "task"]).unwrap();
    match cli.command {
        Some(Command::Run { max_turns, auto_approve, .. }) => {
            assert_eq!(max_turns, 50);
            assert!(!auto_approve);
        }
        other => panic!("expected Run command, got {other:?}"),
    }
}

#[test]
fn config_show_action() {
    let cli = Cli::try_parse_from(["lcode", "config", "show"]).unwrap();
    match cli.command {
        Some(Command::Config { action }) => assert!(matches!(action, ConfigAction::Show)),
        other => panic!("expected Config command, got {other:?}"),
    }
}

#[test]
fn config_list_action() {
    let cli = Cli::try_parse_from(["lcode", "config", "list"]).unwrap();
    match cli.command {
        Some(Command::Config { action }) => assert!(matches!(action, ConfigAction::List)),
        other => panic!("expected Config command, got {other:?}"),
    }
}

#[test]
fn config_get_action() {
    let cli = Cli::try_parse_from(["lcode", "config", "get", "llm.model"]).unwrap();
    match cli.command {
        Some(Command::Config { action }) => match action {
            ConfigAction::Get { key } => assert_eq!(key, "llm.model"),
            other => panic!("expected Get action, got {other:?}"),
        },
        other => panic!("expected Config command, got {other:?}"),
    }
}

#[test]
fn config_set_action() {
    let cli =
        Cli::try_parse_from(["lcode", "config", "set", "llm.provider", "openai"]).unwrap();
    match cli.command {
        Some(Command::Config { action }) => match action {
            ConfigAction::Set { key, value } => {
                assert_eq!(key, "llm.provider");
                assert_eq!(value, "openai");
            }
            other => panic!("expected Set action, got {other:?}"),
        },
        other => panic!("expected Config command, got {other:?}"),
    }
}

#[test]
fn repl_command_with_prompt() {
    let cli = Cli::try_parse_from(["lcode", "repl", "--prompt", "hello"]).unwrap();
    match cli.command {
        Some(Command::Repl { prompt }) => assert_eq!(prompt.as_deref(), Some("hello")),
        other => panic!("expected Repl command, got {other:?}"),
    }
}

#[test]
fn global_project_flag_parses() {
    let cli = Cli::try_parse_from(["lcode", "--project", "/tmp/proj", "run", "task"]).unwrap();
    assert_eq!(cli.project, "/tmp/proj");
}

#[test]
fn global_verbose_flag_parses() {
    let cli = Cli::try_parse_from(["lcode", "-v", "config", "list"]).unwrap();
    assert!(cli.verbose);
}

#[test]
fn unknown_subcommand_is_rejected() {
    assert!(Cli::try_parse_from(["lcode", "bogus"]).is_err());
}

// ---------------------------------------------------------------------------
// App routing (migrated from src/app.rs)
// ---------------------------------------------------------------------------

/// Run the async `run()` on a fresh single-threaded runtime.
fn run_blocking(cli: Cli, cfg: Config) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()
        .expect("failed to create tokio runtime")
        .block_on(run(cli, cfg))
}

fn cli_with(command: Command) -> Cli {
    Cli { command: Some(command), verbose: false, project: ".".to_string(), config_file: None }
}

/// Point $HOME at a fresh temp dir so `dirs::config_dir()` resolves to an
/// isolated, empty location. Must be called from a `#[serial]` test.
fn isolate_home(temp_dir: &tempfile::TempDir) {
    std::env::set_var("HOME", temp_dir.path());
    std::env::remove_var("XDG_CONFIG_HOME");
}

// ------------------------------------------------------------------
// Command::Run validation
// ------------------------------------------------------------------

#[test]
fn run_with_empty_task_returns_error() {
    let cli = cli_with(Command::Run { task: vec![], max_turns: 50, auto_approve: false });
    let err = run_blocking(cli, Config::default()).unwrap_err();
    assert!(
        err.to_string().contains("Task description cannot be empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn run_with_whitespace_only_task_returns_error() {
    let cli = cli_with(Command::Run {
        task: vec!["   ".to_string()],
        max_turns: 50,
        auto_approve: false,
    });
    assert!(run_blocking(cli, Config::default()).is_err());
}

// ------------------------------------------------------------------
// Command::Config routing (no network involved)
// ------------------------------------------------------------------

#[test]
#[serial]
fn run_config_show_routes_to_config_module() {
    let temp_dir = tempfile::tempdir().unwrap();
    isolate_home(&temp_dir);

    let cli = cli_with(Command::Config { action: ConfigAction::Show });
    assert!(run_blocking(cli, Config::default()).is_ok());
}

#[test]
fn run_config_list_routes_to_config_module() {
    let cli = cli_with(Command::Config { action: ConfigAction::List });
    assert!(run_blocking(cli, Config::default()).is_ok());
}

#[test]
#[serial]
fn run_config_get_routes_to_config_module() {
    let temp_dir = tempfile::tempdir().unwrap();
    isolate_home(&temp_dir);

    let cli = cli_with(Command::Config {
        action: ConfigAction::Get { key: "llm.provider".to_string() },
    });
    assert!(run_blocking(cli, Config::default()).is_ok());

    // Unknown keys surface as errors through the same route.
    let cli = cli_with(Command::Config {
        action: ConfigAction::Get { key: "does.not.exist".to_string() },
    });
    let err = run_blocking(cli, Config::default()).unwrap_err();
    assert!(err.to_string().contains("Unknown config key"), "unexpected error: {err}");
}

#[test]
#[serial]
fn run_config_set_routes_to_config_module_and_persists() {
    let temp_dir = tempfile::tempdir().unwrap();
    isolate_home(&temp_dir);

    let cli = cli_with(Command::Config {
        action: ConfigAction::Set {
            key: "llm.provider".to_string(),
            value: "openai".to_string(),
        },
    });
    assert!(run_blocking(cli, Config::default()).is_ok());

    let content = std::fs::read_to_string(global_config_path().unwrap()).unwrap();
    assert!(content.contains("provider = \"openai\""), "content: {content}");
}

// ---------------------------------------------------------------------------
// Error types (migrated from src/utils/mod.rs)
// ---------------------------------------------------------------------------

#[test]
fn config_error_display_message() {
    let err = LCodeError::Config("invalid provider".to_string());
    assert_eq!(err.to_string(), "Configuration error: invalid provider");
}

#[test]
fn llm_api_error_display_message() {
    let err = LCodeError::LlmApi("rate limited".to_string());
    assert_eq!(err.to_string(), "LLM API error: rate limited");
}

#[test]
fn tool_execution_error_display_message() {
    let err = LCodeError::ToolExecution("command failed".to_string());
    assert_eq!(err.to_string(), "Tool execution error: command failed");
}

#[test]
fn io_error_display_message() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let err = LCodeError::Io(io_err);
    assert_eq!(err.to_string(), "I/O error: file not found");
}

#[test]
fn io_error_via_from_conversion() {
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
    let err: LCodeError = io_err.into();
    assert_eq!(err.to_string(), "I/O error: permission denied");
}

#[test]
fn serde_error_via_from_conversion() {
    let serde_err = serde_json::from_str::<serde_json::Value>("{invalid").unwrap_err();
    let err: LCodeError = serde_err.into();
    assert!(err.to_string().starts_with("Serialization error: "));
}

#[test]
fn agent_error_display_message() {
    let err = LCodeError::Agent("loop exceeded".to_string());
    assert_eq!(err.to_string(), "Agent error: loop exceeded");
}

#[test]
fn result_alias_carries_lcode_error() {
    fn ok_fn() -> Result<i32> {
        Ok(42)
    }
    fn err_fn() -> Result<i32> {
        Err(LCodeError::Config("boom".to_string()))
    }
    assert_eq!(ok_fn().unwrap(), 42);
    assert!(matches!(err_fn(), Err(LCodeError::Config(msg)) if msg == "boom"));
}
