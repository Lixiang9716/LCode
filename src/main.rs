//! LCode - A powerful CLI code agent for autonomous software development.
//!
//! LCode is an AI-powered coding assistant that can:
//! - Understand your codebase
//! - Plan and execute complex development tasks
//! - Write, edit, search, and manage files
//! - Execute shell commands safely
//!
//! # Quick Start
//!
//! ```bash
//! # Interactive mode
//! lcode
//!
//! # Single-shot task
//! lcode run "Add unit tests for the auth module"
//!
//! # Configure LLM provider
//! lcode config set provider openai
//! lcode config set api-key sk-...
//! ```

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use lcode::{app, cli, config};

/// Application entry point.
///
/// Initializes logging, parses CLI arguments, and runs the appropriate mode:
/// interactive REPL, single-shot task, or configuration management.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing/logging
    setup_logging();

    // Parse CLI arguments
    let args = cli::parse();

    // Load configuration
    let cfg = config::load()?;

    // Run the application, racing it against Ctrl+C so an interrupt
    // shuts the process down gracefully. In a full implementation the
    // interrupt would publish `AgentEvent::TaskAborted` and send
    // `AgentCommand::Abort` to the session runtime so in-flight work can
    // drain; here returning `Ok` drops the tokio runtime, which cancels
    // the agent tasks (including the spawned stdout renderer) — enough
    // for the CLI.
    tokio::select! {
        result = app::run(args, cfg) => result,
        _ = tokio::signal::ctrl_c() => {
            println!("\n👋 Received interrupt, shutting down gracefully...");
            Ok(())
        }
    }
}

/// Set up structured logging with env-filter support.
///
/// Set `RUST_LOG` to control log levels, e.g.:
/// - `RUST_LOG=lcode=debug` for debug output
/// - `RUST_LOG=info` for info and above
fn setup_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let layer = fmt::layer().with_target(false).with_file(true).with_line_number(true);

    tracing_subscriber::registry().with(filter).with(layer).init();
}
