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
        Command::Run { task, max_turns, auto_approve } => {
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
