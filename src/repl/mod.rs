//! Interactive REPL (Read-Eval-Print Loop) for LCode.
//!
//! Provides an interactive terminal interface where users can:
//! - Chat with the agent
//! - Request code changes
//! - Approve/reject tool calls
//! - Manage sessions

use crate::agent::{ConversationMemory, SessionSnapshot, SessionStore};
use crate::config::Config;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

/// Start the interactive REPL.
pub async fn start(initial_prompt: Option<String>, config: Config) -> anyhow::Result<()> {
    println!();
    println!("╔══════════════════════════════════════════╗");
    println!("║        🤖  LCode Code Agent  🚀          ║");
    println!("║  Type /help for commands, /quit to exit   ║");
    println!("╚══════════════════════════════════════════╝");
    println!();

    let mut editor = DefaultEditor::new()?;
    let _ = editor.load_history(".lcode_history");

    // If there's an initial prompt, process it immediately
    if let Some(prompt) = initial_prompt {
        println!("> {}", prompt);
        process_input(&prompt, &config).await?;
    }

    loop {
        let readline = editor.readline("lcode> ");

        match readline {
            Ok(line) => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    continue;
                }

                // Add to history
                let _ = editor.add_history_entry(trimmed);

                // Handle the line (command or task); Quit breaks the loop
                match handle_line(trimmed, &config).await {
                    Ok(ExitStatus::Quit) => break,
                    Ok(ExitStatus::Continue) => continue,
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C (type /quit to exit)");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("Goodbye! 👋");
                break;
            }
            Err(err) => {
                eprintln!("REPL error: {}", err);
                break;
            }
        }
    }

    let _ = editor.save_history(".lcode_history");
    Ok(())
}

/// Process a single REPL line: slash command or task description.
async fn handle_line(line: &str, config: &Config) -> anyhow::Result<ExitStatus> {
    if line.starts_with('/') {
        handle_command(line, config).await
    } else {
        process_input(line, config).await?;
        Ok(ExitStatus::Continue)
    }
}

/// Exit status from command handlers.
enum ExitStatus {
    Quit,
    Continue,
}

/// Handle slash commands like /help, /quit, /config, etc.
async fn handle_command(input: &str, config: &Config) -> anyhow::Result<ExitStatus> {
    let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
    let cmd = parts[0];
    let args = parts.get(1).unwrap_or(&"");

    match cmd {
        "help" | "h" => {
            print_help();
            Ok(ExitStatus::Continue)
        }
        "quit" | "q" | "exit" => {
            println!("Goodbye! 👋");
            Ok(ExitStatus::Quit)
        }
        "clear" | "c" => {
            print!("\x1B[2J\x1B[1;1H");
            Ok(ExitStatus::Continue)
        }
        "config" => {
            show_config()?;
            Ok(ExitStatus::Continue)
        }
        "tools" => {
            print_tools();
            Ok(ExitStatus::Continue)
        }
        "model" => {
            println!("Current model: {}", config.llm.model);
            println!("Provider: {}", config.llm.provider);
            Ok(ExitStatus::Continue)
        }
        "save" | "resume" | "sessions" => {
            handle_session_command(cmd, args, config).await?;
            Ok(ExitStatus::Continue)
        }
        "" => {
            println!("Type /help for available commands");
            Ok(ExitStatus::Continue)
        }
        unknown => {
            println!("Unknown command: /{} — type /help for available commands", unknown);
            Ok(ExitStatus::Continue)
        }
    }
}

/// Show the current configuration as TOML.
fn show_config() -> anyhow::Result<()> {
    let cfg = crate::config::load()?;
    println!("{}", toml::to_string_pretty(&cfg)?);
    Ok(())
}

/// Print the list of available tools.
fn print_tools() {
    println!("Available tools:");
    println!("  read_file   - Read a file's contents");
    println!("  write_file  - Write content to a file");
    println!("  edit_file   - Edit a file with find-and-replace");
    println!("  list_dir    - List directory contents");
    println!("  grep        - Search file contents");
    println!("  glob        - Find files by pattern");
    println!("  shell       - Execute shell commands");
}

