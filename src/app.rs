//! Application orchestration layer.
//!
//! Routes CLI commands to the appropriate handler: REPL, single-shot run, or config management.

use crate::agent::{ConversationMemory, SessionSnapshot, SessionStore};
use crate::cli::{Cli, Command, SessionAction};
use crate::config::Config;

/// Main application runner — dispatches based on the parsed CLI command.
pub async fn run(args: Cli, cfg: Config) -> anyhow::Result<()> {
    match args.command.unwrap_or(Command::Repl { prompt: None }) {
        Command::Repl { prompt } => {
            crate::repl::start(prompt, cfg).await?;
        }
        Command::Run { task, max_turns, auto_approve, stream, resume } => {
            if resume {
                return handle_resume(auto_approve, stream, &cfg).await;
            }
            let task_desc = task.join(" ");
            if task_desc.trim().is_empty() {
                anyhow::bail!("Task description cannot be empty. Usage: lcode run \"<task>\"");
            }
            tracing::info!(task = %task_desc, max_turns, stream, "Starting single-shot task");
            let outcome =
                crate::agent::run_task(&task_desc, max_turns, auto_approve, stream, &cfg).await?;
            if !outcome.completed {
                // Non-zero exit so scripts can distinguish an aborted
                // session (e.g. max turns) from a finished one.
                std::process::exit(2);
            }
        }
        Command::Config { action } => {
            crate::config::handle_command(action)?;
        }
        Command::Session { action } => {
            handle_session(action, cfg).await?;
        }
        Command::Update { check, force } => {
            crate::update::run(check, force).await?;
        }
        Command::Assets { action } => {
            handle_assets(action)?;
        }
        Command::Doctor => {
            crate::doctor::run(&cfg).await?;
        }
        Command::Events { last } => {
            let dir = crate::agent::workspace_root()?.join(".transcripts");
            let report = crate::events::analyze(&dir, last)?;
            println!("{}", crate::events::render(&report));
        }
    }
    Ok(())
}

/// Handle the `lcode assets` subcommands.
fn handle_assets(action: crate::cli::AssetsAction) -> anyhow::Result<()> {
    match action {
        crate::cli::AssetsAction::Check => {
            let report = crate::assets::check(&std::env::current_dir()?)?;
            println!("{}", report.render());
            if !report.ok() {
                anyhow::bail!("asset check found {} issue(s)", report.issues.len());
            }
            Ok(())
        }
    }
}

/// `lcode run --resume`: continue the latest checkpoint (P1).
async fn handle_resume(auto_approve: bool, stream: bool, cfg: &Config) -> anyhow::Result<()> {
    let store = crate::agent::CheckpointStore::new(&crate::agent::workspace_root()?);
    let Some(checkpoint) = store.load() else {
        anyhow::bail!(
            "no checkpoint to resume (the last run either finished or checkpointing is disabled)"
        );
    };
    tracing::info!(turns = checkpoint.turns_used, "Resuming from checkpoint");
    let outcome = crate::agent::run_task_resume(checkpoint, auto_approve, stream, cfg).await?;
    if !outcome.completed {
        std::process::exit(2);
    }
    Ok(())
}

/// Handle the `lcode session` subcommands (save / list / resume).
async fn handle_session(action: SessionAction, cfg: Config) -> anyhow::Result<()> {
    let store = SessionStore::new(&crate::agent::workspace_root()?);
    match action {
        SessionAction::Save { task, id } => {
            let task_desc = task.join(" ");
            if task_desc.trim().is_empty() {
                anyhow::bail!(
                    "Task description cannot be empty. Usage: lcode session save \"<task>\" [--id <id>]"
                );
            }
            let saved_id = store.save(&SessionSnapshot::empty(task_desc, id))?;
            println!("Session saved: {saved_id}");
            println!("Resume later with: lcode session resume {saved_id}");
        }
        SessionAction::List => {
            let sessions = store.list();
            if sessions.is_empty() {
                println!("No saved sessions.");
                return Ok(());
            }
            println!("{:<12} {:<12} TASK", "ID", "CREATED");
            for session in sessions {
                println!("{:<12} {:<12} {}", session.id, session.created_at, session.task);
            }
        }
        SessionAction::Resume { id } => {
            let snapshot = store.load(&id)?;
            tracing::info!(session = %id, task = %snapshot.task, "Resuming session");
            let memory = ConversationMemory::from_messages(
                cfg.agent.system_prompt.clone(),
                snapshot.messages,
            );
            // `require_approval` negates auto-approve (same inversion
            // bug class as the REPL fix).
            let outcome = crate::agent::run_task_with_memory(
                &snapshot.task,
                cfg.agent.max_turns,
                !cfg.agent.require_approval,
                false,
                &cfg,
                Some(memory),
                None,
            )
            .await?;
            if !outcome.completed {
                std::process::exit(2);
            }
        }
    }
    Ok(())
}
