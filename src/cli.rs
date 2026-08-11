//! CLI argument parsing using clap derive macros.
//!
//! Supports three modes:
//! - **interactive**: Launch the REPL (default)
//! - **run**: Execute a single-shot task
//! - **config**: Manage configuration settings

use clap::{Parser, Subcommand};

/// LCode - Your AI-powered code agent
#[derive(Parser, Debug)]
#[command(name = "lcode", version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Path to project root (defaults to current directory)
    #[arg(short, long, global = true, default_value = ".")]
    pub project: String,

    /// Path to config file
    #[arg(short = 'C', long, global = true)]
    pub config_file: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Launch interactive REPL mode (default)
    Repl {
        /// Initial prompt to load in the REPL
        #[arg(long)]
        prompt: Option<String>,
    },

    /// Execute a single-shot coding task
    Run {
        /// The task description to execute
        task: Vec<String>,

        /// Maximum turns before stopping
        #[arg(short = 'n', long, default_value = "50")]
        max_turns: u32,

        /// Auto-approve all tool calls (use with caution!)
        #[arg(short = 'y', long)]
        auto_approve: bool,
    },

    /// Manage LCode configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Show current configuration
    Show,

    /// Set a configuration value
    Set {
        /// Configuration key
        key: String,
        /// Configuration value
        value: String,
    },

    /// Get a configuration value
    Get {
        /// Configuration key
        key: String,
    },

    /// List all available configuration keys
    List,
}

/// Parse CLI arguments and return the structured result.
pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some(Command::Run {
                max_turns,
                auto_approve,
                ..
            }) => {
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
        let cli = Cli::try_parse_from(["lcode", "config", "set", "llm.provider", "openai"]).unwrap();
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
}
