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

    // Run the application
    app::run(args, cfg).await
}

/// Set up structured logging with env-filter support.
///
/// Set `RUST_LOG` to control log levels, e.g.:
/// - `RUST_LOG=lcode=debug` for debug output
/// - `RUST_LOG=info` for info and above
fn setup_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let layer = fmt::layer()
        .with_target(false)
        .with_file(true)
        .with_line_number(true);

    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .init();
}
