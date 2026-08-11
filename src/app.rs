//! Application orchestration layer.
//!
//! Routes CLI commands to the appropriate handler: REPL, single-shot run, or config management.

use crate::cli::{Cli, Command};
use crate::config::Config;

/// Main application runner — dispatches based on the parsed CLI command.
pub async fn run(args: Cli, cfg: Config) -> anyhow::Result<()> {
    match args.command.unwrap_or(Command::Repl { prompt: None }) {
        Command::Repl { prompt } => {
            crate::repl::start(prompt, cfg).await?;
        }
        Command::Run {
            task,
            max_turns,
            auto_approve,
        } => {
            let task_desc = task.join(" ");
            if task_desc.trim().is_empty() {
                anyhow::bail!("Task description cannot be empty. Usage: lcode run \"<task>\"");
            }
            tracing::info!(task = %task_desc, max_turns, "Starting single-shot task");
            crate::agent::run_task(&task_desc, max_turns, auto_approve, &cfg).await?;
        }
        Command::Config { action } => {
            crate::config::handle_command(action)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, ConfigAction};
    use serial_test::serial;

    /// Run the async `run()` on a fresh single-threaded runtime.
    fn run_blocking(cli: Cli, cfg: Config) -> anyhow::Result<()> {
        tokio::runtime::Runtime::new()
            .expect("failed to create tokio runtime")
            .block_on(run(cli, cfg))
    }

    fn cli_with(command: Command) -> Cli {
        Cli {
            command: Some(command),
            verbose: false,
            project: ".".to_string(),
            config_file: None,
        }
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
        let cli = cli_with(Command::Run {
            task: vec![],
            max_turns: 50,
            auto_approve: false,
        });
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

        let cli = cli_with(Command::Config {
            action: ConfigAction::Show,
        });
        assert!(run_blocking(cli, Config::default()).is_ok());
    }

    #[test]
    fn run_config_list_routes_to_config_module() {
        let cli = cli_with(Command::Config {
            action: ConfigAction::List,
        });
        assert!(run_blocking(cli, Config::default()).is_ok());
    }

    #[test]
    #[serial]
    fn run_config_get_routes_to_config_module() {
        let temp_dir = tempfile::tempdir().unwrap();
        isolate_home(&temp_dir);

        let cli = cli_with(Command::Config {
            action: ConfigAction::Get {
                key: "llm.provider".to_string(),
            },
        });
        assert!(run_blocking(cli, Config::default()).is_ok());

        // Unknown keys surface as errors through the same route.
        let cli = cli_with(Command::Config {
            action: ConfigAction::Get {
                key: "does.not.exist".to_string(),
            },
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

        let content =
            std::fs::read_to_string(crate::config::global_config_path().unwrap()).unwrap();
        assert!(content.contains("provider = \"openai\""), "content: {content}");
    }
}