/// Handle the session-related slash commands: `/save [--id <id>] <task>`,
/// `/resume <id>`, and `/sessions`.
async fn handle_session_command(cmd: &str, args: &str, config: &Config) -> anyhow::Result<()> {
    match cmd {
        "save" => save_session(args),
        "resume" => resume_session(args, config).await,
        "sessions" => list_sessions(),
        _ => unreachable!("only save/resume/sessions reach this handler"),
    }
}

/// `/save [--id <id>] <task description>` — store a session snapshot.
/// Errors are printed rather than propagated so the REPL keeps running.
fn save_session(args: &str) -> anyhow::Result<()> {
    // Optional `--id <id>` flag, then the task description.
    let (id, task_desc) = if let Some(rest) = args.strip_prefix("--id ") {
        match rest.split_once(' ') {
            Some((id, task)) => (Some(id.to_string()), task.to_string()),
            None => (Some(rest.to_string()), String::new()),
        }
    } else {
        (None, args.to_string())
    };
    if task_desc.trim().is_empty() {
        println!("Usage: /save [--id <id>] <task description>");
        return Ok(());
    }
    let store = SessionStore::new(&std::env::current_dir()?);
    match store.save(&SessionSnapshot::empty(task_desc, id)) {
        Ok(saved_id) => {
            println!("Session saved: {saved_id}");
            println!("Resume later with: /resume {saved_id}");
        }
        Err(e) => println!("Error: {e}"),
    }
    Ok(())
}

/// `/resume <id>` — load a saved session and continue its task with the
/// restored conversation history.
async fn resume_session(args: &str, config: &Config) -> anyhow::Result<()> {
    let id = args.trim();
    if id.is_empty() {
        println!("Usage: /resume <session id> (see /sessions)");
        return Ok(());
    }
    let store = SessionStore::new(&std::env::current_dir()?);
    match store.load(id) {
        Ok(snapshot) => {
            println!("Resuming session {id}: {}", snapshot.task);
            let memory = ConversationMemory::from_messages(
                config.agent.system_prompt.clone(),
                snapshot.messages,
            );
            if let Err(e) = crate::agent::run_task_with_memory(
                &snapshot.task,
                config.agent.max_turns,
                config.agent.require_approval,
                config,
                Some(memory),
            )
            .await
            {
                eprintln!("Error: {e}");
            }
        }
        Err(e) => println!("Error: {e}"),
    }
    Ok(())
}

/// `/sessions` — list saved sessions, newest first.
fn list_sessions() -> anyhow::Result<()> {
    let store = SessionStore::new(&std::env::current_dir()?);
    let sessions = store.list();
    if sessions.is_empty() {
        println!("No saved sessions.");
        return Ok(());
    }
    println!("{:<12} {:<12} TASK", "ID", "CREATED");
    for session in sessions {
        println!("{:<12} {:<12} {}", session.id, session.created_at, session.task);
    }
    Ok(())
}

/// Print help information for the REPL.
fn print_help() {
    println!();
    println!("LCode REPL Help");
    println!("══════════════");
    println!();
    println!("Type any task description to start the agent:");
    println!("  > Add unit tests for the UserService class");
    println!("  > Refactor the auth module to use async/await");
    println!();
    println!("Slash commands:");
    println!("  /help, /h       - Show this help");
    println!("  /quit, /q       - Exit LCode");
    println!("  /clear, /c      - Clear the screen");
    println!("  /config         - Show current configuration");
    println!("  /tools          - List available tools");
    println!("  /model          - Show current model/provider");
    println!("  /save [--id]    - Save a task as a session snapshot");
    println!("  /resume <id>    - Resume a saved session");
    println!("  /sessions       - List saved sessions");
    println!();
    println!("Key bindings:");
    println!("  Ctrl+C          - Interrupt current operation");
    println!("  Ctrl+D          - Exit (on empty line)");
    println!("  Up/Down arrows  - Navigate command history");
    println!();
}

/// Process a user input as a task.
async fn process_input(input: &str, config: &Config) -> anyhow::Result<()> {
    // The REPL stays on the plain (non-streamed) path; `lcode run
    // --stream` enables the typewriter effect (G11).
    crate::agent::run_task(
        input,
        config.agent.max_turns,
        config.agent.require_approval,
        false,
        config,
    )
    .await
}
