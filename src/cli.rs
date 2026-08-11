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
        #[arg(short, long)]
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
