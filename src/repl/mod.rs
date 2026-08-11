//! Interactive REPL (Read-Eval-Print Loop) for LCode.
//!
//! Provides an interactive terminal interface where users can:
//! - Chat with the agent
//! - Request code changes
//! - Approve/reject tool calls
//! - Manage sessions

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
        let prompt = "lcode> ";
        let readline = editor.readline(prompt);

        match readline {
            Ok(line) => {
                let trimmed = line.trim();

                if trimmed.is_empty() {
                    continue;
                }

                // Add to history
                let _ = editor.add_history_entry(trimmed);

                // Handle commands
                if trimmed.starts_with('/') {
                    match handle_command(trimmed, &config).await {
                        Ok(ExitStatus::Quit) => break,
                        Ok(ExitStatus::Continue) => continue,
                        Err(e) => eprintln!("Error: {}", e),
                    }
                } else {
                    // Process as a task
                    if let Err(e) = process_input(trimmed, &config).await {
                        eprintln!("Error: {}", e);
                    }
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

/// Exit status from command handlers.
enum ExitStatus {
    Quit,
    Continue,
}

/// Handle slash commands like /help, /quit, /config, etc.
async fn handle_command(input: &str, config: &Config) -> anyhow::Result<ExitStatus> {
    let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
    let cmd = parts[0];
    let _args = parts.get(1).unwrap_or(&"");

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
            // Show config
            let cfg = crate::config::load()?;
            println!("{}", toml::to_string_pretty(&cfg)?);
            Ok(ExitStatus::Continue)
        }
        "tools" => {
            println!("Available tools:");
            println!("  read_file   - Read a file's contents");
            println!("  write_file  - Write content to a file");
            println!("  edit_file   - Edit a file with find-and-replace");
            println!("  list_dir    - List directory contents");
            println!("  grep        - Search file contents");
            println!("  glob        - Find files by pattern");
            println!("  shell       - Execute shell commands");
            Ok(ExitStatus::Continue)
        }
        "model" => {
            println!("Current model: {}", config.llm.model);
            println!("Provider: {}", config.llm.provider);
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
    println!();
    println!("Key bindings:");
    println!("  Ctrl+C          - Interrupt current operation");
    println!("  Ctrl+D          - Exit (on empty line)");
    println!("  Up/Down arrows  - Navigate command history");
    println!();
}

/// Process a user input as a task.
async fn process_input(input: &str, config: &Config) -> anyhow::Result<()> {
    crate::agent::run_task(input, config.agent.max_turns, config.agent.require_approval, config)
        .await
}
